//! The loop server: `show` and `list --live` answered from one event-loop thread on a second
//! socket. One task polls the listener and every connection; whatever blocks (the store, `/proc`,
//! the personas file, the span) runs on the loop's own `Pool` and is awaited, never run here.

use std::future::Future;
use std::io::ErrorKind;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use runtime::offload::{Offloaded, Pool};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::time::Sleep;

use crate::callers::Answer;
use crate::serve::{Called, Cut, Door, Peer, Request, Unread};

const WHAT: &str = "lc-loop";
pub(crate) const REFUSED_VERB: &str = "cannot tell: the loop serves show and list --live";
const TICK: Duration = Duration::from_millis(50);
const CHUNK: usize = 8 << 10;

/// What the loop answers: `show`, and `list` with `--live`.
pub(crate) fn serves(argv: &[String]) -> bool {
    match argv.first().map(String::as_str) {
        Some("show") => true,
        Some("list") => argv.iter().any(|a| a == "--live"),
        _ => false,
    }
}

/// Counts every job of the loop's pool that has ended, and wakes the loop when one does.
#[derive(Default)]
pub(crate) struct Finished {
    count: AtomicU64,
    wake: Notify,
}

struct Signal(Arc<Finished>);

impl Drop for Signal {
    fn drop(&mut self) {
        self.0.count.fetch_add(1, Ordering::SeqCst);
        self.0.wake.notify_one();
    }
}

pub(crate) struct Shared {
    pub(crate) door: Arc<Door>,
    pub(crate) pool: Arc<Pool>,
    pub(crate) spans: Option<&'static spira_otel::Sink>,
    pub(crate) finished: Arc<Finished>,
}

impl Shared {
    pub(crate) fn new(door: Arc<Door>, pool: Arc<Pool>, spans: Option<&'static spira_otel::Sink>) -> Shared {
        Shared { door, pool, spans, finished: Arc::default() }
    }
}

/// What follows the answer, as a job of its own.
enum Post {
    Nothing,
    Unread { size: usize, msg: String },
    Served { called: Box<Called>, size: usize },
}

/// A request read and not yet answered, and the clock it must be answered by.
struct Held {
    req: Request,
    size: usize,
    limit: Duration,
    due: Pin<Box<Sleep>>,
}

enum Phase {
    Reading(Vec<u8>),
    Deciding { held: Held, tried: Option<u64> },
    Calling { held: Held, job: Offloaded<Called> },
    Answering { out: Vec<u8>, sent: usize, post: Post },
    Closed,
}

enum Step {
    Next(Phase),
    Wait(Phase),
    Done,
}

struct Conn {
    stream: UnixStream,
    accepted: Instant,
    started: SystemTime,
    peer: Peer,
    read_at: Instant,
    phase: Phase,
}

pub(crate) enum Parsed {
    Need,
    Nothing,
    Ready(Request, usize),
    Bad(Unread),
}

/// The request in `buf`, if it is all there: `parse_head` of its first line, then the `len`
/// bytes it names. At `eof` what is there is all there will be.
pub(crate) fn parse(buf: &[u8], eof: bool) -> Parsed {
    let end = match buf.iter().position(|b| *b == b'\n') {
        Some(n) => n + 1,
        None if eof => buf.len(),
        None => return Parsed::Need,
    };
    if end == 0 {
        return Parsed::Nothing;
    }
    let Ok(line) = std::str::from_utf8(&buf[..end]) else { return Parsed::Nothing };
    let head = match crate::serve::parse_head(line) {
        Ok(h) => h,
        Err(e) => return Parsed::Bad(e),
    };
    let body = match head.len {
        None => None,
        Some(n) => {
            let have = buf.len() - end;
            if (have as u64) < n {
                if !eof {
                    return Parsed::Need;
                }
                return Parsed::Bad(crate::serve::body_unread(head.line_len, n, &"failed to fill whole buffer"));
            }
            Some(buf[end..end + n as usize].to_vec())
        }
    };
    match crate::serve::finish_request(head, body) {
        Ok((req, size)) => Parsed::Ready(req, size),
        Err(e) => Parsed::Bad(e),
    }
}

fn answering(ans: &Answer, read_ended: bool, post: Post) -> Phase {
    Phase::Answering { out: crate::serve::encode_response(ans.code, &ans.stdout, &ans.stderr, read_ended), sent: 0, post }
}

