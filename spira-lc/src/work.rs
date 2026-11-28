//! The dwarf semantic layer (design §3.5): show/note/submit/done/blocked/file-followup/
//! split/superseded-by, dispatched from the same socket and `Conn` as show/list/history/
//! event. This dispatch is itself generic — it acts on whatever bead id its first
//! argument names, like every other spira-lc verb — because binding to one called bead
//! is the `work` client crate's job (a separate, thinner binary that never links a direct
//! DB connection at all), not this service's. See main.rs's `dispatch` for the socket path
//! this shares with `show`/`list`/`history`/`event`.
//!
//! Deploys inert: nothing calls `work` over the socket yet.

use lifecycle::bead::{self, BeadEventKind, HoldKind};
use lifecycle::reason::HoldCause;

use crate::content_sql;
use crate::{flag, CANNOT_TELL, REFUSED};

pub fn dispatch(args: &[String]) -> (i32, String) {
    let Some(bead_id) = args.first() else {
        return (CANNOT_TELL, "work: missing <bead-id>".into());
    };
    let Some(verb) = args.get(1) else {
        return (CANNOT_TELL, "work: missing <verb>".into());
    };
    let rest = &args[2..];
    // The persona gate (sp-st0mm): every verb, before anything it does.
    if let Err(refusal) = gate(verb, rest) {
        return refusal;
    }
    if crate::plan::serving() && exec_verb(verb) {
        return record(bead_id, args);
    }
    run(bead_id, verb, rest)
}

/// The verb's handler, run in the calling process: `lc-serve` for a verb that starts no program,
/// the `work-exec` process for one that does.
fn run(bead_id: &str, verb: &str, rest: &[String]) -> (i32, String) {
    match verb {
        "show" => cmd_show(bead_id),
        "note" => cmd_note(bead_id, rest),
        "submit" => cmd_submit(bead_id, rest),
        "done" => cmd_done(bead_id, rest),
        "blocked" => cmd_blocked(bead_id, rest),
        "file-followup" => cmd_file(bead_id, rest, "file-followup"),
        "split" => cmd_file(bead_id, rest, "split"),
        "superseded-by" => cmd_superseded_by(bead_id, rest),
        "ack" => cmd_ack(bead_id, rest),
        "logs" => cmd_logs(bead_id, rest),
        "cite-check" => cmd_cite_check(bead_id, rest, &mut |ids| crate::callers::Bd::known(&mut crate::bd::LiveBd, ids)),
        "fence" => cmd_fence(rest),
        "forge" => cmd_forge(bead_id, rest),
        other if LANE_VERBS.contains(&other) => cmd_lane(bead_id, other, rest),
        other => (
            CANNOT_TELL,
            format!("work: unknown verb {other:?} (want show, note, submit, done, blocked, file-followup, split, superseded-by, ack, logs, cite-check, {})", LANE_VERBS.join(", ")),
        ),
    }
}

/// `work cite-check -`: the message on stdin, judged by the bound bead's own id prefix.
fn cmd_cite_check(bead_id: &str, rest: &[String], known: &mut spira_config::cite::Lookup<'_>) -> (i32, String) {
    let Some(message) = split_reserved(rest).stdin else {
        return (CANNOT_TELL, "work cite-check: usage: work cite-check - (the commit message on stdin)".into());
    };
    crate::cite_check::message_verdict(&message, bead_id.split('-').next().unwrap_or_default(), known)
}

fn require_actor(args: &[String]) -> Result<String, (i32, String)> {
    flag(args, "--actor").ok_or((CANNOT_TELL, "work: --actor is required".to_string()))
}

fn cmd_show(bead_id: &str) -> (i32, String) {
    let text = match crate::bd::show(bead_id) {
        Ok(text) => text,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    match crate::plane::read(&[s("show"), bead_id.to_string()]) {
        (0, row) => {
            let facts = crate::facts::read(&[bead_id.to_string()], &[spira_config::ejection::KIND.to_string()], None).map(crate::facts::shape);
            (0, with_logs(show_text(&text, &row, facts), kept_entries(bead_id).len()))
        }
        (1, _) => (0, format!("{text}\nLIFECYCLE: no row\n")),
        (_, e) => (CANNOT_TELL, e),
    }
}

const LOGS_KIND: &str = spira_config::ejection::LOGS_KIND;

fn with_logs(text: String, kept: usize) -> String {
    if kept == 0 {
        text
    } else {
        format!("{text}LOGS: {kept} kept — work logs\n")
    }
}

/// The bead's `send-back-logs` facts, newest first: (epoch, actor, entry id).
fn kept_entries(bead_id: &str) -> Vec<(i64, String, String)> {
    let rows = crate::facts::read(&[bead_id.to_string()], &[LOGS_KIND.to_string()], None).unwrap_or_default();
    entries_of(&rows)
}

fn entries_of(rows: &[serde_json::Value]) -> Vec<(i64, String, String)> {
    let text = |r: &serde_json::Value, k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut out: Vec<(i64, String, String)> = rows
        .iter()
        .filter(|r| text(r, "event_type") == LOGS_KIND)
        .map(|r| (text(r, "at").parse().unwrap_or(0), text(r, "actor"), text(r, "new_value")))
        .collect();
    out.reverse();
    out
}

/// `work logs [<entry> <file>]`: the entries this bead's own facts name, or one file of one.
fn cmd_logs(bead_id: &str, args: &[String]) -> (i32, String) {
    let run = match spira_config::process::cfg("SPIRA_RUN") {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let rows = match crate::facts::read(&[bead_id.to_string()], &[LOGS_KIND.to_string()], None) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    logs_out(&rows, std::path::Path::new(&run), &split_reserved(args).args)
}

fn logs_out(rows: &[serde_json::Value], run: &std::path::Path, args: &[String]) -> (i32, String) {
    use spira_config::sendback;
    let kept = entries_of(rows);
    match args {
        [] if kept.is_empty() => (0, "no send-back logs kept for this bead\n".into()),
        [] => {
            let mut out = String::new();
            for (at, actor, id) in &kept {
                out += &format!("{} {actor} {id}\n", crate::callers::fmt_utc(*at));
                match sendback::list(run, id) {
                    Ok(files) => files.iter().for_each(|(f, n)| out += &format!("    {f} {n} bytes\n")),
                    Err(e) => out += &format!("    ({e})\n"),
                }
            }
            (0, out)
        }
        [id, file] => {
            if !kept.iter().any(|(_, _, k)| k == id) {
                return (REFUSED, format!("work logs: {id:?} is not an entry this bead's facts name; `work logs` lists them"));
            }
            match sendback::read(run, id, file) {
                Ok(bytes) => (0, String::from_utf8_lossy(&bytes).into_owned()),
                Err(e) => (REFUSED, format!("work logs: {e}")),
            }
        }
        _ => (CANNOT_TELL, "work logs: usage: work logs [<entry> <file>]".into()),
    }
}

/// bd's `show` text with its status word replaced by the lifecycle state, then the row itself:
/// a session reads the machine's state, never bd's status, as where the bead stands.
fn with_lifecycle(bd_text: &str, show_json: &str) -> String {
    let row: serde_json::Value = serde_json::from_str(show_json).unwrap_or_default();
    let bead = &row["bead"];
    let state = bead["state"].as_str().unwrap_or("?");
    let mut lines: Vec<String> = bd_text.lines().map(str::to_string).collect();
    if let Some(head) = lines.first_mut() {
        if let (Some(dot), true) = (head.rfind(" · "), head.ends_with(']')) {
            head.replace_range(dot + " · ".len().., &format!("{state}]"));
        }
    }
    let field = |k: &str| bead[k].as_str().filter(|v| !v.is_empty()).map(|v| format!("  {k}: {v}\n")).unwrap_or_default();
    format!(
        "{}\n\nLIFECYCLE: {state}\n{}{}{}{}{}",
        lines.join("\n"),
        field("holds"),
        field("reason"),
        field("holder"),
        field("tip"),
        row["delivery"]["state"].as_str().map(|d| format!("  delivery: {d}\n")).unwrap_or_default(),
    )
}

/// bd's text, the lifecycle block, then the last red: a failed fact read costs one line, never the show.
fn show_text(bd_text: &str, row_json: &str, facts: Result<Vec<serde_json::Value>, String>) -> String {
    let row: serde_json::Value = serde_json::from_str(row_json).unwrap_or_default();
    let shown = with_lifecycle(bd_text, row_json);
    match facts {
        Ok(rows) => with_last_red(&shown, &rows, row["bead"]["tip"].as_str()),
        Err(e) => format!("{shown}LAST RED: cannot tell: {}\n", e.split_whitespace().collect::<Vec<_>>().join(" ")),
    }
}

/// `text` with the newest `eject-failure` row (rows come oldest first) under a `LAST RED` heading.
fn with_last_red(text: &str, facts: &[serde_json::Value], tip: Option<&str>) -> String {
    let Some(row) = facts.last() else {
        return text.to_string();
    };
    let field = |k: &str| row.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let digest = field("new_value");
    let mut out = format!("{text}LAST RED ({}, {} by {})\n", field("event_type"), field("created_at"), field("actor"));
    let judged = spira_config::ejection::parse(digest).tip;
    if let Some(now) = tip.filter(|now| !judged.is_empty() && *now != judged) {
        out.push_str(&format!("  judged tip {judged}; the bead's tip is now {now}\n"));
    }
    for l in digest.lines() {
        out.push_str(&format!("  {l}\n"));
    }
    out
}

fn cmd_note(bead_id: &str, args: &[String]) -> (i32, String) {
    let Some(text) = args.first() else {
        return (CANNOT_TELL, "work note: missing <text>".into());
    };
    // `work note -`: the text arrived on the client's stdin, carried as `--stdin` (prose
    // belongs on stdin — law-commit-messages-via-stdin).
    if text == "-" {
        let call = split_reserved(args);
        let Some(body) = call.stdin else {
            return (CANNOT_TELL, "work note: `-` given but no text arrived on stdin".into());
        };
        return match crate::bd::run_stdin(&[s("note"), bead_id.to_string(), s("--stdin")], Some(&body)) {
            Ok(out) => (0, out),
            Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
        };
    }
    match crate::bd::note(bead_id, text) {
        Ok(out) => (0, out),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

fn unanswered_amendment_refusal(bead_id: &str) -> Option<(i32, String)> {
    let mail = spira_config::process::cfg("SPIRA_MAIL").ok().filter(|m| !m.is_empty())?;
    spira_config::amendment::submit_refusal(&mail, bead_id).map(|m| (REFUSED, m))
}

/// `work ack <amend-id> --accept | --decline "<why>"`: the answer is recorded and carried out by `bead.sh amend --ack`.
fn cmd_ack(bead_id: &str, args: &[String]) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let call = split_reserved(args);
    let a = &call.args;
    let Some(aid) = a.first().filter(|x| spira_config::amendment::is_amend_id(x)) else {
        return (CANNOT_TELL, "work ack: usage: work ack <amend-id> --accept | --decline \"<why>\"".into());
    };
    let verdict: Vec<String> = match (a.get(1).map(String::as_str), a.get(2), a.len()) {
        (Some("--accept"), None, 2) => vec![s("--accept")],
        (Some("--decline"), Some(why), 3) if !why.trim().is_empty() => vec![s("--decline"), why.clone()],
        _ => return (CANNOT_TELL, "work ack: give exactly one of --accept or --decline \"<why>\"".into()),
    };
    let mut argv = vec![s("amend"), bead_id.to_string(), s("--ack"), aid.clone()];
    argv.extend(verdict);
    let (code, out) = crate::bd::tool("bead.sh", &argv, None, &actor, 60);
    if code == 0 {
        (0, format!("{aid} answered{}", if out.trim().is_empty() { String::new() } else { format!("\n{out}") }))
    } else {
        (code, out)
    }
}

fn cmd_submit(bead_id: &str, args: &[String]) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(tip) = flag(args, "--tip") else {
        return (CANNOT_TELL, "work submit: --tip is required (the client reads it from the worktree; the verb itself never takes one)".into());
    };
    let row = match crate::plane::bead(bead_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("work submit: no bead row for {bead_id}")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    if let Some(refusal) = submit_state_refusal(&row, &tip) {
        return refusal;
    }
    if let Some(refusal) = working_exit_refusal("submit", "SUBMITTED", &row, &actor) {
        return refusal;
    }
    if let Some(refusal) = unanswered_amendment_refusal(bead_id) {
        return refusal;
    }
    let mut stack_tips: Vec<String> = Vec::new();
    if !row.stack.is_empty() {
        let ids: Vec<&String> = row.stack.keys().collect();
        let prereqs = match ids.iter().map(|id| crate::plane::bead(id)).collect::<Result<Vec<_>, _>>() {
            Ok(rs) => rs.into_iter().flatten().collect::<Vec<_>>(),
            Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
        };
        stack_tips = prereqs.iter().filter_map(|p| p.tip.clone()).collect();
        if let Some(refusal) = bead::submit_refusal_for_stale_stack(&row, &prereqs) {
            let lifecycle::Refusal::StackStale { prereqs: stale } = &refusal else { unreachable!() };
            return (
                REFUSED,
                format!(
                    "refused: base withdrawn on {} — rebase {bead_id} onto its current stack before resubmitting",
                    stale.join(", ")
                ),
            );
        }
    }
    if row.ejected_red_tip.as_deref() == Some(tip.as_str()) {
        let round = ejected_reasons(bead_id, &tip);
        let gate = if round.is_none() { gate_red_at(bead_id, &tip) } else { None };
        return (REFUSED, red_tip_refusal(bead_id, &tip, round.as_deref(), gate.as_ref()));
    }
    if let Some(refusal) = branch_refusal(bead_id, &tip, &stack_tips) {
        return refusal;
    }
    if let Some(refusal) = lint_refusal(bead_id, &tip, &actor) {
        return refusal;
    }
    let run = spira_config::process::cfg("SPIRA_RUN").ok().filter(|r| !r.is_empty()).map(std::path::PathBuf::from);
    if let Some(run) = &run {
        let repo = match submit_repo(bead_id) {
            Ok(r) => r,
            Err(refusal) => return refusal,
        };
        let checkout = repo_checkout(&repo);
        if let Some((root, landref)) = &checkout {
            let record = |cause: &str| crate::facts::append(&[crate::facts::fact_of(bead_id, "suite-dropped", cause, &actor, crate::db::now_epoch())]);
            if let Err(e) = spira_config::proof::drop_deleted_suites(run, bead_id, root, landref, &record) {
                return (CANNOT_TELL, format!("cannot tell: {e}"));
            }
        }
        let runner = |check: &str, secs: u64| {
            let Some((program, args)) = check_invocation(check, &tip, &repo, checkout.as_ref().map(|(r, l)| (r.as_str(), l.as_str()))) else {
                return (CANNOT_TELL, format!("{check} is a fence and {repo} names no checkout or landing ref to run it in"));
            };
            // batch-job: one check by its own runner, bounded by what is left of the submit's budget
            crate::bd::tool(program, &args, None, &actor, secs)
        };
        let owed = match owed_tiers(&tip, &repo, &actor) {
            Ok(o) => o,
            Err(refusal) => return refusal,
        };
        if let Some(refusal) = proof_refusal(run, bead_id, &tip, &owed, PROOF_BUDGET_SECS, &runner) {
            return refusal;
        }
    }
    if let Err(refusal) = record_target_verdict(bead_id, "submit", args) {
        return refusal;
    }
    if let Err(refusal) = record_unit_red_on_base(bead_id, args) {
        return refusal;
    }
    let build_dir = flag(args, "--build-dir").filter(|d| !d.is_empty());
    let done = crate::plane::apply(bead_id, &actor, BeadEventKind::Submit { tip: tip.clone() });
    if let (0, Some(dir)) = (done.0, build_dir) {
        let fact = crate::facts::build_dir(bead_id, &tip, &dir, &actor, crate::db::now_epoch());
        if let Err(e) = crate::facts::append(&[fact]) {
            return (done.0, format!("{}\nwork submit: the build directory was not recorded ({e}); the gate builds cold", done.1));
        }
    }
    if let (0, Some(run)) = (done.0, &run) {
        if !spira_config::proof::failing(run, bead_id).iter().any(|c| left_to_round(c)) {
            spira_config::proof::clear_failing(run, bead_id);
        }
    }
    done
}

/// Full-workspace checks the landing round runs on every member anyway. An in-band run outlasts
/// any RPC deadline and builds inside the broker's process tree, so submit leaves these to the
/// round: a red there ejects the bead again with the failing line.
fn left_to_round(check: &str) -> bool {
    check == "unit-tests" || spira_config::round_step::proven_by_round(check)
}

/// The runner that owns a named check: the round's lint and build steps are the repository's
/// compile-and-lint tier, a `fence-<name>` is one fence of the tip's `gate.steps`, anything else
/// is a testenv suite. `None` when a fence has no checkout to run in.
fn check_invocation(check: &str, tip: &str, repo: &str, checkout: Option<(&str, &str)>) -> Option<(&'static str, Vec<String>)> {
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    if matches!(check, "spira-lint" | "workspace-build") {
        return Some(("testenv", s(&["lint", tip, repo])));
    }
    if let Some(fence) = check.strip_prefix("fence-") {
        let (root, base) = checkout?;
        return Some(("gate", s(&["fence-run", fence, "--repo", root, "--tip", tip, "--base", base])));
    }
    Some(("testenv", s(&["--suites", check, tip, repo])))
}

/// The repository's checkout and landing ref.
fn repo_checkout(repo: &str) -> Option<(String, String)> {
    let root = crate::repo_config::registered_root(repo).ok()?.display().to_string();
    let home = spira_config::resolve::locate_home_for_process().ok()?;
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let reg = spira_config::repos::Registry::from_env(env, std::path::Path::new(&home));
    let landref = spira_config::repos::landref(&reg, &root)?;
    Some((root, landref))
}

/// What `work submit` may spend running the checks an ejection named; a retry resumes, because
/// every pass is recorded as it happens.
const PROOF_BUDGET_SECS: u64 = 270;

/// A bead whose ejection named failing checks is submitted only when each has a passing run
/// recorded at `tip`. The broker runs the missing ones itself (the dwarf has no testenv), records
/// the exit code and the check's own summary line, and refuses naming whatever is still unproven.
fn proof_refusal(run: &std::path::Path, bead_id: &str, tip: &str, owed: &[String], budget: u64, runner: &dyn Fn(&str, u64) -> (i32, String)) -> Option<(i32, String)> {
    use spira_config::proof;
    let unproven = || {
        let mut all = proof::failing(run, bead_id);
        for o in owed {
            if !all.contains(o) {
                all.push(o.clone());
            }
        }
        all.retain(|c| !left_to_round(c) && !proof::proven(run, bead_id, tip, c));
        all
    };
    let missing = unproven();
    if missing.is_empty() {
        return None;
    }
    let started = std::time::Instant::now();
    let mut notes = Vec::new();
    for check in &missing {
        let left = budget.saturating_sub(started.elapsed().as_secs());
        if left == 0 {
            break;
        }
        let (rc, out) = runner(check, left);
        match rc {
            0 | 1 => {
                if let Err(e) = proof::record_run(run, bead_id, tip, check, rc, &out) {
                    return Some((CANNOT_TELL, format!("cannot tell: the run of {check} could not be recorded: {e}")));
                }
                if rc == 1 {
                    notes.push(format!("{check} FAILED at {tip}: {}", out.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no output")));
                }
            }
            _ => notes.push(format!("{check} could not run (rc={rc}): {}", out.trim_end())),
        }
    }
    let still = unproven();
    if still.is_empty() {
        return None;
    }
    let ejected = proof::failing(run, bead_id);
    let mut msg = format!("refused: no passing run is recorded at {tip} for: {}.", still.join(", "));
    if !ejected.is_empty() {
        msg.push_str(&format!(" {bead_id} was ejected for failing {}.", ejected.join(", ")));
    }
    if !owed.is_empty() {
        msg.push_str(&format!(" The diff obliges its repository's own CI tiers: {}.", owed.join(", ")));
    }
    for n in notes {
        msg.push_str(&format!("\n  {n}"));
    }
    msg.push_str(&format!("\nFix the failure, commit, and run `work submit` again — it runs each unproven check with the runner that owns it (a suite by testenv, a fence by gate fence-run, lint by testenv lint) at {tip}."));
    Some((REFUSED, msg))
}

/// The tiers its repository's own declaration says this tip's diff obliges it to have passed;
/// the repository decides, so a harness bead or a repository declaring none owes nothing.
fn owed_tiers(tip: &str, repo: &str, actor: &str) -> Result<Vec<String>, (i32, String)> {
    let args = ["owed".to_string(), tip.to_string(), repo.to_string()];
    // batch-job: one git diff and one git show
    match crate::bd::tool("testenv", &args, None, actor, 60) {
        (0, out) => Ok(out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()),
        (_, out) => Err((CANNOT_TELL, format!("cannot tell: the CI tiers this diff obliges could not be determined: {}", out.trim_end()))),
    }
}

fn submit_repo(bead_id: &str) -> Result<String, (i32, String)> {
    match crate::plane::repo(bead_id) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => match crate::bd::repo_label(bead_id) {
            Ok(Some(r)) => Ok(r),
            Ok(None) => Err((CANNOT_TELL, format!("cannot tell: {bead_id} names no repository (no bead.repo, no repo: label)"))),
            Err(e) => Err((CANNOT_TELL, format!("cannot tell: {bead_id}'s row names no repository and its label could not be read: {e}"))),
        },
        Err(e) => Err((CANNOT_TELL, format!("cannot tell: {e:?}"))),
    }
}

/// The repository checkout and landing ref the submit's branch rules read; `None` when the
/// registry names no checkout for the repository (a world or fixture with no content behind it).
fn branch_refusal(bead_id: &str, tip: &str, stack_tips: &[String]) -> Option<(i32, String)> {
    let repo = submit_repo(bead_id).ok()?;
    let home = spira_config::resolve::locate_home_for_process().ok()?;
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let reg = spira_config::repos::Registry::from_env(env, std::path::Path::new(&home));
    let root = reg.root(&repo)?;
    let landref = spira_config::repos::landref(&reg, &root)?;
    let in_flight = |id: &str| sibling_in_flight(id, &repo);
    let (fork, commits) = branch_evidence(std::path::Path::new(&root), &landref, tip, stack_tips);
    if let Some(m) = finishing_refusal(&landref, tip, bead_id, fork.as_deref(), commits.as_deref(), &in_flight) {
        return Some((REFUSED, m));
    }
    let prefix = match spira_config::process::cfg("SPIRA_ID_PREFIX") {
        Ok(p) => p,
        Err(e) => return Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    };
    let Some(commits) = commits.as_deref() else {
        return Some((CANNOT_TELL, format!("cannot tell: the commits of {tip} could not be read for the citation check")));
    };
    let pairs: Vec<(String, String)> = commits.iter().map(|(sha, _, msg)| (sha.clone(), msg.clone())).collect();
    if let Some(r) = phantom_refusal(&pairs, &prefix, &mut |ids| crate::callers::Bd::known(&mut crate::bd::LiveBd, ids)) {
        return Some(r);
    }
    if !stack_tips.is_empty() {
        return None;
    }
    let base_fq = Some(spira_config::repos::qualify_base_ref(&landref, &root)).filter(|b| !b.is_empty()).unwrap_or_else(|| landref.clone());
    merge_refusal(&|a| crate::git_evidence::git_code_output(std::path::Path::new(&root), a), &landref, &base_fq, tip)
}

/// A tip with a content conflict against the landing ref stays WORKING, so the session that wrote
/// it resolves the conflict; the exit check asks the same function.
fn merge_refusal(git: &dyn Fn(&[&str]) -> (i32, String), landref: &str, base_fq: &str, tip: &str) -> Option<(i32, String)> {
    match spira_config::rebase_check::merge_onto(git, base_fq, tip) {
        spira_config::rebase_check::Merge::Clean => None,
        spira_config::rebase_check::Merge::Conflict { paths, .. } => {
            let named = if paths.is_empty() { "unknown".to_string() } else { paths.join(", ") };
            Some((
                REFUSED,
                format!("refused: {tip} does not merge onto the landing ref {landref} — conflicts in: {named}. Rebase onto {landref}, resolve the conflict, commit, and run `work submit` again"),
            ))
        }
        spira_config::rebase_check::Merge::Unknown(out) => {
            let last = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
            Some((CANNOT_TELL, format!("cannot tell: git merge-tree of {tip} onto {base_fq} failed: {last}")))
        }
    }
}

/// A commit citing a bead id the store does not hold refuses; a store that cannot answer is
/// `CANNOT_TELL`, never a pass.
pub(crate) fn phantom_refusal(
    commits: &[(String, String)],
    prefix: &str,
    known: &mut spira_config::cite::Lookup<'_>,
) -> Option<(i32, String)> {
    match spira_config::cite::phantoms(commits, prefix, known) {
        Ok(p) if p.is_empty() => None,
        Ok(p) => Some((REFUSED, spira_config::cite::refusal(&p))),
        Err(e) => Some((CANNOT_TELL, format!("cannot tell: the bead store did not answer the citation check: {e}"))),
    }
}

/// A sibling's commits may ride under this bead's (law-a-rebase-loop-is-sequenced-not-split): the
/// sibling is in flight — submitted, certified, in a round, or landed — in the same repository.
fn sibling_in_flight(id: &str, repo: &str) -> bool {
    use bead::BeadState::{Certified, InDelivery, Landed, Submitted};
    crate::plane::repo(id).ok().flatten().as_deref() == Some(repo)
        && crate::plane::bead(id)
            .ok()
            .flatten()
            .is_some_and(|r| matches!(r.state, Submitted | Certified | InDelivery | Landed))
}

/// A commit's sha, subject and full message.
type Commit = (String, String, String);

/// The git reads behind `finishing_refusal`: the fork with the landing ref and the commits above it.
fn branch_evidence(root: &std::path::Path, landref: &str, tip: &str, stack_tips: &[String]) -> (Option<String>, Option<Vec<Commit>>) {
    let fork = crate::git_evidence::git_output(root, &["merge-base", landref, tip]).map(|f| f.trim().to_string()).filter(|f| !f.is_empty());
    let mut args = vec!["log".to_string(), "--no-merges".to_string(), "--format=%H%x1f%s%x1f%B%x1e".to_string(), tip.to_string(), format!("^{landref}")];
    args.extend(stack_tips.iter().map(|t| format!("^{t}")));
    let commits = state_store::phases::child("git", || crate::git_evidence::git_in(root)?.args(&args).output())
        .ok()
        .filter(|o| o.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .split('\x1e')
                .map(|rec| rec.trim_start_matches('\n'))
                .filter(|rec| !rec.is_empty())
                .map(|rec| {
                    // A record cut short keeps empty fields, so it names nothing and refuses.
                    let mut f = rec.splitn(3, '\x1f').map(str::to_string);
                    (f.next().unwrap_or_default(), f.next().unwrap_or_default(), f.next().unwrap_or_default())
                })
                .collect()
        });
    (fork, commits)
}

/// The tip shares history with the landing ref, and every commit above the fork names the bead or
/// an in-flight sibling. A tip behind the landing ref is fine: the round merges it.
fn finishing_refusal(
    landref: &str,
    tip: &str,
    bead_id: &str,
    fork: Option<&str>,
    commits: Option<&[(String, String, String)]>,
    in_flight: &dyn Fn(&str) -> bool,
) -> Option<String> {
    if fork.is_none() {
        return Some(format!("refused: {tip} shares no history with the landing ref {landref} — branch from {landref} and run `work submit` again"));
    }
    let commits = commits?;
    let unnamed = unnamed_commits(commits, bead_id, in_flight);
    if unnamed.is_empty() {
        return None;
    }
    Some(format!(
        "refused: these commits name neither {bead_id} nor a bead in flight in this repository:\n{}\nReword the tip with `git commit --amend -m \"{bead_id}: <subject>\"`, or earlier ones with `git rebase -i {landref}` (mark each `reword`), then run `work submit` again",
        unnamed.join("\n")
    ))
}

/// The commits (sha, subject, message) that name neither `bead_id` nor a bead `in_flight`, each
/// rendered as a refusal line.
fn unnamed_commits(commits: &[(String, String, String)], bead_id: &str, in_flight: &dyn Fn(&str) -> bool) -> Vec<String> {
    commits
        .iter()
        .filter(|(_, _, message)| {
            !crate::git_evidence::message_names_bead(message, bead_id) && !named_beads(message, bead_id).iter().any(|b| b != bead_id && in_flight(b))
        })
        .map(|(sha, subject, _)| format!("  {} {subject}", &sha[..sha.len().min(12)]))
        .collect()
}

/// The whole bead ids a commit message names.
fn named_beads(message: &str, bead_id: &str) -> Vec<String> {
    spira_config::cite::citations(message, bead_id.split('-').next().unwrap_or_default())
}

/// A tip is submitted only on a recorded pass of its repository's compile-and-lint check; the
/// broker runs it (a model session has no `testenv`) and records the pass for that exact tip.
/// The repository comes from the bead's lifecycle row: a world or a fixture holds lifecycle rows
/// with no content behind them. Only a row that names none (one created at the claim, before the repo was recorded
/// at filing) falls back to the content store's repo: label.
fn lint_refusal(bead_id: &str, tip: &str, actor: &str) -> Option<(i32, String)> {
    let repo = match submit_repo(bead_id) {
        Ok(r) => r,
        Err(refusal) => return Some(refusal),
    };
    let args = ["lint".to_string(), tip.to_string(), repo];
    // batch-job: runs the repository's declared lint tiers, bounded just under the client's wait
    match crate::bd::tool("testenv", &args, None, actor, 280) {
        (0, _) => None,
        (1, out) => Some((REFUSED, out.trim_end().to_string())),
        (_, out) => Some((CANNOT_TELL, format!("cannot tell: the compile-and-lint check could not run: {}", out.trim_end()))),
    }
}

#[derive(Debug, PartialEq)]
enum UnitRedError {
    Refused(String),
    CannotTell(String),
}

/// The note a submission's `--unit-red-on-base <id>` leaves on the submitting bead: the filed
/// bead that records the base red, and the failing tests the client's check row printed. `Err`
/// is a refusal or a lookup that could not tell; `Ok(None)` is a submission that names no base red.
fn unit_red_line(args: &[String], self_id: &str, exists: &dyn Fn(&str) -> Result<bool, String>) -> Result<Option<String>, UnitRedError> {
    use UnitRedError::{CannotTell, Refused};
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).filter(|v| !v.trim().is_empty());
    if !args.iter().any(|a| a == "--unit-red-on-base") {
        return Ok(None);
    }
    let Some(id) = value("--unit-red-on-base") else { return Err(Refused("refused: --unit-red-on-base needs the id of the bead recording the base red".to_string())) };
    if id == self_id {
        return Err(Refused(format!("refused: --unit-red-on-base {id} is this bead; file a bead recording the base red")));
    }
    if !exists(id).map_err(CannotTell)? {
        return Err(Refused(format!("refused: --unit-red-on-base {id} does not exist; file it first (work file-followup)")));
    }
    let tests = value("--unit-red-tests").map(String::as_str).unwrap_or("(none printed)");
    Ok(Some(format!("Unit red on base: {id} records it; failing tests the row printed: {tests}")))
}

fn record_unit_red_on_base(bead_id: &str, args: &[String]) -> Result<(), (i32, String)> {
    let exists = |id: &str| match crate::bd::show(id) {
        Ok(_) => Ok(true),
        Err(e) if e.contains("no issue found matching") => Ok(false),
        Err(e) => Err(e),
    };
    match unit_red_line(args, bead_id, &exists) {
        Err(UnitRedError::CannotTell(e)) => Err((CANNOT_TELL, format!("cannot tell: {e}"))),
        Err(UnitRedError::Refused(e)) => Err((REFUSED, format!("work submit: {e}"))),
        Ok(None) => Ok(()),
        Ok(Some(line)) => crate::bd::note(bead_id, &line).map(|_| ()).map_err(|e| (CANNOT_TELL, format!("cannot tell: {e}"))),
    }
}

/// A bead that declares a target closes only with a verdict (MET, CONTRADICTED, or SHORT with a
/// follow-on or escalation). The verdict is written to the bead as a note, where Sift reads it.
fn record_target_verdict(bead_id: &str, verb: &str, args: &[String]) -> Result<(), (i32, String)> {
    let text = match crate::bd::show(bead_id) {
        Ok(t) => t,
        Err(e) if e.contains("no issue found matching") => return Ok(()),
        Err(e) => return Err((CANNOT_TELL, format!("cannot tell: {e}"))),
    };
    let lookup = |id: &str| -> Result<Option<spira_config::target::Named>, String> {
        let Ok(shown) = crate::bd::show(id) else { return Ok(None) };
        Ok(Some(spira_config::target::Named { target: spira_config::target::parse_target(&shown), labels: crate::bd::labels(id)? }))
    };
    let ask_label = spira_config::process::cfg("SPIRA_ASK_LABEL").map_err(|e| (CANNOT_TELL, format!("cannot tell: {e}")))?;
    match spira_config::target::judge(spira_config::target::parse_target(&text).as_deref(), args, &lookup, bead_id, &ask_label) {
        Err(e) => Err((REFUSED, format!("work {verb}: {e}"))),
        Ok(None) => Ok(()),
        Ok(Some(line)) => crate::bd::note(bead_id, &line).map(|_| ()).map_err(|e| (CANNOT_TELL, format!("cannot tell: {e}"))),
    }
}

/// What the gate recorded when it turned a tip red: the step and that step's message.
struct GateRedAt {
    step: Option<String>,
    message: Option<String>,
}

/// The refusal for a tip already red: a round's eject names its reasons, anything else is the
/// gate's red and names the step and message it recorded.
fn red_tip_refusal(bead_id: &str, tip: &str, round: Option<&str>, gate: Option<&GateRedAt>) -> String {
    let next = "change the tree (fix the named reds, rebase) and commit again before resubmitting";
    if let (None, Some(g)) = (round, gate) {
        let step = g.step.as_deref().map(|s| format!(" at step {s}")).unwrap_or_default();
        let message = g.message.as_deref().map(|m| format!(": {m}")).unwrap_or_default();
        return format!("refused: {tip} is the tip the gate turned red for {bead_id}{step}{message} — {next}");
    }
    let why = round.map(|r| format!(": {r}")).unwrap_or_default();
    format!("refused: {tip} is the tip a round ejected {bead_id} for as red{why} — {next}")
}

fn gate_red_at(bead_id: &str, tip: &str) -> Option<GateRedAt> {
    let (0, out) = crate::plane::call(&[s("plane-gate-red"), bead_id.to_string(), tip.to_string()]) else { return None };
    let answer: serde_json::Value = serde_json::from_str(&out).ok()?;
    answer["red"].is_object().then(|| gate_red_from(&answer["red"])).flatten()
}

/// A step or message is present only when non-empty.
fn gate_red_from(red: &serde_json::Value) -> Option<GateRedAt> {
    let text = |k: &str| red.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_string);
    Some(GateRedAt { step: text("step"), message: text("message") })
}

/// The reason the newest round that ejected `bead_id` at `tip` gave.
fn ejected_reasons(bead_id: &str, tip: &str) -> Option<String> {
    let (0, out) = crate::plane::call(&[s("plane-ejected"), bead_id.to_string(), tip.to_string()]) else { return None };
    serde_json::from_str::<serde_json::Value>(&out).ok()?["reason"].as_str().map(str::to_string)
}

/// A mender's calls the queue already dropped ends without an event the machine would refuse.
fn mender_calls_closed(bead_id: &str, actor: &str, verb: &str) -> Option<(i32, String)> {
    if actor != "mender" {
        return None;
    }
    match crate::plane::bead(bead_id) {
        Ok(Some(row)) if row.state.is_terminal() => Some((0, format!("{bead_id} is already {}; work {verb} has nothing to do", row.state.as_str()))),
        _ => None,
    }
}

/// The failure a mender calls names in its body (`Batch:`, `Pass:`, `Suite:` lines).
fn calls_failure(text: &str) -> Option<(String, u32, String)> {
    let field = |k: &str| text.lines().find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim().to_string()));
    Some((field("Batch: ")?, field("Pass: ")?.parse().ok()?, field("Suite: ")?))
}

