//! Shells to `bd` for the non-lifecycle half of the aeon semantic layer (design §3.5):
//! titles, descriptions, dependencies, notes and filing stay bd's job (§3.3/3.4) — this
//! module never touches `spira_lifecycle`. `SPIRA_BD`/`SPIRA_DB` are declared config
//! (spira/conf.d), resolved from `$SPIRA_TOML` (one source of config, per Ryan 2026-10-05) —
//! never this process's own environment.

use std::process::Command;

fn bd_bin() -> Result<String, String> {
    // SPIRA_BD resolves empty when unset (no default in its registry entry); an empty program
    // name fails every call, so the door's store half silently never ran in such a setup
    // (test-aeon-prod-dirty, sp-swh8b8). Empty means bd on PATH, as every other bd caller reads it.
    spira_config::process::cfg("SPIRA_BD").map(|b| if b.trim().is_empty() { "bd".to_string() } else { b })
}

/// Pushes one bead-store call's count and duration, labelled by its bd verb (the first
/// non-flag word) and whether it succeeded; a failed push is counted, never raised.
fn verb_of<'a>(args: &[&'a str]) -> &'a str {
    args.iter().find(|a| !a.starts_with('-')).copied().unwrap_or("unknown")
}

fn record_call(args: &[&str], started: std::time::Instant, ok: bool) {
    use spira_otel::metrics::{MetricsSink, Point};
    let Ok(endpoint) = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT") else { return };
    let verb = verb_of(args);
    MetricsSink::new(Some(&endpoint), "spira-lc", std::time::Duration::from_millis(250)).push(
        &[Point::Count { name: "spira_bead_store_calls", value: 1 }, Point::Seconds { name: "spira_bead_store_call_seconds", value: started.elapsed().as_secs_f64() }],
        &[("verb", verb), ("outcome", if ok { "ok" } else { "error" })],
    );
}

pub fn run(args: &[&str]) -> Result<String, String> {
    let started = std::time::Instant::now();
    let res = run_read(args);
    record_call(args, started, res.is_ok());
    res
}