impl Conn {
    fn new(stream: UnixStream) -> Conn {
        let peer = crate::serve::peer_of(&stream);
        let now = Instant::now();
        Conn { stream, accepted: now, started: SystemTime::now(), peer, read_at: now, phase: Phase::Reading(Vec::new()) }
    }

    /// Runs the connection as far as it can go without waiting; true once it is over.
    fn drive(&mut self, cx: &mut Context<'_>, sh: &Shared) -> bool {
        loop {
            let phase = std::mem::replace(&mut self.phase, Phase::Closed);
            match self.advance(phase, cx, sh) {
                Step::Next(p) => self.phase = p,
                Step::Wait(p) => {
                    self.phase = p;
                    return false;
                }
                Step::Done => return true,
            }
        }
    }

    fn advance(&mut self, phase: Phase, cx: &mut Context<'_>, sh: &Shared) -> Step {
        match phase {
            Phase::Closed => Step::Done,
            Phase::Reading(buf) => self.read(buf, cx, sh),
            Phase::Deciding { held, tried } => self.decide(held, tried, cx, sh),
            Phase::Calling { held, job } => self.call(held, job, cx),
            Phase::Answering { out, sent, post } => self.answer(out, sent, post, cx, sh),
        }
    }

    fn read(&mut self, mut buf: Vec<u8>, cx: &mut Context<'_>, sh: &Shared) -> Step {
        loop {
            match self.stream.poll_read_ready(cx) {
                Poll::Pending => return Step::Wait(Phase::Reading(buf)),
                Poll::Ready(Err(_)) => return Step::Done,
                Poll::Ready(Ok(())) => {}
            }
            let mut chunk = [0u8; CHUNK];
            let eof = match self.stream.try_read(&mut chunk) {
                Ok(0) => true,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    false
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Step::Done,
            };
            match parse(&buf, eof) {
                Parsed::Need => {}
                Parsed::Nothing => return Step::Done,
                Parsed::Ready(req, size) => {
                    self.read_at = Instant::now();
                    return Step::Next(self.deciding(req, size, sh));
                }
                Parsed::Bad((size, msg)) => {
                    let ans = Answer::out(2, msg.clone());
                    return Step::Next(answering(&ans, false, Post::Unread { size, msg }));
                }
            }
            if eof {
                return Step::Done;
            }
        }
    }

    fn deciding(&self, req: Request, size: usize, sh: &Shared) -> Phase {
        let limit = crate::serve::request_deadline(&req.argv, &sh.door.deadlines);
        let due = Box::pin(tokio::time::sleep(limit.saturating_sub(self.accepted.elapsed()))); // waits-on: the request's answer; cap its deadline
        Phase::Deciding { held: Held { req, size, limit, due }, tried: None }
    }

    fn cut(&self, held: &Held) -> Phase {
        let Held { req, size, limit, .. } = held;
        let (size, limit) = (*size, *limit);
        let called = Called::unworked(req, "-".to_string(), crate::serve::deadline_answer(), Some(limit), self.accepted);
        answering(&called.ans.clone(), false, Post::Served { called: Box::new(called), size })
    }

    fn decide(&mut self, mut held: Held, tried: Option<u64>, cx: &mut Context<'_>, sh: &Shared) -> Step {
        if !serves(&held.req.argv) {
            return Step::Next(answering(&Answer::out(2, REFUSED_VERB.to_string()), false, Post::Nothing));
        }
        if held.due.as_mut().poll(cx).is_ready() {
            return Step::Next(self.cut(&held));
        }
        let seen = sh.finished.count.load(Ordering::SeqCst);
        if tried == Some(seen) {
            return Step::Wait(Phase::Deciding { held, tried });
        }
        let (door, peer, accepted, signal, work) = (sh.door.clone(), self.peer, self.accepted, Signal(sh.finished.clone()), held.req.clone());
        match sh.pool.offload(WHAT, None, move || {
            let _signal = signal;
            crate::serve::call_request(work, peer, &door, accepted, accepted, Cut::ByCaller)
        }) {
            Ok(job) => Step::Next(Phase::Calling { held, job }),
            Err(_) => Step::Wait(Phase::Deciding { held, tried: Some(seen) }),
        }
    }