/// A mender's calls does not close DONE while the failure it was filed for is still unsettled:
/// the round would sit in attributing with nothing to move it.
fn unrecorded_outcome_refusal(bead_id: &str, actor: &str, verb: &str) -> Option<(i32, String)> {
    if actor != "mender" {
        return None;
    }
    let (batch, pass, suite) = calls_failure(&crate::bd::show(bead_id).ok()?)?;
    let row = crate::mending::fetch(&batch, pass, &suite).ok()??;
    if row.state.is_terminal() {
        return None;
    }
    Some((
        REFUSED,
        format!(
            "refused: work {verb} would close {bead_id} while {batch} pass {pass} {suite} is still {} — record it with `work mender outcome <spec> <mended|environment|flaky|return|unsure>`; if the calls names no spec, say so with `work blocked`",
            row.state.as_str()
        ),
    ))
}

fn already_finished_noop(bead_id: &str, state: bead::BeadState) -> Option<(i32, String)> {
    match state {
        bead::BeadState::Done | bead::BeadState::Dropped => Some((0, format!("{bead_id} is already {}; nothing to do", state.as_str()))),
        _ => None,
    }
}

fn cmd_done(bead_id: &str, args: &[String]) -> (i32, String) {
    if let Some(refusal) = unanswered_amendment_refusal(bead_id) {
        return refusal;
    }
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    if let Some(quiet) = mender_calls_closed(bead_id, &actor, "done") {
        return quiet;
    }
    if let Some(refusal) = unrecorded_outcome_refusal(bead_id, &actor, "done") {
        return refusal;
    }
    let Some(delivers) = flag(args, "--delivers") else {
        return (CANNOT_TELL, "work done: --delivers <path> is required".into());
    };
    match crate::plane::bead(bead_id) {
        Ok(Some(row)) => {
            if let Some(noop) = already_finished_noop(bead_id, row.state) {
                return noop;
            }
        }
        Ok(None) => {}
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
    if actor == "groomer" {
        match crate::bd::labels(bead_id) {
            Ok(labels) if labels.iter().any(|l| l == GROOM_TRIGGER_MARKER) => {}
            Ok(_) if crate::plan::serving() => {
                let call = split_reserved(args);
                let mut argv = vec!["work".to_string(), bead_id.to_string(), "done".to_string()];
                argv.extend(call.args);
                return record_argv(bead_id, &actor, &argv, call.stdin.as_deref(), exec_bound_ms("done"));
            }
            Ok(_) => return release_untriggered_groom(bead_id, &actor),
            Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
        }
    }
    match crate::plane::bead(bead_id) {
        Ok(Some(row)) => {
            if let Some(refusal) = working_exit_refusal("done", "DONE", &row, &actor) {
                return refusal;
            }
        }
        Ok(None) => {}
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
    if actor != "groomer" {
        if let Err(refusal) = record_target_verdict(bead_id, "done", args) {
            return refusal;
        }
        if let Some(refusal) = red_tracker_done_refusal(bead_id) {
            return refusal;
        }
        if let Some(refusal) = skuld_draft_refusal(bead_id) {
            return refusal;
        }
        if let Some(refusal) = skuld_review_apply(bead_id) {
            return refusal;
        }
        if flag(args, "--after-ask").is_none() && released_on_ask_history(bead_id) {
            return (
                REFUSED,
                format!(
                    "refused: work done: {bead_id} was released on an unanswered-or-answered ask and has not landed anything; DONE would read as success. \
                     Submit the work, ask again with `work blocked`, or state what the answer settled with --after-ask \"<what the reply decided>\""
                ),
            );
        }
    }
    let delivers = match flag(args, "--after-ask") {
        Some(settled) => format!("{delivers} (after ask: {settled})"),
        None => delivers,
    };
    crate::plane::apply(bead_id, &actor, BeadEventKind::Done { delivers })
}

/// A bead leaves WORKING only by an exit its claiming rune declares (`RUNE_WORKING_EXITS`);
/// the refusal names the rune, the attempted state and the exits it may take.
/// Asks the bead machine itself whether this row's state takes a submit, before any check
/// spends time on a tip that could never be recorded.
fn submit_state_refusal(row: &bead::LifecycleRow, tip: &str) -> Option<(i32, String)> {
    let ev = bead::BeadEvent { expect: row.state, version: row.version, kind: BeadEventKind::Submit { tip: tip.to_string() }, actor: String::new(), at: None };
    let Some(lifecycle::Refusal::IllegalTransition { .. } | lifecycle::Refusal::Terminal { .. }) = bead::apply(row, &ev).refusal else {
        return None;
    };
    let (id, state) = (&row.bead_id, row.state.as_str());
    let way_out = match row.state {
        bead::BeadState::Ready | bead::BeadState::Rework => "claim it first".to_string(),
        s if s.is_terminal() => format!("it is already {state}; there is nothing left to submit"),
        _ => format!("{state} is not the holder's to move"),
    };
    Some((REFUSED, format!("refused: {id} is {state}, not WORKING — {way_out}")))
}

fn working_exit_refusal(verb: &str, exit: &str, row: &bead::LifecycleRow, actor: &str) -> Option<(i32, String)> {
    if row.state != bead::BeadState::Working {
        return None;
    }
    match spira_config::resolve::locate_home_for_process() {
        Ok(home) => working_exit_refusal_in(&spira_config::runes::runes_dir(std::path::Path::new(&home)), verb, exit, row, actor),
        Err(e) => Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    }
}

fn working_exit_refusal_in(runes: &std::path::Path, verb: &str, exit: &str, row: &bead::LifecycleRow, actor: &str) -> Option<(i32, String)> {
    if row.state != bead::BeadState::Working {
        return None;
    }
    let rune = row.persona.clone().unwrap_or_else(|| actor.to_string());
    let exits = match spira_config::runes::working_exits_in(runes, &rune) {
        Ok(e) => e,
        Err(e) => return Some((REFUSED, format!("refused: work {verb}: {e}"))),
    };
    working_exit_check(verb, exit, &rune, &exits)
}

fn working_exit_check(verb: &str, exit: &str, rune: &str, exits: &[String]) -> Option<(i32, String)> {
    if exits.iter().any(|e| e == exit) {
        return None;
    }
    Some((
        REFUSED,
        format!("refused: work {verb}: rune {rune} may not leave WORKING for {exit}; its exits are {}", exits.join(" ")),
    ))
}

fn red_tracker_done_refusal(bead_id: &str) -> Option<(i32, String)> {
    let label = match spira_config::process::cfg("SPIRA_RED_TRACKER_LABEL") {
        Ok(l) => l,
        Err(e) => return Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    };
    match crate::bd::labels(bead_id) {
        Ok(labels) if !label.is_empty() && labels.contains(&label) => Some((
            REFUSED,
            format!(
                "refused: work done: {bead_id} is a {label} bead and DONE would read as a handled red with no fix landed. \
                 Submit a fix that lands, or park it with `work blocked` and a reason"
            ),
        )),
        Ok(_) => None,
        Err(e) => Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    }
}

fn skuld_draft_refusal(bead_id: &str) -> Option<(i32, String)> {
    let label = match spira_config::process::cfg("SPIRA_SKULD_LABEL") {
        Ok(l) => l,
        Err(e) => return Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    };
    let labels = match crate::bd::labels(bead_id) {
        Ok(l) => l,
        Err(e) => return Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    };
    if label.is_empty() || !labels.contains(&label) {
        return None;
    }
    match crate::bd::notes(bead_id) {
        Ok(notes) => skuld_draft_notes_refusal(bead_id, &notes),
        Err(e) => Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    }
}

/// A Skuld request applies its notes before it is done: each follow-up it reviews takes its draft
/// as a body, or is folded into the waiting item its amendment names. A request that reviews
/// nothing adds its amendments' evidence to their items. `None` lets `Done` proceed.
///
/// Only the request's notes are read: its description shows the headings as a template.
fn skuld_review_apply(bead_id: &str) -> Option<(i32, String)> {
    let label = match spira_config::process::cfg("SPIRA_SKULD_LABEL") {
        Ok(l) => l,
        Err(e) => return Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    };
    let labels = match crate::bd::labels(bead_id) {
        Ok(l) => l,
        Err(e) => return Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    };
    if label.is_empty() || !labels.contains(&label) {
        return None;
    }
    let read = crate::bd::blocked_dependents(bead_id).and_then(|d| Ok((d, crate::bd::description(bead_id)?, crate::bd::notes(bead_id)?)));
    let (dependents, description, notes) = match read {
        Ok(r) => r,
        Err(e) => return Some((CANNOT_TELL, format!("cannot tell: {e}"))),
    };
    let filed = ::bead::review_filed_from(&description);
    // A closed follow-up still owes an amendment its evidence and the filer's note.
    let amends = ::bead::amend_notes(&notes);
    let dependents: Vec<(String, String)> =
        dependents.into_iter().filter(|d| d.open || amends.iter().any(|a| a.title == d.title.trim())).map(|d| (d.id, d.title)).collect();
    if dependents.is_empty() {
        apply_amends(bead_id, &notes, filed_item, crate::bd::amend_note)
    } else {
        apply_reviews(bead_id, filed.as_ref(), &dependents, &notes, filed_item, crate::bd::amend_replace, crate::bd::amend_note, supersede_into)
    }
}

/// What an amendment is judged against: an item's lifecycle state and repo, and its title.
struct Item {
    state: Option<bead::BeadState>,
    superseded_by: Option<String>,
    repo: Option<String>,
    title: String,
    /// The items it has a `blocks` edge onto that have not ended.
    waits_on: Vec<String>,
}

fn filed_item(id: &str) -> Result<Item, String> {
    let row = crate::plane::bead(id).map_err(|e| format!("{e:?}"))?;
    let state = row.as_ref().map(|r| r.state);
    let superseded_by = row.filter(|r| r.state == bead::BeadState::Superseded).and_then(|r| r.reason);
    let repo = crate::plane::repo(id).map_err(|e| format!("{e:?}"))?;
    Ok(Item { state, superseded_by, repo, title: crate::bd::title(id)?.unwrap_or_default(), waits_on: unended_blockers(id)? })
}

fn unended_blockers(id: &str) -> Result<Vec<String>, String> {
    let mut unended = Vec::new();
    for (on, _) in crate::bd::edges(id)?.into_iter().filter(|(_, kind)| kind == "blocks") {
        if !crate::plane::bead(&on).map_err(|e| format!("{e:?}"))?.is_some_and(|r| r.state.is_terminal()) {
            unended.push(on);
        }
    }
    Ok(unended)
}

/// Retires `dep` as SUPERSEDED by `survivor`, on its row and then in the content store. A row
/// already retired that way is left alone, so a repeat finishes the content half.
fn supersede_into(dep: &str, survivor: &str) -> Result<(), String> {
    // First, so that nothing waiting on `dep` is released by its closing.
    for waiter in crate::bd::blocked_dependents(dep)?.into_iter().filter(|w| w.open) {
        if waiter.id != survivor {
            crate::bd::write(&["dep", "add", &waiter.id, survivor, "--type", "blocks"])?;
        }
        crate::bd::write(&["dep", "remove", &waiter.id, dep])?;
    }
    // A pair holds one edge, so any other would stop the `supersedes` edge below.
    if crate::bd::edges(dep)?.iter().any(|(on, kind)| on == survivor && kind != "supersedes") {
        crate::bd::write(&["dep", "remove", dep, survivor])?;
    }
    let row = crate::plane::bead(dep).map_err(|e| format!("{e:?}"))?;
    if !row.is_some_and(|r| r.state == bead::BeadState::Superseded && r.reason.as_deref() == Some(survivor)) {
        let (code, out) = crate::plane::apply(dep, SKULD_PERSONA, BeadEventKind::Supersede { by: survivor.to_string() });
        if code != 0 {
            return Err(out);
        }
    }
    crate::bd::write(&["supersede", dep, "--with", survivor]).map(|_| ())
}

/// The survivor, when it is a waiting item of `repo`; else the refusal. `instead` is what to do
/// other than name another one.
fn waiting_survivor(item: &impl Fn(&str) -> Result<Item, String>, survivor: &str, repo: Option<&str>, instead: &str) -> Result<Item, (i32, String)> {
    let found = item(survivor).map_err(|e| (CANNOT_TELL, format!("cannot tell: reading {survivor}: {e}")))?;
    let waiting = matches!(found.state, Some(bead::BeadState::Ready | bead::BeadState::Rework));
    if waiting && repo.is_some() && found.repo.as_deref() == repo {
        return Ok(found);
    }
    let state = found.state.map_or("unknown", |s| s.as_str());
    let repo = repo.unwrap_or("an unnamed repo");
    Err((REFUSED, format!("refused: work done: {survivor} is {state}, not a waiting item of {repo}; {instead}name a waiting item")))
}

enum Verdict {
    Draft(::bead::Draft),
    Amend { evidence: String, survivor: String, survivor_title: String, retire: bool },
}

/// Every dependent's note is judged before any of them is applied, so a refusal changes nothing.
#[allow(clippy::too_many_arguments)]
fn apply_reviews(
    request: &str,
    filed: Option<&(String, String)>,
    dependents: &[(String, String)],
    notes: &str,
    item: impl Fn(&str) -> Result<Item, String>,
    mut replace: impl FnMut(&str, &str) -> Result<(), String>,
    mut note: impl FnMut(&str, &str) -> Result<(), String>,
    mut supersede: impl FnMut(&str, &str) -> Result<(), String>,
) -> Option<(i32, String)> {
    let amends = ::bead::amend_notes(notes);
    let amend_of = |title: &str| amends.iter().find(|a| a.title == title.trim());
    let mut verdicts = Vec::new();
    for (dep, title) in dependents {
        let verdict = if let Some(a) = amend_of(title) {
            if a.survivor == *dep || dependents.iter().any(|(d, t)| *d == a.survivor && amend_of(t).is_some()) {
                return Some((REFUSED, format!("refused: work done: {} is itself being folded by {request}; draft {dep} instead or name a waiting item", a.survivor)));
            }
            let followup = match item(dep) {
                Ok(i) => i,
                Err(e) => return Some((CANNOT_TELL, format!("cannot tell: reading {dep}: {e}"))),
            };
            // A repeat of a fold the row already records: the survivor may have moved on since.
            let folded = followup.superseded_by.as_deref() == Some(a.survivor.as_str());
            let survivor = if folded {
                item(&a.survivor).map_err(|e| (CANNOT_TELL, format!("cannot tell: reading {}: {e}", a.survivor)))
            } else {
                waiting_survivor(&item, &a.survivor, followup.repo.as_deref(), &format!("draft {dep} instead or "))
            };
            let retire = folded || !followup.state.is_some_and(|s| s.is_terminal());
            let ordered: Vec<&str> = followup.waits_on.iter().map(String::as_str).filter(|w| *w != request && *w != a.survivor).collect();
            if retire && !folded && !ordered.is_empty() {
                return Some((REFUSED, format!("refused: work done: {dep} waits on {}, an order {} would not keep; draft {dep} instead", ordered.join(" "), a.survivor)));
            }
            match survivor {
                Ok(s) => Verdict::Amend { evidence: a.evidence.clone(), survivor: a.survivor.clone(), survivor_title: s.title, retire },
                Err(refusal) => return Some(refusal),
            }
        } else if let Some(d) = ::bead::draft_body(notes, title) {
            Verdict::Draft(d)
        } else {
            return Some((
                REFUSED,
                format!(
                    "refused: work done: {request} reviews {dep} (\"{title}\"); no note holds a draft headed \"### Draft: {title}\" or an amendment headed \"### Amend <survivor-id>: {title}\". \
                     Write one with work note - (the heading verbatim, a blank line, the text), then run work done again"
                ),
            ));
        };
        verdicts.push((dep, verdict));
    }
    for (dep, verdict) in verdicts {
        match verdict {
            Verdict::Draft(d) => {
                if let Err(e) = replace(dep, &format!("Drafted by Skuld: {request}\n\n{}\n", d.body)) {
                    return Some((CANNOT_TELL, format!("cannot tell: {request} was left WORKING; the draft could not replace the body of {dep}, which keeps waiting (run work done again): {e}")));
                }
                if let Some(open) = &d.open {
                    if let Err(e) = note(dep, open) {
                        return Some((CANNOT_TELL, format!("cannot tell: {request} was left WORKING; {dep}'s body was replaced but the question could not be noted on it (run work done again): {e}")));
                    }
                }
                crate::bd::record_event("spira_skuld_drafts_applied", "done");
            }
            Verdict::Amend { evidence, survivor, survivor_title, retire } => {
                let from = filed.map_or(String::new(), |(filer, bound)| format!(" filed by {filer} from {bound}"));
                if let Err(e) = note(&survivor, &format!("Amended by Skuld: {request}, from {dep}{from}\n\n{evidence}")) {
                    return Some((CANNOT_TELL, format!("cannot tell: {request} was left WORKING; the evidence could not be noted on {survivor}, and {dep} was not retired (run work done again): {e}")));
                }
                // A follow-up that ended some other way has nothing to retire, and was not folded.
                if !retire {
                    if let Some((_, bound)) = filed {
                        let moved = format!("{survivor} (\"{survivor_title}\") took the evidence of {dep}");
                        if let Err(e) = note(bound, &moved) {
                            return Some((CANNOT_TELL, format!("cannot tell: {request} was left WORKING; the evidence was noted on {survivor}, but {bound} was not told `{moved}` (run work done again): {e}")));
                        }
                    }
                    crate::bd::record_event("spira_skuld_amendments_applied", "done");
                    continue;
                }
                if let Err(e) = supersede(dep, &survivor) {
                    return Some((CANNOT_TELL, format!("cannot tell: {request} was left WORKING; the evidence note was written on {survivor} but {dep} could not be superseded by it (run work done again): {e}")));
                }
                if let Some((_, bound)) = filed {
                    let folded = format!("{dep} was folded into {survivor} (\"{survivor_title}\")");
                    if let Err(e) = note(bound, &folded) {
                        return Some((CANNOT_TELL, format!("cannot tell: {request} was left WORKING; the evidence was noted on {survivor} and {dep} superseded by it, but {bound} was not told `{folded}` (run work done again): {e}")));
                    }
                }
                crate::bd::record_event("spira_skuld_amendments_applied", "done");
            }
        }
    }
    None
}

/// A request with no follow-up to retire: each amendment's evidence goes onto its waiting item.
fn apply_amends(
    request: &str,
    notes: &str,
    item: impl Fn(&str) -> Result<Item, String>,
    mut note: impl FnMut(&str, &str) -> Result<(), String>,
) -> Option<(i32, String)> {
    let amends = ::bead::amend_notes(notes);
    if amends.is_empty() {
        return None;
    }
    let repo = match item(request) {
        Ok(i) => i.repo,
        Err(e) => return Some((CANNOT_TELL, format!("cannot tell: reading {request}: {e}"))),
    };
    if let Some(refusal) = amends.iter().find_map(|a| waiting_survivor(&item, &a.survivor, repo.as_deref(), "draft instead or ").err()) {
        return Some(refusal);
    }
    let mut noted = Vec::new();
    for a in amends {
        if let Err(e) = note(&a.survivor, &format!("Amended by Skuld: {request}\n\n{}", a.evidence)) {
            let done = if noted.is_empty() { "nothing".to_string() } else { noted.join(" ") };
            return Some((CANNOT_TELL, format!("cannot tell: {request} was left WORKING; the evidence could not be noted on {} (noted so far: {done}; run work done again): {e}", a.survivor)));
        }
        noted.push(a.survivor);
        crate::bd::record_event("spira_skuld_amendments_applied", "done");
    }
    None
}

fn skuld_draft_notes_refusal(bead_id: &str, notes: &str) -> Option<(i32, String)> {
    let amends = ::bead::amend_refusals(notes);
    if !amends.is_empty() {
        return Some((REFUSED, format!("refused: work done: {bead_id} holds an amendment that would move no evidence.\n{}\nResend it with `work note` and run `work done` again", amends.join("\n"))));
    }
    let mut found = ::bead::draft_depends_refusals(notes);
    found.extend(::bead::draft_trailer_refusals(notes));
    if found.is_empty() {
        return None;
    }
    Some((
        REFUSED,
        format!(
            "refused: work done: {bead_id} holds a draft the filer would refuse: a Depends line carries only bead ids, `draft N` or `none` (move prose into Context), and nothing follows a draft's `Open:` line.\n{}\nFix the note with `work note` and run `work done` again",
            found.join("\n")
        ),
    ))
}

fn released_on_ask_history(bead_id: &str) -> bool {
    let argv = ["history", bead_id, "--machine", "bead"].map(String::from);
    let (0, out) = crate::plane::call(&argv) else { return true };
    let Ok(serde_json::Value::Array(found)) = serde_json::from_str::<serde_json::Value>(&out) else { return true };
    let history: Vec<(String, String)> = found
        .iter()
        .filter(|r| r.get("applied").and_then(serde_json::Value::as_bool) == Some(true))
        .map(|r| (r.get("event").and_then(serde_json::Value::as_str).unwrap_or("").to_string(), r.get("evidence").map(serde_json::Value::to_string).unwrap_or_default()))
        .collect();
    released_on_ask(&history)
}

/// True when an applied operator-question Hold is followed by a Release: the holder gave the
/// bead back on a question rather than finishing it.
fn released_on_ask(history: &[(String, String)]) -> bool {
    let mut asked = false;
    for (event, evidence) in history {
        match event.as_str() {
            "Hold" if evidence.contains("\"operator-question\"") => asked = true,
            "Release" if asked => return true,
            _ => {}
        }
    }
    false
}

/// The label groom-trigger.sh stamps on the beads it files; the groom label alone also
/// selects work beads, which a groom pass must never close.
const GROOM_TRIGGER_MARKER: &str = "groom-trigger";

/// Subject, default and body of the ask that gets an untriggered bead relabelled.
fn untriggered_groom_ask(bead_id: &str) -> (String, String, String) {
    let body = format!("## Question\n{bead_id} carries the groom label without the {GROOM_TRIGGER_MARKER} marker, so the groomer released it instead of closing it. Which builder persona should it be relabelled for?\n\n## Default\nrelabel {bead_id} for its builder\n");
    ("A groom-labelled bead is not a groom trigger".to_string(), "relabel for its builder".to_string(), body)
}

fn release_untriggered_groom(bead_id: &str, actor: &str) -> (i32, String) {
    let (code, out) = crate::plane::apply(bead_id, actor, BeadEventKind::Release);
    if code != 0 {
        return (code, out);
    }
    let why = "claimed by the groom predicate but is not a groom trigger; relabel for its builder";
    let _ = crate::bd::note(bead_id, why);
    let (subject, default, body) = untriggered_groom_ask(bead_id);
    match crate::bd::ask_concierge(&format!("{actor} <{actor}@spira>"), &subject, &default, bead_id, &body) {
        Ok(_) => (REFUSED, format!("refused: work done: {why}; released, ask filed")),
        Err(e) => (REFUSED, format!("refused: work done: {why}; released, but filing the ask failed: {e}")),
    }
}

fn not_a_question(text: &str) -> Option<String> {
    (text.trim().is_empty() || text.starts_with("--")).then(|| format!("{text:?} is not a question"))
}

const BLOCKED_USAGE: &str = "usage: work blocked \"<the question>\" --default \"<what you would do>\" --class <permissions|policy|destructive|design> --class-basis \"<why it is that class>\"";

fn cmd_blocked(bead_id: &str, args: &[String]) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    if let Some(quiet) = mender_calls_closed(bead_id, &actor, "blocked") {
        return quiet;
    }
    let Some(question) = args.first() else {
        return (CANNOT_TELL, "work blocked: missing \"<question>\"".into());
    };
    if let Some(e) = not_a_question(question) {
        return (CANNOT_TELL, format!("work blocked: {e}; {BLOCKED_USAGE}"));
    }
    let Some(default) = flag(args, "--default") else {
        return (CANNOT_TELL, format!("work blocked: --default <default> is required; {BLOCKED_USAGE}"));
    };
    let required = |name: &str| {
        flag(args, name).filter(|v| !v.trim().is_empty() && !v.starts_with("--")).ok_or_else(|| (CANNOT_TELL, format!("work blocked: {name} is required; {BLOCKED_USAGE}")))
    };
    let (class, class_basis) = match (required("--class"), required("--class-basis")) {
        (Ok(c), Ok(b)) => (c, b),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let body = blocked_ask_body(question, &default, &class_basis);
    if let Err(e) = crate::bd::ask_operator_classed(&format!("{actor} <{actor}@spira>"), question, &default, Some(&class), bead_id, &body) {
        return (REFUSED, format!("refused: work blocked: the question could not be delivered, so no hold was applied: {e}"));
    }
    let hold = BeadEventKind::Hold { kind: HoldKind::Ask, cause: HoldCause::OperatorQuestion, detail: Some(question.clone()) };
    match apply_ask_hold(bead_id, &actor, hold) {
        (0, _) => (0, format!("ask filed; hold applied for {bead_id}")),
        (code, e) => (code, format!("the question WAS delivered (do not ask again), but the ask hold on {bead_id} was not applied: {e}")),
    }
}

fn blocked_ask_body(question: &str, default: &str, class_basis: &str) -> String {
    format!("## Question\n{question}\n\n## Default\n{default}\n\n## Class basis\n{class_basis}\n")
}

/// A hold write whose reply was lost may still have applied: read the row back before
/// reporting it failed, so a timed-out write never reads as an absent hold.
fn apply_ask_hold(bead_id: &str, actor: &str, hold: BeadEventKind) -> (i32, String) {
    let out = crate::plane::apply(bead_id, actor, hold);
    if out.0 != 0 && matches!(crate::plane::bead(bead_id), Ok(Some(r)) if r.holds.contains(&HoldKind::Ask)) {
        return (0, String::new());
    }
    out
}

/// A child's repo is always named by the caller, never inherited from the parent: a child's
/// work often lives in a different repository than the bead it was split from.
fn child_repo(args: &[String]) -> Result<String, String> {
    match flag(args, "--repo") {
        Some(r) if !r.trim().is_empty() && !r.starts_with('-') => Ok(r),
        _ => Err("--repo <name> is required: the child's repository is not inherited from the parent; name the repo its work lives in".into()),
    }
}

fn cmd_file(bead_id: &str, args: &[String], verb: &str) -> (i32, String) {
    let Some(title) = args.first() else {
        return (CANNOT_TELL, format!("work {verb}: missing <title>"));
    };
    let after_landing = args.iter().any(|a| a == "--after-landing");
    let prerequisite = args.iter().any(|a| a == "--prerequisite");
    if prerequisite && verb != "split" {
        return (REFUSED, format!("refused: work {verb}: --prerequisite is a split's flag; use `work split --prerequisite` for a piece your bead must wait on"));
    }
    if prerequisite && after_landing {
        return (REFUSED, format!("refused: work {verb}: --prerequisite and --after-landing contradict each other: --after-landing makes the new bead wait, --prerequisite makes your bead wait; pass one"));
    }
    let rank = match followup_rank(verb, &split_reserved(args).args) {
        Ok(r) => r,
        Err(why) => return (REFUSED, why),
    };
    let Some(filer) = flag(args, "--for") else {
        return (CANNOT_TELL, format!("work {verb}: --for <persona> is required"));
    };
    let persona = match filed_as(verb, &filer) {
        Ok(p) => p,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let repo = match child_repo(args) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("refused: work {verb}: {e}")),
    };
    if prerequisite {
        let read = spira_config::resolve::locate_home_for_process().and_then(|home| Ok((home, spira_config::process::cfg("SPIRA_INCIDENT_LABEL")?)));
        let (home, incident) = match read {
            Ok(r) => r,
            Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
        };
        if let Some(why) = incident_lane_refusal(&persona, &spira_config::runes::rune_get(&home, &persona, "RUNE_LABELS", ""), &incident) {
            return (REFUSED, why);
        }
    }
    let actor = flag(args, "--actor").unwrap_or_default();
    let body = split_reserved(args).stdin;
    let register = |new_id: &str| crate::plane::call(&[s("create-bead"), new_id.to_string(), s("--ready"), s("--repo"), repo.clone()]);
    let filing = Filing { verb, actor: &actor, title, persona: &persona, repo: &repo, bound: bead_id, after_landing, prerequisite, body: body.as_deref(), priority: rank.as_ref().map(|r| r.priority), statement: rank.as_ref().and_then(|r| r.statement.as_deref()) };
    let file = |f: &Filing, parent: Option<&str>, considered: Option<&str>| crate::bd::file_child(f.title, f.persona, f.repo, parent, f.body, considered, f.priority.map(|p| p.to_string()).as_deref());
    let note = |id: &str, text: &str| crate::bd::note(id, text).map(|_| ());
    let filed = file_and_bind(&filing, file, note, register, crate::bd::label_add, crate::bd::add_blocker);
    if verb != "file-followup" || filed.0 != 0 || !lane_requires_skuld(&persona) {
        return filed;
    }
    let priority = filing.priority.map(|p| p.to_string());
    review_followup(&filing, &filed.1, |t, b| crate::bd::file_child(t, SKULD_PERSONA, &repo, None, Some(b), None, priority.as_deref()), crate::bd::add_blocker)
}

/// What a `file-followup` files at, and the filer's reason when it names one.
#[derive(Debug, PartialEq, Eq)]
struct Rank {
    priority: i64,
    statement: Option<String>,
}

const FOLLOWUP_PRIORITY: i64 = 3;
const STATED_PRIORITY: i64 = 2;

/// `None` for any other verb: a `split` files at the store's default.
fn followup_rank(verb: &str, args: &[String]) -> Result<Option<Rank>, String> {
    if verb != "file-followup" {
        return Ok(None);
    }
    let refuse = |why: String| Err(format!("refused: work file-followup: {why}; nothing was filed"));
    let value = |name: &str| match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => match args.get(i + 1).map(|v| v.trim()) {
            Some(v) if !v.is_empty() && !v.starts_with("--") => Ok(Some(v.to_string())),
            _ => Err(format!("refused: work file-followup: {name} needs a value; nothing was filed")),
        },
    };
    let (blocks, defect) = (value("--blocks")?, value("--defect")?);
    if let Some(b) = &blocks {
        let names_a_bead = b.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.')).any(|w| ::bead::looks_like_bead_id(w.trim_end_matches('.')));
        if !names_a_bead {
            return refuse(format!("--blocks {b:?} names no bead id; name the work it blocks"));
        }
    }
    let said: Vec<String> = [("blocks", blocks), ("defect", defect)].into_iter().filter_map(|(k, v)| v.map(|v| format!("{k}: {v}"))).collect();
    let statement = (!said.is_empty()).then(|| said.join("; "));
    let priority = match value("--priority")? {
        None if statement.is_some() => STATED_PRIORITY,
        None => FOLLOWUP_PRIORITY,
        Some(arg) => match ::bead::parse_priority_arg(&arg) {
            Some(p) => p,
            None => return refuse(format!("--priority takes 0..4 (or P0..P4), not {arg:?}")),
        },
    };
    if priority < FOLLOWUP_PRIORITY && statement.is_none() {
        return refuse(format!("P{priority} needs --blocks \"<the work it blocks>\" or --defect \"<what is broken for a user now>\"; without one the item files at P{FOLLOWUP_PRIORITY}"));
    }
    Ok(Some(Rank { priority, statement }))
}

