//! One request and its whole answer over a [`Stream`], and the dial that opens the real one.

use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::task::{Context, Poll};
use std::time::Duration;

use ports::{Dial, Peer, Stream};

use super::{dropped, lost, Fail, Reply, ReplyHead};

enum Phase {
    Write { at: usize },
    Head,
    Body(ReplyHead),
}

/// One request in flight: [`poll_exchange`](Exchange::poll_exchange) writes it, reads the head
/// line, then the counted bytes, and answers when it has them all.
pub struct Exchange {
    request: Vec<u8>,
    phase: Phase,
    read: Vec<u8>,
}

impl Exchange {
    pub fn new(request: Vec<u8>) -> Self {
        Exchange { request, phase: Phase::Write { at: 0 }, read: Vec::new() }
    }

    pub fn poll_exchange(&mut self, stream: &mut dyn Stream, cx: &mut Context<'_>) -> Poll<Result<Reply, Fail>> {
        loop {
            match &mut self.phase {
                Phase::Write { at } => {
                    if *at == self.request.len() {
                        self.phase = Phase::Head;
                        continue;
                    }
                    match stream.poll_write(cx, &self.request[*at..]) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(n)) if n > 0 => *at += n,
                        Poll::Ready(_) => return Poll::Ready(Err(dropped("write"))),
                    }
                }
                Phase::Head => {
                    if let Some(line) = self.take_line() {
                        match ReplyHead::parse(&line) {
                            Ok(head) => self.phase = Phase::Body(head),
                            Err(f) => return Poll::Ready(Err(f)),
                        }
                        continue;
                    }
                    match self.fill(stream, cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(n)) if n > 0 => {}
                        Poll::Ready(Ok(_)) if !self.read.is_empty() => {
                            let line = String::from_utf8_lossy(&std::mem::take(&mut self.read)).into_owned();
                            match ReplyHead::parse(&line) {
                                Ok(head) => self.phase = Phase::Body(head),
                                Err(f) => return Poll::Ready(Err(f)),
                            }
                        }
                        Poll::Ready(_) => return Poll::Ready(Err(dropped("no reply"))),
                    }
                }
                Phase::Body(head) => {
                    if self.read.len() >= head.body_len() {
                        let Phase::Body(head) = std::mem::replace(&mut self.phase, Phase::Head) else { unreachable!() };
                        self.read.truncate(head.body_len());
                        return Poll::Ready(head.reply(std::mem::take(&mut self.read)));
                    }
                    match self.fill(stream, cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(n)) if n > 0 => {}
                        Poll::Ready(_) => return Poll::Ready(Err(lost("truncated reply"))),
                    }
                }
            }
        }
    }

    fn fill(&mut self, stream: &mut dyn Stream, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let mut buf = [0u8; 8192];
        let got = stream.poll_read(cx, &mut buf);
        if let Poll::Ready(Ok(n)) = got {
            self.read.extend_from_slice(&buf[..n]);
        }
        got
    }

    fn take_line(&mut self) -> Option<String> {
        let end = self.read.iter().position(|b| *b == b'\n')?;
        let line = String::from_utf8_lossy(&self.read[..end]).into_owned();
        self.read.drain(..=end);
        Some(line)
    }
}

/// A blocking Unix socket as a [`Stream`]: every poll is `Ready`, and a timeout is its error.
pub struct UnixPort(UnixStream);

impl From<UnixStream> for UnixPort {
    fn from(s: UnixStream) -> Self {
        UnixPort(s)
    }
}

fn retrying<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match op() {
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            done => return done,
        }
    }
}

impl Stream for UnixPort {
    fn poll_read(&mut self, _: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(retrying(|| self.0.read(buf)))
    }

    fn poll_write(&mut self, _: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(retrying(|| self.0.write(buf)))
    }

    fn poll_shutdown(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.0.shutdown(std::net::Shutdown::Write))
    }

    fn peer(&self) -> Peer {
        Peer { pid: None, uid: None }
    }
}

/// Connects to the store's socket, bounding each read and write by `limit`.
pub struct UnixDial {
    pub path: PathBuf,
    pub limit: Duration,
}