    fn call(&mut self, mut held: Held, mut job: Offloaded<Called>, cx: &mut Context<'_>) -> Step {
        let size = held.size;
        match Pin::new(&mut job).poll(cx) {
            Poll::Ready(Ok(called)) => {
                let read_ended = called.read_ended();
                let ans = called.ans.clone();
                Step::Next(answering(&ans, read_ended, Post::Served { called: Box::new(called), size }))
            }
            Poll::Ready(Err(e)) => {
                let called = Called::unworked(&held.req, "-".to_string(), Answer::out(2, format!("cannot tell: {e}")), None, self.accepted);
                let ans = called.ans.clone();
                Step::Next(answering(&ans, false, Post::Served { called: Box::new(called), size }))
            }
            Poll::Pending if held.due.as_mut().poll(cx).is_ready() => Step::Next(self.cut(&held)),
            Poll::Pending => Step::Wait(Phase::Calling { held, job }),
        }
    }

    fn answer(&mut self, out: Vec<u8>, mut sent: usize, post: Post, cx: &mut Context<'_>, sh: &Shared) -> Step {
        while sent < out.len() {
            match self.stream.poll_write_ready(cx) {
                Poll::Pending => return Step::Wait(Phase::Answering { out, sent, post }),
                Poll::Ready(Err(_)) => return Step::Done,
                Poll::Ready(Ok(())) => {}
            }
            match self.stream.try_write(&out[sent..]) {
                Ok(n) => sent += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Step::Done,
            }
        }
        let answered = self.read_at.elapsed();
        let (spans, started, accepted, signal) = (sh.spans, self.started, self.accepted, Signal(sh.finished.clone()));
        match post {
            Post::Nothing => {}
            Post::Unread { size, msg } => {
                let _ = sh.pool.offload(WHAT, None, move || {
                    let _signal = signal;
                    crate::serve::record_unread(spans, size, &msg, started, Duration::ZERO);
                });
            }
            Post::Served { called, size } => {
                let _ = sh.pool.offload(WHAT, None, move || {
                    let _signal = signal;
                    crate::serve::count_answer(&called, answered);
                    crate::serve::record_served(spans, &called, size, started, accepted, answered);
                });
            }
        }
        Step::Done
    }

    /// The answer the drain gives a connection it cannot wait for.
    fn refuse_for_restart(&self) -> bool {
        if matches!(self.phase, Phase::Answering { .. } | Phase::Closed) {
            return false;
        }
        let out = crate::serve::encode_response(2, &crate::serve::stopped_for_restart(), "", false);
        let _ = self.stream.try_write(&out);
        true
    }
}

/// Serves `listener` until `stopping`, then answers what is in flight for up to `drain_bound`
/// and returns how many connections the bound cut. `on_stop` runs once, when the stop is seen,
/// before anything else: it takes the socket path away so a new connect is refused.
pub(crate) async fn serve(listener: UnixListener, sh: Arc<Shared>, stopping: &AtomicBool, drain_bound: Duration, on_stop: impl FnOnce()) -> usize {
    let mut listener = Some(listener);
    let mut conns: Vec<Conn> = Vec::new();
    let mut on_stop = Some(on_stop);
    let mut stopped_at: Option<Instant> = None;
    let mut tick = Box::pin(tokio::time::sleep(TICK)); // waits-on: the stop flag; cap TICK
    let mut woken = Box::pin(sh.finished.wake.notified());
    std::future::poll_fn(|cx| {
        while tick.as_mut().poll(cx).is_ready() {
            tick.as_mut().reset(tokio::time::Instant::now() + TICK); // waits-on: the stop flag; cap TICK
        }
        while woken.as_mut().poll(cx).is_ready() {
            woken.set(sh.finished.wake.notified());
        }
        if stopping.load(Ordering::SeqCst) && stopped_at.is_none() {
            stopped_at = Some(Instant::now());
            if let Some(f) = on_stop.take() {
                f();
            }
        }
        if let Some(l) = &listener {
            while let Poll::Ready(Ok((stream, _))) = l.poll_accept(cx) {
                conns.push(Conn::new(stream));
            }
        }
        if stopped_at.is_some_and(|at| at.elapsed() >= TICK) {
            listener = None;
        }
        conns.retain_mut(|c| !c.drive(cx, &sh));
        match stopped_at {
            Some(_) if listener.is_none() && conns.is_empty() => Poll::Ready(0),
            Some(at) if at.elapsed() >= drain_bound => Poll::Ready(conns.iter().filter(|c| c.refuse_for_restart()).count()),
            _ => Poll::Pending,
        }
    })
    .await
}
