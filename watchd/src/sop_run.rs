//! Before a row escalates, run the tool a matching SOP names. `Handled` means the Concierge
//! was told the outcome of a passing tool; anything else escalates as it always did, with a
//! line saying what was tried.

use crate::context::Context;
use crate::ops::{Ops, SopMatch, Target, ToolRun};
use crate::paths;
use sop::run_spec::{self, RunSpec};
use std::collections::BTreeMap;

/// Most runs of one SOP for one incident within a row's window.
const MAX_RUNS: usize = 2;
const OUTPUT_LINES: usize = 12;

pub enum Verdict {
    Handled,
    /// Escalate; the note, when there is one, names the SOP and tool that were tried.
    Escalate(Option<String>),
}

/// `bead=`, `batch=` and `repo=` tokens in the incident's lines; the first of each wins.
pub fn incident_fields(payload: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for tok in payload.split_whitespace() {
        let Some((k, v)) = tok.split_once('=') else { continue };
        let v = v.trim_end_matches([',', ';', '.']);
        let clean = !v.is_empty() && !v.starts_with(['-', '.']) && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if clean && run_spec::FIELDS.contains(&k) {
            out.entry(k.to_string()).or_insert_with(|| v.to_string());
        }
    }
    out
}

fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(OUTPUT_LINES)..].join("\n")
}

fn runs_file(ctx: &Context, row: &str) -> std::path::PathBuf {
    paths::watchd_dir(&ctx.run).join(format!("{row}.sop-runs"))
}

/// Entries are `<epoch> <slug> <key>`. Returns how many fall inside the window, and rewrites
/// the file without the ones that do not.
fn runs_in_window(ctx: &Context, row: &str, slug: &str, key: &str, window: i64) -> (usize, Vec<String>) {
    let text = std::fs::read_to_string(runs_file(ctx, row)).unwrap_or_default();
    let live: Vec<String> = text
        .lines()
        .filter(|l| l.split_once(' ').and_then(|(t, _)| t.parse::<i64>().ok()).is_some_and(|t| ctx.now - t < window))
        .map(str::to_string)
        .collect();
    let n = live.iter().filter(|l| l.split(' ').skip(1).eq([slug, key])).count();
    (n, live)
}

fn record_run(ctx: &Context, row: &str, mut live: Vec<String>, slug: &str, key: &str) {
    live.push(format!("{} {slug} {key}", ctx.now));
    let _ = std::fs::create_dir_all(paths::watchd_dir(&ctx.run));
    let _ = std::fs::write(runs_file(ctx, row), live.join("\n") + "\n");
}

fn tried(note: String, runbooks: &[String]) -> Verdict {
    let pointers = if runbooks.is_empty() { String::new() } else { format!("; runbooks that matched: {}", runbooks.join(", ")) };
    Verdict::Escalate(Some(format!("tried before escalating: {note}{pointers}")))
}

fn runnable(ops: &dyn Ops, payload: &str) -> Result<(String, RunSpec, Vec<String>), Verdict> {
    let matches = ops.sop_match(payload).map_err(|e| tried(format!("sop match could not decide: {e}"), &[]))?;
    let (runs, books): (Vec<SopMatch>, Vec<SopMatch>) = matches.into_iter().partition(|m| m.runs);
    let runbooks: Vec<String> = books.into_iter().map(|m| m.slug).collect();
    let slug = match runs.as_slice() {
        [] if runbooks.is_empty() => return Err(Verdict::Escalate(None)),
        [] => return Err(Verdict::Escalate(Some(format!("runbooks that matched: {}", runbooks.join(", "))))),
        [one] => one.slug.clone(),
        many => {
            let names: Vec<&str> = many.iter().map(|m| m.slug.as_str()).collect();
            return Err(tried(format!("several SOPs with a RUN: match ({}); none was run", names.join(", ")), &runbooks));
        }
    };
    let text = ops.sop_show(&slug).map_err(|e| tried(format!("{slug} matched but could not be read: {e}"), &runbooks))?;
    match run_spec::parse(&text) {
        Ok(Some(spec)) => Ok((slug, spec, runbooks)),
        Ok(None) => Err(tried(format!("{slug} matched but declares no RUN:"), &runbooks)),
        Err(e) => Err(tried(format!("{slug} matched but its RUN: is unusable: {e}"), &runbooks)),
    }
}

