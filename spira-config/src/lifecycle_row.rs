//! Every path that creates a bead creates its lifecycle row too: a bead with no
//! `spira_lifecycle` row can never be claimed.

use std::process::{Command, Stdio};

pub const LC_BIN_ENV: &str = "SPIRA_LC_BIN";
const LC_TIMEOUT_SECS: &str = "15";

pub fn lc_bin() -> String {
    std::env::var(LC_BIN_ENV).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "spira-lc".to_string())
}

/// An ask is its own lifecycle machine, never a bead row: `spira-lc create-ask` opens it
/// (idempotent), naming the work bead whose `ask` hold its close lifts.
pub fn create_ask_with(bin: &str, id: &str, work_bead: &str) -> Result<(), String> {
    let mut args = vec!["create-ask", id];
    if !work_bead.is_empty() {
        args.extend(["--work-bead", work_bead]);
    }
    lc_status(bin, &args)
}

pub fn create_ask(id: &str, work_bead: &str) -> Result<(), String> {
    create_ask_with(&lc_bin(), id, work_bead)
}

/// How an ask closes (`spira-lc close-ask --exit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskExit {
    /// The operator's answer; `channel` names where it came from.
    Answered,
    /// Dismissed, the default executed.
    Default,
    /// Moot.
    Withdrawn,
}

impl AskExit {
    fn as_str(self) -> &'static str {
        match self {
            AskExit::Answered => "answered",
            AskExit::Default => "default",
            AskExit::Withdrawn => "withdrawn",
        }
    }
}

/// Close an ask on its own machine, recording who and the quoted words, and lift the `ask`
/// hold on the work bead it names. Exit 1 (the id is no ask: a legacy ask bead) is `Ok(false)`.
pub fn close_ask_with(bin: &str, id: &str, exit: AskExit, quote: &str, actor: &str, channel: &str) -> Result<bool, String> {
    let mut args = vec!["close-ask", id, "--exit", exit.as_str(), "--quote", quote, "--actor", actor];
    if !channel.is_empty() {
        args.extend(["--channel", channel]);
    }
    match run_lc(bin, &args)? {
        (0, _) => Ok(true),
        (1, _) => Ok(false),
        (code, msg) => Err(format!("{bin} close-ask {id} exited {code}: {msg}")),
    }
}

pub fn close_ask(id: &str, exit: AskExit, quote: &str, actor: &str, channel: &str) -> Result<bool, String> {
    close_ask_with(&lc_bin(), id, exit, quote, actor, channel)
}

/// Whether `id` is an ask on the ask machine: the only authority on ask-ness (not a label).
pub fn is_ask_with(bin: &str, id: &str) -> Result<bool, String> {
    match run_lc(bin, &["show-ask", id])? {
        (0, _) => Ok(true),
        (1, _) => Ok(false),
        (code, msg) => Err(format!("{bin} show-ask {id} exited {code}: {msg}")),
    }
}

pub fn is_ask(id: &str) -> Result<bool, String> {
    is_ask_with(&lc_bin(), id)
}