/// Notes the filer's statement on the new item: a Skuld draft replaces the body and keeps the notes.
fn record_statement(f: &Filing, new_id: &str, note: impl FnOnce(&str, &str) -> Result<(), String>) -> Result<(), String> {
    let (Some(priority), Some(statement)) = (f.priority, f.statement) else { return Ok(()) };
    let text = format!("Filed at P{priority} by {} from {}: {statement}", f.actor, f.bound);
    note(new_id, &text).map_err(|e| format!("{new_id} was filed but its statement was not recorded (do not file it again; note it with: {text}): {e}"))
}

/// The store refuses a blocks edge onto an incident, and would do so only after the piece is filed.
fn incident_lane_refusal(persona: &str, lane: &str, incident: &str) -> Option<String> {
    let in_lane = !incident.is_empty() && lane.split(',').any(|l| l.trim() == incident);
    in_lane.then(|| format!("refused: work split: --prerequisite files into the {persona} lane, which carries the {incident} label, and nothing may wait on an incident; nothing was filed. Record the piece with `work note` and hold your bead for it with `work blocked`"))
}

const SKULD_PERSONA: &str = "skuld";

fn lane_requires_skuld(persona: &str) -> bool {
    match spira_config::resolve::locate_home_for_process() {
        Ok(home) => lane_requires_skuld_in(std::path::Path::new(&home), persona),
        Err(_) => false,
    }
}

fn lane_requires_skuld_in(home: &std::path::Path, persona: &str) -> bool {
    spira_config::runes::rune_get(home, persona, "RUNE_REQUIRES_SKULD", "") == "1"
}

/// Files the Skuld request that reviews `followup` and makes `followup` wait on it. The first
/// line of the answer stays the follow-up's id.
fn review_followup(
    f: &Filing,
    followup: &str,
    file: impl FnOnce(&str, &str) -> Result<String, String>,
    wait: impl FnOnce(&str, &str) -> Result<(), String>,
) -> (i32, String) {
    let request_title = format!("Draft {followup}: {}", f.title);
    let request_body = ::bead::review_request_body(followup, f.title, f.repo, f.actor, f.bound, f.body.unwrap_or(""), (f.priority.unwrap_or(FOLLOWUP_PRIORITY), f.statement));
    let request = match file(&request_title, &request_body) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("{followup} was filed but its Skuld review could not be filed (do not file it again; file the review and make {followup} wait on it): {e}")),
    };
    if let Err(e) = wait(followup, &request) {
        return (CANNOT_TELL, format!("{followup} was filed and so was its Skuld review {request}, but {followup} could not be made to wait on it (do not file it again; add the dependency): {e}"));
    }
    crate::bd::record_event("spira_skuld_reviews_filed", "file-followup");
    (0, format!("{followup}\n{request} is the Skuld review of {followup}; {followup} waits on it and cannot be claimed until it is done"))
}

struct Filing<'a> {
    verb: &'a str,
    actor: &'a str,
    title: &'a str,
    persona: &'a str,
    repo: &'a str,
    bound: &'a str,
    after_landing: bool,
    prerequisite: bool,
    body: Option<&'a str>,
    priority: Option<i64>,
    statement: Option<&'a str>,
}

/// A pilgrim files what its walk found; a defect against a repo that does not meet the landing
/// contract is exactly what must be queued, so the broker states the reason the guard asks for.
const PILGRIM_CONTRACT_REASON: &str = "filed from a pilgrimage: the defect is the finding, whatever state the repo is in";

/// One side of a filing's blocks edge: the new bead has no id until it is filed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Bound,
    New,
}

/// How a filing is tied to the bound bead. `edge` is `(blocked, blocker)`.
#[derive(Debug, PartialEq, Eq)]
struct Binding<'a> {
    parent: Option<&'a str>,
    split_label: bool,
    edge: Option<(End, End)>,
}

/// The store refuses a bead blocked by its own descendant, so a filing with a blocks edge to or
/// from the bound bead is filed unparented.
fn binding<'a>(f: &Filing<'a>) -> Binding<'a> {
    let edge = if f.after_landing {
        Some((End::New, End::Bound))
    } else if f.prerequisite {
        Some((End::Bound, End::New))
    } else {
        blocking_edge(f.verb, f.actor, End::Bound, End::New)
    };
    let parent = edge.is_none().then_some(f.bound);
    Binding { parent, split_label: parent.is_some() && f.verb == "split", edge }
}

fn file_and_bind(
    f: &Filing,
    file: impl FnOnce(&Filing, Option<&str>, Option<&str>) -> Result<String, String>,
    note: impl FnOnce(&str, &str) -> Result<(), String>,
    register: impl FnOnce(&str) -> (i32, String),
    label: impl FnOnce(&str, &str) -> Result<(), String>,
    block: impl FnOnce(&str, &str) -> Result<(), String>,
) -> (i32, String) {
    let tie = binding(f);
    let pilgrim = blocking_edge(f.verb, f.actor, End::Bound, End::New).is_some();
    let new_id = match file(f, tie.parent, pilgrim.then_some(PILGRIM_CONTRACT_REASON)) {
        Ok(id) => id,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let unrecorded = record_statement(f, &new_id, note).err();
    let bound = bind_filed(f, tie, new_id, register, label, block);
    match unrecorded {
        None => bound,
        Some(why) => (CANNOT_TELL, format!("{}\n{why}", bound.1)),
    }
}

/// Registers a new item and ties it to the bound bead.
fn bind_filed(
    f: &Filing,
    tie: Binding,
    new_id: String,
    register: impl FnOnce(&str) -> (i32, String),
    label: impl FnOnce(&str, &str) -> Result<(), String>,
    block: impl FnOnce(&str, &str) -> Result<(), String>,
) -> (i32, String) {
    let bead_id = f.bound;
    let (code, e) = register(&new_id);
    if code != 0 {
        return (CANNOT_TELL, format!("LIFECYCLE: {new_id} was filed but has NO lifecycle row and cannot be claimed (do not file it again): {e}"));
    }
    if tie.split_label {
        if let Err(e) = label(&new_id, spira_config::nonwork::SPLIT_LABEL) {
            return (CANNOT_TELL, format!("{new_id} was filed but could not be marked a split child, so it would be claimable before {bead_id} lands (do not file it again; add the label {}): {e}", spira_config::nonwork::SPLIT_LABEL));
        }
    }
    if let Some((blocked, blocker)) = tie.edge {
        let id = |end| match end {
            End::Bound => bead_id,
            End::New => new_id.as_str(),
        };
        if let Err(e) = block(id(blocked), id(blocker)) {
            let failed = match blocked {
                End::New => format!("could not be made to wait on {bead_id}"),
                End::Bound => format!("{bead_id} could not be made to wait on it"),
            };
            return (CANNOT_TELL, format!("{new_id} was filed but {failed} (do not file it again; add the dependency): {e}"));
        }
    }
    (0, new_id)
}

/// A pilgrimage is called again only when its defects land, so a pilgrim's filing blocks the bound
/// bead on the new one: `(blocked, blocker)`.
fn blocking_edge<T>(verb: &str, actor: &str, bound: T, new_id: T) -> Option<(T, T)> {
    (verb == "file-followup" && actor == "pilgrim").then_some((bound, new_id))
}

/// The persona a filing takes its partition from: a `file-followup` goes to the lane its filer's
/// rune names, so a defect lands where it can be built; a `split` stays in its own lane.
fn filed_as(verb: &str, filer: &str) -> Result<String, String> {
    if verb != "file-followup" {
        return Ok(filer.to_string());
    }
    let home = spira_config::resolve::locate_home_for_process()?;
    spira_config::runes::followup_persona_in(&spira_config::runes::runes_dir(&home), filer)
}

fn cmd_superseded_by(bead_id: &str, args: &[String]) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let successor = match args.first() {
        Some(id) if is_bead_id(id) && id != bead_id => id.clone(),
        Some(other) => return (REFUSED, format!("refused: work superseded-by: {other:?} is not another bead's id; nothing was held or asked. Usage: work superseded-by <successor-id>")),
        None => return (CANNOT_TELL, "work superseded-by: missing <successor-id>".into()),
    };
    let successor_title = match crate::bd::title(&successor) {
        Ok(Some(t)) => t,
        Ok(None) => return (REFUSED, format!("refused: work superseded-by: no bead {successor} exists; nothing was held or asked")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let own_title = crate::bd::title(bead_id).ok().flatten().unwrap_or_default();
    let landing = landing_commit(&successor);
    let subject = format!("Retire as superseded by {successor}?");
    let default = format!("supersede {bead_id} by {successor}");
    let body = supersede_ask_body(bead_id, &own_title, &successor, &successor_title, landing.as_deref());
    if let Err(e) = crate::bd::ask_operator_classed(&format!("{actor} <{actor}@spira>"), &subject, &default, Some(SUPERSEDE_CLASS), bead_id, &body) {
        return (REFUSED, format!("refused: work superseded-by: the confirmation ask could not be delivered, so no hold was applied: {e}"));
    }
    // Not BeadEventKind::Supersede: that terminal transition is the groomer's or operator's
    // confirmation; the dwarf's request is a hold plus the ask.
    let hold = BeadEventKind::Hold { kind: HoldKind::Manual, cause: HoldCause::SupersedeRequest, detail: Some(successor.clone()) };
    let (code, out) = crate::plane::apply(bead_id, &actor, hold);
    let recorded = matches!(crate::plane::bead(bead_id), Ok(Some(r)) if r.holds.contains(&HoldKind::Manual) && r.reason.as_deref() == Some(successor.as_str()));
    match (code, recorded) {
        (0, true) => (0, format!("supersede ask filed; hold applied for {bead_id}")),
        (0, false) => (CANNOT_TELL, format!("the supersede ask WAS delivered (do not ask again), but no hold was applied to {bead_id}: the store does not list it")),
        (code, _) => (code, format!("the supersede ask WAS delivered (do not ask again), but the hold on {bead_id} was not applied: {out}")),
    }
}

const SUPERSEDE_CLASS: &str = "policy";

/// Mail lint refuses a body line that names a bead without saying what the work is, so every
/// id rides beside its title and, for the successor, where it landed.
fn supersede_ask_body(bead_id: &str, own_title: &str, successor: &str, successor_title: &str, landing: Option<&str>) -> String {
    let landed = match landing {
        Some(l) => format!("It landed in commit {l}."),
        None => "No landing commit naming it was found on its repository's landing ref.".to_string(),
    };
    format!(
        "## Question\n\
         Should {bead_id} (\"{own_title}\") be retired as superseded by {successor} (\"{successor_title}\")?\n\
         The successor {successor} is \"{successor_title}\". {landed}\n\n\
         ## Default\n\
         Confirm the supersede: the bead's work already landed under its successor.\n\n\
         ## Class basis\n\
         Retiring a bead as superseded is terminal, and only the groomer or the operator may confirm it.\n"
    )
}

/// `<short sha> <subject>` of the newest commit on the successor's landing ref naming it.
fn landing_commit(id: &str) -> Option<String> {
    let repo = submit_repo(id).ok()?;
    let home = spira_config::resolve::locate_home_for_process().ok()?;
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let reg = spira_config::repos::Registry::from_env(env, std::path::Path::new(&home));
    let root = reg.root(&repo)?;
    let landref = spira_config::repos::landref(&reg, &root)?;
    let out = state_store::phases::child("git", || {
        crate::git_evidence::git_in(std::path::Path::new(&root))?
            .args(["log", "-n", "50", "--fixed-strings", &format!("--grep={id}"), "--format=%h%x1f%s%x1f%B%x1e", &landref])
            .output()
    })
    .ok()
        .filter(|o| o.status.success())?;
    String::from_utf8_lossy(&out.stdout).split('\x1e').map(|r| r.trim_start_matches('\n')).find_map(|rec| {
        let mut f = rec.splitn(3, '\x1f');
        let (sha, subject, message) = (f.next()?, f.next()?, f.next()?);
        crate::git_evidence::message_names_bead(message, id).then(|| format!("{sha} ({subject})"))
    })
}

// ---- the lane verbs and the persona gate (sp-st0mm) --------------------------------------
//
// Operator order 2026-10-05: no dwarf model runs `bd`. Everything a persona legitimately does
// to the graph — including to beads OTHER than the one it was called for (a groomer closes
// duplicates, ops adds a dependency, czar ejects a member) — is a named verb here, and this
// broker is the only thing that calls bd or a bd-calling tool on its behalf. Each verb is one
// narrow operation with its arguments validated; there is deliberately no "run this bd
// command" passthrough, because that would re-open exactly the hole this closes.
//
// Who may run what is ONE table, [`ALLOW`]. The caller's persona is the `--actor` the `work`
// client appends from its own environment (`SPIRA_RUNE`, else `SPIRA_WORK_ACTOR`); the
// client refuses a user-typed `--actor`; serve binds it to the peer's uid ([`bind_peer`]) for
// every uid listed in `<socket>.personas`, since the environment is the caller's to set.

/// Every verb that is not bound to the called bead. The bound verbs above act on the
/// bead the client is bound to; these name their target (or none) themselves.
pub const LANE_VERBS: &[&str] = &[
    "ask", "read", "list", "search", "open-items", "note-on", "label-add", "label-remove", "dep-add", "relate", "reopen", "close-other", "close-ask", "file", "groom", "incident",
    "sop", "census", "queue", "landing-pass", "strand", "fence", "hel", "mender", "forge", "claim",
];

const BOUND_VERBS: &[&str] = &["show", "note", "submit", "done", "blocked", "file-followup", "split", "superseded-by", "ack", "logs", "cite-check"];

/// The tool verbs: `work <verb> <sub> ...` runs `<program> <sub> ...` — a harness tool that
/// itself reaches bd, with `<sub>` one of the operations [`ALLOW`] names for it. `census`
/// takes no subcommand.
const TOOLS: &[(&str, &str)] = &[
    ("groom", "groomer"),
    ("incident", "incident.sh"),
    ("sop", "sop"),
    ("census", "census"),
    ("queue", "queue"),
    ("landing-pass", "landing-pass"),
    ("strand", "strand"),
    ("hel", "hel"),
    ("mender", "mender"),
];

/// A tool call's wall: a groomer sweep or a queue step reads the whole graph, so this is the
/// tool's own runtime bound, not a query deadline.
// batch-job: a broker-run harness tool (groomer sweep, queue step), bounded at 300 s.
const TOOL_SECS: u64 = 300;

pub fn is_tool(verb: &str) -> bool {
    TOOLS.iter().any(|(v, _)| *v == verb)
}

/// The verbs that run a brokered tool (lint, testenv, bead.sh, a harness sweep) and so wait on
/// the client's tool bound, not the store-call deadline; `work`'s client waits the same set.
pub fn brokers_tool(verb: &str) -> bool {
    is_tool(verb) || LANE_VERBS.contains(&verb) || matches!(verb, "file-followup" | "split" | "submit" | "ack")
}

/// The brokered verbs that start no program in the scheduler: a content read, a config lookup, or a note or label write the service makes itself.
const ANSWERED_IN_SERVICE: &[&str] = &["read", "list", "search", "open-items", "fence", "note-on", "label-add", "label-remove", "claim"];

/// The answered-in-service verbs held to the latency limit like any store call, not exempted as
/// batch jobs: the writes, and `open-items`, which every filing asks.
pub fn held_to_latency_limit(verb: &str) -> bool {
    matches!(verb, "note-on" | "label-add" | "label-remove" | "open-items")
}

/// The verbs lc-serve records as a request for the scheduler to run: the brokered tools not answered in the service.
pub fn exec_verb(verb: &str) -> bool {
    (brokers_tool(verb) && !ANSWERED_IN_SERVICE.contains(&verb)) || matches!(verb, "blocked" | "superseded-by")
}

/// How long the scheduler lets a recorded verb run: the tool bound every program-running verb keeps.
pub fn exec_bound_ms(verb: &str) -> u64 {
    match verb {
        "submit" => TOOL_SECS.max(PROOF_BUDGET_SECS + 30) * 1000,
        _ => TOOL_SECS * 1000,
    }
}

/// The answer to a verb recorded for the scheduler: its id, on a line the client recognises.
pub fn queued_answer(id: &str) -> String {
    format!("queued {id}")
}

/// Records `args` (`<bead> <verb> ...`) as a request for the scheduler and answers with its id.
fn record(bead_id: &str, args: &[String]) -> (i32, String) {
    let call = split_reserved(&args[2..]);
    let mut argv = vec!["work".to_string(), bead_id.to_string(), args[1].clone()];
    argv.extend(call.args.iter().cloned());
    record_argv(bead_id, call.actor.as_deref().unwrap_or(""), &argv, call.stdin.as_deref(), exec_bound_ms(&args[1]))
}

/// Records an `argv` for the scheduler's `work-exec` and answers with the id the store allocated.
pub fn record_argv(bead: &str, actor: &str, argv: &[String], stdin: Option<&str>, bound_ms: u64) -> (i32, String) {
    let store = match crate::control::served::store() {
        Ok(s) => s,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    match store.put_work_request(&crate::control::work_requests::NewRequest { bead, actor, argv, stdin, bound_ms }, crate::db::now_epoch(), &crate::control::served::call_request_id()) {
        Ok(id) => (0, queued_answer(&id)),
        Err(e) => (CANNOT_TELL, format!("cannot tell: recording the request: {e}")),
    }
}

/// A world's stand-in for the scheduler's `broker.exec`: takes each queued request and runs it
/// in a thread of this process, until the service stops answering.
pub fn run_queue() -> (i32, String) {
    let call = |args: &[&str]| crate::plane::call(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>());
    let mut last_failure: Option<String> = None;
    loop {
        let polled = poll_once(&call, |id| {
            std::thread::spawn(move || exec(&id));
        });
        if let Some(line) = poll_report(&mut last_failure, &polled) {
            eprintln!("work-run: {line}");
        }
        std::thread::sleep(std::time::Duration::from_millis(100)); // waits-on: nothing; polls the queue while the service answers
    }
}

/// The line a poll's outcome earns in the log: a failure that differs from the last one, or the
/// first success after a failure. `last_failure` is the text last reported.
fn poll_report(last_failure: &mut Option<String>, polled: &Result<usize, String>) -> Option<String> {
    match polled {
        Ok(_) => last_failure.take().map(|_| "the queue answers again".to_string()),
        Err(failure) if last_failure.as_ref() != Some(failure) => {
            *last_failure = Some(failure.clone());
            Some(failure.clone())
        }
        Err(_) => None,
    }
}

/// One poll of the queue: takes each queued request and hands its id to `start`. A failed list is
/// an error, never an empty queue; a failed `take` means another runner has it, so it is skipped.
fn poll_once(call: &dyn Fn(&[&str]) -> (i32, String), start: impl Fn(String)) -> Result<usize, String> {
    let (code, listed) = call(&["work-request", "list", "--state", "queued"]);
    if code != 0 {
        return Err(format!("listing queued requests failed, code {code}: {listed}"));
    }
    let queued: Vec<crate::control::work_requests::WorkRequest> = serde_json::from_str(&listed).map_err(|e| format!("listing queued requests gave an answer that is not the request list ({e}): {listed}"))?;
    let mut taken = 0;
    for r in queued {
        if call(&["work-request", "take", &r.id, "--actor", "spira-runtime"]).0 == 0 {
            taken += 1;
            start(r.id);
        }
    }
    Ok(taken)
}

/// Runs the recorded request `id` in this process and records its exit code and output. This is
/// `spira-lc work-exec`, started by the scheduler; every store call it makes goes to lc-serve.
pub fn exec(id: &str) -> (i32, String) {
    let call = |args: &[&str]| crate::plane::call(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>());
    let (code, shown) = call(&["work-request", "show", id]);
    if code != 0 {
        return (code, shown);
    }
    let request: crate::control::work_requests::WorkRequest = match serde_json::from_str(&shown) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: work-exec {id}: {e}")),
    };
    let (code, out) = run_recorded(&request);
    let code = code.to_string();
    let (done, said) = crate::plane::call(&["work-request".into(), "finish".into(), id.into(), "--code".into(), code, "--stdin".into(), out]);
    if done == 0 { (0, String::new()) } else { (done, said) }
}

fn run_recorded(request: &crate::control::work_requests::WorkRequest) -> (i32, String) {
    let mut argv = request.argv.clone();
    if let Some(body) = &request.stdin {
        argv.extend(["--stdin".to_string(), body.clone()]);
    }
    if !request.actor.is_empty() {
        argv.extend(["--actor".to_string(), request.actor.clone()]);
    }
    let (Some(bead), Some(verb)) = (argv.get(1).cloned(), argv.get(2).cloned()) else {
        return (CANNOT_TELL, format!("work-exec {}: the recorded request names no verb", request.id));
    };
    run(&bead, &verb, &argv[3..])
}

const ANY: &[&str] = &["*"];

/// THE allow table: operation → the personas that may run it (`*`: every persona). An
/// operation absent from this table is refused for everyone. A tool verb's operation is
/// `"<verb> <sub>"`.
pub const ALLOW: &[(&str, &[&str])] = &[
    // Bound to the called bead: every persona acts on its own bead.
    ("show", ANY),
    ("note", ANY),
    ("submit", ANY),
    ("done", ANY),
    ("blocked", ANY),
    ("file-followup", ANY),
    ("split", ANY),
    ("superseded-by", ANY),
    ("ack", ANY),
    ("logs", ANY),
    ("cite-check", ANY),
    // Reads and the operator mailbox.
    ("ask", ANY),
    ("read", ANY),
    ("list", ANY),
    ("search", ANY),
    ("open-items", ANY),
    // Writes to other beads.
    ("note-on", &["groomer", "czar", "mimir", "ops", "batcher", "archivist", "warden"]),
    ("label-add", &["groomer", "batcher", "mimir"]),
    ("label-remove", &["groomer", "batcher"]),
    ("dep-add", &["ops", "groomer", "batcher", "czar", "warden"]),
    ("relate", &["archivist", "groomer"]),
    ("reopen", &["groomer", "czar", "mimir"]),
    ("close-other", &["czar"]),
    ("close-ask", &["archivist"]),
    ("file", &["ops", "spike", "archivist", "mimir", "czar", "groomer", "batcher", "warden"]),
    // Tools.
    ("groom sweep", &["groomer"]),
    ("groom split-piece", &["groomer"]),
    ("groom supersede", &["groomer"]),
    ("groom close", &["groomer"]),
    ("groom correct-lane", &["groomer"]),
    ("groom depends-on-fix", &["groomer"]),
    ("groom unpoison", &["groomer"]),
    ("groom triage-poison", &["groomer"]),
    ("groom deadlocked", &["groomer"]),
    ("groom unwanted", &["groomer"]),
    ("incident list", &["ops", "czar"]),
    ("incident file", &["ops", "czar"]),
    ("incident collapse", &["ops", "czar"]),
    ("sop match", &["ops"]),
    ("sop show", &["ops"]),
    ("sop list", &["ops"]),
    ("sop log", &["ops"]),
    ("sop applied", &["ops"]),
    ("sop write", &["ops"]),
    ("census", &["mimir"]),
    ("queue stats", &["czar"]),
    ("queue step", &["czar"]),
    ("queue eject", &["czar"]),
    ("queue abandon", &["czar"]),
    ("landing-pass halt", &["czar"]),
    ("strand report", &["czar", "groomer"]),
    ("strand detect-livelocked", &["groomer"]),
    ("fence", &["czar"]),
    ("forge ami-result", &["builder"]),
    ("forge ami-dispatch", &["builder"]),
    ("forge ami-log", &["builder"]),
    ("forge ami-push", &["builder"]),
    ("forge ami-delete", &["builder"]),
    ("hel --dry-run", &["ops", "czar"]),
    ("mender start", &["mender"]),
    ("mender pack", &["mender"]),
    ("mender diff", &["mender"]),
    ("mender bead", &["mender"]),
    ("mender members", &["mender"]),
    ("mender failures", &["mender"]),
    ("mender passes", &["mender"]),
    ("mender verdicts", &["mender"]),
    ("mender landing", &["mender"]),
    ("mender triaged", &["mender"]),
    ("mender diagnose", &["mender"]),
    ("mender outcome", &["mender"]),
];

/// A request's own arguments with the client's reserved trailer split off: `... [--stdin
/// <text>] --actor <persona>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub actor: Option<String>,
    actors: usize,
}

pub fn split_reserved(rest: &[String]) -> Call {
    let mut args = rest.to_vec();
    let (mut actor, mut stdin) = (None, None);
    if args.len() >= 2 && args[args.len() - 2] == "--actor" {
        actor = args.pop();
        args.pop();
    }
    if args.len() >= 2 && args[args.len() - 2] == "--stdin" {
        stdin = args.pop();
        args.pop();
    }
    let actors = rest.iter().filter(|a| *a == "--actor").count();
    Call { args, stdin, actor, actors }
}

/// `<uid> <persona>` lines (`#` comments): the personas whose own OS account is the identity.
pub fn parse_persona_uids(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.split('#').next().unwrap_or("").trim();
            let mut it = l.split_whitespace();
            let (uid, persona) = (it.next()?.parse().ok()?, it.next()?.to_string());
            it.next().is_none().then_some((uid, persona))
        })
        .collect()
}

/// Binds a `work` request's persona to the connecting process's uid. A uid in `map` IS its
/// persona: a different `--actor` is refused, never overridden, and an absent one is filled
/// in. A uid not in `map` keeps the client-sent `--actor` (nothing else identifies it).
pub fn bind_peer(argv: &[String], peer_uid: Option<u32>, map: &[(u32, String)]) -> Result<Vec<String>, (i32, String)> {
    if argv.first().map(String::as_str) != Some("work") || map.is_empty() {
        return Ok(argv.to_vec());
    }
    let Some(uid) = peer_uid else {
        return Err((REFUSED, "refused: the caller's uid could not be read from the socket, and personas are bound to uids here".into()));
    };
    let Some((_, persona)) = map.iter().find(|(u, _)| *u == uid) else {
        return Ok(argv.to_vec());
    };
    let sent = argv.iter().filter(|a| *a == "--actor").count();
    let mut out = argv.to_vec();
    match sent {
        0 => out.extend(["--actor".to_string(), persona.clone()]),
        1 if out.len() >= 2 && out[out.len() - 2] == "--actor" => {
            if out[out.len() - 1] != *persona {
                return Err((REFUSED, format!("refused: uid {uid} is persona {persona}, not {}", out[out.len() - 1])));
            }
        }
        _ => return Err((REFUSED, "refused: work: --actor is the client's trailer alone, never the caller's".into())),
    }
    Ok(out)
}

/// The operation key [`ALLOW`] is read with.
pub fn op_key(verb: &str, args: &[String]) -> String {
    match TOOLS.iter().find(|(v, _)| *v == verb) {
        None if verb == "forge" => format!("{verb} {}", args.first().map(String::as_str).unwrap_or("")),
        Some(_) if verb != "census" => format!("{verb} {}", args.first().map(String::as_str).unwrap_or("")),
        _ => verb.to_string(),
    }
}

/// What the pilgrim may run, and only in a pilgrim world: its scratch beads stay in a store the
/// dispatcher never claims from.
pub const PILGRIM_WORLD_ALLOW: &[&str] = &["file", "dep-add", "close-other", "claim"];