/// `key` identifies the incident across passes; `payload` is what `sop match` reads.
pub fn attempt(ops: &dyn Ops, ctx: &Context, row: &str, key: &str, payload: &str) -> Verdict {
    let window = match ctx.notify_age() {
        Ok(w) => w,
        Err(e) => return tried(e, &[]),
    };
    let (slug, spec, runbooks) = match runnable(ops, payload) {
        Ok(found) => found,
        Err(v) => return v,
    };
    let fields = incident_fields(payload);
    let argv = match spec.argv(&fields) {
        Ok(a) => a,
        Err(e) => return tried(format!("{slug} matched but {}: {e}", spec.program), &runbooks),
    };
    let ikey = crate::notify::fingerprint(key);
    let (ran, live) = runs_in_window(ctx, row, &slug, &ikey, window);
    if ran >= MAX_RUNS {
        return tried(format!(
            "{slug} ({}) already ran {ran} times for this incident within {window}s; not running it again",
            spec.program
        ), &runbooks);
    }
    record_run(ctx, row, live, &slug, &ikey);

    let (passed, summary, output) = match ops.run_tool(&spec.program, &argv, spec.deadline_secs) {
        ToolRun::Unavailable(why) => return tried(format!("{slug} names {}, which could not run: {why}", spec.program), &runbooks),
        ToolRun::Exited { code: 0, output } => (true, "exit 0".to_string(), output),
        ToolRun::Exited { code, output } => (false, format!("exit code {code}"), output),
        ToolRun::TimedOut { output } => (false, format!("timed out after {}s", spec.deadline_secs), output),
    };
    let target = match fields.get("bead") {
        Some(b) => Target::Bead(b.clone()),
        None => Target::Pass(format!("watchd-{row}-{}", &ikey[..8])),
    };
    let check = if passed { "pass" } else { "fail" };
    let last = tail(&output);
    let command = format!("{} {}", spec.program, argv.join(" "));
    let why = format!("watchd ran `{}` for {row}: {summary}", command.trim());
    if let Err(e) = ops.sop_applied(&slug, &target, check, &why) {
        eprintln!("watchd: could not record {slug} as applied: {e}");
    }
    let body = format!("{why}\n\n{}\n\nlast output:\n{}\n", incident_line(payload), if last.is_empty() { "(none)" } else { &last });
    let (kind, heading, verdict) = if passed { ("note", "Note", "ran") } else { ("alert", "Alert", "FAILED") };
    let subject = format!("watchd: {slug} {verdict} for {row} ({summary})");
    let mailed = ops.mail_concierge(kind, &subject, &format!("## {heading}\n{body}"));
    if let (false, Err(e)) = (passed, &mailed) {
        eprintln!("watchd: could not tell the Concierge {slug} failed: {e}");
    }
    if !passed {
        return tried(format!("{slug}: `{}` {summary}", command.trim()), &runbooks);
    }
    match mailed {
        Ok(()) => Verdict::Handled,
        Err(e) => tried(format!("{slug}: `{}` passed but the Concierge could not be told: {e}", command.trim()), &runbooks),
    }
}

