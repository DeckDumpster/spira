//! Every path that creates a bead creates its lifecycle row too: a bead with no
//! `spira_lifecycle` row can never be claimed.

use lifecycle::bead::BeadEventKind;

use crate::lc_call::{LcClient, LcError, LC_TIMEOUT, REFUSED};

pub const LC_BIN_ENV: &str = "SPIRA_LC_BIN";

pub fn lc_bin() -> Result<String, String> {
    match std::env::var(LC_BIN_ENV).ok().filter(|v| !v.is_empty()) {
        Some(v) => Ok(v),
        None => crate::release_env::tool("spira-lc"),
    }
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
    create_ask_with(&lc_bin()?, id, work_bead)
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
    close_ask_with(&lc_bin()?, id, exit, quote, actor, channel)
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
    is_ask_with(&lc_bin()?, id)
}

fn client(bin: &str) -> Result<LcClient, String> {
    // batch-job: a write through the CLI queues behind a loaded store; the socket keeps its 5 s.
    Ok(LcClient::for_bin(bin)?.within(LC_TIMEOUT))
}

fn run_lc(bin: &str, args: &[&str]) -> Result<(i32, String), String> {
    run_on(&client(bin)?, args)
}

fn run_on(c: &LcClient, args: &[&str]) -> Result<(i32, String), String> {
    let r = c.call(args)?;
    Ok((r.code, r.text()))
}

fn lc_status(bin: &str, args: &[&str]) -> Result<(), String> {
    status_on(&client(bin)?, args).map_err(|e| format!("{bin} {e}"))
}

fn status_on(c: &LcClient, args: &[&str]) -> Result<(), String> {
    match run_on(c, args)? {
        (0, _) => Ok(()),
        (code, msg) => Err(format!("{} exited {code}: {msg}", args.join(" "))),
    }
}

/// `express`/`unexpress` is a caller verb of the CLI; the socket serves its primitives, so a
/// socket client reads the row and sends the event itself, as the CLI does.
pub fn set_express_on(c: &LcClient, id: &str, on: bool) -> Result<(), String> {
    let verb = if on { "express" } else { "unexpress" };
    if !c.is_socket() {
        return status_on(c, &[verb, id]);
    }
    let shown = c.call(&["show", id])?;
    if shown.code != 0 {
        return Err(format!("{verb} {id}: no lifecycle row to mark: {}", shown.why()));
    }
    let v: serde_json::Value = serde_json::from_str(shown.out.trim()).map_err(|e| format!("{verb} {id}: show is not JSON: {e}"))?;
    let field = |k: &str| match &v["bead"][k] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let (state, version) = (field("state"), field("version"));
    if state.is_empty() || version.is_empty() {
        return Err(format!("{verb} {id}: no lifecycle row to mark"));
    }
    let kind = if on { BeadEventKind::Express } else { BeadEventKind::Unexpress };
    let kind = serde_json::to_string(&kind).map_err(|e| e.to_string())?;
    status_on(c, &["event", "bead", id, "--expect", &state, "--version", &version, "--actor", "bead", "--kind", &kind])
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
    lc_status(bin, &["create-bead", id, "--ready"])
}

/// Record `Express`/`Unexpress` on the bead's lifecycle row: express is lifecycle state.
pub fn set_express_with(bin: &str, id: &str, on: bool) -> Result<(), String> {
    set_express_on(&client(bin)?, id, on).map_err(|e| format!("{bin} {e}"))
}

pub fn set_express(id: &str, on: bool) -> Result<(), String> {
    set_express_with(&lc_bin()?, id, on)
}

/// Filing has finished: the row born OPEN before the bead's edges were written becomes READY.
pub fn promote_with(bin: &str, id: &str) -> Result<(), String> {
    lc_status(bin, &["promote-filed", id])
}

pub fn promote(id: &str) -> Result<(), String> {
    promote_with(&lc_bin()?, id)
}