fn in_pilgrim_world() -> bool {
    spira_config::process::cfg("SPIRA_LC_SOCKET").is_ok_and(|socket| socket_is_a_pilgrim_world(&socket))
}

fn socket_is_a_pilgrim_world(socket: &str) -> bool {
    !socket.is_empty() && spira_store::lc_call::socket_in_sim_world(socket) && std::path::Path::new(&spira_store::lc_call::pilgrim_world_marker(socket)).is_file()
}

/// May `actor` run `op`? `Err` is the refusal to answer with.
pub fn permitted(op: &str, actor: Option<&str>, pilgrim_world: bool) -> Result<(), (i32, String)> {
    if pilgrim_world && actor == Some("pilgrim") && PILGRIM_WORLD_ALLOW.contains(&op) {
        return Ok(());
    }
    let Some((_, who)) = ALLOW.iter().find(|(k, _)| *k == op) else {
        return Err((REFUSED, format!("refused: `work {op}` is not an operation any persona may run")));
    };
    if who.contains(&"*") {
        return Ok(());
    }
    let Some(actor) = actor else {
        return Err((REFUSED, format!("refused: `work {op}` needs the caller's persona (--actor) and none was sent")));
    };
    if who.contains(&actor) {
        Ok(())
    } else {
        Err((REFUSED, format!("refused: persona {actor} may not run `work {op}` (allowed: {})", who.join(", "))))
    }
}

/// The pilgrim's claim: a builder's, held for as long as a walk takes.
fn pilgrim_claim() -> BeadEventKind {
    BeadEventKind::Claim { holder: "pilgrim".into(), lease_until: crate::db::now_epoch() + PILGRIM_LEASE_SECS, stack: Default::default(), stack_depth: 0, stack_max_depth: 0, persona: Some("builder".into()), directed: None }
}

const PILGRIM_LEASE_SECS: i64 = 3_600;

fn gate(verb: &str, rest: &[String]) -> Result<(), (i32, String)> {
    gate_in(verb, rest, in_pilgrim_world)
}

fn gate_in(verb: &str, rest: &[String], pilgrim_world: impl Fn() -> bool) -> Result<(), (i32, String)> {
    if !BOUND_VERBS.contains(&verb) && !LANE_VERBS.contains(&verb) {
        return Ok(()); // dispatch's own "unknown verb" answer
    }
    let call = split_reserved(rest);
    if call.actors > 1 {
        return Err((REFUSED, format!("refused: work {verb}: more than one --actor — the persona is the client's to send, never the caller's")));
    }
    let op = op_key(verb, &call.args);
    let world = PILGRIM_WORLD_ALLOW.contains(&op.as_str()) && pilgrim_world();
    permitted(&op, call.actor.as_deref(), world)
}

/// One thing a lane verb does: a bd call, a harness tool, — after a `bead.sh file` for a
/// persona — the lifecycle row for the id it printed, or — after a bound session's
/// question reached the operator — the `ask` hold on the asking session's own bead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Bd { args: Vec<String>, stdin: Option<String> },
    /// `work list`: the lifecycle rows in these states (every non-terminal state when empty,
    /// every state with `all`), joined to the SQL reader's content rows narrowed by `filters`.
    LcList { states: Vec<String>, all: bool, filters: Vec<String>, json: bool, full: bool, limit: Option<usize> },
    /// `work read`: another bead, answered by the SQL reader.
    Read { id: String, json: bool },
    /// `work search`: the SQL reader's `search`, ranked as the door ranks it.
    Search { query: String, limit: Option<String>, json: bool },
    /// `work open-items`: one repository's READY and REWORK items with their `## Outcome` text.
    OpenItems { repo: String, exclude: Vec<String>, json: bool },
    Tool { program: &'static str, args: Vec<String>, stdin: Option<String> },
    /// `work claim`: the caller takes a READY bead as a builder; served only in a pilgrim world.
    Claim { bead: String },
    CreateRow { repo: String },
    /// `Hold { Ask, OperatorQuestion }` on `bead`: the bead waits on the operator's answer
    /// (only a `Reply` or `AskWithdrawn` lifts it), and dwarf's teardown reads the hold to
    /// release the session operator-wait, uncharged (sp-v62vn follow-up: it replaced the
    /// `<bead>.operator-wait` marker `mail` wrote, which a restricted model cannot run).
    AskHold { bead: String, question: String },
    /// `mail send operator ...` for `work ask`, run with `BEAD_ID=<classify_as>`: mail's
    /// escalation-class check (`mail::cmds::class_refusal`) engages only when BEAD_ID is set —
    /// the old direct-mail path had it from the dwarf's own environment, the broker's process
    /// does not — so without it an unclassed question went straight to the operator. With it,
    /// an ask lacking a PERMISSIONS/POLICY/DESTRUCTIVE `--class` and a `## Class basis` is
    /// routed to the concierge mailbox (law-escalate-decisions-not-problems).
    Ask { args: Vec<String>, stdin: String, classify_as: String },
}

fn s(x: &str) -> String {
    x.to_string()
}

fn usage(verb: &str, msg: &str) -> (i32, String) {
    (CANNOT_TELL, format!("work {verb}: {msg}"))
}

pub(crate) fn is_bead_id(x: &str) -> bool {
    x.len() > 3 && x[..3].eq_ignore_ascii_case("sp-") && x[3..].chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

fn bead_arg(verb: &str, args: &[String], i: usize, what: &str) -> Result<String, (i32, String)> {
    match args.get(i) {
        Some(id) if is_bead_id(id) => Ok(id.clone()),
        Some(other) => Err(usage(verb, &format!("{what} must be a bead id, not {other:?}"))),
        None => Err(usage(verb, &format!("missing {what}"))),
    }
}

/// `--flag value` pairs and bare switches from `args[from..]`, every token checked against
/// `values`/`switches`; anything else is refused rather than forwarded.
#[allow(clippy::type_complexity)]
fn parse_flags(verb: &str, args: &[String], from: usize, values: &[&str], switches: &[&str]) -> Result<Vec<(String, Option<String>)>, (i32, String)> {
    let mut out = Vec::new();
    let mut i = from;
    while i < args.len() {
        let a = args[i].as_str();
        if values.contains(&a) {
            let Some(v) = args.get(i + 1) else { return Err(usage(verb, &format!("{a} needs a value"))) };
            out.push((a.to_string(), Some(v.clone())));
            i += 2;
        } else if switches.contains(&a) {
            out.push((a.to_string(), None));
            i += 1;
        } else {
            return Err(usage(verb, &format!("unexpected argument {a:?}")));
        }
    }
    Ok(out)
}

fn get<'a>(flags: &'a [(String, Option<String>)], name: &str) -> Option<&'a str> {
    flags.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.as_deref())
}

fn flatten(flags: &[(String, Option<String>)]) -> Vec<String> {
    flags.iter().flat_map(|(k, v)| std::iter::once(k.clone()).chain(v.clone())).collect()
}

/// The mail display name a persona sends as (`From: <name> <persona@spira>`).
pub fn display_name(actor: &str) -> String {
    match actor {
        "batcher" => "Judge".to_string(),
        _ => {
            let mut c = actor.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        }
    }
}

/// The label a lane verb never adds or removes: poison is lifted only by `groom unpoison`
/// with its cause and evidence (sp-pl7zg's `spira-claim unpoison` discipline), never by a
/// bare label edit.
const POISON_LABEL: &str = "spira-poison";

/// What a lane verb does, decided from its arguments alone — no I/O, so every rule below is
/// covered without a fixture. `bound` is the client's called bead (`-` when unbound).
pub fn plan(verb: &str, bound: &str, call: &Call) -> Result<Vec<Step>, (i32, String)> {
    let a = &call.args;
    let actor = call.actor.as_deref().unwrap_or("dwarf");
    let bd = |args: Vec<String>| Step::Bd { args, stdin: None };
    match verb {
        "read" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let f = parse_flags(verb, a, 1, &[], &["--json"])?;
            Ok(vec![Step::Read { id, json: f.iter().any(|(k, _)| k == "--json") }])
        }
        "list" => {
            if a.iter().any(|x| x == "--status") {
                return Err(usage(verb, "--status is bd's status; a bead's state is the lifecycle's — use --state <STATE[,STATE]>"));
            }
            let f = parse_flags(verb, a, 0, &["--state", "--label", "--title-contains", "--limit"], &["--json", "--all", "--full"])?;
            let mut states = Vec::new();
            for st in get(&f, "--state").into_iter().flat_map(|v| v.split(',')) {
                let up = st.to_ascii_uppercase();
                if bead::BeadState::from_str(&up).is_none() {
                    return Err(usage(verb, &format!("--state {st:?} is not a lifecycle state")));
                }
                states.push(up);
            }
            let limit = match get(&f, "--limit") {
                Some(n) => Some(n.parse::<usize>().map_err(|_| usage(verb, &format!("--limit {n:?} is not a number")))?),
                None => None,
            };
            let mut filters = Vec::new();
            for k in ["--label", "--title-contains"] {
                if let Some(v) = get(&f, k) {
                    filters.extend([s(k), v.to_string()]);
                }
            }
            Ok(vec![Step::LcList { states, all: f.iter().any(|(k, _)| k == "--all"), filters, json: f.iter().any(|(k, _)| k == "--json"), full: f.iter().any(|(k, _)| k == "--full"), limit }])
        }
        "search" => {
            let Some(q) = a.first().filter(|q| !q.is_empty() && !q.starts_with('-')) else {
                return Err(usage(verb, "missing <query>"));
            };
            let f = parse_flags(verb, a, 1, &["--limit"], &["--json"])?;
            Ok(vec![Step::Search { query: q.clone(), limit: get(&f, "--limit").map(str::to_string), json: f.iter().any(|(k, _)| k == "--json") }])
        }
        "open-items" => {
            const WANT: &str = "work open-items --repo <name> [--exclude <id,…>] [--json]";
            let refuse = |why: String| usage(verb, &format!("{why}; usage: {WANT}"));
            let f = parse_flags(verb, a, 0, &["--repo", "--exclude"], &["--json"]).map_err(|(_, why)| refuse(why))?;
            let Some(repo) = get(&f, "--repo").filter(|r| !r.is_empty() && !r.starts_with('-')) else {
                return Err(refuse("--repo <name> is required".into()));
            };
            let exclude: Vec<String> = get(&f, "--exclude").into_iter().flat_map(|v| v.split(',')).filter(|x| !x.is_empty()).map(str::to_string).collect();
            if let Some(bad) = exclude.iter().find(|x| !is_bead_id(x)) {
                return Err(refuse(format!("--exclude {bad:?} is not a bead id")));
            }
            Ok(vec![Step::OpenItems { repo: repo.to_string(), exclude, json: f.iter().any(|(k, _)| k == "--json") }])
        }
        "note-on" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            match (a.get(1).map(String::as_str), a.len()) {
                (Some("-"), 2) => match &call.stdin {
                    Some(text) => Ok(vec![Step::Bd { args: vec![s("note"), id, s("--stdin")], stdin: Some(text.clone()) }]),
                    None => Err(usage(verb, "`-` given but no text arrived on stdin")),
                },
                (Some(text), 2) if !text.is_empty() => Ok(vec![bd(vec![s("note"), id, text.to_string()])]),
                _ => Err(usage(verb, "want <bead-id> \"<text>\" (or - for stdin)")),
            }
        }
        "label-add" | "label-remove" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let Some(label) = a.get(1).filter(|_| a.len() == 2) else {
                return Err(usage(verb, "want <bead-id> <label>"));
            };
            if label.is_empty() || label.starts_with('-') || label.contains(',') || label.chars().any(char::is_whitespace) {
                return Err(usage(verb, &format!("{label:?} is not one label")));
            }
            if label == POISON_LABEL {
                return Err((REFUSED, format!("refused: work {verb}: poison is lifted with `work groom unpoison <id> --cause .. --evidence ..`, never by a label edit")));
            }
            let op = if verb == "label-add" { "add" } else { "remove" };
            Ok(vec![bd(vec![s("label"), s(op), id, label.clone()])])
        }
        "dep-add" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let on = bead_arg(verb, a, 1, "<depends-on-id>")?;
            let f = parse_flags(verb, a, 2, &["--type"], &[])?;
            if let Some(t) = get(&f, "--type") {
                if !["blocks", "related", "parent-child", "discovered-from"].contains(&t) {
                    return Err(usage(verb, &format!("--type {t:?} (want blocks, related, parent-child, discovered-from)")));
                }
            }
            // bead.sh's own wrapper, not raw `bd dep add`: it refuses a blocks edge onto an
            // incident bead, which has no completion path (law-a-refusal-names-its-exit).
            Ok(vec![Step::Tool { program: "bead.sh", args: [vec![s("dep"), s("add"), id, on], flatten(&f)].concat(), stdin: None }])
        }
        "relate" => {
            let x = bead_arg(verb, a, 0, "<bead-id>")?;
            let y = bead_arg(verb, a, 1, "<other-bead-id>")?;
            if a.len() != 2 {
                return Err(usage(verb, "want <bead-id> <other-bead-id>"));
            }
            Ok(vec![bd(vec![s("dep"), s("relate"), x, y])])
        }
        "reopen" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let f = parse_flags(verb, a, 1, &["--evidence"], &[])?;
            let Some(ev) = get(&f, "--evidence").filter(|e| !e.trim().is_empty()) else {
                return Err(usage(verb, "--evidence <text> is required: a reopen with no evidence hands the next session nothing"));
            };
            Ok(vec![
                Step::Tool { program: "spira-lc", args: vec![s("reopen"), id.clone(), s("operator-reopen"), actor.to_string()], stdin: None },
                Step::Bd { args: vec![s("note"), id, s("--stdin")], stdin: Some(format!("reopened by {actor}: {ev}")) },
            ])
        }
        "claim" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            parse_flags(verb, a, 1, &[], &[])?;
            Ok(vec![Step::Claim { bead: id }])
        }
        "close-other" => {
            let id = bead_arg(verb, a, 0, "<bead-id>")?;
            let f = parse_flags(verb, a, 1, &["--evidence"], &[])?;
            let Some(ev) = get(&f, "--evidence").filter(|e| !e.trim().is_empty()) else {
                return Err(usage(verb, "--evidence <text> is required"));
            };
            Ok(vec![Step::Tool { program: "groomer", args: vec![s("close"), id, s("--evidence"), ev.to_string()], stdin: None }])
        }
        "close-ask" => {
            let id = bead_arg(verb, a, 0, "<ask-id>")?;
            let f = parse_flags(verb, a, 1, &["--exit", "--source", "--body-file"], &[])?;
            let quote = match (get(&f, "--body-file"), &call.stdin) {
                (Some("-"), Some(text)) if !text.trim().is_empty() => text.trim().to_string(),
                _ => return Err(usage(verb, "--body-file - with the closing words on stdin is required: the quote of the answer, or the statute or fix that decided it")),
            };
            match get(&f, "--exit") {
                Some("answered") => {
                    let Some(src) = get(&f, "--source").filter(|x| !x.trim().is_empty()) else {
                        return Err(usage(verb, "--exit answered needs --source <transcript:turn> naming where the operator said it"));
                    };
                    Ok(vec![Step::Tool {
                        program: "spira-lc",
                        args: vec![s("close-ask"), id, s("--exit"), s("answered"), s("--quote"), quote, s("--actor"), actor.to_string(), s("--channel"), s("transcript"), s("--message-id"), src.to_string()],
                        stdin: None,
                    }])
                }
                Some("withdrawn") => Ok(vec![Step::Tool {
                    program: "spira-lc",
                    args: vec![s("close-ask"), id, s("--exit"), s("withdrawn"), s("--quote"), quote, s("--actor"), actor.to_string()],
                    stdin: None,
                }]),
                other => Err(usage(verb, &format!("--exit {other:?} (want answered, or withdrawn for an ask a statute or a landed fix decided; taking the default is the operator's)"))),
            }
        }
        "file" => {
            let Some(title) = a.first().filter(|t| !t.trim().is_empty() && !t.starts_with('-')) else {
                return Err(usage(verb, "missing \"<title>\""));
            };
            let f = parse_flags(verb, a, 1, &["--for", "--kind", "--repo", "--priority", "--parent", "--body-file"], &[])?;
            let persona = get(&f, "--for");
            if persona.is_some() == get(&f, "--kind").is_some() {
                return Err(usage(verb, "exactly one of --for <persona> (work) or --kind <kind> (a record) is required"));
            }
            if persona.is_some() && get(&f, "--repo").is_none() {
                return Err(usage(verb, "--for needs --repo <name>"));
            }
            if let Some(p) = get(&f, "--parent") {
                if !is_bead_id(p) {
                    return Err(usage(verb, &format!("--parent {p:?} is not a bead id")));
                }
            }
            match get(&f, "--body-file") {
                Some("-") if call.stdin.is_none() => return Err(usage(verb, "--body-file - given but no body arrived on stdin")),
                Some("-") | None => {}
                Some(other) => return Err(usage(verb, &format!("--body-file {other:?}: the client sends the body; the broker reads only -"))),
            }
            let mut steps = vec![Step::Tool {
                program: "bead.sh",
                args: [vec![s("file"), title.clone()], flatten(&f)].concat(),
                stdin: call.stdin.clone(),
            }];
            if persona.is_some() {
                steps.push(Step::CreateRow { repo: get(&f, "--repo").unwrap_or_default().to_string() });
            }
            Ok(steps)
        }
        "ask" => {
            let f = parse_flags(verb, a, 0, &["--subject", "--kind", "--default", "--class", "--bead", "--body-file"], &["--dry-run"])?;
            let dry_run = f.iter().any(|(k, _)| k == "--dry-run");
            let (Some(subject), Some(kind), Some(default)) = (get(&f, "--subject"), get(&f, "--kind"), get(&f, "--default")) else {
                return Err(usage(verb, "--subject, --kind and --default are all required (an ask without a default is incomplete)"));
            };
            if let Some(e) = not_a_question(subject) {
                return Err(usage(verb, &format!("{e}; usage: work ask --subject \"<the question>\" --kind question --default \"<what you would do>\"")));
            }
            if !MAIL_KINDS.contains(&kind) {
                return Err(usage(verb, &format!("--kind {kind:?} (want one of {})", MAIL_KINDS.join(", "))));
            }
            let cited = match get(&f, "--bead") {
                Some(b) if is_bead_id(b) => Some(b.to_string()),
                Some(b) => return Err(usage(verb, &format!("--bead {b:?} is not a bead id"))),
                None if is_bead_id(bound) => Some(bound.to_string()),
                None => None,
            };
            let body = match (get(&f, "--body-file"), &call.stdin) {
                (Some("-"), Some(text)) => text.clone(),
                (Some(other), _) => return Err(usage(verb, &format!("--body-file {other:?}: the client sends the body on stdin"))),
                // mail's "question" kind requires filled "## Question"/"## Default" sections
                // (see cmd_blocked); a bare ask gets them from its own flags.
                (None, _) => format!("## Question\n{subject}\n\n## Default\n{default}\n"),
            };
            let mut args = vec![
                s("send"),
                s("operator"),
                s("--from"),
                format!("{} <{actor}@spira>", display_name(actor)),
                s("--subject"),
                subject.to_string(),
                s("--kind"),
                kind.to_string(),
                s("--default"),
                default.to_string(),
            ];
            if let Some(c) = get(&f, "--class") {
                args.extend([s("--class"), c.to_string()]);
            }
            let own = cited.as_deref() == Some(bound) && is_bead_id(bound);
            // mail reads BEAD_ID only as "a dwarf is asking"; a caller bound to nothing and
            // citing nothing is still a persona asking, so it is classified too.
            let classify_as = cited.clone().unwrap_or_else(|| bound.to_string());
            if let Some(b) = cited {
                args.extend([s("--bead"), b]);
            }
            if dry_run {
                args.push(s("--dry-run"));
                return Ok(vec![Step::Ask { args, stdin: body, classify_as }]);
            }
            let mut steps = vec![Step::Ask { args, stdin: body, classify_as }];
            // A bound session's question about its OWN bead is a wait on the operator: that
            // bead is held once the question is delivered (the mail goes first, so a hold
            // never stands without a question behind it). A question citing another bead —
            // the groomer's "Close <id>?" — is not this session's wait, and holds nothing.
            if kind == "question" && own {
                steps.push(Step::AskHold { bead: bound.to_string(), question: subject.to_string() });
            }
            Ok(steps)
        }
        tool => {
            let Some(&(_, program)) = TOOLS.iter().find(|(v, _)| *v == tool) else {
                return Err(usage(tool, "unknown verb"));
            };
            if tool == "census" && !(a.is_empty() || a == &[s("--with-suppressed")]) {
                return Err(usage(tool, "takes only --with-suppressed"));
            }
            if tool == "hel" && !matches!(a.iter().map(String::as_str).collect::<Vec<_>>().as_slice(), ["--dry-run"] | ["--dry-run", "--no-fetch"]) {
                return Err(usage(tool, "takes only --dry-run [--no-fetch]"));
            }
            if tool == "groom" && a.first().map(String::as_str) == Some("split-piece") {
                split_piece_args(&a[1..])?;
            }
            Ok(vec![Step::Tool { program, args: a.clone(), stdin: call.stdin.clone() }])
        }
    }
}

/// `work fence <class>` — `czar-fence.sh`'s answer, given here so the czar needs no path
/// into the release: `act` exits 0, `shadow` (the default, and the answer when the stage
/// cannot be read) exits 1, anything else exits 2. The stage is this installation's own
/// `SPIRA_CZAR_STAGE_<CLASS>`, resolved by the broker — never the caller's environment.
pub fn fence_answer(class: &str, stage: Option<&str>) -> (i32, String) {
    if class.is_empty() || !class.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
        return (2, format!("czar-fence: class {class:?} is not a czar class"));
    }
    let var = format!("SPIRA_CZAR_STAGE_{}", class.to_ascii_uppercase().replace('-', "_"));
    match stage.unwrap_or("shadow") {
        "act" => (0, format!("czar-fence: {class} is act")),
        "shadow" => (1, format!("czar-fence: {class} is shadow — mutation refused ({var}=act to enable)")),
        other => (2, format!("czar-fence: unknown stage {other} in {var} (shadow or act)")),
    }
}

fn cmd_fence(rest: &[String]) -> (i32, String) {
    let call = split_reserved(rest);
    let [class] = call.args.as_slice() else {
        return usage("fence", "want <class>");
    };
    let var = format!("SPIRA_CZAR_STAGE_{}", class.to_ascii_uppercase().replace('-', "_"));
    // One source of config (per Ryan 2026-10-05): the declared config-file value, never this
    // process's own environment.
    let stage = spira_config::process::cfg(&var).ok();
    fence_answer(class, stage.as_deref())
}

/// What a session may ask the forge for on its own bead's behalf: the round image build and
/// nothing else. The session holds no forge credentials; the service that does acts here, on
/// a candidate branch that names the bound bead, so no other branch can be pushed or deleted.
#[derive(Debug, PartialEq, Eq)]
pub enum ForgeOp {
    Verb { verb: &'static str, args: Vec<String> },
    Push { branch: String, tip: String },
    Delete { branch: String },
}

pub fn forge_op(bound: &str, args: &[String]) -> Result<ForgeOp, (i32, String)> {
    let candidate = |b: &str| -> Result<String, (i32, String)> {
        let prefix = format!("spira-round-candidate/{bound}-");
        let ok = b.starts_with(&prefix) && !b.contains("..") && b.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c));
        if ok { Ok(b.to_string()) } else { Err(usage("forge", &format!("{b:?} is not a round image candidate branch of {bound}"))) }
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["ami-result", b] => Ok(ForgeOp::Verb { verb: "ami-result", args: vec![candidate(b)?] }),
        ["ami-dispatch", b] => Ok(ForgeOp::Verb { verb: "ami-dispatch", args: vec![candidate(b)?] }),
        ["ami-dispatch", b, "force"] => Ok(ForgeOp::Verb { verb: "ami-dispatch", args: vec![candidate(b)?, "force".to_string()] }),
        ["ami-delete", b] => Ok(ForgeOp::Delete { branch: candidate(b)? }),
        ["ami-push", b, tip] if (7..=64).contains(&tip.len()) && tip.chars().all(|c| c.is_ascii_hexdigit()) => Ok(ForgeOp::Push { branch: candidate(b)?, tip: tip.to_string() }),
        ["ami-log", run, n] if run.chars().all(|c| c.is_ascii_digit()) && n.chars().all(|c| c.is_ascii_digit()) => Ok(ForgeOp::Verb { verb: "ami-log", args: vec![run.to_string(), n.to_string()] }),
        _ => Err(usage("forge", "want ami-result|ami-delete <candidate-branch>, ami-dispatch <candidate-branch> [force], ami-push <candidate-branch> <tip>, or ami-log <run-id> <lines>")),
    }
}

/// `work forge <sub> ...`: the round image build's forge reads and writes, run here because
/// the calling session has no forge credentials.
fn cmd_forge(bound: &str, rest: &[String]) -> (i32, String) {
    let call = split_reserved(rest);
    let op = match forge_op(bound, &call.args) {
        Ok(o) => o,
        Err(e) => return e,
    };
    let actor = call.actor.as_deref().unwrap_or("-");
    let repo = match forge_repo(bound) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("work forge: {e}")),
    };
    match op {
        ForgeOp::Verb { verb, args } => {
            let forge = match spira_config::process::cfg("SPIRA_FORGE") {
                Ok(f) => f,
                Err(e) => return (CANNOT_TELL, format!("work forge: {e}")),
            };
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let r = forge::client::run(&forge::client::route_of(std::path::Path::new(&forge)), verb, &repo, &args, None);
            if r.ok() {
                return (r.code, r.out);
            }
            let line = format!("work forge {verb}: refused for persona {actor}: {}", forge_said(&format!("{}{}", r.out, r.err), r.code));
            log_forge_refusal(&line);
            (r.code, line)
        }
        ForgeOp::Push { branch, tip } => forge_refspec(&repo, "ami-push", actor, &format!("{tip}:refs/heads/{branch}")),
        ForgeOp::Delete { branch } => forge_refspec(&repo, "ami-delete", actor, &format!(":refs/heads/{branch}")),
    }
}

fn forge_repo(bound: &str) -> Result<std::path::PathBuf, String> {
    let name = crate::bd::repo_label(bound)?.ok_or_else(|| format!("{bound} carries no repo: label"))?;
    crate::repo_config::registered_root(&name)
}

/// A network round trip outlasts the five-second store-call bound; the request's own
/// deadline (`TOOL_DEADLINE`) is the outer limit.
const FORGE_PUSH_SECS: &str = "120";

fn forge_refspec(repo: &std::path::Path, verb: &str, actor: &str, refspec: &str) -> (i32, String) {
    let remote = match spira_config::process::cfg("SPIRA_PUBLISH_REMOTE") {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("work forge: {e}")),
    };
    let (code, out) = forge_git(repo, &remote, refspec, verb, actor, FORGE_PUSH_SECS);
    if code != 0 {
        log_forge_refusal(&out);
    }
    (code, out)
}

/// A refusal names the verb, the persona and what git said, and says so when git said
/// nothing (a run cut off at its deadline).
fn forge_git(repo: &std::path::Path, remote: &str, refspec: &str, verb: &str, actor: &str, secs: &str) -> (i32, String) {
    let o = match state_store::phases::child("git", || crate::proc::bounded_for("git", secs).map_err(std::io::Error::other)?.args(["push", "--", remote, refspec]).current_dir(repo).output()) {
        Ok(o) => o,
        Err(e) => return (CANNOT_TELL, format!("work forge {verb}: persona {actor}: git: {e}")),
    };
    if o.status.success() {
        return (0, String::new());
    }
    let said = String::from_utf8_lossy(&o.stderr);
    let why = match o.status.code() {
        Some(124) if said.trim().is_empty() => format!("git did not finish within {secs}s and was stopped"),
        code => forge_said(&said, code.unwrap_or(-1)),
    };
    (REFUSED, format!("work forge {verb}: refused for persona {actor}: the forge remote {remote} failed: {why}"))
}

fn forge_said(said: &str, code: i32) -> String {
    match said.trim() {
        "" => format!("exit {code}, and the forge printed nothing"),
        t => t.to_string(),
    }
}

fn log_forge_refusal(line: &str) {
    if let Some(run) = spira_config::process::cfg("SPIRA_RUN").ok().filter(|r| !r.is_empty()) {
        append_refusal(std::path::Path::new(&run), line);
    }
}

fn append_refusal(run: &std::path::Path, line: &str) {
    crate::disk::log_line(&run.join("broker.log"), format!("{line}\n"));
}

/// `groom split-piece <original-id> "<title>" [flags]`: the tail reaches `bd create`, so it
/// is held to the create flags a piece needs — never `-C`/`--db` or anything else bd takes.
fn split_piece_args(a: &[String]) -> Result<(), (i32, String)> {
    let verb = "groom split-piece";
    bead_arg(verb, a, 0, "<original-id>")?;
    const VALUES: &[&str] = &["--title", "-d", "--description", "-p", "--priority", "-t", "--type", "-l", "--labels"];
    let (mut i, mut positional) = (1, 0);
    while i < a.len() {
        let t = a[i].as_str();
        if VALUES.contains(&t) {
            if i + 1 >= a.len() {
                return Err(usage(verb, &format!("{t} needs a value")));
            }
            i += 2;
        } else if t.starts_with('-') {
            return Err(usage(verb, &format!("{t:?} is not a create flag a split piece may pass")));
        } else {
            positional += 1;
            i += 1;
        }
    }
    if positional > 1 {
        return Err(usage(verb, "one positional <title> only"));
    }
    Ok(())
}

struct ListAsk<'a> {
    states: &'a [String],
    all: bool,
    filters: &'a [String],
    json: bool,
    full: bool,
    limit: Option<usize>,
}

fn lifecycle_list_args(states: &[String], all: bool) -> Vec<String> {
    let narrowing = if !states.is_empty() {
        vec![s("--state"), states.join(","), s("--cached")]
    } else if all {
        vec![s("--cached")]
    } else {
        vec![s("--live")]
    };
    std::iter::once(s("list")).chain(narrowing).chain(std::iter::once(s("--bare"))).collect()
}

/// `work list`: lifecycle state decides which beads, the SQL reader supplies what they say. A state
/// filter is the machine's; `--label`/`--title-contains` narrow by content in the reader, and the
/// two are intersected here so neither side's idea of "open" is consulted.
fn list_with_content(reader: &ReadFn, ask: ListAsk) -> (i32, String) {
    let ListAsk { states, all, filters, json, full, limit } = ask;
    let narrowing = lifecycle_list_args(states, all);
    let (rc, rows) = crate::plane::read(&narrowing);
    if rc != 0 {
        return (rc, rows);
    }
    let rows: Vec<serde_json::Value> = serde_json::from_str(&rows).unwrap_or_default();
    let keep = |st: &str| if !states.is_empty() { states.iter().any(|x| x == st) } else { all || !bead::BeadState::from_str(st).is_some_and(|b| b.is_terminal()) };
    let by_id: std::collections::HashMap<String, serde_json::Value> =
        rows.into_iter().filter(|r| keep(r["state"].as_str().unwrap_or(""))).filter_map(|r| Some((r["bead_id"].as_str()?.to_string(), r))).collect();
    let mut ids: Vec<&str> = by_id.keys().map(String::as_str).collect();
    ids.sort_unstable();
    let mut content: Vec<serde_json::Value> = Vec::new();
    for chunk in ids.chunks(LIST_ID_CHUNK) {
        if filters.is_empty() && !full {
            match crate::plan::fetch(chunk) {
                Ok(rows) => content.extend(rows),
                Err(e) => return (CANNOT_TELL, e),
            }
            continue;
        }
        match content_chunk(reader, chunk, filters, full) {
            Ok(rows) => content.extend(rows),
            Err(e) => return (CANNOT_TELL, e),
        }
    }
    let mut out = Vec::new();
    for mut c in content {
        let Some(lc) = c["id"].as_str().and_then(|id| by_id.get(id)).cloned() else { continue };
        c["lifecycle_state"] = lc["state"].clone();
        c["lifecycle_holds"] = lc["holds"].clone();
        if let Some(h) = lc.get("holder").filter(|h| !h.is_null()) {
            c["assignee"] = h.clone();
        }
        out.push(c);
    }
    out.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    if let Some(n) = limit {
        out.truncate(n);
    }
    if json {
        if full {
            return (0, serde_json::to_string_pretty(&out).unwrap_or_default());
        }
        let rows: Vec<serde_json::Value> = out.iter().map(project_row).collect();
        return (0, serde_json::to_string(&rows).unwrap_or_default());
    }
    let lines: Vec<String> =
        out.iter().map(|c| format!("{} {} {}", c["id"].as_str().unwrap_or("?"), c["lifecycle_state"].as_str().unwrap_or("?"), c["title"].as_str().unwrap_or(""))).collect();
    (0, lines.join("\n"))
}

