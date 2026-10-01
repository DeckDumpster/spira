//! Native escalation logic (sp-31hjr, wave 4.30, family C): what lib.sh's `spira_ask_*`,
//! `spira_land_noverdict` and `bead_context` composed as text, kept pure and unit-tested
//! here. The I/O — `bd`, `git`, the `mail` subprocess, the noverdict counter files — lives
//! in `real.rs`; this module only decides what the message says.
//!
//! Every mail body below is built to match lib.sh byte-for-byte (the parity proof the wave
//! brief asks for); where this port intentionally differs, the difference is named inline.

use serde_json::Value;

/// lib.sh `spira_is_generated_file`: a path substring match against a space-separated
/// pattern list (`SPIRA_REBASE_GENERATED_FILES`).
pub fn is_generated_file(path: &str, patterns: &str) -> bool {
    patterns.split_whitespace().any(|p| !p.is_empty() && path.contains(p))
}

/// The rebase-loop note's per-file listing: lines (one per conflicted file, GENERATED ones
/// flagged), and whether any file was generated.
pub fn conflict_file_lines(conflicts: &str, generated_patterns: &str) -> (String, bool) {
    let files: Vec<&str> = conflicts.split_whitespace().collect();
    if files.is_empty() {
        let f = if conflicts.trim().is_empty() { "unknown" } else { conflicts.trim() };
        return (format!("  - {f}"), false);
    }
    let mut any_gen = false;
    let lines: Vec<String> = files
        .iter()
        .map(|f| {
            if is_generated_file(f, generated_patterns) {
                any_gen = true;
                format!("  - {f} (GENERATED — regenerate it, do not merge it by hand)")
            } else {
                format!("  - {f}")
            }
        })
        .collect();
    (lines.join("\n"), any_gen)
}

/// Everything `rebase_loop_mail` needs that isn't already an argument of the lib.sh call:
/// the bead's title/status (a `bd show`), the branch's tip/ahead-count (git, when the
/// caller passed repo coordinates), and how many files the branch's own diff touches.
#[derive(Default)]
pub struct RebaseLoopCtx<'a> {
    pub bead_title: Option<&'a str>,
    pub bead_status: Option<&'a str>,
    pub tip_short: Option<&'a str>,
    pub ahead: Option<&'a str>,
    pub nfiles: u32,
}

/// lib.sh `spira_ask_rebase_loop` — mail send **concierge**, kind note (never an
/// operator ask; never deduped through `ask_already_open`: every reopen is reported,
/// because the repetition itself, not the first occurrence, is what CHECK 6 escalates on
/// at the bash layer above this one).
#[allow(clippy::too_many_arguments)]
pub fn rebase_loop_mail(
    id: &str,
    branch: &str,
    repo_name: &str,
    n: &str,
    conflicts: &str,
    others: &str,
    base: Option<&str>,
    ctx: &RebaseLoopCtx,
    decompose_files: u32,
    generated_patterns: &str,
) -> (String, String) {
    let subj = match ctx.bead_title.filter(|t| !t.is_empty()) {
        Some(t) => format!("{t}: {branch} rebase loop x{n} in {repo_name}"),
        None => format!("{branch} rebase loop x{n} in {repo_name}"),
    };
    let (files_note, any_gen) = conflict_file_lines(conflicts, generated_patterns);
    let ctx_line = if !others.is_empty() {
        format!(" The conflicted files were also changed on the base by {others}.")
    } else {
        String::new()
    };
    let decompose_line = if ctx.nfiles >= decompose_files {
        format!(
            " {branch} touches {} files — a bead this wide re-enters the rebase race every landing; consider splitting it into smaller beads instead of hand-rebasing the whole thing again.",
            ctx.nfiles
        )
    } else {
        String::new()
    };
    let suggestion = if any_gen {
        "regenerate the GENERATED file(s) named above via their own generator and rebase again — do not hand-merge them".to_string()
    } else if ctx.nfiles >= decompose_files {
        format!("split {branch} into smaller beads by file/deliverable and land those independently, rather than rebasing the whole thing by hand again")
    } else if !others.is_empty() {
        format!("check whether {branch} is a duplicate of {others} and close it if so; if the work is genuinely new, rebase by hand and push")
    } else {
        format!("rebase {branch} by hand and push, or close it if the work is already landed")
    };
    let mut extra = String::new();
    if let Some(st) = ctx.bead_status.filter(|s| !s.is_empty()) {
        extra.push_str(&format!("Status: {st}."));
    }
    if let (Some(tip), Some(ahead)) = (ctx.tip_short.filter(|s| !s.is_empty()), ctx.ahead.filter(|s| !s.is_empty())) {
        if !extra.is_empty() {
            extra.push('\n');
        }
        extra.push_str(&format!("Branch: {tip} ({ahead} commit(s) ahead of {}).", base.unwrap_or("")));
    }
    let body = format!(
        "## Note\n\n{subj}\n\n{id} has been reopened for a rebase conflict {n} times and the loop is not converging. This is\nmachinery cycling on itself, not a decision for Ryan (law-a-rebase-loop-is-sequenced-not-split)\n— rebase with explicit guidance and fast-track the bead into a round the moment it certifies.\n\nConflicting file(s):\n{files_note}\n{ctx_line}{decompose_line}\n\nSuggested action: {suggestion}\n\n{extra}\n"
    );
    (subj, body)
}

