//! The CLI's socket client: when the system-user service is installed and running, forward
//! the request to it instead of opening a fresh, slow, same-user connection. Never used for
//! `serve` itself or for `admin-apply-ddl`/`admin-migrate`, which needs its own explicit credentials (root,
//! typically) rather than whatever the daemon happens to hold.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(12); // batch-job: spira-lc requests queue behind a loaded Dolt in the round VM

/// `None` means "no usable service — fall back to a direct connection", not an error: no
/// socket configured or nothing listening, because same-user fallback exists precisely to keep
/// working when the service is absent.
pub fn try_socket(args: &[String]) -> Option<(i32, String)> {
    // `classify` reads bd, git and the ledger files from the CALLER's environment and runs far
    // past this client's timeout on a real store: forwarded, the service would read its own
    // environment and the timed-out caller would fall back and run it a second time.
    if matches!(args.first().map(|s| s.as_str()), Some("serve") | Some("admin-apply-ddl") | Some("admin-migrate") | Some("classify")) {
        return None;
    }
    // One source of config (per Ryan 2026-10-05): $SPIRA_TOML's declared socket path, never
    // this process's own environment. A resolution failure folds into "no usable service",
    // same as every other reason this falls back to a direct connection.
    let socket_path = spira_config::process::cfg("SPIRA_LC_SOCKET").ok()?;
    let stream = UnixStream::connect(&socket_path).ok()?;
    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(TIMEOUT)).ok()?;
    Some(exchange(stream, args))
}

/// Once the request is on the wire the service may apply it whatever happens to the reply, so a
/// lost reply is "cannot tell" and never a fallback: a direct run would repeat the write and
/// bypass the service's live rows.
fn exchange(stream: UnixStream, args: &[String]) -> (i32, String) {
    let lost = |why: &str| (2, format!("cannot tell: the service took the request but its reply was lost ({why})"));
    let Ok(request) = serde_json::to_string(args) else { return lost("unserialisable request") };
    let Ok(mut writer) = stream.try_clone() else { return lost("clone") };
    if writeln!(writer, "{request}").is_err() {
        return lost("write");
    }

    let mut reader = BufReader::with_capacity(1 << 20, stream);
    let mut line = String::new();
    if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
        return lost("no reply");
    }
    let Ok(resp) = serde_json::from_str::<serde_json::Value>(line.trim()) else { return lost("malformed reply") };
    let (Some(code), Some(len)) = (resp.get("exit_code").and_then(|v| v.as_i64()), resp.get("len").and_then(|v| v.as_u64())) else {
        return lost("malformed reply");
    };
    let mut body = vec![0u8; len as usize];
    if reader.read_exact(&mut body).is_err() {
        return lost("truncated reply");
    }
    match String::from_utf8(body) {
        Ok(out) => (code as i32, out),
        Err(_) => lost("non-utf8 reply"),
    }
}

#[cfg(test)]
mod tests {
    /// The admin verbs carry their own (superuser) credentials: forwarding one to the
    /// service would run it as spira_lc instead, which cannot create tables or users. classify
    /// reads the caller's own bd, git and ledger files (see try_socket).
    #[test]
    fn admin_verbs_never_go_through_the_socket() {
        for verb in ["admin-apply-ddl", "admin-migrate", "serve", "classify"] {
            assert!(super::try_socket(&[verb.to_string(), "/x".to_string()]).is_none(), "{verb}");
        }
    }

    fn serve_once(reply: &'static [u8]) -> (std::os::unix::net::UnixStream, std::thread::JoinHandle<()>) {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let h = std::thread::spawn(move || {
            let mut line = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(&server), &mut line).unwrap();
            std::io::Write::write_all(&mut &server, reply).unwrap();
        });
        (client, h)
    }

    #[test]
    fn a_lost_reply_is_cannot_tell_never_a_fallback() {
        for reply in [&b""[..], b"not json\n", b"{\"exit_code\":0}\n", b"{\"exit_code\":0,\"len\":9}\nshort"] {
            let (client, h) = serve_once(reply);
            let (code, out) = super::exchange(client, &["close".to_string()]);
            h.join().unwrap();
            assert_eq!(code, 2, "{reply:?}");
            assert!(out.starts_with("cannot tell"), "{out}");
        }
    }

    #[test]
    fn a_whole_reply_is_passed_through() {
        let (client, h) = serve_once(b"{\"exit_code\":3,\"len\":2}\nhi");
        assert_eq!(super::exchange(client, &["x".to_string()]), (3, "hi".to_string()));
        h.join().unwrap();
    }
}
