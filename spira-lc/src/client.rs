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

use std::time::Duration;

/// law-calls-have-deadlines: a client waits at most 5 s; the service stops at 3 s.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);
/// The repair sweeps, which the service bounds at the query deadline, not 3 s.
// batch-job: a sweep, bounded by the service's query deadline plus the reply.
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
            "cannot tell: the lifecycle service (lc-serve) is not answering on {socket_path}: {e} — nothing was read or written; spira-lc does not fall back to a direct database connection (check: systemctl --user status lc-serve.service)"
        ),
    )
}

/// `None`: run this verb in-process (it is never forwarded, or direct mode is declared).
/// Otherwise the service's answer, or exit 2 naming the service when it does not answer.
pub fn try_socket(args: &[String]) -> Option<(i32, String)> {
    // The admin verbs run with the caller's own credentials and environment, never the service's.
    if matches!(args.first().map(|s| s.as_str()), Some("serve") | Some("admin-create-store") | Some("admin-drop-lifecycle") | Some("admin-store") | Some("admin-provision-reader") | Some("admin-provision-writer") | Some("admin-ensure-indexes")) {
        return None;
    }
    // One source of config (per Ryan 2026-10-05): $SPIRA_TOML's declared socket path, never
    // this process's own environment.
    route_call(args, route())
}

fn route_call(args: &[String], routed: Result<Route, String>) -> Option<(i32, String)> {
    let socket_path = match routed {
        Ok(Route::Direct) => return None,
        Ok(Route::Service(p)) => p,
        Err(e) => return Some((2, e)),
    };
    Some(forward(&socket_path, args))
}

/// One request to the service on `socket_path`.
pub(crate) fn forward(socket_path: &str, args: &[String]) -> (i32, String) {
    let (code, out, err) = forward_with(socket_path, args, None);
    match spira_store::lc_call::queued_id(&out) {
        Some(id) if code == 0 => await_request(socket_path, id),
        _ => (code, format!("{out}{err}")),
    }
}

/// One request with `stdin` as its body; the answer's stdout and stderr apart. A refused or
/// absent socket is waited for as long as the call's own limit.
pub(crate) fn forward_with(socket_path: &str, args: &[String], stdin: Option<&str>) -> (i32, String, String) {
    let batch = args.first().is_some_and(|v| crate::composed::SWEEPS.contains(&v.as_str()));
    let limit = if batch { BATCH_TIMEOUT } else { TIMEOUT };
    forward_within(socket_path, args, stdin, limit, limit)
}

fn forward_within(socket_path: &str, args: &[String], stdin: Option<&str>, limit: Duration, not_running_wait: Duration) -> (i32, String, String) {
    use spira_store::lc_call::{connect_within, retrying_resent};
    let path = std::path::Path::new(socket_path);
    let answer = retrying_resent(args, path, limit, not_running_wait, |left| spira_store::lc_call::exchange_once(connect_within(path, left)?, args, stdin));
    match answer {
        Ok(r) => (r.code, r.out, r.err),
        Err(f) => (2, f.into_text(), String::new()),
    }
}

/// One attempt on `stream`. Once the request is on the wire the service may apply it whatever
/// happens to the reply, so a lost reply is "cannot tell" and never a fallback.
#[cfg(test)]
pub(crate) fn exchange(stream: std::os::unix::net::UnixStream, args: &[String], stdin: Option<&str>) -> (i32, String, String) {
    match spira_store::lc_call::exchange(Box::new(spira_store::lc_call::UnixPort::from(stream)), args, stdin) {
        Ok(r) => (r.code, r.out, r.err),
        Err(msg) => (2, msg, String::new()),
    }
}