/// lib.sh `spira_ask_red_recurring`'s dedupe subject.
pub fn red_recurring_subject(branch: &str, reason_class: &str) -> String {
    format!("{branch} red recurring {reason_class}")
}

/// lib.sh `spira_ask_red_recurring`'s mail: (subject, default, body).
pub fn red_recurring_mail(id: &str, branch: &str, repo_name: &str, reason_class: &str, elapsed_h: i64) -> (String, String, String) {
    let subj = format!("{branch} red recurring: {reason_class} twice on {id} in {repo_name}");
    let dflt = format!("investigate why {branch} cannot land ({reason_class}); close the bead if the work is superseded, or rebase by hand if the root cause is external");
    let body = format!(
        "## Question\n{subj}\n\n## Default\n{dflt}\n\n{id} has gone RED twice with the same reason class ({reason_class}) on {branch} in {repo_name}. The shas changed between marks, so each reopen charged a session to work that hit the same wall. Elapsed since first RED: {elapsed_h}h.\n"
    );
    (subj, dflt, body)
}

/// lib.sh `spira_ask_rebase_refused`'s dedupe subject.
pub fn rebase_refused_subject(branch: &str) -> String {
    format!("{branch} rebase refused")
}

/// lib.sh `spira_ask_rebase_refused`'s mail: (subject, default, body).
pub fn rebase_refused_mail(id: &str, branch: &str, repo_name: &str, reason: &str) -> (String, String, String) {
    let subj = format!("{branch} rebase refused in {repo_name}: {reason}");
    let dflt = format!("fix the infrastructure; {id} stays closed and its branch will land on the next pass");
    let body = format!(
        "## Question\n{subj}\n\n## Default\n{dflt}\n\n{id} is closed; its branch {branch} cannot be rebased onto the base in {repo_name}.\nThe failure is not a merge conflict — the work is not being reopened.\nReason: {reason}.\n"
    );
    (subj, dflt, body)
}

/// lib.sh `spira_ask_budget_deferred`'s dedupe subject.
pub fn budget_deferred_subject(branch: &str) -> String {
    format!("{branch} budget-deferred")
}

/// lib.sh `spira_ask_budget_deferred`'s mail (kind `alert`, no `--default`): (subject, body).
pub fn budget_deferred_mail(branch: &str, repo_name: &str, n: u32) -> (String, String) {
    let subj = format!("{branch} budget-deferred: {n} consecutive passes in {repo_name}");
    let body = format!(
        "## Alert\n{subj}\n\nBranch {branch} has been deferred by budget exhaustion {n} consecutive landing passes in {repo_name}.\nThe pass runs out of gate budget before reaching this branch.\n"
    );
    (subj, body)
}

/// lib.sh `spira_ask_refresh_loop`'s dedupe subject.
pub fn refresh_loop_subject(id: &str) -> String {
    format!("{id} refresh cap")
}

/// lib.sh `spira_ask_refresh_loop`'s mail: (subject, default, body).
#[allow(clippy::too_many_arguments)]
pub fn refresh_loop_mail(
    id: &str,
    branch: &str,
    repo_name: &str,
    base: &str,
    behind: &str,
    n: u32,
    cap: u32,
    bead_context: &str,
) -> (String, String, String) {
    let subj = format!("Spira: {id}'s pull request has been rebased {n} time(s) and still has not merged");
    let dflt = format!("reopen {id} at P0 so an aeon owns the pull request's own failure, and leave the branch alone until it does");
    let ev = format!(
        "BRANCH    {branch} in {repo_name}\nBASE      {base}, {behind} commit(s) ahead of the branch\nREFRESHED {n} time(s); the cap is {cap}\n\n{bead_context}\n"
    );
    let body = format!(
        "## Question\n{subj}\n\n## Default\n{dflt}\n\nthe bead is closed and its aeon is gone, so nothing is watching this pull request. Spira has been dragging {branch} back onto {base} every time the base moved, and {n} rebases have not got it merged — which means the obstacle is not staleness.\n\nWHAT THIS BEAD IS FOR:\n{ev}\n"
    );
    (subj, dflt, body)
}

