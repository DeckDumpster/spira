//! The CLI's socket client: when the system-user service is installed and running, forward
//! the request to it instead of opening a fresh, slow, same-user connection. Never used for
//! `serve` itself or for `admin-apply-ddl`/`admin-migrate`, which needs its own explicit credentials (root,
//! typically) rather than whatever the daemon happens to hold.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(12); // batch-job: spira-lc requests queue behind a loaded Dolt in the round VM

/// `None` means "no usable service — fall back to a direct connection", not an error: no
/// socket configured, nothing listening, or a malformed reply are all treated the same way,
/// because same-user fallback exists precisely to keep working when the service is absent.
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

    let request = serde_json::to_string(args).ok()?;
    let mut writer = stream.try_clone().ok()?;
    writeln!(writer, "{request}").ok()?;

    let mut reader = BufReader::with_capacity(1 << 20, stream);
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let resp: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let code = resp.get("exit_code")?.as_i64()? as i32;
    let len = resp.get("len")?.as_u64()? as usize;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).ok()?;
    Some((code, String::from_utf8(body).ok()?))
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
}