fn run_read(args: &[&str]) -> Result<String, String> {
    let mut cmd = spira_config::bounded::bounded(bd_bin()?);
    let db = spira_config::process::cfg("SPIRA_DB")?;
    if !db.is_empty() {
        cmd.args(["-C", &db]);
    }
    cmd.args(args);
    let out = cmd.output().map_err(|e| format!("running bd {args:?}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout)
}

const WRITE_SECS: u64 = 60;

// A write runs the schema gate and a Dolt commit; the 5 s read cap kills it mid-commit though it lands.
fn run_write(args: &[&str]) -> Result<String, String> {
    let started = std::time::Instant::now();
    let res = run_write_unrecorded(args);
    record_call(args, started, res.is_ok());
    res
}

fn run_write_unrecorded(args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("timeout");
    cmd.arg(WRITE_SECS.to_string()).arg(bd_bin()?);
    let db = spira_config::process::cfg("SPIRA_DB")?;
    if !db.is_empty() {
        cmd.args(["-C", &db]);
    }
    cmd.args(args);
    let out = cmd.output().map_err(|e| format!("running bd {args:?}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout)
}

/// A close whose bd call failed is still done when the bead reads closed afterwards: bd can
/// exit non-zero after the write committed.
fn settled(res: Result<(), String>, id: &str, still_open: impl FnOnce() -> Result<Vec<String>, String>) -> Result<(), String> {
    match res {
        Ok(()) => Ok(()),
        Err(e) => match still_open() {
            Ok(open) if !open.iter().any(|o| o == id) => Ok(()),
            _ => Err(e),
        },
    }
}

pub fn show(bead_id: &str) -> Result<String, String> {
    run(&["show", bead_id])
}

/// The bound bead's own `repo:<name>` label, so `file-followup`/`split` never ask the
/// aeon for a repo it could get wrong — the layer reads it from the bead it was summoned
/// for, the same one every filing must land in.
pub fn repo_label(bead_id: &str) -> Result<Option<String>, String> {
    let out = run(&["show", bead_id, "--json"])?;
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd show --json: {e}"))?;
    let doc = match &parsed {
        serde_json::Value::Array(a) => a.first().cloned().unwrap_or(serde_json::Value::Null),
        v => v.clone(),
    };
    let labels = doc.get("labels").and_then(|l| l.as_array()).cloned().unwrap_or_default();
    Ok(labels.iter().filter_map(|l| l.as_str()).find_map(|l| l.strip_prefix("repo:").map(str::to_string)))
}

/// Every label on `bead_id`, read from bd.
pub fn labels(bead_id: &str) -> Result<Vec<String>, String> {
    let out = run(&["show", bead_id, "--json"])?;
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd show --json: {e}"))?;
    let doc = match &parsed {
        serde_json::Value::Array(a) => a.first().cloned().unwrap_or(serde_json::Value::Null),
        v => v.clone(),
    };
    Ok(doc.get("labels").and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect()).unwrap_or_default())
}

/// The live [`crate::callers::Bd`]: bd itself, resolved as [`run`] resolves it.
pub struct LiveBd;

impl crate::callers::Bd for LiveBd {
    fn issue_type(&mut self, id: &str) -> Result<String, String> {
        let out = run(&["show", id, "--json"])?;
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd show --json: {e}"))?;
        let doc = match &parsed {
            serde_json::Value::Array(a) => a.first().cloned().unwrap_or(serde_json::Value::Null),
            v => v.clone(),
        };
        doc.get("issue_type").and_then(|t| t.as_str()).map(str::to_string).ok_or_else(|| format!("bd show {id}: no issue_type"))
    }
    fn close(&mut self, id: &str, reason: &str) -> Result<(), String> {
        // --force: the lifecycle row is already terminal when `close` gets here, so the store
        // must follow it; a refused store close would leave the two disagreeing again.
        let res = run_write(&["close", id, "--force", "--reason", reason]).map(|_| ());
        settled(res, id, || self.open_among(&[id.to_string()]))
    }
    fn closed(&mut self, ids: &[String]) -> Result<Vec<(String, String)>, String> {
        crate::bd_facts::closed(&bd_bin()?, &spira_config::process::cfg("SPIRA_DB")?, ids)
    }
    fn known(&mut self, ids: &[String]) -> Result<Vec<String>, String> {
        crate::bd_facts::known(&bd_bin()?, &spira_config::process::cfg("SPIRA_DB")?, ids)
    }
    fn open_among(&mut self, ids: &[String]) -> Result<Vec<String>, String> {
        crate::bd_facts::open_among(&bd_bin()?, &spira_config::process::cfg("SPIRA_DB")?, ids)
    }
    fn work_beads(&mut self, id: &str) -> Result<Vec<String>, String> {
        let out = run(&["show", id, "--json"])?;
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd show --json: {e}"))?;
        let doc = match &parsed {
            serde_json::Value::Array(a) => a.first().cloned().unwrap_or(serde_json::Value::Null),
            v => v.clone(),
        };
        let labels = doc.get("labels").and_then(|l| l.as_array()).cloned().unwrap_or_default();
        Ok(labels.iter().filter_map(|l| l.as_str()).filter_map(|l| l.strip_prefix("work-bead:").map(str::to_string)).collect())
    }
    fn blocking_dependents(&mut self, id: &str) -> Result<Vec<String>, String> {
        let out = run(&["dep", "list", id, "--direction", "up", "--type", "blocks", "--json"])?;
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd dep list --json: {e}"))?;
        let ids: Vec<String> = parsed
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| d.get("depends_on_id").or_else(|| d.get("id")).and_then(|x| x.as_str()))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            return Ok(ids);
        }
        self.open_among(&ids)
    }
    fn children(&mut self, id: &str) -> Result<Vec<String>, String> {
        let out = run(&["dep", "list", id, "--direction", "up", "--type", "parent-child", "--json"])?;
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd dep list --json: {e}"))?;
        Ok(parsed
            .as_array()
            .map(|a| a.iter().filter_map(|d| d.get("id").or_else(|| d.get("issue_id")).and_then(|x| x.as_str())).filter(|s| !s.is_empty()).map(str::to_string).collect())
            .unwrap_or_default())
    }
    fn external(&mut self, id: &str) -> Result<bool, String> {
        let out = run(&["show", id, "--json"])?;
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd show --json: {e}"))?;
        let doc = match &parsed {
            serde_json::Value::Array(a) => a.first().cloned().unwrap_or(serde_json::Value::Null),
            v => v.clone(),
        };
        Ok(doc.get("external_ref").and_then(|r| r.as_str()).is_some_and(|r| !r.trim().is_empty()))
    }
    fn reopen(&mut self, id: &str) -> Result<(), String> {
        // The store follows the row the door just moved: open, unassigned, and no longer wearing
        // the submitted label (a legacy state label; best effort, it may not be there).
        run_write(&["update", id, "--status", "open", "--assignee", ""]).map(|_| ())?;
        if let Some(label) = spira_config::process::cfg("SPIRA_SUBMITTED_LABEL").ok().filter(|l| !l.is_empty()) {
            let _ = run(&["label", "remove", id, &label]);
        }
        Ok(())
    }
}

pub fn note(bead_id: &str, text: &str) -> Result<String, String> {
    run(&["note", bead_id, text])
}

/// Files through `spira/bead.sh`'s contract (the labelling rules `bd create` alone does not
/// enforce), with `--parent` set by this layer, never the caller — see bead.sh's own
/// `--parent` doc for why an inherited `branch:` label made `groomer split-piece` a
/// two-step dance that this avoids by filing with the right labels from the start.
pub fn file_child(title: &str, persona: &str, repo: &str, parent: Option<&str>, body: Option<&str>) -> Result<String, String> {
    // bead.sh, by name on the launcher's PATH (sp-gypjk) — never repository-relative.
    let bead_sh = "bead.sh";
    let mut cmd = spira_config::bounded::bounded(bead_sh);
    cmd.args(["file", title, "--for", persona, "--repo", repo]);
    if let Some(parent) = parent {
        cmd.args(["--parent", parent]);
    }
    let out = match body {
        None => cmd.output(),
        Some(b) => {
            use std::io::Write;
            cmd.args(["--body-file", "-"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
            cmd.spawn().and_then(|mut child| {
                if let Some(mut w) = child.stdin.take() {
                    let _ = w.write_all(b.as_bytes());
                }
                child.wait_with_output()
            })
        }
    }
    .map_err(|e| format!("running {bead_sh}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout.trim().to_string())
}

/// `bead.sh dep add <id> <depends-on>`: `id` is blocked until `depends-on` is closed.
pub fn add_blocker(id: &str, depends_on: &str) -> Result<(), String> {
    let out = spira_config::bounded::bounded("bead.sh")
        .args(["dep", "add", id, depends_on, "--type", "blocks"])
        .output()
        .map_err(|e| format!("running bead.sh: {e}"))?;
    if !out.status.success() {
        return Err(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// `mail send operator` — an ask carrying the channel back (design §3.5's `work
/// blocked`, and the supersede-by confirmation request). `--bead` ties the tracking
/// decision bead mail files to the one this layer is bound to.
pub fn ask_operator(from: &str, subject: &str, default: &str, bead_id: &str, body: &str) -> Result<String, String> {
    // mail, by name on the launcher's PATH (sp-gypjk); SPIRA_MAIL_SH is the harness-wide
    // binary-override seam.
    let mail_sh = std::env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| "mail".to_string());
    let mut child = spira_config::bounded::bounded(&mail_sh)
        .args(["send", "operator", "--from", from, "--subject", subject, "--kind", "question", "--default", default, "--bead", bead_id])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("running {mail_sh}: {e}"))?;
    use std::io::Write;
    child.stdin.take().expect("piped stdin").write_all(body.as_bytes()).map_err(|e| format!("writing to {mail_sh}: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("waiting on {mail_sh}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout)
}

/// `bd <args>` with `stdin` piped in when given — the lane verbs' one door to bd
/// (sp-st0mm). Same `SPIRA_BD`/`SPIRA_DB` resolution as [`run`].
pub fn run_stdin(args: &[String], stdin: Option<&str>) -> Result<String, String> {
    let started = std::time::Instant::now();
    let res = run_stdin_unrecorded(args, stdin);
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    record_call(&words, started, res.is_ok());
    res
}

fn run_stdin_unrecorded(args: &[String], stdin: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new(bd_bin()?);
    let db = spira_config::process::cfg("SPIRA_DB")?;
    if !db.is_empty() {
        cmd.args(["-C", &db]);
    }
    cmd.args(args);
    let (code, out) = spawn(cmd, stdin, "bd");
    if code == 0 {
        Ok(out)
    } else {
        Err(out)
    }
}

/// A harness tool by bare name on the launcher's PATH (sp-gypjk), run under `timeout` with
/// the caller's persona as `SPIRA_FAYTH` — the czar fence in `queue` and `bdq` reads it, and
/// the broker's own identity is never the persona that asked. Returns the tool's own exit code
/// (stdout; stdout plus stderr when it failed).
pub fn tool(program: &str, args: &[String], stdin: Option<&str>, actor: &str, secs: u64) -> (i32, String) {
    tool_env(program, args, stdin, actor, secs, &[])
}

/// [`tool`], with `env` set on the child as well.
pub fn tool_env(program: &str, args: &[String], stdin: Option<&str>, actor: &str, secs: u64, env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new("timeout");
    cmd.arg(secs.to_string()).arg(program).args(args).env("SPIRA_FAYTH", actor);
    for (k, v) in env {
        cmd.env(k, v);
    }
    spawn(cmd, stdin, program)
}

/// [`tool_env`] for a tool whose stderr is part of its answer even on success (mail's
/// "routed to the concierge" notice): nothing it says is dropped.
pub fn tool_env_noisy(program: &str, args: &[String], stdin: Option<&str>, actor: &str, secs: u64, env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new("timeout");
    cmd.arg(secs.to_string()).arg(program).args(args).env("SPIRA_FAYTH", actor);
    for (k, v) in env {
        cmd.env(k, v);
    }
    spawn_with(cmd, stdin, program, true)
}

fn spawn(cmd: Command, stdin: Option<&str>, what: &str) -> (i32, String) {
    spawn_with(cmd, stdin, what, false)
}

/// A socket-activated broker holds these for its own listener; `podman run` reads `LISTEN_FDS`
/// and preserves fds the child never had, which breaks its libpod database mid-run.
const ACTIVATION_ENV: [&str; 4] = ["LISTEN_FDS", "LISTEN_PID", "LISTEN_FDNAMES", "LISTEN_PIDFDID"];

fn spawn_with(mut cmd: Command, stdin: Option<&str>, what: &str, keep_stderr: bool) -> (i32, String) {
    use std::io::Write;
    use std::process::Stdio;
    for k in ACTIVATION_ENV {
        cmd.env_remove(k);
    }
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (2, format!("running {what}: {e}")),
    };
    if let Some(text) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            if let Err(e) = pipe.write_all(text.as_bytes()) {
                return (2, format!("writing to {what}: {e}"));
            }
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return (2, format!("waiting on {what}: {e}")),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    match out.status.code() {
        Some(0) if keep_stderr => (0, format!("{stdout}{}", String::from_utf8_lossy(&out.stderr))),
        Some(0) => (0, stdout),
        Some(c) => (c, format!("{stdout}{}", String::from_utf8_lossy(&out.stderr))),
        None => (2, format!("{what} was killed by a signal: {stdout}{}", String::from_utf8_lossy(&out.stderr))),
    }
}

#[cfg(test)]
mod settled_tests {
    use super::*;

    #[test]
    fn a_failed_close_of_a_bead_that_reads_closed_is_done() {
        assert!(settled(Err("context canceled".into()), "sp-a", || Ok(vec![])).is_ok());
    }

    #[test]
    fn a_failed_close_of_a_bead_still_open_or_unreadable_stays_failed() {
        assert_eq!(settled(Err("boom".into()), "sp-a", || Ok(vec!["sp-a".into()])), Err("boom".into()));
        assert_eq!(settled(Err("boom".into()), "sp-a", || Err("down".into())), Err("boom".into()));
    }

    #[test]
    fn a_successful_close_is_never_reread() {
        assert!(settled(Ok(()), "sp-a", || panic!("must not read")).is_ok());
    }
}

#[cfg(test)]
mod activation_env_tests {
    use super::*;

    #[test]
    fn a_brokered_tool_does_not_inherit_socket_activation_variables() {
        let env = [("LISTEN_FDS", "1"), ("LISTEN_PID", "1"), ("LISTEN_FDNAMES", "x")];
        let (code, out) = tool_env("sh", &["-c".to_string(), "echo \"[${LISTEN_FDS-}${LISTEN_PID-}${LISTEN_FDNAMES-}]\"".to_string()], None, "t", 10, &env);
        assert_eq!((code, out.trim()), (0, "[]"));
    }
}

#[cfg(test)]
mod noisy_tests {
    use super::*;

    fn sh(script: &str) -> (i32, String) {
        tool_env_noisy("sh", &["-c".to_string(), script.to_string()], None, "t", 10, &[])
    }

    #[test]
    fn a_refused_ask_exits_nonzero_with_the_reason() {
        let (code, out) = sh("echo 'mail: lint: unknown kind x' >&2; exit 1");
        assert_eq!(code, 1);
        assert!(out.contains("unknown kind x"), "{out}");
    }

    #[test]
    fn a_successful_ask_still_shows_the_stderr_notice() {
        let (code, out) = sh("echo 'mail: routed to the concierge' >&2");
        assert_eq!(code, 0);
        assert!(out.contains("routed to the concierge"), "{out}");
        assert!(!tool("sh", &["-c".to_string(), "echo hidden >&2".to_string()], None, "t", 10).1.contains("hidden"));
    }
}

impl crate::ask::AskSource for LiveBd {
    fn open_asks(&mut self, ask_label: &str) -> Result<Vec<(String, Vec<String>)>, String> {
        let out = run(&["list", "--type", "decision", "--label", ask_label, "--status", "open", "--limit", "0", "--json"])?;
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("bd list --json: {e}"))?;
        let rows = parsed.as_array().cloned().unwrap_or_default();
        Ok(rows
            .iter()
            .filter_map(|r| {
                let id = r.get("id")?.as_str()?.to_string();
                let works = r.get("labels").and_then(|l| l.as_array()).map(|l| l.iter().filter_map(|x| x.as_str()).filter_map(|x| x.strip_prefix("work-bead:").map(str::to_string)).collect()).unwrap_or_default();
                Some((id, works))
            })
            .collect())
    }
}

#[cfg(test)]
mod verb_tests {
    use super::verb_of;

    #[test]
    fn the_verb_is_the_first_word_that_is_not_a_flag() {
        assert_eq!(verb_of(&["--json", "show", "sp-1"]), "show");
        assert_eq!(verb_of(&["update", "sp-1"]), "update");
        assert_eq!(verb_of(&["--version"]), "unknown");
    }
}
