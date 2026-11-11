//! Shells to `bd` for the non-lifecycle half of the dwarf semantic layer (design §3.5):
//! titles, descriptions, dependencies, notes and filing stay bd's job (§3.3/3.4) — this
//! module never touches `spira_lifecycle`. `SPIRA_BD`/`SPIRA_DB` are declared config
//! (spira/conf.d), resolved from `$SPIRA_TOML` (one source of config, per Ryan 2026-10-05) —
//! never this process's own environment. The content door's reads are not here: they are SQL
//! (`content_sql`).

use std::process::Command;

use crate::content_sql::{self, Failure, Sql};
use crate::content_sql::Bead;

fn bd_bin() -> Result<String, String> {
    // SPIRA_BD resolves empty when unset (no default in its registry entry); an empty program
    // name fails every call, so the door's store half silently never ran in such a setup
    // (sp-swh8b8). Empty means bd on PATH, as every other bd caller reads it.
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

/// Every planning write this process makes moves the planning watermark once it has landed.
fn stamped<S: AsRef<str>>(args: &[S], res: Result<String, String>) -> Result<String, String> {
    if res.is_ok() && spira_config::planning::is_planning_write(args) {
        crate::plan::stamp_after_write();
    }
    res
}

const WRITE_SECS: u64 = 60;

// A write runs the schema gate and a Dolt commit; the 5 s read cap kills it mid-commit though it lands.
fn run_write(args: &[&str]) -> Result<String, String> {
    stamped(args, write_unstamped(args))
}

/// A write that is not a planning write: the watermark's own copy in bd's kv.
pub fn write_unstamped(args: &[&str]) -> Result<String, String> {
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

pub fn write(args: &[&str]) -> Result<String, String> {
    run_write(args)
}

/// A write with `stdin` as bd's stdin (`note --stdin`, `create --body-file -`), under the
/// write bound and stamped like every planning write.
pub fn write_stdin(args: &[String], stdin: Option<&str>) -> Result<String, String> {
    let started = std::time::Instant::now();
    let res = write_stdin_unrecorded(args, stdin);
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    record_call(&words, started, res.is_ok());
    stamped(args, res)
}

fn write_stdin_unrecorded(args: &[String], stdin: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("timeout");
    cmd.arg(WRITE_SECS.to_string()).arg(bd_bin()?);
    let db = spira_config::process::cfg("SPIRA_DB")?;
    if !db.is_empty() {
        cmd.args(["-C", &db]);
    }
    cmd.args(args);
    match spawn(cmd, stdin, "bd") {
        (0, out) => Ok(out),
        (_, out) => Err(out),
    }
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

/// Runs `ask` on the content store's read-only connection: lc-serve forks bd for writes only.
fn store<T>(ask: impl FnOnce(&dyn Sql, &str) -> Result<T, Failure>) -> Result<T, String> {
    crate::content_sql::served()?.with(ask).map_err(|f| f.text().to_string())
}

/// bd's kv value under `key`; `None` while unset.
pub fn kv_get(key: &str) -> Result<Option<String>, String> {
    store(|sql, db| content_sql::kv(sql, db, key))
}

/// `(id, title)` of the open beads whose title contains `needle`.
pub fn titled(needle: &str) -> Result<Vec<(String, String)>, String> {
    Ok(store(|sql, db| content_sql::titled(sql, db, needle))?.into_iter().map(|b| (b.id, b.title)).collect())
}

const NO_SUCH: &str = "no issue found matching";

pub fn show(bead_id: &str) -> Result<String, String> {
    store(|sql, db| content_sql::show_text(sql, db, bead_id))?.ok_or_else(|| format!("{NO_SUCH} {bead_id:?}"))
}

fn bead(bead_id: &str) -> Result<Option<Bead>, String> {
    store(|sql, db| content_sql::bead(sql, db, bead_id))
}

/// The bound bead's own `repo:<name>` label, so `file-followup`/`split` never ask the
/// dwarf for a repo it could get wrong.
pub fn repo_label(bead_id: &str) -> Result<Option<String>, String> {
    Ok(labels(bead_id)?.iter().find_map(|l| l.strip_prefix("repo:").map(str::to_string)))
}

/// `bead_id`'s title; `Ok(None)` when the store holds no such bead.
pub fn title(bead_id: &str) -> Result<Option<String>, String> {
    Ok(bead(bead_id)?.map(|b| b.title))
}

/// Every label on `bead_id`.
pub fn labels(bead_id: &str) -> Result<Vec<String>, String> {
    Ok(bead(bead_id)?.map(|b| b.labels).unwrap_or_default())
}

fn closed_of(found: Vec<Bead>) -> Vec<(String, String)> {
    found.into_iter().filter(|b| b.status == "closed").map(|b| (b.id, b.close_reason.unwrap_or_default())).collect()
}

fn open_of(found: Vec<Bead>) -> Vec<String> {
    found.into_iter().filter(|b| b.status != "closed").map(|b| b.id).collect()
}

fn is_external(b: &Bead) -> bool {
    b.external_ref.as_deref().is_some_and(|r| !r.trim().is_empty())
}

/// The live [`crate::callers::Bd`]: reads from the content store, writes through bd.
pub struct LiveBd;

impl crate::callers::Bd for LiveBd {
    fn planning_watermark(&mut self) -> Result<Option<u64>, String> {
        let held = kv_get(crate::skew::BD_KV_KEY)?;
        crate::skew::parse_kv(held.as_deref())
    }
    fn issue_type(&mut self, id: &str) -> Result<String, String> {
        bead(id)?.map(|b| b.issue_type).ok_or_else(|| format!("bd show {id}: {NO_SUCH}"))
    }
    fn close(&mut self, id: &str, reason: &str) -> Result<(), String> {
        // --force: the lifecycle row is already terminal when `close` gets here, so the store
        // must follow it; a refused store close would leave the two disagreeing again.
        let res = run_write(&["close", id, "--force", "--reason", reason]).map(|_| ());
        settled(res, id, || self.open_among(&[id.to_string()]))
    }
    fn closed(&mut self, ids: &[String]) -> Result<Vec<(String, String)>, String> {
        let found = store(|sql, db| content_sql::statuses(sql, db, ids))?;
        Ok(closed_of(found))
    }
    fn known(&mut self, ids: &[String]) -> Result<Vec<String>, String> {
        Ok(store(|sql, db| content_sql::statuses(sql, db, ids))?.into_iter().map(|b| b.id).collect())
    }
    fn open_among(&mut self, ids: &[String]) -> Result<Vec<String>, String> {
        Ok(open_of(store(|sql, db| content_sql::statuses(sql, db, ids))?))
    }
    fn work_beads(&mut self, id: &str) -> Result<Vec<String>, String> {
        Ok(labels(id)?.iter().filter_map(|l| l.strip_prefix("work-bead:").map(str::to_string)).collect())
    }
    fn blocking_dependents(&mut self, id: &str) -> Result<Vec<String>, String> {
        let ids = store(|sql, db| content_sql::blocked_by(sql, db, id))?;
        if ids.is_empty() {
            return Ok(ids);
        }
        self.open_among(&ids)
    }
    fn external(&mut self, id: &str) -> Result<bool, String> {
        let b = bead(id)?.ok_or_else(|| format!("bd show {id}: {NO_SUCH}"))?;
        Ok(is_external(&b))
    }
    fn reopen(&mut self, id: &str) -> Result<(), String> {
        // The store follows the row the door just moved: open, unassigned, and no longer wearing
        // the submitted label (a legacy state label; best effort, it may not be there).
        run_write(&["update", id, "--status", "open", "--assignee", ""]).map(|_| ())?;
        if let Some(label) = spira_config::process::cfg("SPIRA_SUBMITTED_LABEL").ok().filter(|l| !l.is_empty()) {
            let _ = run_write(&["label", "remove", id, &label]);
        }
        Ok(())
    }
}

pub fn note(bead_id: &str, text: &str) -> Result<String, String> {
    run_write(&["note", bead_id, text])
}

/// Files through `spira/bead.sh`'s contract (the labelling rules `bd create` alone does not
/// enforce), with `--parent` set by this layer, never the caller — see bead.sh's own
/// `--parent` doc for why an inherited `branch:` label made `groomer split-piece` a
/// two-step dance that this avoids by filing with the right labels from the start.
pub fn file_child(title: &str, persona: &str, repo: &str, parent: Option<&str>, body: Option<&str>) -> Result<String, String> {
    file_child_via("bead.sh", FILING_SECS, title, persona, repo, parent, body)
}

/// A filing is a store write that queues behind a loaded host's other writes; five seconds
/// turned load into an answer with no reason.
const FILING_SECS: &str = "60";

fn failed(program: &str, secs: &str, args: &[&str], out: &std::process::Output) -> String {
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    match out.status.code() {
        Some(124) => format!("`{program} {}` timed out after {secs}s and may have applied; read the store before repeating it", args.join(" ")),
        _ if said.trim().is_empty() => format!("`{program} {}` failed ({}) and said nothing", args.join(" "), out.status),
        _ => said,
    }
}

fn file_child_via(program: &str, secs: &str, title: &str, persona: &str, repo: &str, parent: Option<&str>, body: Option<&str>) -> Result<String, String> {
    let mut args = vec!["file", title, "--for", persona, "--repo", repo];
    if let Some(parent) = parent {
        args.extend(["--parent", parent]);
    }
    let mut cmd = spira_config::bounded::bounded_for(program, secs);
    cmd.args(&args);
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
    .map_err(|e| format!("running {program}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(failed(program, secs, &["file", title], &out));
    }
    Ok(stdout.trim().to_string())
}

fn bead_sh(args: &[&str]) -> Result<(), String> {
    bead_sh_via("bead.sh", FILING_SECS, args)
}

fn bead_sh_via(program: &str, secs: &str, args: &[&str]) -> Result<(), String> {
    let out = spira_config::bounded::bounded_for(program, secs).args(args).output().map_err(|e| format!("running {program}: {e}"))?;
    if !out.status.success() {
        return Err(failed(program, secs, args, &out));
    }
    Ok(())
}

/// `bead.sh dep add <id> <depends-on>`: `id` is blocked until `depends-on` is closed.
pub fn add_blocker(id: &str, depends_on: &str) -> Result<(), String> {
    bead_sh(&["dep", "add", id, depends_on, "--type", "blocks"])
}

pub fn label_add(id: &str, label: &str) -> Result<(), String> {
    bead_sh(&["label", "add", id, label])
}

/// `mail send operator` — an ask carrying the channel back (design §3.5's `work
/// blocked`, and the supersede-by confirmation request). `--bead` ties the tracking
/// decision bead mail files to the one this layer is bound to.
pub fn ask_operator(from: &str, subject: &str, default: &str, bead_id: &str, body: &str) -> Result<String, String> {
    ask_operator_classed(from, subject, default, None, bead_id, body)
}

/// [`ask_operator`] with an escalation `--class`: without one mail routes the ask to the
/// concierge instead of the operator.
pub fn ask_operator_classed(from: &str, subject: &str, default: &str, class: Option<&str>, bead_id: &str, body: &str) -> Result<String, String> {
    // mail, by name on the launcher's PATH (sp-gypjk); SPIRA_MAIL_SH is the harness-wide
    // binary-override seam.
    let mail_sh = std::env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| "mail".to_string());
    let mut cmd = spira_config::bounded::bounded(&mail_sh);
    cmd.args(["send", "operator", "--from", from, "--subject", subject, "--kind", "question", "--default", default, "--bead", bead_id]);
    if let Some(c) = class {
        cmd.args(["--class", c]);
    }
    let mut child = cmd
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
    stamped(args, res)
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
/// the caller's persona as `SPIRA_RUNE` — the czar fence in `queue` and `bdq` reads it, and
/// the broker's own identity is never the persona that asked. Returns the tool's own exit code
/// (stdout; stdout plus stderr when it failed).
pub fn tool(program: &str, args: &[String], stdin: Option<&str>, actor: &str, secs: u64) -> (i32, String) {
    tool_env(program, args, stdin, actor, secs, &[])
}

/// [`tool`], with `env` set on the child as well.
pub fn tool_env(program: &str, args: &[String], stdin: Option<&str>, actor: &str, secs: u64, env: &[(&str, &str)]) -> (i32, String) {
    let exe = match resolve_tool(program) {
        Ok(p) => p,
        Err(e) => return (2, e),
    };
    let mut cmd = Command::new("timeout");
    cmd.arg(secs.to_string()).arg(&exe).args(args).env("SPIRA_RUNE", actor);
    for (k, v) in env {
        cmd.env(k, v);
    }
    spawn(cmd, stdin, program)
}

/// [`tool_env`] for a tool whose stderr is part of its answer even on success (mail's
/// "routed to the concierge" notice): nothing it says is dropped.
pub fn tool_env_noisy(program: &str, args: &[String], stdin: Option<&str>, actor: &str, secs: u64, env: &[(&str, &str)]) -> (i32, String) {
    let exe = match resolve_tool(program) {
        Ok(p) => p,
        Err(e) => return (2, e),
    };
    let mut cmd = Command::new("timeout");
    cmd.arg(secs.to_string()).arg(&exe).args(args).env("SPIRA_RUNE", actor);
    for (k, v) in env {
        cmd.env(k, v);
    }
    spawn_with(cmd, stdin, program, true)
}

/// A bare tool name is the declared release's (`$SPIRA_RELEASE`); a path is taken as given.
/// Never a `PATH` search or the directory this broker happens to run from.
fn resolve_tool(program: &str) -> Result<String, String> {
    if program.contains('/') {
        return Ok(program.to_string());
    }
    spira_config::release_env::tool(program).map_err(|e| format!("running {program}: {e}"))
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
mod verb_tests {
    use super::verb_of;

    #[test]
    fn the_verb_is_the_first_word_that_is_not_a_flag() {
        assert_eq!(verb_of(&["--json", "show", "sp-1"]), "show");
        assert_eq!(verb_of(&["update", "sp-1"]), "update");
        assert_eq!(verb_of(&["--version"]), "unknown");
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[test]
    fn a_path_is_never_searched() {
        assert_eq!(resolve_tool("/x/y").unwrap(), "/x/y");
    }
}

#[cfg(test)]
mod filing_tests {
    use super::*;

    fn slow_bead_sh(dir: &std::path::Path) -> String {
        let p = dir.join("bead.sh");
        testkit::write_exe(&p, "#!/bin/sh\nsleep 5\n");
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn a_filing_that_outlives_its_deadline_names_the_call_that_timed_out() {
        let dir = testkit::TempDir::new("filing");
        let prog = slow_bead_sh(dir.path());
        let err = file_child_via(&prog, "0.2", "t", "builder", "r", Some("sp-x"), None).unwrap_err();
        assert!(err.contains("bead.sh file") && err.contains("timed out"), "reason names the call: {err:?}");
    }

    #[test]
    fn a_label_or_dependency_call_that_outlives_its_deadline_names_itself() {
        let dir = testkit::TempDir::new("filing-dep");
        let prog = slow_bead_sh(dir.path());
        let err = bead_sh_via(&prog, "0.2", &["label", "add", "sp-x", "split"]).unwrap_err();
        assert!(err.contains("label add sp-x split") && err.contains("timed out"), "{err:?}");
    }

    #[test]
    fn a_filing_waits_longer_than_the_plain_call_deadline() {
        let filing: u64 = FILING_SECS.parse().unwrap();
        let plain: u64 = spira_config::bounded::CALL_TIMEOUT_SECS.parse().unwrap();
        assert!(filing > plain);
    }
}

#[cfg(test)]
mod store_read_tests {
    use super::*;

    fn row(id: &str, status: &str, reason: Option<&str>) -> Bead {
        Bead { id: id.into(), status: status.into(), close_reason: reason.map(str::to_string), ..Bead::default() }
    }

    #[test]
    fn closed_rows_carry_their_reason_and_open_ones_are_every_other_status() {
        let found = vec![row("sp-a", "closed", Some("done")), row("sp-b", "in_progress", None), row("sp-c", "closed", None)];
        assert_eq!(closed_of(found.clone()), [("sp-a".to_string(), "done".to_string()), ("sp-c".to_string(), String::new())]);
        assert_eq!(open_of(found), ["sp-b"]);
    }

    #[test]
    fn a_blank_external_ref_is_not_external() {
        let b = |r: Option<&str>| Bead { external_ref: r.map(str::to_string), ..Bead::default() };
        assert!(is_external(&b(Some("github:o/r#4"))));
        assert!(!is_external(&b(Some("  "))) && !is_external(&b(None)));
    }
}

#[cfg(test)]
mod no_read_fork_tests {
    #[test]
    fn this_module_forks_bd_for_no_read_verb() {
        let code = include_str!("bd.rs").split("#[cfg(test)]").next().unwrap();
        for read in ["\"show\"", "\"list\"", "\"kv\"", "\"dep\", \"list\"", "fn run("] {
            assert!(!code.contains(read), "bd.rs forks bd for {read}");
        }
    }
}