/// lib.sh `spira_ask_machinery`'s dedupe subject.
pub fn machinery_subject(branch: &str) -> String {
    format!("{branch} cannot be judged")
}

/// lib.sh `spira_ask_machinery`'s mail: (subject, default, body). `evidence` is already
/// `tail -20` of the gate output.
pub fn machinery_mail(id: &str, branch: &str, repo_name: &str, outcome: &str, reason: &str, n: u32, evidence: &str) -> (String, String, String) {
    let subj = format!("{branch} cannot be judged: {outcome} x{n} in a row ({reason})");
    let dflt = format!("raise the budget or clear the contention this reason names, then let the next pass take it; if it is not obvious, run `gate.sh {branch} {repo_name}` by hand and read the whole output");
    let why = format!(
        "{outcome} means the machinery could not reach a verdict — the branch has NOT been judged and has NOT been charged, and {id} is not at fault. It has now failed to be judged {n} times, so this is no longer a queue clearing itself. Nothing on {branch} can land until a verdict is reached, and every other branch of {repo_name} is behind the same fault."
    );
    let body = format!("## Question\n{subj}\n\n## Default\n{dflt}\n\n{why}\n\n{evidence}\n");
    (subj, dflt, body)
}

/// lib.sh `spira_ask_machinery_class`'s dedupe subject.
pub fn machinery_class_subject(repo_name: &str, outcome: &str, reason: &str) -> String {
    format!("{repo_name} cannot be judged: {outcome} ({reason})")
}

/// lib.sh `spira_ask_machinery_class`'s mail: (subject, default, body).
pub fn machinery_class_mail(repo_name: &str, reason: &str, branches: &str, outcome: &str, n: u32, evidence: &str) -> (String, String, String) {
    let subj = format!("{repo_name} cannot be judged: {outcome} x{n} in a day ({reason}) — {branches}");
    let dflt = "this is one machinery fault behind every branch named above, not one per branch; fix the cause this reason names, then let the next pass take all of them".to_string();
    let why = format!(
        "{outcome}/{reason} means the machinery could not reach a verdict for any of these branches — none of them is at fault and none has been charged. It has recurred {n} times across {repo_name} within a day, so this is escalated once for the class rather than once per branch."
    );
    let body = format!("## Question\n{subj}\n\n## Default\n{dflt}\n\n{why}\n\nAffected branches: {branches}\n\n{evidence}\n");
    (subj, dflt, body)
}