/// `work open-items`: the state index names the waiting items, one read by id says what they
/// are. Neither read walks history, and bd's `status` is never consulted.
pub(crate) fn open_items(list: &dyn Fn(&[String]) -> (i32, String), content: &ContentFn, repo: &str, exclude: &[String], json: bool) -> (i32, String) {
    let (rc, rows) = list(&[s("list"), s("--state"), s("READY,REWORK"), s("--repo"), s(repo), s("--bare")]);
    if rc != 0 {
        return (rc, rows);
    }
    let rows: Vec<serde_json::Value> = match serde_json::from_str(&rows) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: the lifecycle list was not JSON: {e}")),
    };
    let waiting: Vec<(&str, &str)> =
        rows.iter().filter_map(|r| Some((r["bead_id"].as_str()?, r["state"].as_str()?))).filter(|(id, _)| !exclude.iter().any(|x| x == id)).collect();
    let ids: Vec<&str> = waiting.iter().map(|(id, _)| *id).collect();
    let found = match content(&ids) {
        Ok(f) => f,
        Err(e) => return (CANNOT_TELL, e),
    };
    let by_id: std::collections::HashMap<&str, &serde_json::Value> = found.iter().filter_map(|c| Some((c["id"].as_str()?, c))).collect();
    let mut items = Vec::with_capacity(waiting.len());
    for (id, state) in waiting {
        let Some(c) = by_id.get(id) else {
            return (CANNOT_TELL, format!("cannot tell: {id} is {state} and the content store has no row for it"));
        };
        let outcome = c["description"].as_str().and_then(|d| ::bead::section(d, "## Outcome"));
        items.push(OpenItem { id, state, priority: c["priority"].as_i64(), title: c["title"].as_str(), outcome });
    }
    if json {
        return (0, serde_json::to_string(&items).unwrap_or_default());
    }
    let lines: Vec<String> = items.iter().map(OpenItem::plain).collect();
    (0, lines.join("\n"))
}

pub(crate) type ContentFn<'a> = dyn Fn(&[&str]) -> Result<Vec<serde_json::Value>, String> + 'a;

#[derive(serde::Serialize)]
struct OpenItem<'a> {
    id: &'a str,
    state: &'a str,
    priority: Option<i64>,
    title: Option<&'a str>,
    outcome: Option<String>,
}

impl OpenItem<'_> {
    fn plain(&self) -> String {
        let priority = self.priority.map_or("?".to_string(), |p| p.to_string());
        let head = format!("{} {} P{priority} {}", self.id, self.state, self.title.unwrap_or(""));
        self.outcome.iter().flat_map(|o| o.lines()).fold(head, |out, line| out + "\n    " + line)
    }
}

/// One id chunk of `work list`'s content rows, read as SQL on the reader's held connection.
fn content_chunk(reader: &ReadFn, chunk: &[&str], filters: &[String], full: bool) -> Result<Vec<serde_json::Value>, String> {
    let mut args = vec![s("list"), s("--all"), s("--limit"), s("0"), s("--id"), chunk.join(",")];
    args.extend(filters.iter().cloned());
    if !full {
        args.push(s("--brief"));
    }
    let rows = reader(&args).map_err(|e| format!("cannot tell: {}", e.text()))?;
    serde_json::from_str(rows.trim()).map_err(|e| format!("cannot tell: the reader's list was not JSON: {e}"))
}

/// The content reader a read step asks: production's is the one held SQL connection.
type ReadFn<'a> = dyn Fn(&[String]) -> Result<String, content_sql::Failure> + 'a;

fn served_read(args: &[String]) -> Result<String, content_sql::Failure> {
    content_sql::served().map_err(content_sql::Failure::Store)?.read(args)
}

/// The steps that read bead content, answered by `reader`; no `bd` process, no fallback to one.
fn run_read(step: Step, reader: &ReadFn) -> (i32, String) {
    let cannot = |e: content_sql::Failure| (CANNOT_TELL, format!("cannot tell: {}", e.text()));
    match step {
        Step::Read { id, json: false } => cmd_show(&id),
        Step::Read { id, json: true } => reader(&[s("show"), id]).map_or_else(cannot, |t| (0, t)),
        Step::Search { query, limit, json } => {
            let mut args = vec![s("search"), query];
            if let Some(n) = limit {
                args.extend([s("--limit"), n]);
            }
            let rows = match reader(&args) {
                Ok(t) => t,
                Err(e) => return cannot(e),
            };
            if json {
                return (0, rows);
            }
            let Ok(found) = serde_json::from_str::<Vec<serde_json::Value>>(rows.trim()) else {
                return (CANNOT_TELL, "cannot tell: the reader's search was not JSON".to_string());
            };
            let lines: Vec<String> = found
                .iter()
                .map(|b| format!("{} P{} {}", b["id"].as_str().unwrap_or("?"), b["priority"].as_i64().map_or("?".to_string(), |p| p.to_string()), b["title"].as_str().unwrap_or("")))
                .collect();
            (0, lines.join("\n"))
        }
        Step::OpenItems { repo, exclude, json } => open_items(&crate::plane::read, &crate::plan::fetch_described, &repo, &exclude, json),
        Step::LcList { states, all, filters, json, full, limit } => list_with_content(reader, ListAsk { states: &states, all, filters: &filters, json, full, limit }),
        other => (CANNOT_TELL, format!("work: {other:?} is not a read")),
    }
}

/// What `work list --json` carries per bead unless `--full`: the fields a lister decides on.
/// Descriptions, notes and comments are `work read`'s; a whole-store dump of them is what
/// made the reply cost dolt time for every caller.
const LIST_PROJECTION: &[&str] = &["id", "title", "priority", "issue_type", "labels", "assignee", "lifecycle_state", "lifecycle_holds"];

fn project_row(row: &serde_json::Value) -> serde_json::Value {
    LIST_PROJECTION.iter().filter_map(|k| row.get(*k).map(|v| (k.to_string(), v.clone()))).collect::<serde_json::Map<_, _>>().into()
}

/// Ids per reader `list --id`: it is asked only for the beads the lifecycle kept, never the whole store.
const LIST_ID_CHUNK: usize = 400;

/// The kinds mail has a template for; a test holds this equal to `spira/mail/kinds`.
const MAIL_KINDS: &[&str] = &["alert", "decision", "event", "note", "question", "suit"];

/// An ask citing a bead no lifecycle row names is refused by name, never delivered.
fn uncited_bead(args: &[String]) -> Option<(i32, String)> {
    if args.iter().any(|a| a == "--dry-run") {
        return None;
    }
    let at = args.iter().position(|a| a == "--bead")?;
    let bead = args.get(at + 1)?;
    match crate::plane::bead(bead) {
        Ok(Some(_)) => None,
        Ok(None) => Some((REFUSED, format!("refused: work ask --bead {bead}: no such bead; nothing was sent"))),
        Err(e) => Some((CANNOT_TELL, format!("cannot tell: whether {bead} exists ({e:?}); nothing was sent"))),
    }
}

/// A `.live` marker in the archivist's state dir is written for the length of a sweep of a
/// session still being written to; a marker left by a crashed sweep refuses too, which is
/// the safe direction.
fn live_session_refusal(verb: &str, call: &Call, arc: &std::path::Path) -> Option<(i32, String)> {
    if verb != "file" || call.actor.as_deref() != Some("archivist") || !call.args.iter().any(|x| x == "--for") {
        return None;
    }
    let live = std::fs::read_dir(arc).ok()?.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".live"));
    live.then(|| {
        (
            REFUSED,
            "refused: work file --for: the session being swept is still live and will file its own work (law-archivist-does-not-file-live-work); \
             note the nearest bead with `work note-on`, or send `work ask`"
                .to_string(),
        )
    })
}

fn cmd_lane(bound: &str, verb: &str, rest: &[String]) -> (i32, String) {
    let call = split_reserved(rest);
    if let Ok(run) = spira_config::process::cfg("SPIRA_RUN") {
        if let Some(refusal) = live_session_refusal(verb, &call, &std::path::Path::new(&run).join("archivist")) {
            return refusal;
        }
    }
    let steps = match plan(verb, bound, &call) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let actor = call.actor.as_deref().unwrap_or("dwarf");
    let mut out = String::new();
    for step in steps {
        let (code, text) = match step {
            Step::Bd { args, stdin } => match crate::bd::run_stdin(&args, stdin.as_deref()) {
                Ok(t) => (0, t),
                Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
            },
            read @ (Step::Read { .. } | Step::Search { .. } | Step::LcList { .. } | Step::OpenItems { .. }) => run_read(read, &served_read),
            Step::Tool { program, args, stdin } => {
                // mail, by name; SPIRA_MAIL_SH is the harness-wide binary-override seam.
                let prog = if program == "mail" { std::env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| s("mail")) } else { s(program) };
                crate::bd::tool(&prog, &args, stdin.as_deref(), actor, TOOL_SECS)
            }
            Step::Ask { args, stdin, classify_as } => {
                if let Some(refusal) = uncited_bead(&args) {
                    return (refusal.0, out + &refusal.1);
                }
                let prog = std::env::var("SPIRA_MAIL_SH").unwrap_or_else(|_| s("mail"));
                crate::bd::tool_env_noisy(&prog, &args, Some(&stdin), actor, TOOL_SECS, &[("BEAD_ID", &classify_as)])
            }
            Step::AskHold { bead, question } => {
                let hold = BeadEventKind::Hold { kind: HoldKind::Ask, cause: HoldCause::OperatorQuestion, detail: Some(question) };
                match apply_ask_hold(&bead, actor, hold) {
                    (0, _) => (0, format!("question delivered; {bead} is held (ask) until the operator answers\n")),
                    (code, e) => (
                        code,
                        format!("the question WAS delivered (do not ask again), but the ask hold on {bead} was not applied: {e}"),
                    ),
                }
            }
            Step::Claim { bead } => {
                let kind = pilgrim_claim();
                match crate::plane::apply(&bead, actor, kind) {
                    (0, _) => (0, format!("{bead} is WORKING: branch spira/{bead} is yours to commit on and submit\n")),
                    refused => refused,
                }
            }
            Step::CreateRow { repo } => {
                let new_id = out.trim().to_string();
                match crate::plane::call(&[s("create-bead"), new_id.clone(), s("--ready"), s("--repo"), repo]) {
                    (0, _) => (0, String::new()),
                    (_, e) => (
                        CANNOT_TELL,
                        format!("LIFECYCLE: {new_id} was filed but has NO lifecycle row and cannot be claimed (do not file it again): {e}"),
                    ),
                }
            }
        };
        out.push_str(&text);
        if code != 0 {
            return (code, out);
        }
    }
    (0, out.trim_end_matches('\n').to_string())
}

#[cfg(test)]
mod tests {

    fn filing<'a>() -> super::Filing<'a> {
        super::Filing { verb: "file-followup", actor: "builder", title: "Fix x", persona: "builder", repo: "spira", bound: "sp-b0", after_landing: false, prerequisite: false, body: Some("## Build\nthe thing\n"), priority: Some(3), statement: None }
    }

    /// The priority the injected filer receives when `args` are filed, and the note recorded after.
    fn filed_with(verb: &'static str, args: &[&str]) -> Result<(Option<i64>, Vec<String>), String> {
        let args: Vec<String> = args.iter().map(|x| x.to_string()).collect();
        let rank = super::followup_rank(verb, &args)?;
        let f = super::Filing { verb, priority: rank.as_ref().map(|r| r.priority), statement: rank.as_ref().and_then(|r| r.statement.as_deref()), ..filing() };
        let seen = std::cell::Cell::new(None);
        let mut notes = vec![];
        let (code, id) = super::file_and_bind(&f, |f, _, _| { seen.set(f.priority); Ok("sp-new".to_string()) }, |on, text| { notes.push(format!("{on}: {text}")); Ok(()) }, |_| (0, String::new()), |_, _| Ok(()), |_, _| Ok(()));
        assert_eq!(code, 0, "{id}");
        Ok((seen.get(), notes))
    }

    #[test_id(t44nj10)]
    #[test]
    fn a_followup_with_no_statement_files_at_p3_and_p4_needs_none() {
        assert_eq!(filed_with("file-followup", &["t"]), Ok((Some(3), vec![])));
        assert_eq!(filed_with("file-followup", &["t", "--priority", "3"]), Ok((Some(3), vec![])));
        assert_eq!(filed_with("file-followup", &["t", "--priority", "4"]), Ok((Some(4), vec![])));
    }

    #[test_id(tlwok9q)]
    #[test]
    fn a_statement_without_a_priority_files_at_p2_and_is_noted_on_the_new_item() {
        let noted = |s: &str| vec![format!("sp-new: Filed at P2 by builder from sp-b0: {s}")];
        assert_eq!(filed_with("file-followup", &["t", "--blocks", "blocks sp-b9, from landing"]), Ok((Some(2), noted("blocks: blocks sp-b9, from landing"))));
        assert_eq!(filed_with("file-followup", &["t", "--defect", "login fails"]), Ok((Some(2), noted("defect: login fails"))));
    }

    #[test_id(t5kv179)]
    #[test]
    fn a_priority_above_p3_without_a_statement_is_refused_naming_both_flags_and_the_default() {
        for p in ["0", "1", "P2"] {
            let why = filed_with("file-followup", &["t", "--priority", p]).unwrap_err();
            assert!(why.contains("--blocks") && why.contains("--defect") && why.contains("files at P3") && why.contains("nothing was filed"), "{why}");
        }
        let (code, out) = super::cmd_file("sp-b0", &["t".to_string(), "--priority".into(), "1".into()], "file-followup");
        assert_eq!(code, super::REFUSED, "{out}");
    }

    #[test_id(tscfq9w)]
    #[test]
    fn a_blocks_statement_naming_no_bead_is_refused_and_a_flag_needs_a_value() {
        let why = filed_with("file-followup", &["t", "--blocks", "the landing pass"]).unwrap_err();
        assert!(why.contains("names no bead id"), "{why}");
        for args in [["t", "--defect"], ["t", "--blocks"], ["t", "--priority"]] {
            assert!(filed_with("file-followup", &args).unwrap_err().contains("needs a value"), "{args:?}");
        }
        assert!(filed_with("file-followup", &["t", "--priority", "9"]).is_err());
    }

    #[test_id(tlxnxhu)]
    #[test]
    fn a_split_takes_no_priority_and_none_of_the_followup_rules() {
        assert_eq!(filed_with("split", &["t"]), Ok((None, vec![])));
        assert_eq!(filed_with("split", &["t", "--priority", "1", "--blocks", "nothing"]), Ok((None, vec![])));
    }

    #[test_id(tyb9457)]
    #[test]
    fn a_followup_whose_binding_fails_after_filing_still_carries_its_statement() {
        let f = super::Filing { actor: "pilgrim", priority: Some(1), statement: Some("defect: x"), ..filing() };
        for (registered, edge) in [((2, "down".to_string()), Ok(())), ((0, String::new()), Err("no".to_string()))] {
            let mut notes = vec![];
            let (code, out) = super::file_and_bind(&f, |_, _, _| Ok("sp-new".to_string()), |on, text| { notes.push(format!("{on}: {text}")); Ok(()) }, |_| registered, |_, _| Ok(()), |_, _| edge);
            assert_eq!(code, super::CANNOT_TELL, "{out}");
            assert_eq!(notes, ["sp-new: Filed at P1 by pilgrim from sp-b0: defect: x"], "{out}");
        }
    }

    #[test_id(tt65k9o)]
    #[test]
    fn a_statement_that_cannot_be_noted_still_binds_the_item_and_says_not_to_file_again() {
        let f = super::Filing { priority: Some(1), statement: Some("defect: x"), ..filing() };
        let mut registered = false;
        let (code, out) = super::file_and_bind(&f, |_, _, _| Ok("sp-new".to_string()), |_, _| Err("boom".into()), |_| { registered = true; (0, String::new()) }, |_, _| Ok(()), |_, _| Ok(()));
        assert_eq!(code, super::CANNOT_TELL);
        assert!(registered && out.starts_with("sp-new\n") && out.contains("do not file it again") && out.contains("boom"), "{out}");
    }

    #[test_id(t8eduz9)]
    #[test]
    fn a_followup_in_a_skuld_lane_files_a_skuld_request_and_waits_on_it() {
        let filed = std::cell::RefCell::new(Vec::new());
        let edges = std::cell::RefCell::new(Vec::new());
        let (code, out) = super::review_followup(
            &filing(),
            "sp-f1",
            |t, b| {
                filed.borrow_mut().push((t.to_string(), b.to_string()));
                Ok("sp-r1".to_string())
            },
            |blocked, blocker| {
                edges.borrow_mut().push((blocked.to_string(), blocker.to_string()));
                Ok(())
            },
        );
        assert_eq!(code, 0, "{out}");
        assert_eq!(out.lines().next(), Some("sp-f1"));
        assert!(out.lines().nth(1).unwrap().contains("sp-r1"));
        assert!(filed.borrow()[0].1.contains("### Draft: Fix x\n\n"));
        assert!(filed.borrow()[0].1.contains("work open-items --repo spira --exclude sp-f1\n"));
        assert_eq!(*edges.borrow(), vec![("sp-f1".to_string(), "sp-r1".to_string())]);
    }

    #[test_id(tuc3jn0)]
    #[test]
    fn a_failed_review_filing_names_the_followup_and_says_not_to_file_it_again() {
        let (code, out) = super::review_followup(&filing(), "sp-f1", |_, _| Err("boom".into()), |_, _| Ok(()));
        assert_eq!(code, super::CANNOT_TELL);
        assert!(out.contains("sp-f1") && out.contains("do not file it again"), "{out}");
    }

    #[test_id(tp9c9mm)]
    #[test]
    fn a_lane_without_the_skuld_key_files_one_bead() {
        let tmp = testkit::TempDir::new("lane-home");
        let home = tmp.path().to_path_buf();
        let runes = spira_config::runes::runes_dir(&home);
        std::fs::create_dir_all(&runes).unwrap();
        std::fs::write(runes.join("plain.rune"), "RUNE_LABELS=x\n").unwrap();
        std::fs::write(runes.join("strict.rune"), "RUNE_REQUIRES_SKULD=1\n").unwrap();
        assert!(!super::lane_requires_skuld_in(&home, "plain"));
        assert!(super::lane_requires_skuld_in(&home, "strict"));
    }

    fn deps() -> Vec<(String, String)> {
        vec![("sp-f1".to_string(), "Fix x".to_string())]
    }

    #[test_id(t3yn5fy)]
    #[test]
    fn done_on_a_review_refuses_with_the_exit_when_no_draft_matches() {
        let (code, out) = super::apply_reviews("sp-r1", None, &deps(), "### Draft: Other\n\nbody\n", no_item, |_, _| panic!("no amend"), |_, _| panic!("no note"), no_supersede).unwrap();
        assert_eq!(code, super::REFUSED);
        assert!(out.contains("### Draft: Fix x") && out.contains("### Amend <survivor-id>: Fix x") && out.contains("work note -"), "{out}");
    }

    #[test_id(tx6bpmv)]
    #[test]
    fn done_on_a_review_amends_the_dependent_with_the_draft_and_notes_its_open_line() {
        let amended = std::cell::RefCell::new(Vec::new());
        let noted = std::cell::RefCell::new(Vec::new());
        let notes = "### Draft: Fix x\n\n## Build\nbetter\nOpen: which?; default: a\n";
        let out = super::apply_reviews(
            "sp-r1",
            None,
            &deps(),
            notes,
            no_item,
            |id, b| {
                amended.borrow_mut().push((id.to_string(), b.to_string()));
                Ok(())
            },
            |id, t| {
                noted.borrow_mut().push((id.to_string(), t.to_string()));
                Ok(())
            },
            no_supersede,
        );
        assert!(out.is_none());
        assert_eq!(*amended.borrow(), vec![("sp-f1".to_string(), "Drafted by Skuld: sp-r1\n\n## Build\nbetter\n".to_string())]);
        assert_eq!(*noted.borrow(), vec![("sp-f1".to_string(), "Open: which?; default: a".to_string())]);
    }

    #[test_id(tv5jvx6)]
    #[test]
    fn done_on_a_review_whose_amend_fails_leaves_the_request_working() {
        let (code, out) = super::apply_reviews("sp-r1", None, &deps(), "### Draft: Fix x\n\nb\n", no_item, |_, _| Err("no".into()), |_, _| Ok(()), no_supersede).unwrap();
        assert_eq!(code, super::CANNOT_TELL);
        assert!(out.contains("WORKING"), "{out}");
    }

    fn no_item(id: &str) -> Result<super::Item, String> {
        panic!("a draft reads no item, not {id}")
    }

    fn no_supersede(_: &str, _: &str) -> Result<(), String> {
        panic!("nothing is superseded")
    }

    /// A plane where the follow-up and the request are spira's, and `sp-x` is `state` in `repo`.
    fn plane(state: bead::BeadState, repo: &'static str) -> impl Fn(&str) -> Result<super::Item, String> {
        move |id| {
            let (state, repo) = if id == "sp-x" { (state, repo) } else { (bead::BeadState::Ready, "spira") };
            Ok(super::Item { state: Some(state), superseded_by: None, repo: Some(repo.to_string()), title: format!("title of {id}"), waits_on: Vec::new() })
        }
    }

    const AMEND: &str = "### Amend sp-x: Fix x\n\nnew evidence\n";

