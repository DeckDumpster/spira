//! The `Net` port: the listener and streams a server's event loop runs on. A `Stream` carries
//! its peer's identity as a value, so a server never asks a socket who is on the other end.
//! `SimNet` is the in-memory transport: events arrive in an order a seed draws, and a pipe's
//! capacity makes a write pending.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use crate::{Rng, SeededRng, VirtualClock};

/// Who is on the other end of a stream. `None` is a transport that cannot say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pub pid: Option<i32>,
    pub uid: Option<u32>,
}

pub trait Stream: Send {
    fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>>;
    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>>;
    fn poll_shutdown(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>>;
    fn peer(&self) -> Peer;
}

pub trait Listener: Send {
    fn poll_accept(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<Box<dyn Stream>>>;
}

pub trait Net: Send + Sync {
    fn listen(&self, path: &str) -> io::Result<Box<dyn Listener>>;
}

/// The client side of a transport: one new connection to a fixed place per call.
pub trait Dial: Send + Sync {
    fn dial(&self) -> io::Result<Box<dyn Stream>>;
}

#[derive(Default)]
struct Pipe {
    to_server: VecDeque<u8>,
    client_closed: bool,
    reset: bool,
    server_reader: Option<Waker>,
    to_client: VecDeque<u8>,
    capacity: usize,
    server_done: bool,
    client_gone: bool,
    client_reader: Option<Waker>,
    server_writer: Option<Waker>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn wake(w: Option<Waker>) {
    if let Some(w) = w {
        w.wake();
    }
}

enum Event {
    Connect { path: String, peer: Peer },
    Bytes(Vec<u8>),
    Close,
    Reset,
}

impl Event {
    fn label(&self) -> String {
        match self {
            Event::Connect { path, .. } => format!("connect {path}"),
            Event::Bytes(b) => format!("bytes {}", b.len()),
            Event::Close => "close".into(),
            Event::Reset => "reset".into(),
        }
    }
}

#[derive(Default)]
struct Accepting {
    ready: VecDeque<SimStream>,
    waker: Option<Waker>,
}

struct World {
    rng: SeededRng,
    queues: BTreeMap<usize, VecDeque<Event>>,
    pipes: HashMap<usize, Arc<Mutex<Pipe>>>,
    listeners: HashMap<String, Arc<Mutex<Accepting>>>,
    next: usize,
    trace: Vec<String>,
}

pub struct SimNet {
    clock: VirtualClock,
    world: Arc<Mutex<World>>,
}

impl SimNet {
    pub fn new(clock: VirtualClock, rng: SeededRng) -> Arc<SimNet> {
        let world = World { rng, queues: BTreeMap::new(), pipes: HashMap::new(), listeners: HashMap::new(), next: 0, trace: Vec::new() };
        Arc::new(SimNet { clock, world: Arc::new(Mutex::new(world)) })
    }

    /// The client end of a new connection to `path`, whose server end reports `peer`. The
    /// connection reaches the listener when `step` delivers it.
    pub fn connect(self: &Arc<Self>, path: &str, peer: Peer) -> SimClient {
        let mut w = lock(&self.world);
        let id = w.next;
        w.next += 1;
        let capacity = 1 + (w.rng.next_u64() % 24) as usize;
        let pipe = Arc::new(Mutex::new(Pipe { capacity, ..Pipe::default() }));
        w.pipes.insert(id, pipe.clone());
        w.queues.entry(id).or_default().push_back(Event::Connect { path: path.to_string(), peer });
        SimClient { net: self.clone(), id, pipe, chunk: None }
    }

    /// [`connect`](Self::connect) for a client whose every write and whose pipe's capacity are
    /// at most `size` bytes, so both directions arrive that many at a time.
    pub fn connect_chunked(self: &Arc<Self>, path: &str, peer: Peer, size: usize) -> SimClient {
        let size = size.max(1);
        let mut client = self.connect(path, peer);
        lock(&client.pipe).capacity = size;
        client.chunk = Some(size);
        client
    }

