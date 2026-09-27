//! The CLI's socket client: when the system-user service is installed and running, forward
//! the request to it instead of opening a fresh, slow, same-user connection. Never used for
//! `serve` itself or for `admin-apply-ddl`, which needs its own explicit credentials (root,
//! typically) rather than whatever the daemon happens to hold.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// `None` means "no usable service — fall back to a direct connection", not an error: no
/// socket configured, nothing listening, or a malformed reply are all treated the same way,
/// because same-user fallback exists precisely to keep working when the service is absent.
pub fn try_socket(args: &[String]) -> Option<(i32, String)> {
    if matches!(args.first().map(|s| s.as_str()), Some("serve") | Some("admin-apply-ddl")) {
        return None;
    }
    let socket_path = std::env::var("SPIRA_LC_SOCKET").unwrap_or_else(|_| "/run/spira-lc/sock".to_string());
    let stream = UnixStream::connect(&socket_path).ok()?;
    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(TIMEOUT)).ok()?;

    let request = serde_json::to_string(args).ok()?;
    let mut writer = stream.try_clone().ok()?;
    writeln!(writer, "{request}").ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let resp: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let code = resp.get("exit_code")?.as_i64()? as i32;
    let out = resp.get("stdout").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some((code, out))
}