    type Effect = (&'static str, String, String);

    fn amend_review(item: impl Fn(&str) -> Result<super::Item, String>, supersede: Result<(), String>) -> (Option<(i32, String)>, Vec<Effect>) {
        let effects = std::cell::RefCell::new(Vec::new());
        let log = |what, a: &str, b: &str| effects.borrow_mut().push((what, a.to_string(), b.to_string()));
        let filed = ("builder".to_string(), "sp-b0".to_string());
        let out = super::apply_reviews(
            "sp-r1",
            Some(&filed),
            &deps(),
            AMEND,
            item,
            |_, _| panic!("an amendment replaces no body"),
            |id, t| {
                log("note", id, t);
                Ok(())
            },
            |dep, survivor| {
                log("supersede", dep, survivor);
                supersede.clone()
            },
        );
        (out, effects.into_inner())
    }

    fn effect(what: &'static str, a: &str, b: &str) -> Effect {
        (what, a.to_string(), b.to_string())
    }

    #[test_id(tfkfl8x)]
    #[test]
    fn done_on_a_review_that_amends_notes_the_survivor_supersedes_the_followup_and_tells_the_filer() {
        for state in [bead::BeadState::Ready, bead::BeadState::Rework] {
            let (out, effects) = amend_review(plane(state, "spira"), Ok(()));
            assert_eq!(out, None);
            assert_eq!(
                effects,
                [
                    effect("note", "sp-x", "Amended by Skuld: sp-r1, from sp-f1 filed by builder from sp-b0\n\nnew evidence"),
                    effect("supersede", "sp-f1", "sp-x"),
                    effect("note", "sp-b0", "sp-f1 was folded into sp-x (\"title of sp-x\")"),
                ]
            );
        }
    }

    #[test_id(t0gwg4h)]
    #[test]
    fn an_amendment_of_an_item_that_is_not_waiting_in_that_repo_is_refused_and_changes_nothing() {
        for (state, repo) in [(bead::BeadState::Working, "spira"), (bead::BeadState::Landed, "spira"), (bead::BeadState::Ready, "deck")] {
            let (out, effects) = amend_review(plane(state, repo), Ok(()));
            let (code, out) = out.unwrap();
            assert_eq!(code, super::REFUSED);
            assert_eq!(out, format!("refused: work done: sp-x is {}, not a waiting item of spira; draft sp-f1 instead or name a waiting item", state.as_str()));
            assert!(effects.is_empty(), "{effects:?}");
        }
    }

    /// A plane after `sp-f1` ended as `state`, by `successor`, with `sp-x` claimed since.
    fn ended(state: bead::BeadState, successor: Option<&'static str>, survivor: bead::BeadState) -> impl Fn(&str) -> Result<super::Item, String> {
        move |id| {
            let mut item = plane(survivor, "spira")(id)?;
            if id == "sp-f1" {
                item.state = Some(state);
                item.superseded_by = successor.map(str::to_string);
            }
            Ok(item)
        }
    }

    #[test_id(teutlnf)]
    #[test]
    fn a_repeat_after_the_filers_note_failed_tells_the_filer_even_when_the_survivor_has_moved_on() {
        let filed = ("builder".to_string(), "sp-b0".to_string());
        let (code, out) = super::apply_reviews("sp-r1", Some(&filed), &deps(), AMEND, plane(bead::BeadState::Ready, "spira"), |_, _| panic!("no replace"), |id, _| if id == "sp-b0" { Err("no".into()) } else { Ok(()) }, |_, _| Ok(())).unwrap();
        assert_eq!(code, super::CANNOT_TELL);
        assert!(out.contains("sp-b0 was not told") && out.contains("run work done again"), "{out}");
        let (out, effects) = amend_review(ended(bead::BeadState::Superseded, Some("sp-x"), bead::BeadState::Working), Ok(()));
        assert_eq!(out, None);
        assert_eq!(effects.iter().map(|e| (e.0, e.1.as_str())).collect::<Vec<_>>(), [("note", "sp-x"), ("supersede", "sp-f1"), ("note", "sp-b0")]);
    }

    #[test_id(thoy7gy)]
    #[test]
    fn a_followup_that_ended_another_way_still_has_its_evidence_moved_and_is_not_retired() {
        for (state, successor) in [(bead::BeadState::Dropped, None), (bead::BeadState::Superseded, Some("sp-other"))] {
            let (out, effects) = amend_review(ended(state, successor, bead::BeadState::Ready), Ok(()));
            assert_eq!(out, None);
            assert_eq!(effects.iter().map(|e| (e.0, e.1.as_str())).collect::<Vec<_>>(), [("note", "sp-x"), ("note", "sp-b0")]);
            assert_eq!(effects[1].2, "sp-x (\"title of sp-x\") took the evidence of sp-f1");
        }
        let (out, effects) = amend_review(ended(bead::BeadState::Dropped, None, bead::BeadState::Landed), Ok(()));
        assert_eq!(out.unwrap().0, super::REFUSED);
        assert!(effects.is_empty(), "{effects:?}");
    }

    #[test_id(tncpebf)]
    #[test]
    fn an_amendment_of_a_followup_with_an_order_of_its_own_is_refused() {
        let ordered = |id: &str| {
            let mut item = plane(bead::BeadState::Ready, "spira")(id)?;
            if id == "sp-f1" {
                item.waits_on = vec!["sp-r1".into(), "sp-x".into(), "sp-b0".into()];
            }
            Ok(item)
        };
        let (out, effects) = amend_review(ordered, Ok(()));
        let (code, out) = out.unwrap();
        assert_eq!(code, super::REFUSED);
        assert!(out.contains("sp-f1 waits on sp-b0,") && out.contains("draft sp-f1 instead"), "{out}");
        assert!(effects.is_empty(), "{effects:?}");
    }

    #[test_id(tlkgx8o)]
    #[test]
    fn an_amendment_that_would_move_no_evidence_is_refused_at_done() {
        let (code, out) = super::skuld_draft_notes_refusal("sp-r1", "### Amend sp-x: Fix x\nno blank line\n").unwrap();
        assert_eq!(code, super::REFUSED);
        assert!(out.contains("Fix x") && out.contains("work note"), "{out}");
    }

    #[test_id(tfzotau)]
    #[test]
    fn an_amendment_naming_the_followup_it_would_retire_is_refused() {
        let (code, out) = super::apply_reviews("sp-r1", None, &deps(), "### Amend sp-f1: Fix x\n\ne\n", no_item, |_, _| panic!("no replace"), |_, _| panic!("no note"), no_supersede).unwrap();
        assert_eq!(code, super::REFUSED, "{out}");
    }

    #[test_id(tydp651)]
    #[test]
    fn a_failed_supersede_leaves_the_request_working_and_says_the_note_was_written() {
        let (out, effects) = amend_review(plane(bead::BeadState::Ready, "spira"), Err("no".into()));
        let (code, out) = out.unwrap();
        assert_eq!(code, super::CANNOT_TELL);
        assert!(out.contains("WORKING") && out.contains("the evidence note was written on sp-x"), "{out}");
        assert_eq!(effects.iter().map(|e| e.0).collect::<Vec<_>>(), ["note", "supersede"]);
    }

    #[test_id(tocrefe)]
    #[test]
    fn a_request_with_nothing_to_retire_notes_each_amendment_on_its_item() {
        let noted = std::cell::RefCell::new(Vec::new());
        let note = |id: &str, t: &str| {
            noted.borrow_mut().push((id.to_string(), t.to_string()));
            Ok(())
        };
        assert_eq!(super::apply_amends("sp-r1", AMEND, plane(bead::BeadState::Ready, "spira"), note), None);
        assert_eq!(*noted.borrow(), [("sp-x".to_string(), "Amended by Skuld: sp-r1\n\nnew evidence".to_string())]);
        assert_eq!(super::apply_amends("sp-r1", "### Draft: New\n\nbody\n", no_item, |_, _| panic!("a draft files nothing")), None);
        assert_eq!(super::skuld_draft_notes_refusal("sp-r1", AMEND), None);
    }

    #[test_id(ta56fwl)]
    #[test]
    fn a_request_amending_an_item_that_is_not_waiting_notes_none_of_them() {
        let notes = "### Amend sp-y: One\n\ne\n### Amend sp-x: Two\n\ne\n";
        let (code, out) = super::apply_amends("sp-r1", notes, plane(bead::BeadState::Landed, "spira"), |_, _| panic!("no note")).unwrap();
        assert_eq!(code, super::REFUSED);
        assert!(out.contains("sp-x is LANDED, not a waiting item of spira"), "{out}");
    }
    fn queued_json(id: &str) -> String {
        let r = crate::control::work_requests::WorkRequest { id: id.into(), bead: "sp-x".into(), actor: "a".into(), argv: vec![], stdin: None, bound_ms: 1, state: "queued".into(), code: None, output: String::new(), truncated: false, queued_at: 0, started_at: None, finished_at: None };
        serde_json::to_string(&vec![r]).unwrap()
    }

    #[test_id(tpnzgqu)]
    #[test]
    fn the_runner_keeps_polling_after_a_failed_list_and_takes_the_request_listed_next() {
        let lists = std::cell::Cell::new(0);
        let call = |args: &[&str]| -> (i32, String) {
            match args[1] {
                "list" => {
                    lists.set(lists.get() + 1);
                    if lists.get() == 1 { (2, "cannot tell: Backend(\"read ended: held past max_read_ms (100 ms)\")".into()) } else { (0, queued_json("wr-1")) }
                }
                _ => (0, String::new()),
            }
        };
        let started = std::cell::RefCell::new(Vec::new());
        let mut results = Vec::new();
        for _ in 0..2 {
            results.push(super::poll_once(&call, |id| started.borrow_mut().push(id)));
        }
        let err = results[0].clone().unwrap_err();
        assert!(err.contains("code 2") && err.contains("max_read_ms"), "{err}");
        assert_eq!(results[1], Ok(1));
        assert_eq!(*started.borrow(), vec!["wr-1".to_string()]);
    }

    #[test_id(t0bcp04)]
    #[test]
    fn a_failure_is_logged_once_until_it_changes_and_recovery_is_logged_once() {
        let mut last = None;
        let down = Err("code 2: down".to_string());
        assert_eq!(super::poll_report(&mut last, &down).as_deref(), Some("code 2: down"));
        assert_eq!(super::poll_report(&mut last, &down), None);
        assert_eq!(super::poll_report(&mut last, &Err("code 3: other".to_string())).as_deref(), Some("code 3: other"));
        assert_eq!(super::poll_report(&mut last, &Ok(0)).as_deref(), Some("the queue answers again"));
        assert_eq!(super::poll_report(&mut last, &Ok(1)), None);
    }

    #[test_id(t1bq62h)]
    #[test]
    fn an_unparseable_list_is_a_failed_poll_not_an_empty_queue() {
        let call = |_: &[&str]| (0, "not json".to_string());
        let err = super::poll_once(&call, |_| {}).unwrap_err();
        assert!(err.contains("not the request list") && err.contains("not json"), "{err}");
    }

    #[test_id(toxrvud)]
    #[test]
    fn a_failed_take_is_skipped() {
        let call = |args: &[&str]| if args[1] == "list" { (0, queued_json("wr-2")) } else { (1, "not queued".into()) };
        assert_eq!(super::poll_once(&call, |_| panic!("not ours")), Ok(0));
    }
    #[test_id(tz7tibq)]
    #[test]
    fn every_verb_that_starts_a_program_is_recorded_and_the_reads_stay_in_the_service() {
        for verb in ["submit", "ack", "file-followup", "split", "blocked", "superseded-by", "groom", "incident", "sop", "census", "queue", "landing-pass", "strand", "hel", "mender", "forge", "file", "ask"] {
            assert!(super::exec_verb(verb), "{verb} starts a program, so the scheduler runs it");
        }
        for verb in ["show", "note", "done", "logs", "cite-check", "read", "list", "search", "open-items", "fence", "note-on", "label-add", "label-remove", "no-such-verb"] {
            assert!(!super::exec_verb(verb), "{verb} starts no program in the service");
        }
        assert!(super::exec_bound_ms("groom") >= super::exec_bound_ms("submit") && super::exec_bound_ms("submit") == super::TOOL_SECS * 1000);
    }

    fn cite_check_of(msg: &str, held: &[&str], store_down: bool) -> (i32, String) {
        let held: Vec<String> = held.iter().map(|h| h.to_string()).collect();
        let mut known = |ids: &[String]| if store_down { Err("down".to_string()) } else { Ok(ids.iter().filter(|i| held.contains(i)).cloned().collect()) };
        super::cmd_cite_check("sp-me1", &["-".into(), "--stdin".into(), msg.into(), "--actor".into(), "builder".into()], &mut known)
    }

    #[test_id(t5mljbr)]
    #[test]
    fn cite_check_refuses_a_phantom_naming_the_id_and_the_fix() {
        let (code, out) = cite_check_of("sp-me1: x\n\nsee sp-zzzzzz", &["sp-me1"], false);
        assert_eq!(code, REFUSED);
        assert!(out.contains("this message cites sp-zzzzzz") && out.contains("reword"), "{out}");
    }

    #[test_id(to3z58j)]
    #[test]
    fn cite_check_passes_fixture_tokens_and_real_beads() {
        assert_eq!(cite_check_of("see sp-a and sp-b", &[], false).0, 0);
        assert_eq!(cite_check_of("sp-me1: x", &["sp-me1"], false).0, 0);
    }

    #[test_id(tybotom)]
    #[test]
    fn every_persona_may_run_cite_check() {
        assert!(permitted("cite-check", Some("builder"), false).is_ok());
    }

    #[test_id(tlfc4oz)]
    #[test]
    fn cite_check_cannot_tell_when_the_store_errs() {
        assert_eq!(cite_check_of("sp-me1: x", &[], true).0, CANNOT_TELL);
    }

    fn send_tip(repo: &std::path::Path, remote: &std::path::Path, secs: &str) -> (i32, String) {
        let tip = testkit::git(repo, &["rev-parse", "HEAD"]);
        super::forge_git(repo, remote.to_str().unwrap(), &format!("{}:refs/heads/spira-round-candidate/sp-x-1", tip.trim()), "ami-push", "builder", secs)
    }

    fn forge_fixture(tag: &str) -> (testkit::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = testkit::TempDir::new(tag);
        let (repo, remote) = (dir.path().join("repo"), dir.path().join("forge.git"));
        std::fs::create_dir_all(&repo).unwrap();
        testkit::git(dir.path(), &["init", "--bare", "-q", remote.to_str().unwrap()]);
        testkit::git(&repo, &["init", "-q"]);
        testkit::git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-q", "-m", "x"]);
        (dir, repo, remote)
    }

    fn forge_hook(remote: &std::path::Path, body: &str) {
        testkit::write_exe(remote.join("hooks/pre-receive"), &format!("#!/bin/sh\n{body}\n"));
    }

    #[test_id(tnq38xy)]
    #[test]
    fn a_forge_refusal_prints_the_verb_the_persona_and_the_forges_message() {
        let (_dir, repo, remote) = forge_fixture("forge-refuse");
        forge_hook(&remote, "echo 'stub forge: protected branch' >&2\nexit 1");
        let (code, out) = send_tip(&repo, &remote, "20");
        assert_eq!(code, super::REFUSED);
        for want in ["ami-push", "builder", "stub forge: protected branch"] {
            assert!(out.contains(want), "{want} missing from {out:?}");
        }
    }

    #[test_id(t7zelhy)]
    #[test]
    fn a_forge_call_cut_off_at_its_deadline_says_so_instead_of_printing_nothing() {
        let (_dir, repo, remote) = forge_fixture("forge-slow");
        forge_hook(&remote, "sleep 30");
        let (code, out) = send_tip(&repo, &remote, "0.3");
        assert_eq!(code, super::REFUSED);
        assert!(out.contains("did not finish within 0.3s"), "{out:?}");
    }

    #[test_id(td2p8wm)]
    #[test]
    fn a_forge_call_gets_more_than_a_store_call_and_the_request_deadline_covers_it() {
        let secs: f64 = super::FORGE_PUSH_SECS.parse().unwrap();
        assert!(secs > spira_config::bounded::CALL_TIMEOUT_SECS.parse::<f64>().unwrap());
        assert!(secs < crate::serve::TOOL_DEADLINE.as_secs_f64());
        assert!(super::brokers_tool("forge"));
    }

    #[test_id(tagyfh8)]
    #[test]
    fn a_forge_refusal_is_appended_to_the_broker_log_under_the_run_dir() {
        let dir = testkit::TempDir::new("forge-log");
        super::append_refusal(dir.path(), "work forge ami-push: refused for persona builder: x");
        super::append_refusal(dir.path(), "second");
        let log = std::fs::read_to_string(dir.path().join("broker.log")).unwrap();
        assert_eq!(log, "work forge ami-push: refused for persona builder: x\nsecond\n");
    }

    #[test_id(t9o3h3s)]
    #[test]
    fn a_builder_may_ask_the_forge_about_its_own_round_image_candidate_and_no_other_branch() {
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let own = "spira-round-candidate/sp-95s21z.2-0123456789ab-7";
        assert_eq!(super::forge_op("sp-95s21z.2", &v(&["ami-result", own])).unwrap(), super::ForgeOp::Verb { verb: "ami-result", args: v(&[own]) });
        assert_eq!(super::forge_op("sp-95s21z.2", &v(&["ami-push", own, "0123456789abcdef"])).unwrap(), super::ForgeOp::Push { branch: own.into(), tip: "0123456789abcdef".into() });
        for refused in [
            v(&["ami-delete", "main"]),
            v(&["ami-push", "main", "0123456789abcdef"]),
            v(&["ami-delete", "spira-round-candidate/sp-other-0123456789ab-7"]),
            v(&["ami-delete", "spira-round-candidate/sp-95s21z.2-../main"]),
            v(&["ami-push", own, "--force"]),
            v(&["ami-runs"]),
        ] {
            assert!(super::forge_op("sp-95s21z.2", &refused).is_err(), "{refused:?}");
        }
        let op = super::op_key("forge", &v(&["ami-result", own]));
        assert!(super::permitted(&op, Some("builder"), false).is_ok());
        assert!(super::permitted(&op, Some("groomer"), false).is_err());
    }

    #[test_id(tc4elbj)]
    #[test]
    fn a_builder_may_force_its_candidates_image_build_and_pass_no_other_dispatch_word() {
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let own = "spira-round-candidate/sp-95s21z.2-0123456789ab-7";
        assert_eq!(super::forge_op("sp-95s21z.2", &v(&["ami-dispatch", own, "force"])).unwrap(), super::ForgeOp::Verb { verb: "ami-dispatch", args: v(&[own, "force"]) });
        let (_, said) = super::forge_op("sp-95s21z.2", &v(&["ami-dispatch", own, "other"])).unwrap_err();
        assert!(said.contains("ami-dispatch <candidate-branch> [force]"), "{said}");
        assert!(super::forge_op("sp-95s21z.2", &v(&["ami-dispatch", "main", "force"])).is_err());
    }

    #[test_id(trazvhp)]
    #[test]
    fn a_pilgrims_follow_up_is_filed_into_the_builder_partition_but_a_split_stays_put() {
        let runes = spira_config::resolve::locate_home_for_process().unwrap().join("runes");
        let at = |p: &str| spira_config::runes::followup_persona_in(&runes, p).unwrap();
        assert_eq!(at("pilgrim"), "builder");
        assert_eq!(at("builder"), "builder");
        assert_eq!(at("archivist"), "archivist");
        assert_eq!(super::filed_as("split", "pilgrim").unwrap(), "pilgrim");
    }

    #[test_id(t7mz1cy)]
    #[test]
    fn a_prerequisite_split_is_filed_unparented_and_the_bound_bead_waits_on_it() {
        let tie = super::binding(&super::Filing { verb: "split", prerequisite: true, ..filing() });
        assert_eq!(tie, super::Binding { parent: None, split_label: false, edge: Some((super::End::Bound, super::End::New)) });
    }

    #[test_id(t7mbnih)]
    #[test]
    fn a_plain_split_is_a_parented_split_child_with_no_edge() {
        let tie = super::binding(&super::Filing { verb: "split", ..filing() });
        assert_eq!(tie, super::Binding { parent: Some("sp-b0"), split_label: true, edge: None });
    }

    #[test_id(tdun5mw)]
    #[test]
    fn a_pilgrims_follow_up_is_bound_unparented_with_the_bound_bead_waiting() {
        let tie = super::binding(&super::Filing { actor: "pilgrim", ..filing() });
        assert_eq!(tie, super::Binding { parent: None, split_label: false, edge: Some((super::End::Bound, super::End::New)) });
    }

    #[test_id(ta6ed0r)]
    #[test]
    fn an_after_landing_filing_is_unparented_and_waits_on_the_bound_bead() {
        for verb in ["split", "file-followup"] {
            let tie = super::binding(&super::Filing { verb, after_landing: true, ..filing() });
            assert_eq!(tie, super::Binding { parent: None, split_label: false, edge: Some((super::End::New, super::End::Bound)) }, "{verb}");
        }
    }

    /// Runs `file_and_bind` over a store that records each call; `edge` is what adding it answers.
    fn bound_calls(f: &super::Filing, edge: Result<(), String>) -> ((i32, String), Vec<String>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let said = |c: String| calls.borrow_mut().push(c);
        let out = super::file_and_bind(
            f,
            |_, parent, _| {
                said(format!("file parent={parent:?}"));
                Ok("sp-new".to_string())
            },
            |_, _| Ok(()),
            |_| (0, String::new()),
            |id, label| {
                said(format!("label {id} {label}"));
                Ok(())
            },
            |blocked, blocker| {
                said(format!("block {blocked} on {blocker}"));
                edge
            },
        );
        (out, calls.into_inner())
    }

    #[test_id(tdx8kfm)]
    #[test]
    fn a_filing_makes_exactly_the_store_calls_its_binding_names() {
        let split = || super::Filing { verb: "split", ..filing() };
        let (out, calls) = bound_calls(&super::Filing { prerequisite: true, ..split() }, Ok(()));
        assert_eq!(out, (0, "sp-new".to_string()));
        assert_eq!(calls, ["file parent=None", "block sp-b0 on sp-new"]);
        let (_, calls) = bound_calls(&split(), Ok(()));
        assert_eq!(calls, ["file parent=Some(\"sp-b0\")", "label sp-new split-child"]);
        let (_, calls) = bound_calls(&super::Filing { after_landing: true, ..split() }, Ok(()));
        assert_eq!(calls, ["file parent=None", "block sp-new on sp-b0"]);
    }

    #[test_id(t69k3ex)]
    #[test]
    fn a_refused_edge_says_which_bead_could_not_be_made_to_wait() {
        let split = || super::Filing { verb: "split", ..filing() };
        let ((code, out), _) = bound_calls(&super::Filing { prerequisite: true, ..split() }, Err("no".into()));
        assert_eq!(code, super::CANNOT_TELL);
        assert!(out.contains("sp-new was filed but sp-b0 could not be made to wait on it (do not file it again; add the dependency): no"), "{out}");
        let ((_, out), _) = bound_calls(&super::Filing { after_landing: true, ..split() }, Err("no".into()));
        assert!(out.contains("sp-new was filed but could not be made to wait on sp-b0"), "{out}");
    }

    #[test_id(tnohsgp)]
    #[test]
    fn a_prerequisite_from_an_incident_lane_is_refused_with_a_way_through() {
        let why = super::incident_lane_refusal("ops", "scope,incident-x", "incident-x").unwrap();
        assert!(why.contains("nothing was filed") && why.contains("work blocked"), "{why}");
        assert_eq!(super::incident_lane_refusal("builder", "scope,plan", "incident-x"), None);
        assert_eq!(super::incident_lane_refusal("builder", "", ""), None);
    }

    // No --for: a call that slips past the flag refusals stops here and files nothing.
    #[test_id(tn62wt6)]
    #[test]
    fn contradictory_or_misplaced_prerequisite_flags_are_refused_before_anything_is_filed() {
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let (code, out) = super::cmd_file("sp-b0", &v(&["t", "--prerequisite", "--after-landing"]), "split");
        assert_eq!(code, super::REFUSED, "{out}");
        assert!(out.contains("--prerequisite") && out.contains("--after-landing"), "{out}");
        let (code, out) = super::cmd_file("sp-b0", &v(&["t", "--prerequisite"]), "file-followup");
        assert_eq!(code, super::REFUSED, "{out}");
        assert!(out.contains("work split --prerequisite"), "{out}");
    }

    #[test_id(t6kzjhm)]
    #[test]
    fn a_pilgrims_follow_up_blocks_the_bound_bead_on_the_new_one_and_a_builders_does_not() {
        assert_eq!(super::blocking_edge("file-followup", "pilgrim", "sp-walk", "sp-new"), Some(("sp-walk", "sp-new")));
        assert_eq!(super::blocking_edge("file-followup", "builder", "sp-b", "sp-new"), None);
        assert_eq!(super::blocking_edge("split", "pilgrim", "sp-walk", "sp-new"), None);
    }

    #[test_id(tiifcmo)]
    #[test]
    fn every_work_verb_a_persona_prompt_names_is_in_that_personas_allow_row() {
        let runes = spira_config::resolve::locate_home_for_process().unwrap().join("runes");
        let mut bad = vec![];
        for e in std::fs::read_dir(&runes).unwrap().flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let persona = path.file_stem().unwrap().to_string_lossy().into_owned();
            for line in std::fs::read_to_string(&path).unwrap().lines() {
                let mut words = line.split_whitespace().map(|w| w.trim_matches(|c: char| "`'\"(),.:;".contains(c)));
                while let Some(w) = words.next() {
                    if w != "work" {
                        continue;
                    }
                    let Some(verb) = words.next() else { break };
                    let is_verb = BOUND_VERBS.contains(&verb) || LANE_VERBS.contains(&verb);
                    if !is_verb {
                        continue;
                    }
                    let sub: Vec<String> = words.clone().next().map(|x| vec![x.to_string()]).unwrap_or_default();
                    let op = super::op_key(verb, &sub);
                    let op = if ALLOW.iter().any(|(k, _)| *k == op) { op } else { verb.to_string() };
                    if super::permitted(&op, Some(&persona), persona == "pilgrim").is_err() || (persona == "pilgrim" && PILGRIM_WORLD_ALLOW.contains(&op.as_str()) && super::permitted(&op, Some(&persona), false).is_ok()) {
                        bad.push(format!("{persona}.md: `work {op}`"));
                    }
                }
            }
        }
        bad.sort();
        bad.dedup();
        assert!(bad.is_empty(), "prompt names verbs its persona may not run: {bad:?}");
    }

    #[test_id(trq4c92)]
    #[test]
    fn the_pilgrims_contract_files_defects_with_the_verb_the_broker_admits() {
        let md = std::fs::read_to_string(spira_config::resolve::locate_home_for_process().unwrap().join("runes/pilgrim.md")).unwrap();
        assert!(md.contains("work file-followup \""));
        assert!(!md.lines().any(|l| l.trim_start().starts_with("work file \"")));
    }

    #[test_id(tjn7gqx)]
    #[test]
    fn the_pilgrims_world_prefix_names_the_registry_beside_the_socket() {
        let md = std::fs::read_to_string(spira_config::resolve::locate_home_for_process().unwrap().join("runes/pilgrim.md")).unwrap();
        let socket_uses: Vec<&str> = md.lines().filter(|l| l.contains("SPIRA_LC_SOCKET=$PILGRIM_LC_SOCKET")).collect();
        assert!(!socket_uses.is_empty());
        for l in socket_uses {
            assert!(l.contains("SPIRA_TOML=$PILGRIM_TOML"), "the socket alone is overridden by the session's registry: {l}");
        }
    }

    #[test_id(ttsc70u)]
    #[test]
    fn a_unit_red_on_base_must_name_a_bead_that_exists_and_leaves_a_note_line() {
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let has = |known: &'static str| move |id: &str| Ok(id == known);
        let args = v(&["--unit-red-on-base", "sp-base1", "--unit-red-tests", "a::bad,b::worse"]);
        let line = super::unit_red_line(&args, "sp-me", &has("sp-base1")).unwrap().unwrap();
        assert!(line.contains("sp-base1") && line.contains("a::bad,b::worse"), "{line}");
        let err = super::unit_red_line(&v(&["--unit-red-on-base", "sp-ghost"]), "sp-me", &has("sp-base1")).unwrap_err();
        assert!(matches!(&err, super::UnitRedError::Refused(m) if m.contains("sp-ghost") && m.contains("does not exist")), "{err:?}");
        assert!(super::unit_red_line(&v(&["--unit-red-on-base", "sp-me"]), "sp-me", &has("sp-me")).is_err());
        assert!(super::unit_red_line(&v(&["--unit-red-on-base"]), "sp-me", &has("sp-base1")).is_err());
        assert_eq!(super::unit_red_line(&v(&["--met", "x"]), "sp-me", &has("sp-base1")), Ok(None));
    }

    #[test_id(t53i73c)]
    #[test]
    fn a_calls_body_names_its_failure() {
        let body = "  spec: /x/y.json\n  Batch: r-1\n  Pass: 2\n  Suite: test-a.sh\n";
        assert_eq!(super::calls_failure(body), Some(("r-1".into(), 2, "test-a.sh".into())));
        assert_eq!(super::calls_failure("an integrate calls"), None);
    }

    #[test_id(t2oncv2)]
    #[test]
    fn a_release_after_an_ask_is_seen_and_a_plain_release_is_not() {
        let h = |xs: &[(&str, &str)]| xs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        let ask = r#"{"Hold":{"cause":"operator-question","detail":"q"}}"#;
        assert!(released_on_ask(&h(&[("Claim", "{}"), ("Hold", ask), ("Release", "\"Release\"")])));
        assert!(!released_on_ask(&h(&[("Claim", "{}"), ("Release", "\"Release\"")])));
        assert!(!released_on_ask(&h(&[("Claim", "{}"), ("Hold", ask)])), "an ask still held is refused by the machine itself");
        assert!(!released_on_ask(&h(&[("Release", "\"Release\""), ("Hold", ask)])));
    }

    #[test_id(t5nfw7o)]
    #[test]
    fn a_child_names_its_repo_and_never_inherits_the_parents() {
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(child_repo(&v(&["dumpstar browse tiles"])).is_err());
        assert!(child_repo(&v(&["t", "--repo"])).is_err());
        assert!(child_repo(&v(&["t", "--repo", "--after-landing"])).is_err());
        assert_eq!(child_repo(&v(&["t", "--repo", "dumpstar"])).unwrap(), "dumpstar");
    }

    #[test_id(tag3p83)]
    #[test]
    fn a_line_of_prose_or_a_truncated_prefix_is_not_a_bead_id() {
        for bad in ["SPIRA_BEAD_LANE_OVERRIDE=1", "bead:", "override:", "sp-", "sp", "", "sp-a b", "sp-x/y", "x-abc"] {
            assert!(!is_bead_id(bad), "{bad:?}");
        }
        for ok in ["sp-b411iv", "sp-b411iv.1"] {
            assert!(is_bead_id(ok), "{ok}");
        }
    }

    /// mail's lint: a body line naming a bead must say, in at least four other words, what the work is.
    fn bare_id_lines(body: &str) -> Vec<String> {
        body.lines()
            .filter(|l| l.contains("sp-"))
            .filter(|l| l.split_whitespace().filter(|w| !w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-').starts_with("sp-")).count() < 4)
            .map(str::to_string)
            .collect()
    }

    #[test_id(t3a3vvw)]
    #[test]
    fn a_supersede_ask_describes_each_bead_it_names_and_argues_its_class() {
        let body = supersede_ask_body("sp-old1", "fix the gate", "sp-new2", "the gate rewrite", Some("abc1234 (spira: land sp-new2)"));
        assert_eq!(bare_id_lines(&body), Vec::<String>::new(), "{body}");
        for want in ["fix the gate", "the gate rewrite", "abc1234", "## Question\n", "## Default\n", "## Class basis\n"] {
            assert!(body.contains(want), "{want:?} missing from {body}");
        }
        let unlanded = supersede_ask_body("sp-old1", "", "sp-new2", "t", None);
        assert_eq!(bare_id_lines(&unlanded), Vec::<String>::new(), "{unlanded}");
        assert!(unlanded.contains("No landing commit"));
        assert!(!bare_id_lines("## Question\nsp-old1 sp-new2?\n").is_empty(), "POSITIVE CONTROL: the lint shape catches a bare-id line");
    }

    #[test_id(t17aspn)]
    #[test]
    fn a_blocked_ask_is_one_mail_delivers_to_the_operator() {
        let (question, default) = ("Has the Control run passed?", "wait for it");
        let body = super::blocked_ask_body(question, default, "retiring a running unit is policy");
        assert_eq!(mail::cmds::operator_refusal("question", default, "policy", &body, ""), None, "{body}");
        let kinds = spira_config::resolve::locate_home_for_process().unwrap().join("mail/kinds");
        let input = mail::lint::LintInput { kind: "question", subject: question, default, from: "builder <builder@spira>", urgent: false, body: &body, digest: false };
        assert_eq!(mail::lint::lint_check(&input, &kinds, "sp"), Ok(()), "{body}");
        let led = mail::lint::LintInput { subject: "sp-abcd1 has the Control run passed?", ..input };
        let refused = mail::lint::lint_check(&led, &kinds, "sp").unwrap_err();
        assert!(refused.contains("Subject leads with a bead id"), "POSITIVE CONTROL: {refused}");
    }

    #[test_id(txxgymk)]
    #[test]
    fn a_blocked_call_without_its_question_or_default_is_refused_with_the_usage() {
        let dir = testkit::TempDir::new("blocked-no-question");
        let _env = testkit::env(&[("SPIRA_MAIL_SH", Some(&dir.path().join("mail").to_string_lossy()))]);
        let calls: [&[&str]; 3] = [&[""], &["--default", "wait for it"], &["Has the Control run passed?"]];
        for call in calls {
            let args: Vec<String> = call.iter().chain(&["--actor", "builder"]).map(|a| a.to_string()).collect();
            let (code, out) = super::cmd_blocked("sp-x", &args);
            assert_eq!(code, super::CANNOT_TELL, "{call:?}: {out}");
            assert!(out.ends_with(super::BLOCKED_USAGE), "{call:?}: {out}");
        }
    }

    #[test_id(tvpek5o)]
    #[test]
    fn an_untriggered_groom_ask_passes_mails_lint() {
        let (subject, default, body) = super::untriggered_groom_ask("sp-abc123");
        let kinds = spira_config::resolve::locate_home_for_process().unwrap().join("mail/kinds");
        let lint = |subject: &str| {
            let input = mail::lint::LintInput { from: "groomer <groomer@spira>", subject, kind: "question", default: &default, urgent: false, body: &body, digest: false };
            mail::lint::lint_check(&input, &kinds, "sp")
        };
        let refused = lint("sp-abc123 was claimed by the groomer").unwrap_err();
        assert!(refused.contains("Subject leads with a bead id"), "POSITIVE CONTROL: {refused}");
        assert_eq!(lint(&subject), Ok(()), "{body}");
        assert!(!subject.starts_with("sp-abc123"), "{subject}");
        assert!(body.contains("sp-abc123"), "{body}");
    }

    #[test_id(tfh9r32)]
    #[test]
    fn a_blocked_call_without_its_class_or_basis_is_refused_before_anything_is_sent() {
        let dir = testkit::TempDir::new("blocked-no-mail");
        let _env = testkit::env(&[("SPIRA_MAIL_SH", Some(&dir.path().join("mail").to_string_lossy()))]);
        let base = ["Has the Control run passed?", "--default", "wait for it"];
        let calls: [(&[&str], &str); 7] = [
            (&[], "--class"),
            (&["--class", "policy"], "--class-basis"),
            (&["--class-basis", "it is policy"], "--class"),
            (&["--class", "", "--class-basis", "it is policy"], "--class"),
            (&["--class", "policy", "--class-basis", " "], "--class-basis"),
            (&["--class", "policy", "--class-basis"], "--class-basis"),
            (&["--class", "--class-basis", "it is policy"], "--class"),
        ];
        for (extra, missing) in calls {
            // The client appends --actor after the caller's own arguments.
            let args: Vec<String> = base.iter().chain(extra).chain(&["--actor", "builder"]).map(|a| a.to_string()).collect();
            let (code, out) = super::cmd_blocked("sp-x", &args);
            assert_eq!(code, super::CANNOT_TELL, "{extra:?}: {out}");
            assert!(out.contains(&format!("{missing} is required;")) && out.ends_with(super::BLOCKED_USAGE), "{extra:?}: {out}");
        }
    }

    #[test_id(tj4gi7f)]
    #[test]
    fn a_flag_is_never_taken_as_a_successor() {
        for bad in ["--help", "-h", "--", ""] {
            assert!(!is_bead_id(bad), "{bad:?}");
        }
    }

    use super::*;

    #[test_id(theaybd)]
    #[test]
    fn a_content_write_recorded_by_the_previous_release_is_answered_and_runs_nothing() {
        let argv = ["content-write", "", "gate", "discover", "--branch", "b"].map(String::from).to_vec();
        let row = crate::control::work_requests::WorkRequest { id: "wr-1".into(), bead: "-".into(), actor: "content".into(), argv, stdin: None, bound_ms: 1, state: "running".into(), code: None, output: String::new(), truncated: false, queued_at: 0, started_at: None, finished_at: None };
        let (code, out) = super::run_recorded(&row);
        assert_eq!(code, crate::CANNOT_TELL, "{out}");
        assert!(out.contains("unknown verb \"gate\""), "{out}");
    }

    fn commit(sha: &str, message: &str) -> (String, String, String) {
        (sha.into(), message.lines().next().unwrap_or("").into(), message.into())
    }

    #[test_id(t3bv515)]
    #[test]
    fn a_branch_rebased_onto_a_submitted_siblings_commits_submits() {
        let commits = [commit("aaa1", "sp-sib: the sibling's change\n"), commit("bbb2", "sp-own: my change\n")];
        let in_flight = |id: &str| id == "sp-sib";
        assert!(unnamed_commits(&commits, "sp-own", &in_flight).is_empty());
    }

    #[test_id(t6xza0e)]
    #[test]
    fn a_commit_naming_a_bead_not_in_flight_still_refuses() {
        let commits = [commit("aaa1", "sp-gone: a dropped bead's change\n"), commit("bbb2", "sp-own: my change\n")];
        let in_flight = |id: &str| id == "sp-sib";
        let refused = unnamed_commits(&commits, "sp-own", &in_flight);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("aaa1") && refused[0].contains("sp-gone"), "{refused:?}");
    }

    #[test_id(ti44hol)]
    #[test]
    fn a_commit_naming_no_bead_still_refuses() {
        let commits = [commit("ccc3", "fix the thing\n"), commit("bbb2", "sp-own: my change\n")];
        let in_flight = |_: &str| true;
        let refused = unnamed_commits(&commits, "sp-own", &in_flight);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("ccc3"), "{refused:?}");
    }

    fn pair(sha: &str, message: &str) -> (String, String) {
        (sha.into(), message.into())
    }

