//! `work` — the aeon-facing semantic layer client (design §3.5). Bound to one summoned
//! bead (`SPIRA_WORK_BEAD_ID`), it never takes the bead it acts on as an argument and it
//! never falls back to a direct database connection — see `work::build_request` for the
//! request-shaping and `SPIRA_LC_SOCKET` for the only channel this binary knows how to
//! reach the machine through. If that socket is unreachable this exits 2 ("cannot tell"),
//! the same code spira-lc's own CLI uses for the same reason: a caller that retries a
//! "cannot tell" is safe, one that retries a real refusal is not.
//!
//! THE lifecycle switch gates all of it (DESIGN.md §3): with `lifecycle_enforce` off every
//! verb is refused (exit 3) before the socket is touched — `SPIRA_LIFECYCLE_ENFORCE`
//! (`1`/`true` on), else `spira.lifecycle_enforce`, else off.
//!
//! Deploys inert: nothing runs an aeon through this yet (the aeon.sh cutover bead,
//! sp-xethq, and the restricted unit environment this bead only provides — see
//! aeon/src/restrict.rs, formerly spira/work-env.sh, retired sp-zpaq0).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

const CANNOT_TELL: i32 = work::CANNOT_TELL;
const REFUSED: i32 = work::REFUSED;
const TIMEOUT: Duration = Duration::from_secs(10);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first().cloned() else {
        eprintln!("usage: work <verb> [args...] (verbs: {})", work::VERBS.join(", "));
        std::process::exit(CANNOT_TELL);
    };
    let verb_args = args[1..].to_vec();

    if !spira_config::lifecycle_enforce(None) {
        eprintln!("{}", work::off_refusal(&verb));
        std::process::exit(REFUSED);
    }

    let Ok(bound) = std::env::var("SPIRA_WORK_BEAD_ID") else {
        eprintln!("cannot tell: SPIRA_WORK_BEAD_ID is not set — this environment is not bound to a bead");
        std::process::exit(CANNOT_TELL);
    };
    let actor = std::env::var("SPIRA_FAYTH").or_else(|_| std::env::var("SPIRA_WORK_ACTOR")).unwrap_or_else(|_| "aeon".to_string());

    let tip = if verb == "submit" { read_tip(&bound) } else { None };

    let req = match work::build_request(&bound, &verb, &verb_args, tip.as_deref(), &actor) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(REFUSED);
        }
    };

    match send(&req) {
        Ok((code, out)) => {
            if !out.is_empty() {
                println!("{out}");
            }
            std::process::exit(code);
        }
        Err(e) => {
            eprintln!("cannot tell: {e}");
            std::process::exit(CANNOT_TELL);
        }
    }
}

/// `submit` never takes a tip argument (design §3.5): it is read here, the only place this
/// binary touches git — from the bead's own branch `spira/<id>`, which every worktree of the
/// repo shares, so the answer does not depend on the caller's cwd. `HEAD` read from the
/// wrong directory recorded local/main as the tip and the bead could never certify.
fn read_tip(bead: &str) -> Option<String> {
    let branch = format!("refs/heads/spira/{bead}");
    if let Ok(out) = Command::new("git").args(["rev-parse", "--verify", "-q", &branch]).output() {
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    let out = Command::new("git").args(["rev-parse", "HEAD"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The exact wire protocol spira-lc's own CLI speaks to its socket (client.rs): one JSON
/// line in (the argv array), one JSON line out (`{exit_code, stdout}`). No same-user
/// fallback exists here — see this file's module doc for why that absence is the point.
fn send(argv: &[String]) -> Result<(i32, String), String> {
    let socket_path = spira_config::resolve::key_for_process("SPIRA_LC_SOCKET")?;
    let stream = UnixStream::connect(&socket_path).map_err(|e| format!("connecting to {socket_path}: {e}"))?;
    stream.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;

    let request = serde_json::to_string(argv).map_err(|e| e.to_string())?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    writeln!(writer, "{request}").map_err(|e| e.to_string())?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
        return Err("spira-lc closed the connection with no reply".to_string());
    }
    let resp: serde_json::Value = serde_json::from_str(line.trim()).map_err(|e| format!("malformed reply: {e}"))?;
    let code = resp.get("exit_code").and_then(|v| v.as_i64()).ok_or("reply had no exit_code")? as i32;
    let out = resp.get("stdout").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Ok((code, out))
}