/// lib.sh `bead_context <id>`, rendered from a `bd show` row already in hand (same shape
/// as sentinel's `render::bead_context` / strand's `check::bead_context` — this crate
/// keeps its own copy rather than a shared one, as those two already do).
///
/// Difference from the python this ports: a bead with no priority renders `P9999` here
/// (this crate's `BeadRow` defaults a missing priority to 9999 for sort order elsewhere),
/// where lib.sh's python prints `PNone`. Named per the wave brief; harmless in practice —
/// every bead this is ever called for already carries a priority.
#[allow(clippy::too_many_arguments)]
pub fn bead_context(id: &str, status: &str, priority: i64, created_at: Option<&str>, title: &str, labels: &[String], description: Option<&str>, notes: Option<&Value>, now: i64) -> String {
    let mut o = String::new();
    let status = if status.is_empty() { "None" } else { status };
    o.push_str(&format!("BEAD    {id}  [{status}, P{priority}, open {}]\n", age(created_at, now)));
    o.push_str(&format!("TITLE   {}\n", if title.is_empty() { "(none)" } else { title }));
    let labs = if labels.is_empty() { "(none)".to_string() } else { labels.join(", ") };
    o.push_str(&format!("LABELS  {labs}\n\nWHAT THIS BEAD IS FOR\n"));
    let desc = description.filter(|d| !d.is_empty()).unwrap_or("(no description — that is itself the problem)");
    o.push_str(desc.trim());
    o.push('\n');
    let note_lines: Vec<String> = match notes {
        Some(Value::String(s)) => s.split('\n').filter(|n| !n.trim().is_empty()).map(str::to_string).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|n| match n {
                Value::Object(m) => m.get("text").map(|v| v.as_str().unwrap_or_default().to_string()).unwrap_or_default(),
                Value::String(s) => s.clone(),
                x => x.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    };
    if !note_lines.is_empty() {
        o.push_str("\nMOST RECENT NOTES\n");
        for n in note_lines.iter().skip(note_lines.len().saturating_sub(3)) {
            let t: String = n.trim().chars().take(400).collect();
            o.push_str(&format!("  - {t}\n"));
        }
    }
    o.trim_end_matches('\n').to_string()
}

/// python: `"%dh" % h if h < 48 else "%dd" % (h / 24)`; "?" when `created_at` is absent or
/// unparseable.
fn age(created_at: Option<&str>, now: i64) -> String {
    let Some(ts) = created_at else { return "?".into() };
    let Some(t) = crate::util::parse_iso(ts) else { return "?".into() };
    let h = (now - t) as f64 / 3600.0;
    if h < 48.0 {
        format!("{}h", h.trunc() as i64)
    } else {
        format!("{}d", (h / 24.0).trunc() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_files_match_by_substring() {
        assert!(is_generated_file("spira/boundary/foo.rs", "boundary/ repo-boundary"));
        assert!(!is_generated_file("spira/other.rs", "boundary/ repo-boundary"));
        assert!(!is_generated_file("spira/other.rs", ""));
    }

    #[test]
    fn conflict_lines_flag_generated_files_and_fall_back_to_unknown() {
        let (lines, any_gen) = conflict_file_lines("a.rs boundary/gen.rs", "boundary/");
        assert_eq!(lines, "  - a.rs\n  - boundary/gen.rs (GENERATED — regenerate it, do not merge it by hand)");
        assert!(any_gen);
        let (lines, any_gen) = conflict_file_lines("", "boundary/");
        assert_eq!(lines, "  - unknown");
        assert!(!any_gen);
    }

    #[test]
    fn rebase_loop_mail_picks_the_generated_file_suggestion_over_decompose_and_duplicate() {
        let ctx = RebaseLoopCtx { bead_title: Some("Do the thing"), bead_status: Some("open"), tip_short: Some("abc123"), ahead: Some("3"), nfiles: 10 };
        let (subj, body) = rebase_loop_mail("sp-a", "spira/sp-a", "spira", "4", "gen/x.rs", "sp-b", Some("local/main"), &ctx, 4, "gen/");
        assert_eq!(subj, "Do the thing: spira/sp-a rebase loop x4 in spira");
        assert!(body.contains("regenerate the GENERATED file(s)"), "{body}");
        assert!(body.contains("Status: open."));
        assert!(body.contains("Branch: abc123 (3 commit(s) ahead of local/main)."));
        assert!(body.contains("The conflicted files were also changed on the base by sp-b."));
    }

    #[test]
    fn rebase_loop_mail_falls_back_to_decompose_then_duplicate_then_plain_rebase() {
        let ctx = RebaseLoopCtx { nfiles: 10, ..Default::default() };
        let (subj, body) = rebase_loop_mail("sp-a", "spira/sp-a", "spira", "4", "a.rs", "", None, &ctx, 4, "");
        assert_eq!(subj, "spira/sp-a rebase loop x4 in spira");
        assert!(body.contains("split spira/sp-a into smaller beads by file/deliverable"), "{body}");

        let ctx = RebaseLoopCtx { nfiles: 1, ..Default::default() };
        let (_, body) = rebase_loop_mail("sp-a", "spira/sp-a", "spira", "4", "a.rs", "sp-dup", None, &ctx, 4, "");
        assert!(body.contains("check whether spira/sp-a is a duplicate of sp-dup"), "{body}");

        let ctx = RebaseLoopCtx { nfiles: 1, ..Default::default() };
        let (_, body) = rebase_loop_mail("sp-a", "spira/sp-a", "spira", "4", "a.rs", "", None, &ctx, 4, "");
        assert!(body.contains("rebase spira/sp-a by hand and push, or close it"), "{body}");
    }

    #[test]
    fn red_recurring_matches_lib_sh() {
        assert_eq!(red_recurring_subject("spira/sp-a", "no-rebase"), "spira/sp-a red recurring no-rebase");
        let (subj, dflt, body) = red_recurring_mail("sp-a", "spira/sp-a", "spira", "no-rebase", 5);
        assert_eq!(subj, "spira/sp-a red recurring: no-rebase twice on sp-a in spira");
        assert!(dflt.starts_with("investigate why spira/sp-a cannot land (no-rebase)"));
        assert!(body.contains("Elapsed since first RED: 5h."));
    }

    #[test]
    fn rebase_refused_matches_lib_sh() {
        assert_eq!(rebase_refused_subject("spira/sp-a"), "spira/sp-a rebase refused");
        let (subj, dflt, body) = rebase_refused_mail("sp-a", "spira/sp-a", "spira", "dirty worktree");
        assert_eq!(subj, "spira/sp-a rebase refused in spira: dirty worktree");
        assert!(dflt.contains("sp-a stays closed"));
        assert!(body.contains("Reason: dirty worktree."));
    }

    #[test]
    fn budget_deferred_has_no_default_and_is_an_alert() {
        assert_eq!(budget_deferred_subject("spira/sp-a"), "spira/sp-a budget-deferred");
        let (subj, body) = budget_deferred_mail("spira/sp-a", "spira", 5);
        assert_eq!(subj, "spira/sp-a budget-deferred: 5 consecutive passes in spira");
        assert!(body.starts_with("## Alert\n"));
    }

    #[test]
    fn refresh_loop_matches_lib_sh() {
        assert_eq!(refresh_loop_subject("sp-a"), "sp-a refresh cap");
        let (subj, dflt, body) = refresh_loop_mail("sp-a", "spira/sp-a", "spira", "local/main", "7", 5, 5, "BEAD ...");
        assert_eq!(subj, "Spira: sp-a's pull request has been rebased 5 time(s) and still has not merged");
        assert!(dflt.starts_with("reopen sp-a at P0"));
        assert!(body.contains("REFRESHED 5 time(s); the cap is 5"));
        assert!(body.contains("WHAT THIS BEAD IS FOR:\nBRANCH    spira/sp-a in spira"));
        assert!(body.contains("the cap is 5\n\nBEAD ...\n"));
    }

    #[test]
    fn machinery_mails_match_lib_sh() {
        assert_eq!(machinery_subject("spira/sp-a"), "spira/sp-a cannot be judged");
        let (subj, dflt, body) = machinery_mail("sp-a", "spira/sp-a", "spira", "NO_VERDICT", "lock", 3, "tail");
        assert_eq!(subj, "spira/sp-a cannot be judged: NO_VERDICT x3 in a row (lock)");
        assert!(dflt.contains("gate.sh spira/sp-a spira"));
        assert!(body.contains("tail"));

        assert_eq!(machinery_class_subject("spira", "NO_VERDICT", "harness-fault"), "spira cannot be judged: NO_VERDICT (harness-fault)");
        let (subj, _dflt, body) = machinery_class_mail("spira", "harness-fault", "spira/sp-a,spira/sp-b", "NO_VERDICT", 4, "tail");
        assert_eq!(subj, "spira cannot be judged: NO_VERDICT x4 in a day (harness-fault) — spira/sp-a,spira/sp-b");
        assert!(body.contains("Affected branches: spira/sp-a,spira/sp-b"));
    }

    #[test]
    fn bead_context_matches_the_python() {
        let notes = serde_json::json!("one\n\ntwo\nthree\nfour");
        let now = crate::util::parse_iso("2026-09-28T05:30:00Z").unwrap();
        let s = bead_context("sp-x", "open", 1, Some("2026-09-28T00:00:00Z"), "T", &["a".into(), "b".into()], Some("  do it  \n"), Some(&notes), now);
        assert_eq!(s, "BEAD    sp-x  [open, P1, open 5h]\nTITLE   T\nLABELS  a, b\n\nWHAT THIS BEAD IS FOR\ndo it\n\nMOST RECENT NOTES\n  - two\n  - three\n  - four");

        let now2 = crate::util::parse_iso("2026-09-28T00:00:00Z").unwrap();
        let bare = bead_context("sp-y", "", 9999, Some("2026-09-20T00:00:00Z"), "", &[], None, None, now2);
        assert_eq!(bare, "BEAD    sp-y  [None, P9999, open 8d]\nTITLE   (none)\nLABELS  (none)\n\nWHAT THIS BEAD IS FOR\n(no description — that is itself the problem)");
    }
}