fn incident_line(payload: &str) -> String {
    format!("incident: {}", payload.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::fake::{Fake, ToolRunFake};
    use testkit::TempDir;

    const SOP: &str = "MATCH: FAIL round\nSYMPTOM: s\nCHECK: c\nFIX: f\nRUN: recert --batch {batch}\nDEADLINE: 60";

    fn ctx(run: &str) -> Context {
        Context {
            run: run.to_string(),
            watchers: String::new(),
            watchers_overlay: String::new(),
            conf_file: "spira.conf".into(),
            actionable: crate::filter::DEFAULT.to_string(),
            health_timeout: "10".into(),
            notify_age: "1800".into(),
            id_prefix: String::new(),
            bd: String::new(),
            db: String::new(),
            placeholders: Default::default(),
            systemctl: "systemctl".into(),
            instance: "prod".into(),
            now: 1_790_726_400,
        }
    }

    fn fake(result: ToolRunFake) -> Fake {
        Fake { sops: vec![("sop-recert".into(), SOP.into())], tool_result: Some(result), ..Fake::default() }
    }

    const PLAIN: &str = "MATCH: FAIL\nSYMPTOM: s\nCHECK: c\nFIX: f";

    const PAYLOAD: &str = "FAIL round batch=b42 ended with no verdict";

    #[test]
    fn a_matching_sop_with_a_run_runs_the_tool_records_it_and_tells_the_concierge() {
        let d = TempDir::new("watchd-sop");
        let ops = fake(ToolRunFake::Exit(0, "recertified b42\n".into()));
        let v = attempt(&ops, &ctx(d.path().to_str().unwrap()), "rounds", "k", PAYLOAD);
        assert!(matches!(v, Verdict::Handled));
        assert_eq!(*ops.tool_calls.borrow(), vec![("recert".to_string(), vec!["--batch".to_string(), "b42".to_string()], 60)]);
        assert_eq!(ops.applied.borrow()[0].0, "sop-recert");
        assert_eq!(ops.applied.borrow()[0].2, "pass");
        let mail = ops.concierge_mail.borrow();
        assert_eq!(mail[0].0, "note");
        assert!(mail[0].2.contains("recertified b42"), "{}", mail[0].2);
    }

    #[test]
    fn a_payload_no_sop_matches_escalates_without_a_note_and_runs_nothing() {
        let d = TempDir::new("watchd-sop");
        let ops = fake(ToolRunFake::Exit(0, String::new()));
        let v = attempt(&ops, &ctx(d.path().to_str().unwrap()), "rounds", "k", "everything is fine");
        assert!(matches!(v, Verdict::Escalate(None)));
        assert!(ops.tool_calls.borrow().is_empty());
    }

    #[test]
    fn a_failing_tool_mails_an_alert_and_escalates_naming_the_sop_and_exit_code() {
        let d = TempDir::new("watchd-sop");
        let ops = fake(ToolRunFake::Exit(3, "boom\n".into()));
        let Verdict::Escalate(Some(note)) = attempt(&ops, &ctx(d.path().to_str().unwrap()), "rounds", "k", PAYLOAD) else { panic!("expected an escalation") };
        assert!(note.contains("sop-recert") && note.contains("exit code 3") && note.contains("recert"), "{note}");
        assert_eq!(ops.applied.borrow()[0].2, "fail");
        assert_eq!(ops.concierge_mail.borrow()[0].0, "alert");
        assert!(ops.concierge_mail.borrow()[0].2.contains("boom"));
    }

    #[test]
    fn a_tool_that_times_out_escalates_as_a_failure() {
        let d = TempDir::new("watchd-sop");
        let ops = fake(ToolRunFake::Timeout(String::new()));
        let Verdict::Escalate(Some(note)) = attempt(&ops, &ctx(d.path().to_str().unwrap()), "rounds", "k", PAYLOAD) else { panic!("expected an escalation") };
        assert!(note.contains("timed out after 60s"), "{note}");
    }

    #[test]
    fn a_third_occurrence_within_the_window_escalates_instead_of_running() {
        let d = TempDir::new("watchd-sop");
        let c = ctx(d.path().to_str().unwrap());
        let ops = fake(ToolRunFake::Exit(0, String::new()));
        assert!(matches!(attempt(&ops, &c, "rounds", "k", PAYLOAD), Verdict::Handled));
        assert!(matches!(attempt(&ops, &c, "rounds", "k", PAYLOAD), Verdict::Handled));
        let Verdict::Escalate(Some(note)) = attempt(&ops, &c, "rounds", "k", PAYLOAD) else { panic!("expected an escalation") };
        assert!(note.contains("already ran 2 times"), "{note}");
        assert_eq!(ops.tool_calls.borrow().len(), 2);
        let mut later = ctx(d.path().to_str().unwrap());
        later.now += 1800;
        assert!(matches!(attempt(&ops, &later, "rounds", "k", PAYLOAD), Verdict::Handled), "the window has passed");
        assert!(matches!(attempt(&ops, &c, "rounds", "other", PAYLOAD), Verdict::Handled), "a different incident key counts on its own");
    }

    #[test]
    fn several_matches_a_missing_run_and_a_missing_field_all_escalate_naming_why() {
        let d = TempDir::new("watchd-sop");
        let c = ctx(d.path().to_str().unwrap());
        let mut ops = fake(ToolRunFake::Exit(0, String::new()));
        ops.sops.push(("sop-other".into(), "MATCH: FAIL\nSYMPTOM: s\nCHECK: c\nFIX: f\nRUN: x\nDEADLINE: 5".into()));
        let Verdict::Escalate(Some(n)) = attempt(&ops, &c, "r", "k", PAYLOAD) else { panic!() };
        assert!(n.contains("several SOPs with a RUN: match (sop-recert, sop-other)"), "{n}");
        ops.sops.push(("sop-third".into(), PLAIN.into()));
        let Verdict::Escalate(Some(n)) = attempt(&ops, &c, "r", "k", PAYLOAD) else { panic!() };
        assert!(n.contains("none was run; runbooks that matched: sop-third"), "{n}");
        assert!(ops.tool_calls.borrow().is_empty());

        let ops = Fake { sops: vec![("sop-plain".into(), PLAIN.into())], ..Fake::default() };
        let Verdict::Escalate(Some(n)) = attempt(&ops, &c, "r", "k", PAYLOAD) else { panic!() };
        assert!(n.contains("runbooks that matched: sop-plain") && !n.contains("tried"), "{n}");

        let ops = fake(ToolRunFake::Exit(0, String::new()));
        let Verdict::Escalate(Some(n)) = attempt(&ops, &c, "r", "k", "FAIL round without the field") else { panic!() };
        assert!(n.contains("batch"), "{n}");
        assert!(ops.tool_calls.borrow().is_empty());
    }

    #[test]
    fn a_runbook_with_no_run_does_not_block_the_one_runnable_match() {
        let d = TempDir::new("watchd-sop");
        let mut ops = fake(ToolRunFake::Exit(0, String::new()));
        ops.sops.push(("sop-stale-worktree-ref-cleanup".into(), PLAIN.into()));
        let v = attempt(&ops, &ctx(d.path().to_str().unwrap()), "rounds", "k", PAYLOAD);
        assert!(matches!(v, Verdict::Handled));
        assert_eq!(*ops.tool_calls.borrow(), vec![("recert".to_string(), vec!["--batch".to_string(), "b42".to_string()], 60)]);
        assert_eq!(ops.concierge_mail.borrow().len(), 1);
    }

    #[test]
    fn a_tool_missing_from_the_release_escalates_without_recording_an_application() {
        let d = TempDir::new("watchd-sop");
        let ops = fake(ToolRunFake::Unavailable("not in bin".into()));
        let Verdict::Escalate(Some(n)) = attempt(&ops, &ctx(d.path().to_str().unwrap()), "r", "k", PAYLOAD) else { panic!() };
        assert!(n.contains("recert") && n.contains("not in bin"), "{n}");
        assert!(ops.applied.borrow().is_empty());
    }

    #[test]
    fn an_incident_naming_a_bead_records_the_application_against_it() {
        let d = TempDir::new("watchd-sop");
        let ops = fake(ToolRunFake::Exit(0, String::new()));
        attempt(&ops, &ctx(d.path().to_str().unwrap()), "r", "k", "FAIL round batch=b1 bead=sp-abc123.");
        assert_eq!(ops.applied.borrow()[0].1, "sp-abc123");
    }

    #[test]
    fn incident_fields_take_only_the_named_clean_values() {
        let f = incident_fields("x batch=b1, repo=a/b bead=$(rm) bead=sp-1 other=3");
        assert_eq!(f.get("batch").map(String::as_str), Some("b1"));
        assert_eq!(f.get("bead").map(String::as_str), Some("sp-1"));
        assert_eq!(f.get("repo"), None, "a slash is not a clean value");
        assert_eq!(f.len(), 2);
        assert!(incident_fields("batch=--force repo=-x bead=..").is_empty(), "a value must not read as a flag or a path");
    }
}