    /// Delivers one queued event, drawn among the streams that have one and in order within its
    /// stream, and wakes whoever waits on it. False when nothing was queued.
    pub fn step(&self) -> bool {
        let mut w = lock(&self.world);
        let ids: Vec<usize> = w.queues.iter().filter(|(_, q)| !q.is_empty()).map(|(id, _)| *id).collect();
        if ids.is_empty() {
            return false;
        }
        let id = ids[(w.rng.next_u64() % ids.len() as u64) as usize];
        let event = w.queues.get_mut(&id).and_then(VecDeque::pop_front).expect("a stream with an event");
        w.trace.push(format!("s{id} {}", event.label()));
        let pipe = w.pipes[&id].clone();
        self.clock.advance(1);
        let mut woken = Vec::new();
        match event {
            Event::Connect { path, peer } => match w.listeners.get(&path).cloned() {
                Some(acc) => {
                    let mut a = lock(&acc);
                    a.ready.push_back(SimStream { pipe, peer });
                    woken.push(a.waker.take());
                }
                None => lock(&pipe).reset = true,
            },
            Event::Bytes(b) => {
                let mut p = lock(&pipe);
                p.to_server.extend(b);
                woken.push(p.server_reader.take());
            }
            Event::Close => {
                let mut p = lock(&pipe);
                p.client_closed = true;
                woken.push(p.server_reader.take());
            }
            Event::Reset => {
                let mut p = lock(&pipe);
                p.reset = true;
                woken.push(p.server_reader.take());
                woken.push(p.server_writer.take());
                woken.push(p.client_reader.take());
            }
        }
        drop(w);
        woken.into_iter().for_each(wake);
        true
    }

    /// The events delivered so far, one line each, in delivery order.
    pub fn trace(&self) -> Vec<String> {
        lock(&self.world).trace.clone()
    }

    fn queue(&self, id: usize, event: Event) {
        lock(&self.world).queues.entry(id).or_default().push_back(event);
    }

    fn split(&self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut w = lock(&self.world);
        let mut rest = bytes;
        let mut out = Vec::new();
        while !rest.is_empty() {
            let n = 1 + (w.rng.next_u64() % rest.len() as u64) as usize;
            out.push(rest[..n].to_vec());
            rest = &rest[n..];
        }
        out
    }
}

impl Net for SimNet {
    fn listen(&self, path: &str) -> io::Result<Box<dyn Listener>> {
        let mut w = lock(&self.world);
        if w.listeners.contains_key(path) {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, path.to_string()));
        }
        let acc = Arc::new(Mutex::new(Accepting::default()));
        w.listeners.insert(path.to_string(), acc.clone());
        Ok(Box::new(SimListener { path: path.to_string(), acc, world: self.world.clone() }))
    }
}

struct SimListener {
    path: String,
    acc: Arc<Mutex<Accepting>>,
    world: Arc<Mutex<World>>,
}

impl Listener for SimListener {
    fn poll_accept(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<Box<dyn Stream>>> {
        let mut a = lock(&self.acc);
        match a.ready.pop_front() {
            Some(s) => Poll::Ready(Ok(Box::new(s))),
            None => {
                a.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Drop for SimListener {
    fn drop(&mut self) {
        lock(&self.world).listeners.remove(&self.path);
    }
}

struct SimStream {
    pipe: Arc<Mutex<Pipe>>,
    peer: Peer,
}

impl Stream for SimStream {
    fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let mut p = lock(&self.pipe);
        if p.reset {
            return Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()));
        }
        if !p.to_server.is_empty() {
            let n = buf.len().min(p.to_server.len());
            for (slot, b) in buf.iter_mut().zip(p.to_server.drain(..n)) {
                *slot = b;
            }
            return Poll::Ready(Ok(n));
        }
        if p.client_closed {
            return Poll::Ready(Ok(0));
        }
        p.server_reader = Some(cx.waker().clone());
        Poll::Pending
    }

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let mut p = lock(&self.pipe);
        if p.reset || p.client_gone {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        let room = p.capacity.saturating_sub(p.to_client.len());
        if room == 0 && !buf.is_empty() {
            p.server_writer = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let n = room.min(buf.len());
        p.to_client.extend(&buf[..n]);
        let reader = p.client_reader.take();
        drop(p);
        wake(reader);
        Poll::Ready(Ok(n))
    }

    fn poll_shutdown(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut p = lock(&self.pipe);
        p.server_done = true;
        let reader = p.client_reader.take();
        drop(p);
        wake(reader);
        Poll::Ready(Ok(()))
    }

    fn peer(&self) -> Peer {
        self.peer
    }
}

impl Drop for SimStream {
    fn drop(&mut self) {
        let mut p = lock(&self.pipe);
        p.server_done = true;
        let reader = p.client_reader.take();
        drop(p);
        wake(reader);
    }
}

/// The client end of a `SimNet` connection. What it sends is queued and reaches the server
/// when `SimNet::step` delivers it.
pub struct SimClient {
    net: Arc<SimNet>,
    id: usize,
    pipe: Arc<Mutex<Pipe>>,
    chunk: Option<usize>,
}

impl Stream for SimClient {
    fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        SimClient::poll_read(self, cx, buf)
    }

    /// Queues the bytes, at most one chunk of them when the client has a chunk size.
    fn poll_write(&mut self, _: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let n = self.chunk.map_or(buf.len(), |c| c.min(buf.len()));
        match self.chunk {
            Some(c) => self.send_in(&buf[..n], c),
            None => self.send(&buf[..n]),
        }
        Poll::Ready(Ok(n))
    }

    fn poll_shutdown(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.close();
        Poll::Ready(Ok(()))
    }

    fn peer(&self) -> Peer {
        Peer { pid: None, uid: None }
    }
}

/// Dials `path` on a [`SimNet`] as `peer`, one connection per call.
pub struct SimDial {
    pub net: Arc<SimNet>,
    pub path: String,
    pub peer: Peer,
    pub chunk: Option<usize>,
}

impl Dial for SimDial {
    fn dial(&self) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(match self.chunk {
            Some(n) => self.net.connect_chunked(&self.path, self.peer, n),
            None => self.net.connect(&self.path, self.peer),
        }))
    }
}

impl SimClient {
    /// Queues `bytes` as chunks split at seeded points.
    pub fn send(&self, bytes: &[u8]) {
        for chunk in self.net.split(bytes) {
            self.net.queue(self.id, Event::Bytes(chunk));
        }
    }

