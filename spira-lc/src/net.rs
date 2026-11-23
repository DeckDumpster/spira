//! `RealNet`: the `Net` port over local Unix sockets. The socket's mode and the kernel's word
//! on who connected are decided here and nowhere else in the server.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::pin::Pin;
use std::task::{Context, Poll};

use ports::{Listener, Net, Peer, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(crate) struct RealNet;

impl Net for RealNet {
    fn listen(&self, path: &str) -> io::Result<Box<dyn Listener>> {
        Ok(Box::new(RealListener::adopt(bind_socket(path)?)?))
    }
}

pub(crate) fn bind_socket(socket_path: &str) -> io::Result<UnixListener> {
    if std::path::Path::new(socket_path).exists() {
        let _ = crate::disk::fs().remove(std::path::Path::new(socket_path));
    }
    let listener = UnixListener::bind(socket_path)?;
    // Group-readable/writable only: the operator's group (spira) can reach the socket, the
    // credential this process holds is never written to it, only used to answer requests.
    if let Ok(meta) = std::fs::metadata(socket_path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o660);
        let _ = std::fs::set_permissions(socket_path, perms);
    }
    Ok(listener)
}

pub(crate) struct RealListener {
    std: Option<UnixListener>,
    registered: Option<tokio::net::UnixListener>,
}

impl RealListener {
    /// A bound listener, registered with the runtime the first time it is polled.
    pub(crate) fn adopt(listener: UnixListener) -> io::Result<RealListener> {
        listener.set_nonblocking(true)?;
        Ok(RealListener { std: Some(listener), registered: None })
    }
}

impl Listener for RealListener {
    fn poll_accept(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<Box<dyn Stream>>> {
        if self.registered.is_none() {
            let std = self.std.take().expect("a listener not yet registered holds its socket");
            self.registered = Some(tokio::net::UnixListener::from_std(std)?);
        }
        let listener = self.registered.as_ref().expect("registered above");
        listener.poll_accept(cx).map_ok(|(stream, _)| {
            let peer = peer_of(&stream);
            Box::new(RealStream { stream, peer }) as Box<dyn Stream>
        })
    }
}

struct RealStream {
    stream: tokio::net::UnixStream,
    peer: Peer,
}

impl Stream for RealStream {
    fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let mut read = ReadBuf::new(buf);
        Pin::new(&mut self.stream).poll_read(cx, &mut read).map_ok(|()| read.filled().len())
    }

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_shutdown(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }

    fn peer(&self) -> Peer {
        self.peer
    }
}

#[repr(C)]
struct Ucred {
    pid: i32,
    uid: u32,
    gid: u32,
}

extern "C" {
    fn getsockopt(fd: i32, level: i32, name: i32, val: *mut Ucred, len: *mut u32) -> i32;
}

/// The process and user at the other end of `stream`, as the kernel reports them.
pub(crate) fn peer_of(stream: &impl AsRawFd) -> Peer {
    const SOL_SOCKET: i32 = 1;
    const SO_PEERCRED: i32 = 17;
    let mut cred = Ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<Ucred>() as u32;
    let known = unsafe { getsockopt(stream.as_raw_fd(), SOL_SOCKET, SO_PEERCRED, &mut cred, &mut len) } == 0;
    Peer { pid: known.then_some(cred.pid), uid: known.then_some(cred.uid) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;

    fn cx() -> Context<'static> {
        Context::from_waker(std::task::Waker::noop())
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    async fn accepted(l: &mut dyn Listener) -> Box<dyn Stream> {
        std::future::poll_fn(|cx| l.poll_accept(cx)).await.unwrap()
    }

    async fn read(s: &mut dyn Stream, buf: &mut [u8]) -> usize {
        std::future::poll_fn(|cx| s.poll_read(cx, buf)).await.unwrap()
    }

    #[test_id(tplvtyx)]
    #[test]
    fn a_real_socket_carries_bytes_both_ways_names_its_peer_and_reads_a_close_as_the_end() {
        let dir = testkit::TempDir::new("lc-realnet");
        let path = dir.join("net.sock");
        let path = path.to_str().unwrap();
        runtime().block_on(async {
            let mut listener = RealNet.listen(path).unwrap();
            assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o777, 0o660);
            let mut client = std::os::unix::net::UnixStream::connect(path).unwrap();
            let mut server = accepted(listener.as_mut()).await;
            let me = std::fs::metadata(system_paths::PROC_SELF).unwrap();
            assert_eq!(server.peer(), Peer { pid: Some(std::process::id() as i32), uid: Some(me.uid()) });
            client.write_all(b"ping").unwrap();
            let mut buf = [0u8; 16];
            assert_eq!(read(server.as_mut(), &mut buf).await, 4);
            assert_eq!(&buf[..4], b"ping");
            let sent = std::future::poll_fn(|cx| server.poll_write(cx, b"pong")).await.unwrap();
            assert_eq!(sent, 4);
            let mut got = [0u8; 4];
            client.read_exact(&mut got).unwrap();
            assert_eq!(&got, b"pong");
            drop(client);
            assert_eq!(read(server.as_mut(), &mut buf).await, 0);
            assert!(server.poll_shutdown(&mut cx()).is_ready());
        });
    }
}