impl Dial for UnixDial {
    fn dial(&self) -> io::Result<Box<dyn Stream>> {
        let stream = UnixStream::connect(&self.path)?;
        stream.set_read_timeout(Some(self.limit))?;
        stream.set_write_timeout(Some(self.limit))?;
        Ok(Box::new(UnixPort(stream)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lc_call::{encode_request, exchange_once};
    use ports::{Listener, Net, SeededRng, SimClient, SimNet, VirtualClock};
    use std::sync::Arc;
    use std::task::Waker;

    const ME: Peer = Peer { pid: Some(7), uid: Some(1000) };

    fn cx() -> Context<'static> {
        Context::from_waker(Waker::noop())
    }

    #[derive(Clone, Copy)]
    enum End {
        Close,
        Reset,
    }

    struct Script {
        listener: Box<dyn Listener>,
        stream: Option<Box<dyn Stream>>,
        got: Vec<u8>,
        want: usize,
        reply: Vec<u8>,
        sent: usize,
        end: End,
        ended: bool,
    }

    impl Script {
        fn turn(&mut self, client: &SimClient) {
            if self.stream.is_none() {
                if let Poll::Ready(Ok(s)) = self.listener.poll_accept(&mut cx()) {
                    self.stream = Some(s);
                }
            }
            let Some(stream) = self.stream.as_mut() else { return };
            let mut buf = [0u8; 16];
            while let Poll::Ready(Ok(n)) = stream.poll_read(&mut cx(), &mut buf) {
                if n == 0 {
                    break;
                }
                self.got.extend_from_slice(&buf[..n]);
            }
            if self.got.len() < self.want {
                return;
            }
            while self.sent < self.reply.len() {
                match stream.poll_write(&mut cx(), &self.reply[self.sent..]) {
                    Poll::Ready(Ok(n)) => self.sent += n,
                    _ => return,
                }
            }
            if !self.ended {
                self.ended = true;
                match self.end {
                    End::Close => self.stream = None,
                    End::Reset => client.reset(),
                }
            }
        }
    }

    fn argv() -> Vec<String> {
        vec!["content".to_string(), "create".to_string()]
    }

    fn run(net: &Arc<SimNet>, mut client: SimClient, request: Vec<u8>, reply: &[u8], end: End) -> (Result<Reply, Fail>, Vec<u8>) {
        let mut script = Script { listener: net.listen("store").unwrap(), stream: None, got: Vec::new(), want: request.len(), reply: reply.to_vec(), sent: 0, end, ended: false };
        let mut exchange = Exchange::new(request);
        for _ in 0..200_000 {
            if let Poll::Ready(answer) = exchange.poll_exchange(&mut client, &mut cx()) {
                return (answer, script.got);
            }
            script.turn(&client);
            net.step();
        }
        panic!("the exchange never finished");
    }

    fn net(seed: u64) -> Arc<SimNet> {
        SimNet::new(VirtualClock::new(0), SeededRng::new(seed))
    }

    fn whole_reply() -> (Vec<u8>, Reply) {
        let (out, err) = ("stdout text\nsecond line", "a warning");
        let mut bytes = format!("{{\"exit_code\":3,\"len\":{},\"err_len\":{}}}\n", out.len(), err.len()).into_bytes();
        bytes.extend_from_slice(out.as_bytes());
        bytes.extend_from_slice(err.as_bytes());
        (bytes, Reply { code: 3, out: out.into(), err: err.into(), read_ended: false })
    }

    fn request() -> Vec<u8> {
        encode_request(&argv(), Some("a body\nof two lines and more bytes"), None, None).unwrap()
    }

    #[test_id(t651i7x)]
    #[test]
    fn a_request_and_its_reply_cross_whole_at_one_byte_a_time() {
        let n = net(1);
        let (reply, expected) = whole_reply();
        let client = n.connect_chunked("store", ME, 1);
        let (answer, got) = run(&n, client, request(), &reply, End::Close);
        assert_eq!(got, request());
        assert_eq!(answer, Ok(expected));
    }

    #[test_id(t0oc39e)]
    #[test]
    fn a_request_and_its_reply_cross_whole_however_a_seed_cuts_them() {
        let (reply, expected) = whole_reply();
        for seed in 0..64 {
            let n = net(seed);
            let client = n.connect("store", ME);
            let (answer, got) = run(&n, client, request(), &reply, End::Close);
            assert_eq!(got, request(), "seed {seed}");
            assert_eq!(answer, Ok(expected.clone()), "seed {seed}");
        }
    }

    fn lost_text(why: &str) -> String {
        format!("cannot tell: the lifecycle service took the request but its reply was lost ({why})")
    }

    #[test_id(tixd3ka)]
    #[test]
    fn a_reply_cut_short_fails_in_the_class_and_text_exchange_once_gives() {
        let dropped = |why: &str| Err(Fail::Dropped(lost_text(why)));
        let final_ = |why: &str| Err(Fail::Final(lost_text(why)));
        let cases: [(&str, &[u8], End, Result<Reply, Fail>); 6] = [
            ("reset before the head", b"", End::Reset, dropped("no reply")),
            ("close before the head", b"", End::Close, dropped("no reply")),
            ("reset mid-head", b"{\"exit_code\":0,", End::Reset, dropped("no reply")),
            ("close mid-head", b"{\"exit_code\":0,", End::Close, final_("malformed reply")),
            ("reset mid-body", b"{\"exit_code\":0,\"len\":9}\nabc", End::Reset, final_("truncated reply")),
            ("close mid-body", b"{\"exit_code\":0,\"len\":9}\nabc", End::Close, final_("truncated reply")),
        ];
        for (name, bytes, end, expected) in cases {
            let n = net(5);
            let (answer, _) = run(&n, n.connect_chunked("store", ME, 64), request(), bytes, end);
            assert_eq!(answer, expected, "{name}");
            if matches!(end, End::Close) {
                let (client, server) = UnixStream::pair().unwrap();
                (&server).write_all(bytes).unwrap();
                server.shutdown(std::net::Shutdown::Write).unwrap();
                assert_eq!(exchange_once(Box::new(UnixPort::from(client)), &argv(), Some("b")), expected, "{name}: as over a socket");
            }
        }
    }

    #[test_id(tyuad11)]
    #[test]
    fn a_write_the_peer_refuses_is_dropped_at_write() {
        struct Refusing;
        impl Stream for Refusing {
            fn poll_read(&mut self, _: &mut Context<'_>, _: &mut [u8]) -> Poll<io::Result<usize>> {
                Poll::Ready(Ok(0))
            }
            fn poll_write(&mut self, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
                Poll::Ready(Err(ErrorKind::BrokenPipe.into()))
            }
            fn poll_shutdown(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn peer(&self) -> Peer {
                ME
            }
        }
        let answer = Exchange::new(request()).poll_exchange(&mut Refusing, &mut cx());
        assert_eq!(answer, Poll::Ready(Err(Fail::Dropped(lost_text("write")))));
    }
}