/// The answer of the recorded request `id`, polled until it is done. Every poll is a plain read.
pub fn await_request(socket_path: &str, id: &str) -> (i32, String) {
    match spira_store::lc_call::LcClient::socket(socket_path).await_record(id) {
        Ok(r) => (r.code, r.out),
        Err(e) => (2, format!("cannot tell: request {id}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    /// The admin verbs carry their own (superuser) credentials: forwarding one to the service
    /// would run it as the service user instead (see try_socket).
    #[test_id(twn8y1h)]
    #[test]
    fn admin_verbs_never_go_through_the_socket() {
        for verb in ["admin-create-store", "admin-drop-lifecycle", "admin-store", "admin-provision-reader", "admin-provision-writer", "admin-ensure-indexes", "serve"] {
            assert!(super::super::try_socket(&[verb.to_string(), "x".to_string()]).is_none(), "{verb}");
        }
    }

    #[test_id(tm42klz)]
    #[test]
    fn a_list_carrying_an_old_snapshot_flag_still_reaches_the_service() {
        let dir = testkit::TempDir::new("lc-client-flag");
        let path = dir.join("a-file").join("sock");
        std::fs::write(dir.join("a-file"), "").unwrap();
        let args: Vec<String> = ["list", "--live", "--snapshot"].map(String::from).to_vec();
        let (code, out) = super::super::route_call(&args, Ok(super::super::Route::Service(path.to_str().unwrap().into()))).expect("a routed call is answered");
        assert_eq!(code, 2, "{out}");
        assert!(out.contains(path.to_str().unwrap()), "the call reached the socket: {out}");
    }

    #[test_id(tlqu1im)]
    #[test]
    fn only_a_declared_empty_socket_means_direct_and_a_config_failure_is_never_direct() {
        use super::super::{route_of, Route};
        assert_eq!(route_of(Ok(String::new())), Ok(Route::Direct));
        assert_eq!(route_of(Ok("  ".into())), Ok(Route::Direct));
        assert_eq!(route_of(Ok("run/user/1000/spira-lc/sock".into())), Ok(Route::Service("run/user/1000/spira-lc/sock".into())));
        let refused = route_of(Err("SPIRA_TOML is not set".into())).unwrap_err();
        assert!(refused.starts_with("cannot tell") && refused.contains("spira.lc_socket"), "{refused}");
    }

    /// sp-yvyvzd: a declared socket nothing answers is the service down — exit 2 naming it,
    /// never `None` (which the CLI reads as "open your own connection").
    #[test_id(tyk3x5m)]
    #[test]
    fn a_declared_socket_that_does_not_answer_fails_naming_the_service() {
        let dir = testkit::TempDir::new("lc-client-down");
        std::fs::write(dir.join("a-file"), "").unwrap();
        let path = dir.join("a-file").join("sock");
        let (code, out) = super::super::forward(path.to_str().unwrap(), &["show".to_string(), "sp-x".to_string()]);
        assert_eq!(code, 2, "{out}");
        assert!(out.starts_with("cannot tell") && out.contains("not answering") && out.contains(path.to_str().unwrap()), "names the socket: {out}");
    }

    #[test_id(txaf0bp)]
    #[test]
    fn an_absent_socket_is_store_not_running_after_its_wait() {
        let dir = testkit::TempDir::new("lc-client-not-running");
        let path = dir.join("no-such.sock");
        let wait = std::time::Duration::from_millis(200);
        let (code, out, _) = super::super::forward_within(path.to_str().unwrap(), &["hold".to_string(), "sp-a".to_string()], None, wait, wait);
        assert_eq!(code, 2, "{out}");
        assert!(out.contains("store not running") && out.contains("nothing was sent") && out.contains(path.to_str().unwrap()), "{out}");
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

    #[test_id(tpaj84r)]
    #[test]
    fn a_lost_reply_is_cannot_tell_never_a_fallback() {
        for reply in [&b""[..], b"not json\n", b"{\"exit_code\":0}\n", b"{\"exit_code\":0,\"len\":9}\nshort"] {
            let (client, h) = serve_once(reply);
            let (code, out, _) = super::super::exchange(client, &["close".to_string()], None);
            h.join().unwrap();
            assert_eq!(code, 2, "{reply:?}");
            assert!(out.starts_with("cannot tell"), "{out}");
        }
    }

    fn flaky_socket(drops: usize) -> (testkit::TempDir, String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::Ordering;
        let dir = testkit::TempDir::new("lc-client-flaky");
        let path = dir.join("sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let n = count.fetch_add(1, Ordering::SeqCst);
                let mut line = String::new();
                std::io::BufRead::read_line(&mut std::io::BufReader::new(&stream), &mut line).unwrap();
                if n >= drops {
                    std::io::Write::write_all(&mut &stream, b"{\"exit_code\":0,\"len\":2}\nok").unwrap();
                }
            }
        });
        (dir, path.to_str().unwrap().to_string(), seen)
    }

    #[test_id(twfdev9)]
    #[test]
    fn a_read_dropped_by_a_restarting_service_is_answered_on_the_next_try() {
        let (_dir, path, seen) = flaky_socket(2);
        let (code, out, _) = super::super::forward_with(&path, &["content".to_string(), "list".to_string()], None);
        assert_eq!((code, out.as_str()), (0, "ok"));
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test_id(txkvb5p)]
    #[test]
    fn a_write_dropped_by_a_restarting_service_is_cannot_tell_after_one_attempt() {
        let (_dir, path, seen) = flaky_socket(1);
        let (code, out, _) = super::super::forward_with(&path, &["hold".to_string(), "sp-a".to_string()], None);
        assert!(code == 2 && out.starts_with("cannot tell"), "{out}");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test_id(tmbx0ld)]
    #[test]
    fn a_whole_reply_is_passed_through() {
        let (client, h) = serve_once(b"{\"exit_code\":3,\"len\":2}\nhi");
        assert_eq!(super::super::exchange(client, &["x".to_string()], None), (3, "hi".to_string(), String::new()));
        h.join().unwrap();
    }
}

