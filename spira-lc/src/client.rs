//! The CLI's socket client: every store call goes to the one service that owns the store's
//! connections (`lc-serve`, or the system-user `spira-lc.service`), which keeps them open and
//! commits concurrent writes together (sp-yvyvzd). Never used for `serve` itself or for the
//! admin verbs, which need their own explicit credentials (root, typically) rather than
//! whatever the daemon happens to hold, nor for `classify` (see [`try_socket`]).
//!
//! NO SILENT FALLBACK (sp-yvyvzd; law-a-fallback-has-a-forced-mode,
//! law-a-config-value-has-no-fallback). A declared socket that does not answer is the service
//! being down, and the call fails naming it: a per-call direct connection behind it hid the
//! outage and was the connection churn on :3307. Direct mode is a declared choice, never a
//! fallback: `spira.lc_socket = ""` (test fixtures, which run no service).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// law-calls-have-deadlines: a client waits at most 5 s; the service stops at 3 s.
const TIMEOUT: Duration = Duration::from_secs(5);
/// census's aggregate over the whole fact history (`facts-query`), which the service bounds
/// at the query deadline, not 3 s.
// batch-job: census report, bounded by the service's query deadline plus the reply.
const BATCH_TIMEOUT: Duration = Duration::from_secs(12);

/// Where a store call goes, from the one declared `spira.lc_socket`.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// Through the service on this socket; nothing else.
    Service(String),
    /// Declared empty: this process opens its own connection (fixtures with no service).
    Direct,
}

/// The declared route; `Err` names the config failure, which is never read as "direct".
pub fn route() -> Result<Route, String> {
    route_of(spira_config::process::cfg("SPIRA_LC_SOCKET"))
}

fn route_of(declared: Result<String, String>) -> Result<Route, String> {
    match declared {
        Ok(p) if p.trim().is_empty() => Ok(Route::Direct),
        Ok(p) => Ok(Route::Service(p)),
        Err(e) => Err(format!("cannot tell: the lifecycle service socket (spira.lc_socket) does not resolve: {e}")),
    }
}

/// The answer when the service does not take the call: names the service and the socket, and
/// says that nothing fell back.
fn down(socket_path: &str, e: &std::io::Error) -> (i32, String) {
    (
        2,
        format!(
            "cannot tell: the lifecycle service (lc-serve) is not answering on {socket_path}: {e} — nothing was read or written; spira-lc does not fall back to a direct database connection (check: systemctl --user status lc-serve.socket lc-serve.service)"
        ),
    )
}

/// `None`: run this verb in-process (it is never forwarded, or direct mode is declared).
/// Otherwise the service's answer, or exit 2 naming the service when it does not answer.
pub fn try_socket(args: &[String]) -> Option<(i32, String)> {
    // `classify` reads bd, git and the ledger files from the CALLER's environment and runs far
    // past this client's timeout on a real store: forwarded, the service would read its own
    // environment and the timed-out caller would fall back and run it a second time.
    if matches!(args.first().map(|s| s.as_str()), Some("serve") | Some("admin-apply-ddl") | Some("admin-migrate") | Some("admin-accounts") | Some("verify-login") | Some("classify")) {
        return None;
    }
    // One source of config (per Ryan 2026-10-05): $SPIRA_TOML's declared socket path, never
    // this process's own environment.
    let socket_path = match route() {
        Ok(Route::Direct) => return None,
        Ok(Route::Service(p)) => p,
        Err(e) => return Some((2, e)),
    };
    Some(forward(&socket_path, args))
}

/// One request to the service on `socket_path`.
fn forward(socket_path: &str, args: &[String]) -> (i32, String) {
    let stream = match UnixStream::connect(socket_path) {
        Ok(s) => s,
        Err(e) => return down(socket_path, &e),
    };
    let limit = if args.first().map(String::as_str) == Some("facts-query") { BATCH_TIMEOUT } else { TIMEOUT };
    if let Err(e) = stream.set_read_timeout(Some(limit)).and_then(|_| stream.set_write_timeout(Some(limit))) {
        return down(socket_path, &e);
    }
    exchange(stream, args)
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
        for verb in ["admin-apply-ddl", "admin-migrate", "admin-accounts", "verify-login", "serve", "classify"] {
            assert!(super::try_socket(&[verb.to_string(), "/x".to_string()]).is_none(), "{verb}");
        }
    }

    #[test]
    fn only_a_declared_empty_socket_means_direct_and_a_config_failure_is_never_direct() {
        use super::{route_of, Route};
        assert_eq!(route_of(Ok(String::new())), Ok(Route::Direct));
        assert_eq!(route_of(Ok("  ".into())), Ok(Route::Direct));
        assert_eq!(route_of(Ok("/run/user/1000/spira-lc/sock".into())), Ok(Route::Service("/run/user/1000/spira-lc/sock".into())));
        let refused = route_of(Err("SPIRA_TOML is not set".into())).unwrap_err();
        assert!(refused.starts_with("cannot tell") && refused.contains("spira.lc_socket"), "{refused}");
    }

    /// sp-yvyvzd: a declared socket nothing answers is the service down — exit 2 naming it,
    /// never `None` (which the CLI reads as "open your own connection").
    #[test]
    fn a_declared_socket_that_does_not_answer_fails_naming_the_service() {
        let dir = testkit::TempDir::new("lc-client-down");
        let path = dir.join("no-such.sock");
        let (code, out) = super::forward(path.to_str().unwrap(), &["show".to_string(), "sp-x".to_string()]);
        assert_eq!(code, 2, "{out}");
        assert!(out.contains("lc-serve") && out.contains(path.to_str().unwrap()), "names the service and its socket: {out}");
        assert!(out.contains("does not fall back"), "{out}");
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