    fn fake_store(held: &'static [&'static str]) -> impl FnMut(&[String]) -> Result<Vec<String>, String> {
        move |ids| Ok(ids.iter().filter(|i| held.contains(&i.as_str())).cloned().collect())
    }

    #[test_id(tq7r8iz)]
    #[test]
    fn fixture_ids_in_a_commit_body_do_not_refuse_submit() {
        let commits = [pair("aaa1bbb2ccc3", "sp-own: x\n\nanswers sp-a and sp-b\n")];
        assert_eq!(phantom_refusal(&commits, "sp", &mut fake_store(&["sp-own"])), None);
    }

    #[test_id(tn225xn)]
    #[test]
    fn a_well_formed_id_the_store_lacks_refuses_submit_naming_the_commit() {
        let commits = [pair("aaa1bbb2ccc3", "sp-own: x\n\nsee sp-zzzzzz\n")];
        let (code, text) = phantom_refusal(&commits, "sp", &mut fake_store(&["sp-own"])).unwrap();
        assert_eq!(code, REFUSED);
        assert!(text.contains("aaa1bbb2c") && text.contains("sp-zzzzzz") && text.contains("Reword"), "{text}");
    }

    #[test_id(tff0i7a)]
    #[test]
    fn a_store_that_cannot_answer_the_citation_check_is_cannot_tell() {
        let commits = [pair("aaa1", "sp-own: see sp-zzzzzz\n")];
        let (code, text) = phantom_refusal(&commits, "sp", &mut |_| Err("down".into())).unwrap();
        assert_eq!(code, CANNOT_TELL);
        assert!(text.contains("did not answer the citation check"), "{text}");
    }

    #[test_id(tnlz0ou)]
    #[test]
    fn a_sibling_named_with_trailing_punctuation_or_as_a_child_submits() {
        let commits = [commit("aaa1", "fix for sp-sib.\n"), commit("ddd4", "sp-sib.1: the child\n")];
        let in_flight = |id: &str| id == "sp-sib" || id == "sp-sib.1";
        assert!(unnamed_commits(&commits, "sp-own", &in_flight).is_empty());
    }

    #[test_id(t4k3bum)]
    #[test]
    fn a_sibling_id_is_named_whole_not_as_a_prefix() {
        let commits = [commit("aaa1", "sp-sib2: a longer id\n")];
        let in_flight = |id: &str| id == "sp-sib";
        assert_eq!(unnamed_commits(&commits, "sp-own", &in_flight).len(), 1);
    }

    #[test_id(tw2hnjg)]
    #[test]
    fn a_tip_behind_the_landing_ref_submits() {
        let commits = [commit("bbb2", "sp-own: my change\n")];
        let in_flight = |_: &str| false;
        assert_eq!(finishing_refusal("local/main", "tip1", "sp-own", Some("fork1"), Some(&commits), &in_flight), None);
    }

    #[test_id(tkkzh64)]
    #[test]
    fn a_tip_that_conflicts_with_the_landing_ref_is_refused_naming_the_paths() {
        let out = "t\nCONFLICT (content): Merge conflict in a/one.rs\nCONFLICT (content): Merge conflict in b/two.rs\n";
        let (code, msg) = merge_refusal(&|_| (1, out.to_string()), "local/main", "local/main", "tip1").expect("refused");
        assert_eq!(code, REFUSED);
        assert!(msg.contains("a/one.rs, b/two.rs") && msg.contains("local/main") && msg.contains("work submit"), "{msg}");
    }

    #[test_id(tgf1hkb)]
    #[test]
    fn a_tip_behind_the_landing_ref_that_merges_cleanly_submits() {
        assert_eq!(merge_refusal(&|_| (0, "tree\n".to_string()), "local/main", "local/main", "tip1"), None);
    }

    #[test_id(t9yosdd)]
    #[test]
    fn a_merge_check_that_cannot_run_cannot_tell() {
        let (code, msg) = merge_refusal(&|_| (128, "fatal: x\n\n".to_string()), "local/main", "base", "tip1").expect("refused");
        assert_eq!(code, CANNOT_TELL);
        assert!(msg.contains("onto base failed: fatal: x"), "{msg}");
    }

    #[test_id(tc66zy2)]
    #[test]
    fn a_tip_sharing_no_history_with_the_landing_ref_refuses() {
        let in_flight = |_: &str| false;
        let refused = finishing_refusal("local/main", "tip1", "sp-own", None, Some(&[]), &in_flight).expect("refused");
        assert!(refused.contains("shares no history"), "{refused}");
    }

    #[test_id(tdcpzx4)]
    #[test]
    fn done_on_a_dropped_or_done_bead_is_a_no_op_for_every_actor() {
        for st in [bead::BeadState::Dropped, bead::BeadState::Done] {
            let (code, msg) = already_finished_noop("sp-x", st).unwrap();
            assert_eq!(code, 0);
            assert_eq!(msg, format!("sp-x is already {}; nothing to do", st.as_str()));
        }
        assert!(already_finished_noop("sp-x", bead::BeadState::Working).is_none());
        assert!(already_finished_noop("sp-x", bead::BeadState::Landed).is_none());
    }

    #[test_id(tyhfs6l)]
    #[test]
    fn a_skuld_draft_with_prose_on_its_depends_line_is_refused_at_done() {
        let prose = "### Draft: Fix it\n\nDepends: none. Related: sp-7gyqqy (rollback) and sp-0khlo0 (hotfix). Both touch `activate.rs`; sequence them, do not merge them.\n";
        let (code, msg) = skuld_draft_notes_refusal("sp-r", prose).expect("refused");
        assert_eq!(code, REFUSED);
        assert!(msg.contains("Fix it") && msg.contains("work note") && msg.contains("Context"), "{msg}");
        assert!(skuld_draft_notes_refusal("sp-r", "### Draft: Fix it\n\nDepends: sp-a, draft 1\n").is_none());
    }

    #[test_id(t8lhrc6)]
    #[test]
    fn a_skuld_draft_with_a_paragraph_after_its_open_line_is_refused_at_done() {
        let notes = "### Draft: First\n\nbody\nOpen: q; default: d\nCorrection to \"First\": twelve, not eleven.\n### Draft: Second\n\nbody2\n";
        let (code, msg) = skuld_draft_notes_refusal("sp-r", notes).expect("refused");
        assert_eq!(code, REFUSED);
        assert!(msg.contains("draft \"First\"") && msg.contains("Correction to") && msg.contains("work done"), "{msg}");
        assert!(!msg.contains("Second"), "{msg}");
    }

    #[test_id(tn2wq8d)]
    #[test]
    fn a_resent_skuld_draft_followed_by_a_no_progress_note_closes_at_done() {
        let notes = "### Draft: T\n\nbody\nOpen: q\nConcierge: a note\n\n### Draft: T\n\nbody\nOpen: q\n\nNo progress (no-commit): nothing moved\n\nHeld for 30m (no-progress streak 2, branch stuck at abc) — not re-claimed until 2026-11-24T10:00:00Z. No attempt charged.\n";
        assert_eq!(skuld_draft_notes_refusal("sp-r", notes), None);
    }

    #[test_id(td6fwh4)]
    #[test]
    fn a_working_exit_the_rune_does_not_declare_is_refused_by_name() {
        let exits = v(&["DONE", "ask", "park", "hold"]);
        let (code, msg) = working_exit_check("submit", "SUBMITTED", "spike", &exits).unwrap();
        assert_eq!(code, REFUSED);
        assert!(msg.contains("spike") && msg.contains("SUBMITTED") && msg.contains("DONE ask park hold"), "{msg}");
        assert!(working_exit_check("done", "DONE", "spike", &exits).is_none());
        assert!(working_exit_check("submit", "SUBMITTED", "builder", &v(&["SUBMITTED", "ask"])).is_none());
    }

    #[test_id(t8nwxnz)]
    #[test]
    fn submit_from_ready_is_refused_naming_the_state_and_the_way_out() {
        let tip = "a".repeat(40);
        let mut row = bead::LifecycleRow::filed("sp-x");
        row.state = bead::BeadState::Ready;
        let (code, msg) = submit_state_refusal(&row, &tip).expect("a READY submit is refused");
        assert_eq!(code, REFUSED);
        assert!(msg.contains("sp-x is READY, not WORKING") && msg.contains("claim it first"), "{msg}");
        row.state = bead::BeadState::Rework;
        let (_, msg) = submit_state_refusal(&row, &tip).unwrap();
        assert!(msg.contains("REWORK") && msg.contains("claim it first"), "{msg}");
        row.state = bead::BeadState::Landed;
        let (_, msg) = submit_state_refusal(&row, &tip).unwrap();
        assert!(msg.contains("LANDED") && !msg.contains("claim it first"), "{msg}");
        for legal in [bead::BeadState::Working, bead::BeadState::Submitted, bead::BeadState::Certified] {
            row.state = legal;
            assert!(submit_state_refusal(&row, &tip).is_none(), "{legal:?}");
        }
    }

    fn working_row(persona: &str) -> bead::LifecycleRow {
        let mut row = bead::LifecycleRow::filed("sp-x");
        row.state = bead::BeadState::Working;
        row.persona = Some(persona.to_string());
        row
    }

    #[test_id(t99l63s)]
    #[test]
    fn the_shipped_runes_send_a_spike_to_done_and_a_builder_to_submitted() {
        let runes = spira_config::resolve::locate_home_for_process().unwrap().join("runes");
        let go = |persona: &str, verb: &str, exit: &str| working_exit_refusal_in(&runes, verb, exit, &working_row(persona), persona);
        let (code, msg) = go("spike", "submit", "SUBMITTED").unwrap();
        assert_eq!(code, REFUSED);
        assert!(msg.contains("spike") && msg.contains("SUBMITTED") && msg.contains("DONE ask park hold"), "{msg}");
        assert!(go("spike", "done", "DONE").is_none());
        assert!(go("builder", "submit", "SUBMITTED").is_none());
        assert!(go("builder", "done", "DONE").is_some());
        let mut submitted = working_row("spike");
        submitted.state = bead::BeadState::Submitted;
        assert!(working_exit_refusal_in(&runes, "submit", "SUBMITTED", &submitted, "spike").is_none());
    }

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test_id(tipjnrx)]
    #[test]
    fn a_uid_bound_to_a_persona_cannot_present_as_another() {
        let map = parse_persona_uids("# dwarves\n1001 builder\n1002 czar  # the czar\nbad line here\n");
        assert_eq!(map, vec![(1001, "builder".to_string()), (1002, "czar".to_string())]);
        let forged = v(&["work", "sp-1", "queue", "eject", "sp-2", "--actor", "czar"]);
        let (code, msg) = bind_peer(&forged, Some(1001), &map).unwrap_err();
        assert_eq!(code, REFUSED);
        assert!(msg.contains("builder"), "{msg}");
        assert_eq!(bind_peer(&forged, Some(1002), &map).unwrap(), forged);
        assert_eq!(bind_peer(&v(&["work", "sp-1", "show"]), Some(1001), &map).unwrap(), v(&["work", "sp-1", "show", "--actor", "builder"]));
        assert!(bind_peer(&v(&["work", "sp-1", "show", "--actor", "czar", "--actor", "builder"]), Some(1001), &map).is_err());
        assert!(bind_peer(&forged, None, &map).is_err());
        assert_eq!(bind_peer(&forged, Some(5), &map).unwrap(), forged);
        assert_eq!(bind_peer(&forged, Some(1001), &[]).unwrap(), forged);
        let other = v(&["show", "sp-1"]);
        assert_eq!(bind_peer(&other, Some(1001), &map).unwrap(), other);
    }

    fn call(xs: &[&str], actor: &str) -> Call {
        split_reserved(&[v(xs), v(&["--actor", actor])].concat())
    }

    fn tool(program: &'static str, args: &[&str]) -> Step {
        Step::Tool { program, args: v(args), stdin: None }
    }

    // ---- the gate ----

    fn proof_dir(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(tag)
    }

    const TIP: &str = "3333333333333333333333333333333333333333";

    #[test_id(tuoi53k)]
    #[test]
    fn a_gate_red_tip_is_refused_saying_gate_with_the_step_and_message_and_a_round_eject_still_says_round() {
        let gate = super::GateRedAt { step: Some("fixture-config".into()), message: Some("SPIRA_X is not declared".into()) };
        let said = super::red_tip_refusal("sp-a", "abc", None, Some(&gate));
        assert!(said.contains("the gate") && said.contains("fixture-config") && said.contains("SPIRA_X is not declared") && !said.contains("round"), "{said}");
        let round = super::red_tip_refusal("sp-a", "abc", Some("unit-tests"), Some(&gate));
        assert!(round.contains("a round ejected") && round.contains("unit-tests") && !round.contains("fixture-config"), "{round}");
        assert!(super::red_tip_refusal("sp-a", "abc", None, None).contains("a round ejected"));
    }

    #[test_id(t7xsykf)]
    #[test]
    fn a_stored_gate_red_reads_back_its_step_and_message_and_a_stepless_one_still_reads_as_a_gate_red() {
        let row = serde_json::json!({"step": "fixture-config", "message": "SPIRA_X is not declared"});
        let got = super::gate_red_from(&row).unwrap();
        assert_eq!((got.step.as_deref(), got.message.as_deref()), (Some("fixture-config"), Some("SPIRA_X is not declared")));
        assert!(super::gate_red_from(&serde_json::json!({"step": "", "message": null})).is_some_and(|g| g.step.is_none() && g.message.is_none()));
    }

    #[test_id(twxem95)]
    #[test]
    fn a_resubmit_without_a_passing_run_of_the_ejected_suite_is_refused_naming_it() {
        let dir = proof_dir("refuse");
        let run = dir.path().to_path_buf();
        spira_config::proof::record_failing(&run, "sp-a", &["test-x.sh".into()]).unwrap();
        let red = |_: &str, _: u64| (1, "test-x: 2 failed\n".to_string());
        let (code, msg) = proof_refusal(&run, "sp-a", TIP, &[], 60, &red).expect("a red run must not prove");
        assert_eq!(code, REFUSED);
        assert!(msg.contains("test-x.sh") && msg.contains("FAILED"), "{msg}");
        let broken = |_: &str, _: u64| (2, "no container".to_string());
        assert!(proof_refusal(&run, "sp-a", TIP, &[], 60, &broken).is_some(), "a check that could not run is unproven");
    }

    #[test_id(to1yrlj)]
    #[test]
    fn a_resubmit_with_a_passing_run_at_the_new_tip_is_accepted_and_the_run_is_recorded() {
        let dir = proof_dir("accept");
        let run = dir.path().to_path_buf();
        spira_config::proof::record_failing(&run, "sp-a", &["test-x.sh".into()]).unwrap();
        let green = |c: &str, _: u64| (0, format!("{c}: 12 passed\n"));
        assert!(proof_refusal(&run, "sp-a", TIP, &[], 60, &green).is_none());
        let rec = std::fs::read_to_string(run.join("proof/sp-a/pass").join(TIP).join("test-x.sh")).unwrap();
        assert_eq!(rec, "exit=0\nsummary=test-x.sh: 12 passed\n");
        let never = |_: &str, _: u64| -> (i32, String) { panic!("a proven check is not re-run") };
        assert!(proof_refusal(&run, "sp-a", TIP, &[], 60, &never).is_none());
        let other = "4444444444444444444444444444444444444444";
        assert!(proof_refusal(&run, "sp-a", other, &[], 60, &green).is_none(), "a new tip is proven afresh");
    }

    #[test_id(t77h9i1)]
    #[test]
    fn a_diff_that_owes_a_tier_is_refused_until_it_passes_and_names_the_tier() {
        let dir = proof_dir("owed");
        let run = dir.path().to_path_buf();
        let owed = vec!["frontend".to_string()];
        let red = |_: &str, _: u64| (1, "svelte-check found 1 error\n".to_string());
        let (code, msg) = proof_refusal(&run, "sp-a", TIP, &owed, 60, &red).expect("an unproven owed tier refuses");
        assert_eq!(code, REFUSED);
        assert!(msg.contains("frontend") && msg.contains("FAILED") && msg.contains("the runner that owns it"), "{msg}");
        let green = |c: &str, _: u64| (0, format!("{c}: ok\n"));
        assert!(proof_refusal(&run, "sp-a", TIP, &owed, 60, &green).is_none());
        assert!(run.join("proof/sp-a/pass").join(TIP).join("frontend").is_file());
    }

    #[test_id(th1cib1)]
    #[test]
    fn a_round_proven_check_is_never_run_in_band_and_does_not_refuse() {
        let dir = proof_dir("round-proven");
        let run = dir.path().to_path_buf();
        spira_config::proof::record_failing(&run, "sp-a", &["unit-tests".into()]).unwrap();
        let never = |_: &str, _: u64| -> (i32, String) { panic!("unit-tests is the round's to run") };
        assert!(proof_refusal(&run, "sp-a", TIP, &["unit-tests".to_string()], 60, &never).is_none());
        for step in ["bare-suites", "image", "config-delta", "release-verify"] {
            spira_config::proof::record_failing(&run, "sp-a", &[step.into()]).unwrap();
            assert!(proof_refusal(&run, "sp-a", TIP, &[step.to_string()], 60, &never).is_none(), "{step}");
        }
        let red = |_: &str, _: u64| (1, "test-x: 1 failed\n".to_string());
        spira_config::proof::record_failing(&run, "sp-a", &["unit-tests".into(), "test-x.sh".into()]).unwrap();
        let (_, msg) = proof_refusal(&run, "sp-a", TIP, &[], 60, &red).expect("a cheap check still proves in band");
        assert!(msg.contains("test-x.sh"), "{msg}");
    }

    #[test_id(t5jmu7q)]
    #[test]
    fn each_forced_check_is_run_by_the_runner_that_owns_it() {
        let ck = Some(("repo", "local/main"));
        let inv = |c: &str| check_invocation(c, TIP, "spira", ck).unwrap();
        assert_eq!(inv("test-x.sh"), ("testenv", v(&["--suites", "test-x.sh", TIP, "spira"])));
        assert_eq!(inv("spira-lint"), ("testenv", v(&["lint", TIP, "spira"])));
        assert_eq!(inv("workspace-build"), ("testenv", v(&["lint", TIP, "spira"])));
        assert_eq!(inv("fence-lifecycle-guard"), ("gate", v(&["fence-run", "lifecycle-guard", "--repo", "repo", "--tip", TIP, "--base", "local/main"])));
        assert!(check_invocation("fence-lifecycle-guard", TIP, "spira", None).is_none(), "a fence with no checkout cannot be run, and is not handed to testenv");
    }

    #[test_id(te4x4hs)]
    #[test]
    fn a_resubmit_after_an_eject_naming_a_fence_runs_the_fence_and_passes_when_it_passes() {
        let dir = proof_dir("fence");
        let run = dir.path().to_path_buf();
        spira_config::proof::record_failing(&run, "sp-a", &["fence-lifecycle-guard".into()]).unwrap();
        let ck = Some(("repo", "local/main"));
        let seen = std::cell::RefCell::new(Vec::new());
        let runner = |c: &str, _: u64| {
            let (program, args) = check_invocation(c, TIP, "spira", ck).unwrap();
            seen.borrow_mut().push((program, args));
            if program == "gate" { (0, "fence: lifecycle-guard checked 9 files\n".to_string()) } else { (2, "batch: unknown suite\n".to_string()) }
        };
        assert!(proof_refusal(&run, "sp-a", TIP, &[], 60, &runner).is_none());
        assert_eq!(seen.borrow().len(), 1);
        assert_eq!(seen.borrow()[0].0, "gate");
        assert!(spira_config::proof::proven(&run, "sp-a", TIP, "fence-lifecycle-guard"));
    }

    #[test_id(tnt855t)]
    #[test]
    fn a_resubmit_after_an_eject_forcing_a_declared_tier_runs_that_tier_through_testenv_and_passes() {
        let dir = proof_dir("declared-tier");
        let run = dir.path().to_path_buf();
        spira_config::proof::record_failing(&run, "sp-a", &["gate".into()]).unwrap();
        let runner = |c: &str, _: u64| {
            let (program, args) = check_invocation(c, TIP, "remarkable-pipeline", None).unwrap();
            if (program, args.clone()) == ("testenv", v(&["--suites", "gate", TIP, "remarkable-pipeline"])) { (0, "gate: ok\n".to_string()) } else { (2, "batch: unknown suite: testenv\n".to_string()) }
        };
        assert!(proof_refusal(&run, "sp-a", TIP, &[], 60, &runner).is_none());
        assert!(spira_config::proof::proven(&run, "sp-a", TIP, "gate"));
    }

    #[test_id(t369sas)]
    #[test]
    fn a_diff_that_owes_nothing_runs_nothing() {
        let dir = proof_dir("owes-nothing");
        let run = dir.path().to_path_buf();
        let never = |_: &str, _: u64| -> (i32, String) { panic!("a backend-only diff owes no frontend run") };
        assert!(proof_refusal(&run, "sp-a", TIP, &[], 60, &never).is_none());
    }

    #[test_id(tczlsfb)]
    #[test]
    fn a_bead_with_no_named_checks_owes_no_proof() {
        let dir = proof_dir("none");
        let run = dir.path().to_path_buf();
        let never = |_: &str, _: u64| -> (i32, String) { panic!("nothing to run") };
        assert!(proof_refusal(&run, "sp-a", TIP, &[], 60, &never).is_none());
    }

    #[test_id(tr1ciu7)]
    #[test]
    fn every_allow_row_names_a_known_verb() {
        for (op, who) in ALLOW {
            let verb = op.split(' ').next().unwrap();
            assert!(BOUND_VERBS.contains(&verb) || LANE_VERBS.contains(&verb), "{op}");
            assert!(!who.is_empty(), "{op}");
        }
    }

    #[test_id(t57qmqd)]
    #[test]
    fn a_persona_may_not_run_a_verb_it_is_not_allowed() {
        let (code, msg) = permitted("groom close", Some("builder"), false).unwrap_err();
        assert_eq!(code, REFUSED);
        assert!(msg.contains("builder") && msg.contains("groom close"), "{msg}");
        assert_eq!(gate("note-on", &v(&["sp-x1", "hi", "--actor", "builder"])).unwrap_err().0, REFUSED);
        assert_eq!(gate("queue", &v(&["eject", "sp-x1", "--actor", "groomer"])).unwrap_err().0, REFUSED);
    }

    #[test_id(t2m4mga)]
    #[test]
    fn only_the_mender_runs_the_mender_tool_and_it_forwards_verbatim() {
        for sub in ["start", "pack", "diff", "bead", "members", "failures", "passes", "verdicts", "landing", "triaged", "diagnose", "outcome"] {
            assert!(gate("mender", &v(&[sub, "s", "--actor", "mender"])).is_ok(), "{sub}");
            assert_eq!(gate("mender", &v(&[sub, "s", "--actor", "builder"])).unwrap_err().0, REFUSED, "{sub}");
        }
        assert_eq!(gate("mender", &v(&["delete", "s", "--actor", "mender"])).unwrap_err().0, REFUSED, "an operation the table does not name is refused");
        assert_eq!(
            plan("mender", "-", &call(&["outcome", "s", "return", "--checks", "x"], "mender")).unwrap(),
            vec![tool("mender", &["outcome", "s", "return", "--checks", "x"])]
        );
    }

    #[test_id(tdy26qo)]
    #[test]
    fn only_a_socket_with_the_marker_beside_it_is_a_pilgrim_world() {
        let t = testkit::TempDir::new("pw");
        let socket = t.join("lc.sock").display().to_string();
        let args = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let file = args(&["a title", "--for", "builder", "--repo", "r", "--actor", "pilgrim"]);
        assert!(!super::socket_is_a_pilgrim_world(&socket));
        assert!(!super::socket_is_a_pilgrim_world(""));
        assert_eq!(gate_in("file", &file, || super::socket_is_a_pilgrim_world(&socket)).unwrap_err().0, REFUSED);
        std::fs::write(spira_store::lc_call::pilgrim_world_marker(&socket), "").unwrap();
        assert!(!super::socket_is_a_pilgrim_world(&socket), "a marker beside a socket outside any sim world is not honoured");
        assert_eq!(gate_in("file", &file, || super::socket_is_a_pilgrim_world(&socket)).unwrap_err().0, REFUSED);
        std::fs::write(t.join(spira_store::lc_call::SIM_WORLD_MARKER), "").unwrap();
        assert!(super::socket_is_a_pilgrim_world(&socket));
        assert!(gate_in("file", &file, || super::socket_is_a_pilgrim_world(&socket)).is_ok());
    }

    #[test_id(thcbh0r)]
    #[test]
    fn the_pilgrims_file_plans_a_bead_and_its_lifecycle_row_in_the_named_repo() {
        let mut c = call(&["a defect", "--for", "builder", "--repo", "spira", "--body-file", "-"], "pilgrim");
        c.stdin = Some("body".into());
        assert_eq!(
            plan("file", "-", &c).unwrap(),
            vec![
                Step::Tool { program: "bead.sh", args: v(&["file", "a defect", "--for", "builder", "--repo", "spira", "--body-file", "-"]), stdin: Some("body".into()) },
                Step::CreateRow { repo: s("spira") },
            ]
        );
        assert!(plan("file", "-", &call(&["a defect", "--for", "builder"], "pilgrim")).is_err(), "--for needs --repo");
    }

    #[test_id(tgugcxd)]
    #[test]
    fn the_pilgrims_dep_add_links_a_blocks_edge_through_bead_sh() {
        assert_eq!(plan("dep-add", "-", &call(&["sp-a1", "sp-b2"], "pilgrim")).unwrap(), vec![tool("bead.sh", &["dep", "add", "sp-a1", "sp-b2"])]);
        assert!(plan("dep-add", "-", &call(&["sp-a1"], "pilgrim")).is_err(), "a second bead is required");
    }

    #[test_id(t0b5x6c)]
    #[test]
    fn the_pilgrims_close_other_closes_through_the_groomer_with_its_evidence() {
        assert_eq!(
            plan("close-other", "-", &call(&["sp-b2", "--evidence", "walked"], "pilgrim")).unwrap(),
            vec![tool("groomer", &["close", "sp-b2", "--evidence", "walked"])]
        );
        assert!(plan("close-other", "-", &call(&["sp-b2"], "pilgrim")).is_err(), "evidence is required");
    }

    #[test_id(t06fgcc)]
    #[test]
    fn the_pilgrim_files_links_and_closes_only_in_a_pilgrim_world() {
        let calls: [(&str, Vec<String>); 3] = [
            ("file", v(&["a title", "--for", "builder", "--repo", "r", "--actor", "pilgrim"])),
            ("dep-add", v(&["sp-a1", "sp-b2", "--actor", "pilgrim"])),
            ("close-other", v(&["sp-b2", "--evidence", "e", "--actor", "pilgrim"])),
        ];
        for (verb, args) in &calls {
            assert_eq!(gate_in(verb, args, || false).unwrap_err().0, REFUSED, "{verb} on a production store");
            assert!(gate_in(verb, args, || true).is_ok(), "{verb} in a pilgrim world");
        }
    }

    
    #[test_id(ty3jdm9)]
    #[test]
    fn the_pilgrim_claims_a_bead_only_in_a_pilgrim_world_and_the_service_answers_it_itself() {
        let args = v(&["sp-a1", "--actor", "pilgrim"]);
        assert_eq!(gate_in("claim", &args, || false).unwrap_err().0, REFUSED, "claim on a production store");
        assert!(gate_in("claim", &args, || true).is_ok());
        assert_eq!(gate_in("claim", &v(&["sp-a1", "--actor", "builder"]), || true).unwrap_err().0, REFUSED, "a persona's own claim comes from the dispatcher");
        assert_eq!(plan("claim", "-", &call(&["sp-a1"], "pilgrim")).unwrap(), vec![Step::Claim { bead: s("sp-a1") }]);
        assert!(plan("claim", "-", &call(&[], "pilgrim")).is_err(), "a bead is required");
        assert!(!exec_verb("claim") && brokers_tool("claim"));
    }

    
    #[test_id(tjzwvk3)]
    #[test]
    fn the_pilgrims_claim_is_a_builders_so_its_submit_leaves_working_by_the_builders_exits() {
        let BeadEventKind::Claim { holder, persona, lease_until, .. } = pilgrim_claim() else { panic!("not a claim") };
        assert_eq!((holder.as_str(), persona.as_deref()), ("pilgrim", Some("builder")));
        assert!(lease_until > crate::db::now_epoch());
    }

    #[test_id(tz0g2n9)]
    #[test]
    fn a_pilgrim_world_widens_nothing_but_the_pilgrims_own_verbs() {
        assert_eq!(gate_in("dep-add", &v(&["sp-a1", "sp-b2", "--actor", "builder"]), || true).unwrap_err().0, REFUSED);
        assert_eq!(gate_in("label-add", &v(&["sp-a1", "x", "--actor", "pilgrim"]), || true).unwrap_err().0, REFUSED);
        assert_eq!(gate_in("reopen", &v(&["sp-a1", "--evidence", "e", "--actor", "pilgrim"]), || true).unwrap_err().0, REFUSED);
    }

    #[test_id(tkt551o)]
    #[test]
    fn the_allowed_persona_passes() {
        assert!(gate("groom", &v(&["close", "sp-x1", "--evidence", "e", "--actor", "groomer"])).is_ok());
        assert!(gate("queue", &v(&["eject", "sp-x1", "--actor", "czar"])).is_ok());
        assert!(gate("census", &v(&["--with-suppressed", "--actor", "mimir"])).is_ok());
    }

    #[test_id(t502y9y)]
    #[test]
    fn a_tool_subcommand_absent_from_the_table_is_refused_for_everyone() {
        assert_eq!(gate("queue", &v(&["flush", "--actor", "czar"])).unwrap_err().0, REFUSED);
        assert_eq!(gate("groom", &v(&["--actor", "groomer"])).unwrap_err().0, REFUSED);
    }

    #[test_id(t5zv3nl)]
    #[test]
    fn hel_is_a_read_only_dry_run_for_ops_alone() {
        let ok = |xs: &[&str], who: &str| plan("hel", "-", &call(xs, who));
        assert_eq!(ok(&["--dry-run"], "ops").unwrap(), vec![tool("hel", &["--dry-run"])]);
        assert!(ok(&["--dry-run", "--no-fetch"], "ops").is_ok());
        for bad in [&["--all"][..], &[][..], &["sp-x1"][..], &["--dry-run", "--all"][..], &["reap-stale"][..]] {
            assert!(ok(bad, "ops").is_err(), "{bad:?}");
        }
        assert_eq!(gate("hel", &v(&["--dry-run", "--actor", "builder"])).unwrap_err().0, REFUSED);
        assert!(gate("hel", &v(&["--dry-run", "--actor", "ops"])).is_ok());
    }

    #[test_id(t1zww9t)]
    #[test]
    fn bound_verbs_need_no_persona() {
        assert!(gate("show", &[]).is_ok());
        assert!(gate("note", &v(&["hi"])).is_ok());
    }

    #[test_id(te6euu6)]
    #[test]
    fn a_second_actor_is_refused() {
        assert_eq!(gate("note-on", &v(&["sp-x1", "--actor", "czar", "--actor", "builder"])).unwrap_err().0, REFUSED);
    }

    #[test_id(tvktxfp)]
    #[test]
    fn split_reserved_takes_the_trailer_only() {
        let c = split_reserved(&v(&["a", "-", "--stdin", "body", "--actor", "ops"]));
        assert_eq!(c.args, v(&["a", "-"]));
        assert_eq!(c.stdin.as_deref(), Some("body"));
        assert_eq!(c.actor.as_deref(), Some("ops"));
    }

    // ---- reads ----

    #[test_id(t8s63l8)]
    #[test]
    fn read_shows_another_bead() {
        assert_eq!(plan("read", "sp-me", &call(&["sp-x1", "--json"], "spike")).unwrap(), vec![Step::Read { id: s("sp-x1"), json: true }]);
        assert_eq!(plan("read", "sp-me", &call(&["sp-x1"], "spike")).unwrap(), vec![Step::Read { id: s("sp-x1"), json: false }]);
        assert!(plan("read", "sp-me", &call(&["not-an-id"], "spike")).is_err());
        assert!(plan("read", "sp-me", &call(&["sp-x1", "-C", "elsewhere"], "spike")).is_err());
    }

    #[test_id(tbaq31l)]
    #[test]
    fn a_work_list_asks_the_store_for_bare_lifecycle_rows() {
        assert_eq!(lifecycle_list_args(&[], false), ["list", "--live", "--bare"]);
        assert_eq!(lifecycle_list_args(&["READY".into()], false), ["list", "--state", "READY", "--cached", "--bare"]);
        assert_eq!(lifecycle_list_args(&[], true), ["list", "--cached", "--bare"]);
    }

    #[test_id(tm79n13)]
    #[test]
    fn list_filters_by_lifecycle_state_and_content() {
        assert_eq!(
            plan("list", "-", &call(&["--state", "ready,rework", "--label", "plan", "--json"], "warden")).unwrap(),
            vec![Step::LcList { states: v(&["READY", "REWORK"]), all: false, filters: v(&["--label", "plan"]), json: true, full: false, limit: None }]
        );
        assert!(plan("list", "-", &call(&["--db", "x"], "warden")).is_err());
        assert!(plan("list", "-", &call(&["--state", "closed"], "warden")).is_err(), "a bd status word is not a lifecycle state");
        assert!(plan("list", "-", &call(&["--limit", "many"], "warden")).is_err());
    }

    #[test_id(tq54smt)]
    #[test]
    fn list_json_projects_unless_full() {
        let Ok(Step::LcList { full, .. }) = plan("list", "-", &call(&["--json", "--full"], "groomer")).map(|mut p| p.remove(0)) else { panic!("not a list") };
        assert!(full);
        let row = serde_json::json!({"id": "sp-a", "title": "t", "description": "d".repeat(5000), "notes": "n", "labels": ["x"], "lifecycle_state": "READY", "lifecycle_holds": []});
        let p = project_row(&row);
        assert_eq!(p["title"], "t");
        assert_eq!(p["lifecycle_state"], "READY");
        assert!(p.get("description").is_none() && p.get("notes").is_none());
        let many: Vec<_> = (0..400).map(|_| project_row(&row)).collect();
        assert!(serde_json::to_string(&many).unwrap().len() <= 64 * 1024, "four hundred open beads must still fit the 64 KiB reply bound");
        assert!(serde_json::to_string(&row).unwrap().len() * 400 > 64 * 1024, "the fixture's full list must exceed the bound");
    }

    #[test_id(t9brk7a)]
    #[test]
    fn list_refuses_bds_status_and_names_the_lifecycle_flag() {
        let e = plan("list", "-", &call(&["--status", "open"], "warden")).unwrap_err();
        assert!(e.1.contains("--state"), "{}", e.1);
    }

    #[test_id(tvpsjtp)]
    #[test]
    fn show_replaces_bds_status_word_with_the_lifecycle_state() {
        let bd = "○ sp-1 · title   [P2 · OPEN]\nOwner: x\n";
        let row = r#"{"bead":{"state":"SUBMITTED","holds":"[\"wait\"]","reason":"snooze-until:9","holder":null},"delivery":null}"#;
        let out = with_lifecycle(bd, row);
        assert!(out.starts_with("○ sp-1 · title   [P2 · SUBMITTED]\n"), "{out}");
        assert!(!out.contains("OPEN"), "{out}");
        assert!(out.contains("LIFECYCLE: SUBMITTED\n  holds:"), "{out}");
        assert!(out.contains("  reason: snooze-until:9\n"), "{out}");
        assert!(!out.contains("holder"), "{out}");
    }


    fn red_row(at: &str, digest: &str) -> serde_json::Value {
        serde_json::json!({"issue_id": "sp-1", "event_type": "eject-failure", "new_value": digest, "actor": "batcher", "created_at": at})
    }

    #[test_id(ttfpb0e)]
    #[test]
    fn last_red_prints_only_the_newest_digest_verbatim_after_the_lifecycle_block() {
        let shown = "t\n\nLIFECYCLE: SUBMITTED\n  tip: bbb\n";
        let rows = [red_row("2026-01-01 00:00:00", "tip aaa\nFAIL old"), red_row("2026-01-02 00:00:00", "tip bbb\nFAIL new_test at x.rs:1\n  boom")];
        let out = with_last_red(shown, &rows, Some("bbb"));
        assert!(out.starts_with(shown), "{out}");
        assert!(out.contains("LAST RED (eject-failure, 2026-01-02 00:00:00 by batcher)\n  tip bbb\n  FAIL new_test at x.rs:1\n    boom\n"), "{out}");
        assert!(!out.contains("old"), "{out}");
    }

    #[test_id(t9pn63d)]
    #[test]
    fn last_red_leaves_the_text_alone_when_no_red_was_recorded() {
        assert_eq!(with_last_red("t\n", &[], Some("x")), "t\n");
    }

    #[test_id(ti0bsxx)]
    #[test]
    fn last_red_says_when_the_judged_tip_is_not_the_beads_tip() {
        let rows = [red_row("2026-01-02 00:00:00", "tip aaa\nFAIL t")];
        assert!(with_last_red("t\n", &rows, Some("bbb")).contains("judged tip aaa; the bead's tip is now bbb\n"));
        assert!(!with_last_red("t\n", &rows, Some("aaa")).contains("judged tip"));
    }

    #[test_id(tz3zsrt)]
    #[test]
    fn show_composes_the_last_red_after_the_lifecycle_with_the_rows_tip() {
        let row = r#"{"bead":{"state":"SUBMITTED","tip":"bbb"},"delivery":null}"#;
        let red = [red_row("2026-01-02 00:00:00", "tip aaa\nFAIL t")];
        let out = show_text("○ sp-1 · t   [P2 · OPEN]\n", row, Ok(red.to_vec()));
        let (life, last) = (out.find("LIFECYCLE: SUBMITTED").unwrap(), out.find("LAST RED (").unwrap());
        assert!(life < last && out.contains("judged tip aaa; the bead's tip is now bbb"), "{out}");
        assert_eq!(show_text("t\n", row, Ok(vec![])), with_lifecycle("t\n", row));
    }

    #[test_id(t3dj6n2)]
    #[test]
    fn show_survives_a_failed_fact_read_with_one_line() {
        let row = r#"{"bead":{"state":"SUBMITTED"},"delivery":null}"#;
        let out = show_text("t\n", row, Err("db\n down".into()));
        assert!(out.starts_with(&with_lifecycle("t\n", row)) && out.ends_with("LAST RED: cannot tell: db down\n"), "{out}");
    }

    fn logs_fixture() -> (testkit::TempDir, Vec<serde_json::Value>) {
        let run = testkit::TempDir::new("work-logs");
        let id = spira_config::sendback::keep(run.path(), "gate", &[("trial.out".to_string(), b"FAIL test-b".to_vec())], 7, 1000).unwrap();
        let rows = vec![serde_json::json!({"issue_id": "sp-1", "event_type": LOGS_KIND, "new_value": id, "actor": "gate", "at": "1000"})];
        (run, rows)
    }

    #[test_id(tsdsnin)]
    #[test]
    fn logs_lists_the_entry_and_its_files_and_prints_one_of_them() {
        let (run, rows) = logs_fixture();
        let (code, out) = logs_out(&rows, run.path(), &[]);
        assert_eq!(code, 0);
        assert!(out.contains("gate gate-1000-0") && out.contains("trial.out 11 bytes"), "{out}");
        assert_eq!(logs_out(&rows, run.path(), &[s("gate-1000-0"), s("trial.out")]), (0, s("FAIL test-b")));
    }

    #[test_id(tcy4iaf)]
    #[test]
    fn logs_refuses_an_entry_the_bead_has_no_fact_for() {
        let (run, rows) = logs_fixture();
        let other = spira_config::sendback::keep(run.path(), "gate", &[("trial.out".to_string(), b"x".to_vec())], 7, 2000).unwrap();
        let (code, out) = logs_out(&rows, run.path(), &[other, s("trial.out")]);
        assert_eq!(code, REFUSED, "{out}");
    }

    #[test_id(tehn52v)]
    #[test]
    fn logs_lists_the_newest_entry_first() {
        let rows: Vec<serde_json::Value> = [("a-1-0", "1"), ("b-2-0", "2")]
            .iter()
            .map(|(id, at)| serde_json::json!({"issue_id": "sp-1", "event_type": LOGS_KIND, "new_value": id, "actor": "x", "at": at}))
            .collect();
        let ids: Vec<String> = entries_of(&rows).into_iter().map(|e| e.2).collect();
        assert_eq!(ids, ["b-2-0", "a-1-0"]);
    }

    #[test_id(ti7yjjr)]
    #[test]
    fn show_names_work_logs_when_the_bead_has_kept_entries() {
        assert!(with_logs(s("T\n"), 1).ends_with("LOGS: 1 kept — work logs\n"));
        assert_eq!(with_logs(s("T\n"), 0), "T\n");
    }

    fn scripted<'a>(answer: &str, seen: &'a std::cell::RefCell<Vec<Vec<String>>>) -> impl Fn(&[String]) -> Result<String, content_sql::Failure> + 'a {
        let answer = answer.to_string();
        move |a: &[String]| {
            seen.borrow_mut().push(a.to_vec());
            Ok(answer.clone())
        }
    }

    #[test_id(ta7q2re)]
    #[test]
    fn search_asks_the_reader_once_and_prints_id_priority_title_without_a_status_word() {
        let seen = std::cell::RefCell::new(Vec::new());
        let reader = scripted(r#"[{"id":"sp-a1","title":"merge queue stalls","priority":1,"status":"open"},{"id":"sp-b2","title":"queue docs","priority":3,"status":"closed"}]"#, &seen);
        let step = Step::Search { query: s("merge queue"), limit: Some(s("7")), json: false };
        let (code, out) = run_read(step, &reader);
        assert_eq!((code, out.as_str()), (0, "sp-a1 P1 merge queue stalls\nsp-b2 P3 queue docs"));
        assert_eq!(*seen.borrow(), vec![v(&["search", "merge queue", "--limit", "7"])]);
    }

    #[test_id(t24aagw)]
    #[test]
    fn search_json_passes_the_readers_rows_through_and_a_read_json_is_the_readers_show() {
        let seen = std::cell::RefCell::new(Vec::new());
        let reader = scripted(r#"[{"id":"sp-a1"}]"#, &seen);
        let (_, out) = run_read(Step::Search { query: s("q"), limit: None, json: true }, &reader);
        assert_eq!(out, r#"[{"id":"sp-a1"}]"#);
        let (_, out) = run_read(Step::Read { id: s("sp-a1"), json: true }, &reader);
        assert_eq!(out, r#"[{"id":"sp-a1"}]"#);
        assert_eq!(*seen.borrow(), vec![v(&["search", "q"]), v(&["show", "sp-a1"])]);
    }

    #[test_id(ts6ojdw)]
    #[test]
    fn a_reader_failure_is_cannot_tell_with_its_text_and_nothing_else_is_tried() {
        let reader = |_: &[String]| -> Result<String, content_sql::Failure> { Err(content_sql::Failure::Store("the held connection is gone".into())) };
        for step in [Step::Search { query: s("q"), limit: None, json: false }, Step::Read { id: s("sp-a1"), json: true }] {
            let (code, out) = run_read(step, &reader);
            assert_eq!(code, CANNOT_TELL);
            assert!(out.contains("the held connection is gone"), "{out}");
        }
    }

    #[test_id(tz1bdlu)]
    #[test]
    fn a_search_reply_that_is_not_json_is_cannot_tell_and_a_missing_priority_is_a_question_mark() {
        let seen = std::cell::RefCell::new(Vec::new());
        let step = || Step::Search { query: s("q"), limit: None, json: false };
        let (code, _) = run_read(step(), &scripted("not json", &seen));
        assert_eq!(code, CANNOT_TELL);
        let (code, out) = run_read(step(), &scripted(r#"[{"id":"sp-a1","title":"t"}]"#, &seen));
        assert_eq!((code, out.as_str()), (0, "sp-a1 P? t"));
    }

    #[test_id(t50m1us)]
    #[test]
    fn a_filtered_list_asks_the_reader_once_per_id_chunk_and_never_forks_bd() {
        let ids: Vec<String> = (0..LIST_ID_CHUNK + 1).map(|i| format!("sp-{i:04}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let seen = std::cell::RefCell::new(Vec::new());
        let reader = scripted("[]", &seen);
        for chunk in refs.chunks(LIST_ID_CHUNK) {
            content_chunk(&reader, chunk, &v(&["--label", "repo:spira"]), false).unwrap();
        }
        let seen = seen.borrow();
        assert_eq!(seen.len(), 2, "401 ids are two requests");
        assert_eq!(seen[0][..6], v(&["list", "--all", "--limit", "0", "--id", &ids[..LIST_ID_CHUNK].join(",")])[..]);
        assert_eq!(seen[0][6..], v(&["--label", "repo:spira", "--brief"])[..]);
        assert_eq!(seen[1][5], ids[LIST_ID_CHUNK]);
    }

    #[test_id(trnoit6)]
    #[test]
    fn a_full_list_asks_for_the_whole_row() {
        let seen = std::cell::RefCell::new(Vec::new());
        let reader = scripted("[]", &seen);
        content_chunk(&reader, &["sp-a"], &[], true).unwrap();
        assert!(!seen.borrow()[0].contains(&s("--brief")));
    }

    #[test_id(tvxk8ky)]
    #[test]
    fn search_needs_a_query() {
        assert_eq!(plan("search", "-", &call(&["merge queue"], "groomer")).unwrap(), vec![Step::Search { query: s("merge queue"), limit: None, json: false }]);
        assert_eq!(
            plan("search", "-", &call(&["merge queue", "--limit", "5", "--json"], "groomer")).unwrap(),
            vec![Step::Search { query: s("merge queue"), limit: Some(s("5")), json: true }]
        );
        assert!(plan("search", "-", &call(&["--status", "open"], "groomer")).is_err());
    }

    #[test_id(t6d91g4)]
    #[test]
    fn open_items_names_its_repo_and_refuses_without_one_with_the_usage_line() {
        assert_eq!(
            plan("open-items", "-", &call(&["--repo", "spira", "--exclude", "sp-a"], "skuld")).unwrap(),
            vec![Step::OpenItems { repo: s("spira"), exclude: v(&["sp-a"]), json: false }]
        );
        assert_eq!(
            plan("open-items", "-", &call(&["--json", "--exclude", "sp-a1,sp-b2", "--repo", "spira"], "skuld")).unwrap(),
            vec![Step::OpenItems { repo: s("spira"), exclude: v(&["sp-a1", "sp-b2"]), json: true }]
        );
        for args in [&[][..], &["--exclude", "sp-a"], &["--repo", ""], &["--repo", "--json"]] {
            let (code, out) = plan("open-items", "-", &call(args, "skuld")).unwrap_err();
            assert_eq!(code, CANNOT_TELL, "{args:?}");
            assert!(out.contains("work open-items --repo <name> [--exclude <id,…>] [--json]"), "{args:?}: {out}");
        }
        assert!(plan("open-items", "-", &call(&["--repo", "spira", "--exclude", "nope"], "skuld")).is_err());
    }

    // ---- writes ----

    #[test_id(tysrdp8)]
    #[test]
    fn note_on_takes_text_or_stdin() {
        assert_eq!(plan("note-on", "-", &call(&["sp-x1", "hi"], "czar")).unwrap(), vec![Step::Bd { args: v(&["note", "sp-x1", "hi"]), stdin: None }]);
        let c = split_reserved(&v(&["sp-x1", "-", "--stdin", "long `prose`", "--actor", "czar"]));
        assert_eq!(plan("note-on", "-", &c).unwrap(), vec![Step::Bd { args: v(&["note", "sp-x1", "--stdin"]), stdin: Some("long `prose`".into()) }]);
        assert!(plan("note-on", "-", &call(&["sp-x1", "-"], "czar")).is_err(), "- with nothing on stdin");
        assert!(plan("note-on", "-", &call(&["sp-x1"], "czar")).is_err());
    }

    #[test_id(tq950mj)]
    #[test]
    fn labels_are_one_label_and_never_poison() {
        assert_eq!(plan("label-add", "-", &call(&["sp-x1", "overseer"], "groomer")).unwrap(), vec![Step::Bd { args: v(&["label", "add", "sp-x1", "overseer"]), stdin: None }]);
        assert_eq!(
            plan("label-remove", "-", &call(&["sp-x1", "lane:foo"], "groomer")).unwrap(),
            vec![Step::Bd { args: v(&["label", "remove", "sp-x1", "lane:foo"]), stdin: None }]
        );
        assert!(plan("label-add", "-", &call(&["sp-x1", "a,b"], "groomer")).is_err());
        assert_eq!(plan("label-remove", "-", &call(&["sp-x1", POISON_LABEL], "groomer")).unwrap_err().0, REFUSED);
    }

    #[test_id(tbp8xhr)]
    #[test]
    fn dep_add_goes_through_bead_sh_and_checks_the_type() {
        assert_eq!(plan("dep-add", "-", &call(&["sp-a1", "sp-b2"], "ops")).unwrap(), vec![tool("bead.sh", &["dep", "add", "sp-a1", "sp-b2"])]);
        assert!(plan("dep-add", "-", &call(&["sp-a1", "sp-b2", "--type", "weird"], "ops")).is_err());
        assert!(plan("dep-add", "-", &call(&["sp-a1"], "ops")).is_err());
    }

    #[test_id(tz7vej0)]
    #[test]
    fn relate_is_a_relates_edge() {
        assert_eq!(plan("relate", "-", &call(&["sp-a1", "sp-b2"], "archivist")).unwrap(), vec![Step::Bd { args: v(&["dep", "relate", "sp-a1", "sp-b2"]), stdin: None }]);
    }

    #[test_id(tyxocpp)]
    #[test]
    fn reopen_and_close_other_need_evidence() {
        assert!(plan("reopen", "-", &call(&["sp-a1"], "czar")).is_err());
        assert_eq!(
            plan("reopen", "-", &call(&["sp-a1", "--evidence", "red on main"], "czar")).unwrap(),
            vec![
                Step::Tool { program: "spira-lc", args: v(&["reopen", "sp-a1", "operator-reopen", "czar"]), stdin: None },
                Step::Bd { args: v(&["note", "sp-a1", "--stdin"]), stdin: Some("reopened by czar: red on main".into()) },
            ]
        );
        assert!(plan("close-other", "-", &call(&["sp-a1"], "czar")).is_err());
        assert_eq!(
            plan("close-other", "-", &call(&["sp-a1", "--evidence", "duplicate of sp-b2"], "czar")).unwrap(),
            vec![tool("groomer", &["close", "sp-a1", "--evidence", "duplicate of sp-b2"])]
        );
    }

    #[test_id(trua6zr)]
    #[test]
    fn close_ask_is_the_archivists_and_closes_only_on_a_quote() {
        let ok = call(&["sp-ask1", "--exit", "answered", "--source", "t41", "--body-file", "-"], "archivist");
        let mut ok = ok;
        ok.stdin = Some("delete them\n".into());
        assert_eq!(
            plan("close-ask", "-", &ok).unwrap(),
            vec![tool("spira-lc", &["close-ask", "sp-ask1", "--exit", "answered", "--quote", "delete them", "--actor", "archivist", "--channel", "transcript", "--message-id", "t41"])]
        );
        let mut no_source = call(&["sp-ask1", "--exit", "answered", "--body-file", "-"], "archivist");
        no_source.stdin = Some("yes".into());
        assert!(plan("close-ask", "-", &no_source).is_err());
        assert!(plan("close-ask", "-", &call(&["sp-ask1", "--exit", "answered", "--source", "t1", "--body-file", "-"], "archivist")).is_err());
        let mut dflt = call(&["sp-ask1", "--exit", "default", "--body-file", "-"], "archivist");
        dflt.stdin = Some("x".into());
        assert!(plan("close-ask", "-", &dflt).is_err());
        let mut gone = call(&["sp-ask1", "--exit", "withdrawn", "--body-file", "-"], "archivist");
        gone.stdin = Some("decided by law-x".into());
        assert_eq!(
            plan("close-ask", "-", &gone).unwrap(),
            vec![tool("spira-lc", &["close-ask", "sp-ask1", "--exit", "withdrawn", "--quote", "decided by law-x", "--actor", "archivist"])]
        );
    }

    #[test_id(thqvfs1)]
    #[test]
    fn file_for_a_persona_also_creates_the_lifecycle_row() {
        let c = split_reserved(&v(&["a title", "--for", "builder", "--repo", "spira", "--body-file", "-", "--stdin", "body", "--actor", "warden"]));
        assert_eq!(
            plan("file", "-", &c).unwrap(),
            vec![
                Step::Tool { program: "bead.sh", args: v(&["file", "a title", "--for", "builder", "--repo", "spira", "--body-file", "-"]), stdin: Some("body".into()) },
                Step::CreateRow { repo: "spira".into() }
            ]
        );
        let rec = plan("file", "-", &call(&["a finding", "--kind", "insight", "--repo", "spira"], "archivist")).unwrap();
        assert_eq!(rec.len(), 1, "a record is no work: no lifecycle row");
    }

    #[test_id(tugqqky)]
    #[test]
    fn the_archivist_files_no_work_bead_while_a_swept_session_is_live() {
        let dir = testkit::TempDir::new("lc-live");
        let work = call(&["t", "--for", "builder", "--repo", "spira"], "archivist");
        let record = call(&["t", "--kind", "insight", "--repo", "spira"], "archivist");
        let other = call(&["t", "--for", "builder", "--repo", "spira"], "ops");
        assert!(live_session_refusal("file", &work, &dir).is_none(), "no live marker: no refusal");
        std::fs::write(dir.join("sess-1.live"), "").unwrap();
        let (code, msg) = live_session_refusal("file", &work, &dir).expect("a live sweep must refuse a work bead");
        assert_eq!(code, REFUSED);
        assert!(msg.contains("work note-on") && msg.contains("work ask"), "names its exit: {msg}");
        assert!(live_session_refusal("file", &record, &dir).is_none(), "a record is not work");
        assert!(live_session_refusal("file", &other, &dir).is_none(), "only the archivist is bound");
        assert!(live_session_refusal("note-on", &work, &dir).is_none());
    }

    #[test_id(t9ya7lu)]
    #[test]
    fn file_refuses_a_bad_shape() {
        assert!(plan("file", "-", &call(&["t"], "ops")).is_err(), "neither --for nor --kind");
        assert!(plan("file", "-", &call(&["t", "--for", "builder", "--kind", "insight", "--repo", "r"], "ops")).is_err());
        assert!(plan("file", "-", &call(&["t", "--for", "builder"], "ops")).is_err(), "no repo");
        assert!(plan("file", "-", &call(&["t", "--for", "builder", "--repo", "r", "--body-file", system_paths::ETC_PASSWD], "ops")).is_err());
        assert!(plan("file", "-", &call(&["t", "--for", "builder", "--repo", "r", "-l", "x"], "ops")).is_err());
    }

    #[test_id(tkq5m9f)]
    #[test]
    fn a_dry_run_ask_forwards_the_flag_and_places_no_hold() {
        let p = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "question", "--default", "d", "--dry-run"], "builder")).unwrap();
        assert_eq!(p.len(), 1, "no AskHold: {p:?}");
        let Step::Ask { args, .. } = &p[0] else { panic!() };
        assert!(args.contains(&s("--dry-run")), "{args:?}");
    }

    #[test_id(tbrg284)]
    #[test]
    fn an_ask_whose_question_is_a_flag_name_is_refused_with_the_usage() {
        for subject in ["--default", "", "  "] {
            let e = plan("ask", "sp-me1", &call(&["--subject", subject, "--kind", "question", "--default", "x"], "mender")).unwrap_err();
            assert!(e.1.contains("usage: work ask --subject"), "{e:?}");
        }
        assert!(plan("ask", "sp-me1", &call(&["--subject", "May I?", "--kind", "question", "--default", "x"], "mender")).is_ok());
    }

    #[test_id(tmrko1u)]
    #[test]
    fn a_question_that_is_empty_or_a_flag_name_is_not_a_question() {
        for q in ["--default", "--default x", "", "  "] {
            assert!(not_a_question(q).is_some(), "{q:?}");
        }
        assert!(not_a_question("May I?").is_none());
    }

    #[test_id(tucck2n)]
    #[test]
    fn ask_builds_the_mail_with_the_persona_as_sender() {
        let p = plan("ask", "sp-me1", &call(&["--subject", "Close sp-a1?", "--kind", "question", "--default", "yes"], "groomer")).unwrap();
        let Step::Ask { args, stdin, classify_as } = &p[0] else { panic!() };
        assert_eq!(args[..4], v(&["send", "operator", "--from", "Groomer <groomer@spira>"])[..]);
        assert!(args.ends_with(&v(&["--bead", "sp-me1"])), "cites the bound bead by default: {args:?}");
        assert!(stdin.contains("## Question\nClose sp-a1?") && stdin.contains("## Default\nyes"));
        assert_eq!(classify_as, "sp-me1");
        assert!(plan("ask", "-", &call(&["--subject", "q", "--kind", "question"], "ops")).is_err(), "no default");
        assert!(plan("ask", "-", &call(&["--subject", "q", "--kind", "bogus", "--default", "d"], "ops")).is_err());
        assert!(plan("ask", "-", &call(&["--subject", "q", "--kind", "fyi", "--default", "d", "--from", "Ryan"], "ops")).is_err(), "the sender is the broker's");
        assert_eq!(display_name("batcher"), "Judge");
    }

    /// sp-v62vn follow-up: a restricted model asks through `work ask`; a question from a
    /// bound session also places the `ask` hold on the bound bead's row — the record dwarf's
    /// teardown reads to release the session operator-wait (the marker mail wrote is gone).
    #[test_id(t9gxrmw)]
    #[test]
    fn a_bound_question_holds_the_bound_bead_after_the_mail() {
        let p = plan("ask", "sp-me1", &call(&["--subject", "May I?", "--kind", "question", "--default", "no"], "builder")).unwrap();
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(matches!(&p[0], Step::Ask { .. }), "the question is delivered first: {p:?}");
        assert_eq!(p[1], Step::AskHold { bead: s("sp-me1"), question: s("May I?") });
        let p = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "question", "--default", "d", "--bead", "sp-me1"], "builder")).unwrap();
        assert_eq!(p.last(), Some(&Step::AskHold { bead: s("sp-me1"), question: s("q") }), "citing its own bead explicitly");
        // No hold for a question about another bead (the groomer's "Close <id>?"), for an
        // fyi, nor for an unbound caller (nothing of its own to wait on).
        let other = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "question", "--default", "d", "--bead", "sp-other"], "groomer")).unwrap();
        assert!(!other.iter().any(|x| matches!(x, Step::AskHold { .. })), "{other:?}");
        let fyi = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "note", "--default", "d"], "builder")).unwrap();
        assert!(!fyi.iter().any(|x| matches!(x, Step::AskHold { .. })), "{fyi:?}");
        let unbound = plan("ask", "-", &call(&["--subject", "q", "--kind", "question", "--default", "d"], "archivist")).unwrap();
        assert!(!unbound.iter().any(|x| matches!(x, Step::AskHold { .. })), "{unbound:?}");
    }

    /// sp-v62vn follow-up: the broker's `mail` ran without BEAD_ID, which mail reads as "not
    /// a dwarf", so its escalation-class check never ran and an unclassed question went
    /// straight to the operator. Every ask step now carries the id mail classifies by —
    /// bound, citing another bead, or bound to nothing at all.
    #[test_id(tm5wk41)]
    #[test]
    fn every_ask_is_sent_for_classification() {
        let classify = |bound: &str, extra: &[&str]| {
            let args = [&["--subject", "q", "--kind", "question", "--default", "d"][..], extra].concat();
            match &plan("ask", bound, &call(&args, "builder")).unwrap()[0] {
                Step::Ask { classify_as, .. } => classify_as.clone(),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(classify("sp-me1", &[]), "sp-me1");
        assert_eq!(classify("sp-me1", &["--bead", "sp-other"]), "sp-other");
        assert_eq!(classify("-", &[]), "-", "an unbound caller is a persona asking too");
        // and --class rides through for mail to judge, never decided here
        let p = plan("ask", "sp-me1", &call(&["--subject", "q", "--kind", "question", "--default", "d", "--class", "policy"], "builder")).unwrap();
        let Step::Ask { args, .. } = &p[0] else { panic!() };
        assert!(args.windows(2).any(|w| w == v(&["--class", "policy"])), "{args:?}");
    }

    #[test_id(tqcggaa)]
    #[test]
    fn ask_kinds_are_the_kinds_mail_can_deliver() {
        let dir = spira_config::resolve::locate_home_for_process().unwrap().join("mail/kinds");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path().file_stem().unwrap().to_string_lossy().into_owned())
            .filter(|k| k != "report")
            .collect();
        on_disk.sort();
        assert_eq!(on_disk, MAIL_KINDS);
        assert!(plan("ask", "-", &call(&["--subject", "q", "--kind", "fyi", "--default", "d"], "ops")).is_err(), "fyi has no template");
    }

    // ---- tools ----

    #[test_id(tbdhlys)]
    #[test]
    fn tools_forward_their_subcommand_and_stdin() {
        let c = split_reserved(&v(&["file", "a finding", "-", "--stdin", "payload", "--actor", "ops"]));
        assert_eq!(plan("incident", "-", &c).unwrap(), vec![Step::Tool { program: "incident.sh", args: v(&["file", "a finding", "-"]), stdin: Some("payload".into()) }]);
        assert_eq!(plan("groom", "-", &call(&["sweep"], "groomer")).unwrap(), vec![tool("groomer", &["sweep"])]);
        assert_eq!(plan("queue", "-", &call(&["eject", "sp-a1", "--red"], "czar")).unwrap(), vec![tool("queue", &["eject", "sp-a1", "--red"])]);
        assert_eq!(plan("strand", "-", &call(&["detect-livelocked"], "groomer")).unwrap(), vec![tool("strand", &["detect-livelocked"])]);
        assert_eq!(plan("sop", "-", &call(&["show", "x"], "ops")).unwrap(), vec![tool("sop", &["show", "x"])]);
    }

    #[test_id(tob6541)]
    #[test]
    fn ops_may_collapse_a_duplicate_incident_and_a_builder_may_not() {
        assert!(permitted(&op_key("incident", &v(&["collapse", "sp-a1", "--of", "sp-b2"])), Some("ops"), false).is_ok());
        assert!(permitted("incident collapse", Some("builder"), false).is_err());
        assert_eq!(
            plan("incident", "-", &call(&["collapse", "sp-a1", "--of", "sp-b2"], "ops")).unwrap(),
            vec![Step::Tool { program: "incident.sh", args: v(&["collapse", "sp-a1", "--of", "sp-b2"]), stdin: None }]
        );
    }

    #[test_id(t8sjacd)]
    #[test]
    fn fence_answers_like_czar_fence_sh() {
        assert_eq!(fence_answer("ci-red", Some("act")).0, 0);
        let (code, msg) = fence_answer("ci-red", None);
        assert_eq!(code, 1, "unreadable stage is shadow");
        assert!(msg.contains("SPIRA_CZAR_STAGE_CI_RED=act"), "{msg}");
        assert_eq!(fence_answer("ci-red", Some("loud")).0, 2);
        assert_eq!(fence_answer("../x", Some("act")).0, 2);
        assert_eq!(gate("fence", &v(&["ci-red", "--actor", "groomer"])).unwrap_err().0, REFUSED);
        assert!(gate("fence", &v(&["ci-red", "--actor", "czar"])).is_ok());
    }

    #[test_id(tyn5jnu)]
    #[test]
    fn census_takes_only_its_one_flag() {
        assert_eq!(plan("census", "-", &call(&["--with-suppressed"], "mimir")).unwrap(), vec![tool("census", &["--with-suppressed"])]);
        assert!(plan("census", "-", &call(&["run-events"], "mimir")).is_err());
    }

    #[test_id(t0d0nx4)]
    #[test]
    fn split_piece_never_reaches_bd_with_a_foreign_flag() {
        assert!(plan("groom", "-", &call(&["split-piece", "sp-a1", "piece one", "-p", "2", "-t", "task"], "groomer")).is_ok());
        assert!(plan("groom", "-", &call(&["split-piece", "sp-a1", "piece", "-C", "other/db"], "groomer")).is_err());
        assert!(plan("groom", "-", &call(&["split-piece", "sp-a1", "a", "b"], "groomer")).is_err());
        assert!(plan("groom", "-", &call(&["split-piece", "nope"], "groomer")).is_err());
    }
}