fn run_lc(bin: &str, args: &[&str]) -> Result<(i32, String), String> {
    let out = Command::new("timeout")
        .arg(LC_TIMEOUT_SECS)
        .arg(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    let msg = format!("{}{}", String::from_utf8_lossy(&out.stdout).trim(), String::from_utf8_lossy(&out.stderr).trim());
    Ok((out.status.code().unwrap_or(-1), msg))
}

fn lc_status(bin: &str, args: &[&str]) -> Result<(), String> {
    match run_lc(bin, args)? {
        (0, _) => Ok(()),
        (code, msg) => Err(format!("{bin} {} exited {code}: {msg}", args.join(" "))),
    }
}

/// The id `bd create` printed: `--silent` prints the bare id, `--json` an object (or a
/// one-element array) with `id`.
pub fn created_id(stdout: &str) -> Option<String> {
    let text = stdout.trim();
    if text.starts_with('{') || text.starts_with('[') {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let obj = if v.is_array() { v.get(0)? } else { &v };
        return obj.get("id").and_then(|i| i.as_str()).map(String::from);
    }
    let last = text.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    (!last.contains(char::is_whitespace)).then(|| last.to_string())
}

/// Every id a bead-creating verb printed, as JSON: `import` lists `ids`, `mol pour|wisp` an
/// `id_mapping` of proto to new ids, `mol bond` a `result_id`. A bare id is accepted as `created_id` does.
pub fn created_ids(stdout: &str) -> Vec<String> {
    let text = stdout.trim();
    if !text.starts_with('{') {
        return created_id(text).into_iter().collect();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let mut ids: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        if !ids.iter().any(|i| i == s) {
            ids.push(s.to_string());
        }
    };
    if let Some(a) = v.get("ids").and_then(|a| a.as_array()) {
        a.iter().filter_map(|i| i.as_str()).for_each(&mut push);
    }
    if let Some(i) = v.get("new_epic_id").and_then(|i| i.as_str()) {
        push(i);
    }
    if let Some(m) = v.get("id_mapping").and_then(|m| m.as_object()) {
        m.values().filter_map(|i| i.as_str()).for_each(&mut push);
    }
    if let Some(i) = v.get("result_id").and_then(|i| i.as_str()) {
        push(i);
    }
    if ids.is_empty() {
        return created_id(text).into_iter().collect();
    }
    ids
}

pub fn ensure_rows_with(bin: &str, ids: &[String]) -> Result<(), String> {
    let errs: Vec<String> = ids.iter().filter_map(|id| ensure_row_with(bin, id).err()).collect();
    if errs.is_empty() { Ok(()) } else { Err(errs.join("; ")) }
}

/// `Ok(())`: the row exists now.
pub fn ensure_row_with(bin: &str, id: &str) -> Result<(), String> {
    let out = Command::new("timeout")
        .args([LC_TIMEOUT_SECS, bin, "create-bead", id])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{bin} create-bead {id} exited {}: {}{}",
            out.status.code().map_or("signal".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Record `Express`/`Unexpress` on the bead's lifecycle row: express is lifecycle state.
pub fn set_express_with(bin: &str, id: &str, on: bool) -> Result<(), String> {
    let verb = if on { "express" } else { "unexpress" };
    let out = Command::new("timeout")
        .args([LC_TIMEOUT_SECS, bin, verb, id])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{bin} {verb} {id} exited {}: {}{}",
            out.status.code().map_or("signal".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn set_express(id: &str, on: bool) -> Result<(), String> {
    set_express_with(&lc_bin(), id, on)
}

pub fn ensure_row(id: &str) -> Result<(), String> {
    ensure_row_retrying(&lc_bin(), id, ROW_ATTEMPTS, 250)
}

const ROW_ATTEMPTS: u32 = 3;

/// `create-bead` is idempotent, so a transient failure is retried before it is reported.
pub fn ensure_row_retrying(bin: &str, id: &str, attempts: u32, backoff_ms: u64) -> Result<(), String> {
    let mut last = ensure_row_with(bin, id);
    for n in 1..attempts {
        if last.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(backoff_ms * u64::from(n)));
        last = ensure_row_with(bin, id);
    }
    last
}

/// Call after a successful `bd create` whose stdout is `created_stdout`. A failure is
/// reported on stderr and returned; the bead exists, so the caller must not retry the create.
pub fn after_create(who: &str, created_stdout: &str) -> Result<(), String> {
    let Some(id) = created_id(created_stdout) else {
        let e = format!("cannot read the created bead's id from {created_stdout:?}");
        eprintln!("{who}: LIFECYCLE: {e}; the new bead has no lifecycle row and cannot be claimed");
        return Err(e);
    };
    ensure_row(&id).map_err(|e| {
        eprintln!("{who}: LIFECYCLE: {id} was created but has NO lifecycle row and cannot be claimed: {e}");
        e
    })
}

/// The bead a `bd close <id> ...` argv closes: the first operand, when it is one.
pub fn closed_id(args: &[String]) -> Option<&str> {
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("close"), Some(id)) if !id.starts_with('-') => Some(id),
        _ => None,
    }
}

/// Move the bead's lifecycle row to the terminal state its bd close earned
/// (`spira-lc reconcile-closed`), so a closed bead never stays READY and claimable.
pub fn terminalize_closed_with(bin: &str, id: &str) -> Result<(), String> {
    let out = Command::new("timeout")
        .args([LC_TIMEOUT_SECS, bin, "reconcile-closed", "--apply", id])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{bin} reconcile-closed {id} exited {}: {}{}",
            out.status.code().map_or("signal".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Call after a successful `bd close`. A failure is reported and returned; the bead is
/// closed, so the caller must not retry the close.
pub fn after_close(who: &str, args: &[String]) -> Result<(), String> {
    let Some(id) = closed_id(args) else { return Ok(()) };
    terminalize_closed_with(&lc_bin(), id).map_err(|e| {
        eprintln!("{who}: LIFECYCLE: {id} was closed but its lifecycle row is not terminal and may read as claimable: {e}");
        e
    })
}

/// Close a bead through the lifecycle machine (`spira-lc close`, sp-3fue0j): the row's end is
/// recorded first, then the store is closed. Every Rust closer calls this rather than handing
/// bd a `close` (lifecycle-guard's `bd-close-rust` refuses that at the gate). `actor` names who
/// closed it on the event; `superseded_by` names a successor. The reason goes on stdin, so
/// any length or quoting survives.
pub fn close_with(bin: &str, id: &str, reason: &str, actor: &str, superseded_by: Option<&str>) -> Result<(), String> {
    close_args(bin, id, reason, actor, superseded_by.filter(|b| !b.is_empty()).map(|b| vec!["--superseded-by", b]).unwrap_or_default())
}

/// The landing path's close (`spira-lc close --landing`): the delivery records LANDED on the
/// row itself, so this closes the store only, and spira-lc refuses it for a row not in delivery.
pub fn close_landed(id: &str, reason: &str, actor: &str) -> Result<(), String> {
    close_args(&lc_bin(), id, reason, actor, vec!["--landing"])
}

fn close_args(bin: &str, id: &str, reason: &str, actor: &str, extra: Vec<&str>) -> Result<(), String> {
    use std::io::Write;
    let mut cmd = Command::new("timeout");
    cmd.args([LC_TIMEOUT_SECS, bin, "close", id, "--reason-file", "-", "--actor", actor]);
    cmd.args(extra);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(reason.as_bytes()).map_err(|e| format!("{bin} close {id}: writing the reason: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("{bin} close {id}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{bin} close {id} exited {}: {}{}",
            out.status.code().map_or("signal".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// [`close_with`] through the configured `spira-lc`.
pub fn close(id: &str, reason: &str, actor: &str, superseded_by: Option<&str>) -> Result<(), String> {
    close_with(&lc_bin(), id, reason, actor, superseded_by)
}

/// Return a bead to its builder through the lifecycle machine (`spira-lc reopen`, sp-swh8b8):
/// the row records the event its own state implies. No row (exit 1) is nothing to reopen and is
/// `Ok`; a terminal row (3) or an unreachable machine (2) is an error. Every Rust reopener
/// calls this rather than handing bd a `reopen` (lifecycle-guard's `bd-reopen-rust`).
pub fn reopen_with(bin: &str, id: &str, cause: &str, actor: &str) -> Result<(), String> {
    let out = Command::new("timeout")
        .args([LC_TIMEOUT_SECS, bin, "reopen", id, cause, actor])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    match out.status.code() {
        Some(0 | 1) => Ok(()),
        code => Err(format!(
            "{bin} reopen {id} exited {}: {}{}",
            code.map_or("signal".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// [`reopen_with`] through the configured `spira-lc`.
pub fn reopen(id: &str, cause: &str, actor: &str) -> Result<(), String> {
    reopen_with(&lc_bin(), id, cause, actor)
}

/// `after_create` for a verb that may create many beads (`import`, `mol pour|wisp|bond`),
/// which must have been run with `--json`.
pub fn after_create_ids(who: &str, created_stdout: &str) -> Result<(), String> {
    let ids = created_ids(created_stdout);
    if ids.is_empty() {
        let e = format!("cannot read any created bead id from {created_stdout:?}");
        eprintln!("{who}: LIFECYCLE: {e}; the new beads have no lifecycle row and cannot be claimed");
        return Err(e);
    }
    let errs: Vec<String> = ids.iter().filter_map(|id| ensure_row(id).err()).collect();
    if errs.is_empty() {
        return Ok(());
    }
    let e = errs.join("; ");
    eprintln!("{who}: LIFECYCLE: some created beads have NO lifecycle row and cannot be claimed: {e}");
    Err(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(dir: &std::path::Path, exit: i32) -> String {
        let p = dir.join("lc");
        let log = dir.join("calls");
        testkit::write_exe(&p, &format!("#!/bin/sh\necho \"$@\" >> {}\nexit {exit}\n", log.display()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn an_ask_is_opened_and_closed_on_its_own_machine_and_never_as_a_bead() {
        let t = testkit::TempDir::new("lcask");
        let bin = stub(t.path(), 0);
        assert_eq!(create_ask_with(&bin, "sp-a", "sp-w"), Ok(()));
        assert_eq!(create_ask_with(&bin, "sp-b", ""), Ok(()));
        assert_eq!(close_ask_with(&bin, "sp-a", AskExit::Answered, "ship it", "operator", "pane"), Ok(true));
        assert_eq!(close_ask_with(&bin, "sp-b", AskExit::Withdrawn, "moot", "concierge", ""), Ok(true));
        let calls = std::fs::read_to_string(t.path().join("calls")).unwrap();
        assert_eq!(
            calls,
            "create-ask sp-a --work-bead sp-w\ncreate-ask sp-b\nclose-ask sp-a --exit answered --quote ship it --actor operator --channel pane\nclose-ask sp-b --exit withdrawn --quote moot --actor concierge\n"
        );
        assert!(!calls.contains("create-bead"));
    }

    #[test]
    fn exit_one_is_no_ask_and_anything_else_is_an_error() {
        let t = testkit::TempDir::new("lcask-codes");
        assert_eq!(is_ask_with(&stub(t.path(), 0), "sp-a"), Ok(true));
        let t1 = testkit::TempDir::new("lcask-none");
        assert_eq!(is_ask_with(&stub(t1.path(), 1), "sp-a"), Ok(false));
        assert_eq!(close_ask_with(&stub(t1.path(), 1), "sp-a", AskExit::Default, "d", "a", ""), Ok(false));
        let t3 = testkit::TempDir::new("lcask-refused");
        assert!(is_ask_with(&stub(t3.path(), 2), "sp-a").is_err());
        assert!(close_ask_with(&stub(t3.path(), 3), "sp-a", AskExit::Default, "d", "a", "").unwrap_err().contains("exited 3"));
    }

    #[test]
    fn created_id_reads_silent_and_json() {
        assert_eq!(created_id("sp-abc\n").as_deref(), Some("sp-abc"));
        assert_eq!(created_id("{\"id\":\"sp-j\",\"title\":\"t\"}").as_deref(), Some("sp-j"));
        assert_eq!(created_id("[{\"id\":\"sp-k\"}]").as_deref(), Some("sp-k"));
        assert_eq!(created_id("  \n"), None);
        assert_eq!(created_id("warning: something odd"), None);
    }

    #[test]
    fn created_ids_reads_import_and_mol_json() {
        let import = "{\"created\":2,\"ids\":[\"sp-a\",\"sp-b\"],\"skipped\":0}";
        assert_eq!(created_ids(import), vec!["sp-a", "sp-b"]);
        let pour = "{\"created\":2,\"id_mapping\":{\"p\":\"sp-mol-1\",\"p.c\":\"sp-mol-1.c\"},\"new_epic_id\":\"sp-mol-1\"}";
        assert_eq!(created_ids(pour), vec!["sp-mol-1", "sp-mol-1.c"]);
        let bond = "{\"result_id\":\"sp-x\",\"result_type\":\"compound_molecule\"}";
        assert_eq!(created_ids(bond), vec!["sp-x"]);
        assert_eq!(created_ids("sp-abc\n"), vec!["sp-abc"]);
        assert!(created_ids("{\"error\":\"boom\"}").is_empty());
        assert!(created_ids("Imported 2 issues").is_empty());
    }

    #[test]
    fn after_create_ids_gives_every_id_a_row_and_reports_unreadable_output() {
        let t = testkit::TempDir::new("lcrow-ids");
        let bin = stub(t.path(), 0);
        assert_eq!(ensure_rows_with(&bin, &["sp-a".into(), "sp-b".into()]), Ok(()));
        let calls = std::fs::read_to_string(t.path().join("calls")).unwrap();
        assert_eq!(calls, "create-bead sp-a\ncreate-bead sp-b\n");
        assert!(after_create_ids("t", "no ids here").is_err());
    }

    #[test]
    fn ensure_row_creates_the_row() {
        let t = testkit::TempDir::new("lcrow");
        let bin = stub(t.path(), 0);
        assert_eq!(ensure_row_with(&bin, "sp-x"), Ok(()));
        assert_eq!(std::fs::read_to_string(t.path().join("calls")).unwrap(), "create-bead sp-x\n");
    }

    #[test]
    fn a_failing_create_bead_is_retried_then_reported() {
        let t = testkit::TempDir::new("lcrow-retry");
        let bin = stub(t.path(), 3);
        assert!(ensure_row_retrying(&bin, "sp-r", 3, 0).is_err());
        let calls = std::fs::read_to_string(t.path().join("calls")).unwrap();
        assert_eq!(calls.lines().count(), 3);
    }

    #[test]
    fn a_failing_create_bead_is_an_error() {
        let t = testkit::TempDir::new("lcrow-fail");
        let bin = stub(t.path(), 3);
        assert!(ensure_row_with(&bin, "sp-z").unwrap_err().contains("exited 3"));
        assert!(ensure_row_with("/nonexistent/lc", "sp-z").is_err());
    }

    #[test]
    fn a_close_runs_reconcile_closed_for_exactly_the_bead_it_closed() {
        let t = testkit::TempDir::new("lcrow-close");
        let bin = stub(t.path(), 0);
        assert_eq!(terminalize_closed_with(&bin, "sp-c"), Ok(()));
        assert_eq!(std::fs::read_to_string(t.path().join("calls")).unwrap(), "reconcile-closed --apply sp-c\n");
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(closed_id(&a(&["close", "sp-c", "--reason-file", "-"])), Some("sp-c"));
        assert_eq!(closed_id(&a(&["close", "--force", "sp-c"])), None);
        assert_eq!(closed_id(&a(&["update", "sp-c"])), None);
        assert!(terminalize_closed_with(&stub(t.path(), 3), "sp-c").unwrap_err().contains("exited 3"));
    }

    #[test]
    fn close_hands_spira_lc_the_reason_on_stdin_and_reports_its_refusal() {
        let t = testkit::TempDir::new("lcclose");
        let log = t.path().join("calls");
        let stub = |name: &str, exit: i32| {
            let p = t.path().join(name);
            testkit::write_exe(&p, &format!("#!/bin/sh\necho \"$@\" >> {l}\ncat >> {l}\necho >> {l}\nexit {exit}\n", l = log.display()));
            p.to_string_lossy().into_owned()
        };
        let (ok, refused) = (stub("lc-ok", 0), stub("lc-refused", 3));
        assert_eq!(close_with(&ok, "sp-a", "two\nlines", "operator", Some("sp-b")), Ok(()));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.starts_with("close sp-a --reason-file - --actor operator --superseded-by sp-b\ntwo\nlines"), "{calls}");
        assert!(close_with(&refused, "sp-a", "r", "operator", None).unwrap_err().contains("exited 3"));
    }

    #[test]
    fn reopen_hands_spira_lc_the_cause_and_actor_and_fails_only_on_a_refusal_or_silence() {
        let t = testkit::TempDir::new("lcreopen");
        assert_eq!(reopen_with(&stub(t.path(), 0), "sp-a", "eject", "harness"), Ok(()));
        assert_eq!(std::fs::read_to_string(t.path().join("calls")).unwrap(), "reopen sp-a eject harness\n");
        let t1 = testkit::TempDir::new("lcreopen-norow");
        assert_eq!(reopen_with(&stub(t1.path(), 1), "sp-a", "c", "a"), Ok(()), "no row: nothing to reopen");
        for exit in [2, 3] {
            let tn = testkit::TempDir::new("lcreopen-bad");
            assert!(reopen_with(&stub(tn.path(), exit), "sp-a", "c", "a").unwrap_err().contains(&format!("exited {exit}")));
        }
    }
}