pub fn ensure_row(id: &str) -> Result<(), String> {
    ensure_row_retrying(&lc_bin()?, id, ROW_ATTEMPTS, 250)
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
    lc_status(bin, &["reconcile-closed", "--apply", id])
}

/// Call after a successful `bd close`. A failure is reported and returned; the bead is
/// closed, so the caller must not retry the close.
pub fn after_close(who: &str, args: &[String]) -> Result<(), String> {
    let Some(id) = closed_id(args) else { return Ok(()) };
    terminalize_closed_with(&lc_bin()?, id).map_err(|e| {
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
/// row itself, so this writes no event; bd closes once the row reads LANDED, and spira-lc
/// refuses it for a row not in delivery.
pub fn close_landed(id: &str, reason: &str, actor: &str) -> Result<(), String> {
    close_args(&lc_bin()?, id, reason, actor, vec!["--landing"])
}

fn close_args(bin: &str, id: &str, reason: &str, actor: &str, extra: Vec<&str>) -> Result<(), String> {
    let c = client(bin)?;
    close_on(&c, id, reason, actor, &extra).map_err(|e| match e {
        LcError::Refused(m) => format!("{bin} close {id} exited {REFUSED}: {m}"),
        LcError::Fault(m) => format!("{bin} close {id}: {m}"),
    })
}

/// A close, typed: a refusal (exit 3) and a fault (anything else, or no answer) are variants.
pub fn close_on(c: &LcClient, id: &str, reason: &str, actor: &str, extra: &[&str]) -> Result<(), LcError> {
    let mut args = vec!["close", id, "--reason-file", "-", "--actor", actor];
    args.extend(extra);
    let r = c.call_stdin(&args, reason)?;
    match r.code {
        0 => Ok(()),
        REFUSED => Err(LcError::Refused(r.text())),
        _ => Err(LcError::Fault(format!("exited {}: {}", r.code, r.text()))),
    }
}

/// A close by `actor` through the configured `spira-lc`, typed: the caller reads refusal from the variant.
pub fn close_as(id: &str, reason: &str, actor: &str) -> Result<(), LcError> {
    let c = lc_bin().and_then(|b| client(&b)).map_err(LcError::Fault)?;
    close_on(&c, id, reason, actor, &[])
}

/// [`close_with`] through the configured `spira-lc`.
pub fn close(id: &str, reason: &str, actor: &str, superseded_by: Option<&str>) -> Result<(), String> {
    close_with(&lc_bin()?, id, reason, actor, superseded_by)
}

/// Return a bead to its builder through the lifecycle machine (`spira-lc reopen`, sp-swh8b8):
/// the row records the event its own state implies. No row (exit 1) is nothing to reopen and is
/// `Ok`; a terminal row (3) or an unreachable machine (2) is an error. Every Rust reopener
/// calls this rather than handing bd a `reopen` (lifecycle-guard's `bd-reopen-rust`).
pub fn reopen_with(bin: &str, id: &str, cause: &str, actor: &str) -> Result<(), String> {
    match run_lc(bin, &["reopen", id, cause, actor])? {
        (0 | 1, _) => Ok(()),
        (code, msg) => Err(format!("{bin} reopen {id} exited {code}: {msg}")),
    }
}

/// Point the bead's lifecycle row at `repo`: the row is what a claim reads to pick a checkout.
pub fn set_repo_with(bin: &str, id: &str, repo: &str) -> Result<(), String> {
    lc_status(bin, &["set-repo", id, repo])
}

pub fn set_repo(id: &str, repo: &str) -> Result<(), String> {
    set_repo_with(&lc_bin()?, id, repo)
}

/// [`reopen_with`] through the configured `spira-lc`.
pub fn reopen(id: &str, cause: &str, actor: &str) -> Result<(), String> {
    reopen_with(&lc_bin()?, id, cause, actor)
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
}