    /// Queues `bytes` as chunks of at most `size`.
    pub fn send_in(&self, bytes: &[u8], size: usize) {
        for chunk in bytes.chunks(size.max(1)) {
            self.net.queue(self.id, Event::Bytes(chunk.to_vec()));
        }
    }

    /// Queues the end of what this client sends; it still reads the answer.
    pub fn close(&self) {
        self.net.queue(self.id, Event::Close);
    }

    /// Queues a reset: both ends see the connection fail.
    pub fn reset(&self) {
        self.net.queue(self.id, Event::Reset);
    }

    /// Bytes the server wrote; `Ready(Ok(0))` once it has shut down or dropped the stream and
    /// everything it wrote is read.
    pub fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let mut p = lock(&self.pipe);
        if !p.to_client.is_empty() {
            let n = buf.len().min(p.to_client.len());
            for (slot, b) in buf.iter_mut().zip(p.to_client.drain(..n)) {
                *slot = b;
            }
            let writer = p.server_writer.take();
            drop(p);
            wake(writer);
            return Poll::Ready(Ok(n));
        }
        if p.reset {
            return Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()));
        }
        if p.server_done {
            return Poll::Ready(Ok(0));
        }
        p.client_reader = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for SimClient {
    fn drop(&mut self) {
        let mut p = lock(&self.pipe);
        p.client_gone = true;
        let writer = p.server_writer.take();
        drop(p);
        wake(writer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Waker;

    const ME: Peer = Peer { pid: Some(7), uid: Some(1000) };

    fn net(seed: u64) -> Arc<SimNet> {
        SimNet::new(VirtualClock::new(0), SeededRng::new(seed))
    }

    fn cx() -> Context<'static> {
        Context::from_waker(Waker::noop())
    }

    fn accept(l: &mut Box<dyn Listener>) -> Box<dyn Stream> {
        match l.poll_accept(&mut cx()) {
            Poll::Ready(Ok(s)) => s,
            _ => panic!("no connection was delivered"),
        }
    }

    fn drain(s: &mut dyn Stream) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 5];
        while let Poll::Ready(Ok(n)) = s.poll_read(&mut cx(), &mut buf) {
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        got
    }

    #[test_id(tqadc1h)]
    #[test]
    fn some_seed_delivers_each_order_of_two_streams_events() {
        let mut orders = std::collections::BTreeSet::new();
        for seed in 0..64 {
            let n = net(seed);
            let _l = n.listen("p").unwrap();
            let (a, b) = (n.connect("p", ME), n.connect("p", ME));
            a.send_in(b"x", 1);
            b.send_in(b"y", 1);
            while n.step() {}
            let t = n.trace();
            let at = |label: &str| t.iter().position(|l| l == label).unwrap();
            orders.insert(at("s0 connect p") < at("s1 connect p"));
        }
        assert_eq!(orders.len(), 2, "both orders occur");
    }

    #[test_id(tyowoa1)]
    #[test]
    fn one_seed_gives_ten_identical_traces() {
        let run = || {
            let n = net(11);
            let _l = n.listen("p").unwrap();
            let clients: Vec<SimClient> = (0..3).map(|_| n.connect("p", ME)).collect();
            clients.iter().for_each(|c| c.send(b"hello world"));
            clients[1].close();
            clients[2].reset();
            while n.step() {}
            n.trace()
        };
        let first = run();
        assert!(first.len() > 6);
        assert!((0..10).all(|_| run() == first));
    }

    #[test_id(tc6377w)]
    #[test]
    fn bytes_on_one_stream_never_reorder() {
        for seed in 0..64 {
            let n = net(seed);
            let mut l = n.listen("p").unwrap();
            let (a, b) = (n.connect("p", ME), n.connect("p", ME));
            let text: Vec<u8> = (0..60u8).collect();
            a.send(&text);
            b.send(&text);
            a.close();
            b.close();
            while n.step() {}
            let (mut sa, mut sb) = (accept(&mut l), accept(&mut l));
            assert_eq!(drain(sa.as_mut()), text);
            assert_eq!(drain(sb.as_mut()), text);
        }
    }

    #[test_id(tu93yho)]
    #[test]
    fn a_full_pipe_makes_a_write_pending_until_the_client_reads() {
        let n = net(3);
        let mut l = n.listen("p").unwrap();
        let mut client = n.connect("p", ME);
        while n.step() {}
        let mut s = accept(&mut l);
        let big = [9u8; 200];
        let first = match s.poll_write(&mut cx(), &big) {
            Poll::Ready(Ok(n)) => n,
            _ => panic!("room for some"),
        };
        assert!(first < big.len());
        assert!(s.poll_write(&mut cx(), &big).is_pending());
        let mut buf = [0u8; 64];
        let Poll::Ready(Ok(read)) = client.poll_read(&mut cx(), &mut buf) else { panic!("bytes were written") };
        assert_eq!(read, first);
        assert!(matches!(s.poll_write(&mut cx(), &big), Poll::Ready(Ok(n)) if n > 0));
    }

    #[test_id(ttxopiu)]
    #[test]
    fn the_accepted_stream_reports_the_identity_the_test_gave() {
        let n = net(1);
        let mut l = n.listen("p").unwrap();
        let _c = n.connect("p", Peer { pid: Some(42), uid: Some(1234) });
        while n.step() {}
        assert_eq!(accept(&mut l).peer(), Peer { pid: Some(42), uid: Some(1234) });
    }

    #[test_id(t1vsm7m)]
    #[test]
    fn a_reset_fails_the_servers_read_and_a_connect_to_nothing_is_refused() {
        let n = net(1);
        let mut l = n.listen("p").unwrap();
        let c = n.connect("p", ME);
        let nobody = n.connect("nowhere", ME);
        c.reset();
        while n.step() {}
        let mut s = accept(&mut l);
        let mut buf = [0u8; 4];
        assert!(matches!(s.poll_read(&mut cx(), &mut buf), Poll::Ready(Err(_))));
        drop(nobody);
        assert!(n.listen("p").is_err());
    }
}
