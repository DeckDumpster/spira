//! After the session (DESIGN.md §4.6): the wiki commit and the verdict fences. Closed is not
//! landed — every fence reads the commit graph or the store, never the session's word.
//!
//! The reopen causes are consts: census.sh folds `sp-requeue-<cause>` for every cause that is
//! both a bead_reopen cause and a REQUEUE_CAUSE here (test-census-events.sh sp-ytw2h scans
//! for them).

use std::path::Path;

use crate::bd;
use crate::decide::{self, Eviction};
use crate::ports::{s, Bd, Exec, Git};
use crate::run::Run;
use crate::trace;
use crate::util;

pub const EVICTION_RACE: &str = "eviction-race";
pub const PROD_DIRTY: &str = "prod-dirty";
pub const DESC_CHANGED_SINCE_CLAIM: &str = "desc-changed-since-claim";
pub const UNFINISHED_REASON: &str = "unfinished-reason";
pub const GROOM_SILENT: &str = "groom-silent";
pub const REBASE_CONFLICT: &str = "rebase-conflict";
pub const WORKFLOW_RUN_MISSING: &str = "workflow-run-missing";
pub const WORKFLOW_RUN_WRONG_BRANCH: &str = "workflow-run-wrong-branch";
pub const WORKFLOW_RUN_STALE_SHA: &str = "workflow-run-stale-sha";
pub const WORKFLOW_RUN_WRONG_FILE: &str = "workflow-run-wrong-file";
pub const WORKFLOW_RUN_UNVERIFIABLE: &str = "workflow-run-unverifiable";

/// Prior eviction-race reopens, from spira-claim's ledger of this bead's returns.
pub fn eviction_race_count(ledger_json: &str) -> i64 {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(ledger_json.trim()) else { return 0 };
    v.get("returns")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter(|r| r.get("cause").and_then(|c| c.as_str()) == Some(EVICTION_RACE)).count() as i64)
        .unwrap_or(0)
}

/// `verdict_committed <repo> <branch> <id> [window]` -> is a commit naming `id` already on
/// the branch, or (failing that) on the landing refs? Walks the branch first (this session's
/// own commits, not yet merged to the base), then the landing refs (commits already on the
/// base) — same window for both, to match sentinel CHECK5 and `landed()`'s
/// SPIRA_VERDICT_WINDOW. A bead's commit sits deeper from a rebased branch's tip than from
/// the base tip (leftover commits from a prior attempt shift the depth), so checking only
/// one of the two misses real commits in one direction.
///
/// `land_refs` is `spira_landrefs`' own output (sp-o88bx, "wave 4.12": the caller now
/// resolves it in-process through `spira_config::repos::landrefs`, not a seam call) —
/// space-separated, empty when the land ref itself does not resolve.
pub fn verdict_committed(git: &dyn Git, land_refs: &str, repo: &Path, branch: &str, id: &str, window: i64) -> bool {
    let subjects = git.git(repo, &["log", "--format=%s%n%b", "-n", &window.to_string(), branch]).stdout;
    if subjects.contains(id) {
        return true;
    }
    let refs: Vec<&str> = land_refs.split_whitespace().collect();
    if refs.is_empty() {
        return false;
    }
    let mut args: Vec<String> = vec!["log".into(), "--format=%s%n%b".into(), "-n".into(), window.to_string()];
    args.extend(refs.into_iter().map(String::from));
    let land_subjects = git.git(repo, &args.iter().map(String::as_str).collect::<Vec<_>>()).stdout;
    land_subjects.contains(id)
}

/// `delivers_verdict <id> <delivers-string> <since-epoch>` -> (verified?, failure reason).
/// The one case block for `delivers:TYPE` evidence, shared by the verdict (checking its own
/// session's work, since-epoch = SESSION_EPOCH) and sentinel CHECK5 (checking a closed
/// bead's window since started_at) — both get the same verdict for the same evidence by
/// construction, because it is the same function (now duplicated once, natively, in each
/// crate, rather than both shelling into one bash copy).
///
/// RECOGNISED TYPES: beads (a child bead — not this aeon's own state-change event record —
/// names id as source), note/report:/abs/path (the file exists and postdates since-epoch; a
/// path under `.../applied.jsonl` is checked for a record naming id instead, since mtime
/// alone on that shared ledger is satisfied by any concurrent aeon's own application),
/// check:<command> (exits 0 within `timeout_s`), action (no machine check — the close reason
/// is the evidence).
pub fn delivers_verdict(bd: &dyn Bd, exec: &dyn Exec, timeout_s: u64, id: &str, delivers: &str, since: i64) -> (bool, String) {
    for item in delivers.split(';').filter(|d| !d.is_empty()) {
        let (dtype, dval) = item.split_once(':').unwrap_or((item, item));
        match dtype {
            "beads" => {
                let children = bd::json(bd, &["children", id]);
                if bd::child_work_count(&children) <= 0 {
                    return (false, format!("delivers:beads declared but no child beads name {id} as source"));
                }
            }
            "note" | "report" => {
                if dval == dtype {
                    return (false, format!("delivers:{dtype} has no file path — use delivers:{dtype}:/absolute/path"));
                }
                let p = Path::new(dval);
                if !p.is_file() {
                    return (false, format!("delivers:{dtype}: {dval} does not exist"));
                }
                if dval.ends_with("/applied.jsonl") {
                    let text = std::fs::read_to_string(p).unwrap_or_default();
                    let needle = regex::Regex::new(&format!(r#""bead"\s*:\s*"{}""#, regex::escape(id))).unwrap();
                    if !text.lines().any(|l| needle.is_match(l)) {
                        return (false, format!("delivers:{dtype}: {dval} has no record naming bead {id}"));
                    }
                } else {
                    let mt = std::fs::metadata(p)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    if mt <= since {
                        return (false, format!("delivers:{dtype}: {dval} exists but predates the window (mtime {mt} <= {since})"));
                    }
                }
            }
            "check" => {
                if dval == dtype {
                    return (false, "delivers:check has no command — use delivers:check:<command>".into());
                }
                let o = exec.exec("timeout", &s(&[&timeout_s.to_string(), "bash", "-c", dval]), None, None);
                if o.code == 124 {
                    return (false, format!("delivers:check: command timed out after {timeout_s}s: {dval}"));
                } else if o.code != 0 {
                    return (false, format!("delivers:check: command exited non-zero: {dval}"));
                }
            }
            "action" => {}
            other => {
                return (false, format!("delivers:{other} is not a recognised type (beads, note, report, check, action)"));
            }
        }
    }
    (true, String::new())
}

/// `close_verdict <id> <status> <superseded> <delivers> <committed> <since-epoch>` ->
/// keep|committed, keep|superseded, keep|delivers, reopen|delivers-mismatch or
/// reopen|closed-without-commit. Not-closed and committed=true both answer keep|committed —
/// the caller only acts on this after it already knows the bead is closed.
///
/// THE GIT WALK IS THE CALLER'S OWN (`verdict_committed`, or CHECK5's walk)
/// — this only decides a CLOSED, NOT-YET-committed bead's fate given delivers evidence
/// already available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseVerdict {
    pub outcome: String,
    pub reason: String,
    pub msg: String,
}

pub fn close_verdict(bd: &dyn Bd, exec: &dyn Exec, delivers_timeout_s: u64, id: &str, status: &str, superseded: bool, delivers: &str, committed: bool, since: i64) -> CloseVerdict {
    if status != "closed" || committed {
        return CloseVerdict { outcome: "keep".into(), reason: "committed".into(), msg: String::new() };
    }
    if superseded {
        return CloseVerdict { outcome: "keep".into(), reason: "superseded".into(), msg: String::new() };
    }
    if !delivers.is_empty() {
        let (ok, fail) = delivers_verdict(bd, exec, delivers_timeout_s, id, delivers, since);
        if ok {
            return CloseVerdict { outcome: "keep".into(), reason: "delivers".into(), msg: format!("{delivers} verified") };
        }
        return CloseVerdict { outcome: "reopen".into(), reason: "delivers-mismatch".into(), msg: fail };
    }
    CloseVerdict { outcome: "reopen".into(), reason: "closed-without-commit".into(), msg: String::new() }
}

impl Run<'_> {
    fn ts_print(&self, msg: &str) {
        // aeon.sh printf'd these lines itself: `<ts> spira: <fayth>: <id> …`.
        self.d.sink.out(&util::log_line(self.now(), msg));
    }

    fn requeue(&mut self, cause: &str, why: String) {
        self.s.requeue_cause = Some(cause.to_string());
        self.s.requeue_why = why;
    }

    /// An aeon that writes to the brain wiki commits what ITS OWN tool calls named.
    pub fn wiki_commit(&self) {
        let wiki = self.conf.s("SPIRA_WIKI");
        if wiki.is_empty() || !Path::new(&wiki).is_dir() {
            return;
        }
        let wl = self.d.git.git(Path::new(&wiki), &["worktree", "list", "--porcelain"]);
        let main = wl.stdout.lines().find_map(|l| l.strip_prefix("worktree ")).unwrap_or("").to_string();
        let real = Path::new(&wiki).canonicalize().map(|p| p.display().to_string()).unwrap_or_default();
        if main.is_empty() || real != main {
            return;
        }
        let st = self.d.git.git(Path::new(&wiki), &["status", "--short", "--untracked-files=all"]);
        let mut dirty: Vec<String> = st.stdout.lines().map(|l| l.chars().skip(3).collect::<String>()).collect();
        dirty.sort();
        dirty.dedup();
        let logf = self.s.logf.clone().unwrap_or_default();
        let writes = trace::wiki_write_paths(&logf, Path::new(&wiki), &self.conf.trace_mark());
        let committed = trace::wiki_commit_paths(&writes, &dirty);
        let new = committed.join("\n");
        if new.is_empty() {
            return;
        }
        let lines: Vec<&str> = new.lines().filter(|l| !l.is_empty()).collect();
        let o = self.d.exec.exec("wiki-commit.sh", &s(&[&wiki, &format!("{}: wiki writes for {}", self.f(), self.s.bead)]), Some(format!("{new}\n").into_bytes()), None);
        if o.success() {
            let mut shown = lines.iter().take(5).cloned().collect::<Vec<_>>().join(" ");
            if lines.len() > 5 {
                shown.push(' ');
            }
            self.log(&format!("{}: {} committed wiki changes ({} file(s)): {shown}", self.f(), self.s.bead, lines.len()));
        } else {
            self.log(&format!("{}: {} wiki commit failed", self.f(), self.s.bead));
        }
    }

    pub fn verdict(&mut self) {
        let id = self.s.bead.clone();
        let f = self.fayth.name.clone();
        let repo = self.s.repo.display().to_string();
        let branch = self.s.branch.clone();
        let base = self.s.base.clone();
        let restricted = self.s.lc_model_restricted;

        let row = bd::show(self.d.bd, &id);
        let mut st = row.as_ref().and_then(|r| r.status.clone()).unwrap_or_default();
        let superseded = row.as_ref().is_some_and(|r| r.superseded());
        let delivers = row.as_ref().map(|r| r.delivers()).unwrap_or_default();
        let issue_type = row.as_ref().and_then(|r| r.issue_type.clone()).unwrap_or_default();
        let window = self.conf.n("SPIRA_VERDICT_WINDOW", 400);
        let land_refs = match spira_config::repos::landrefs(&self.conf.repos, &repo) {
            Some((base, Some(local))) => format!("{base} {local}"),
            Some((base, None)) => base,
            None => String::new(),
        };
        let committed = verdict_committed(self.d.git, &land_refs, &self.s.repo, &branch, &id, window);
        self.s.committed = committed;
        let cy = if committed { "yes" } else { "no" };
        let sup = if superseded { "1" } else { "0" };
        self.log(&format!("{f}: {id} status={st} committed={cy} superseded={sup} delivers={}", if delivers.is_empty() { "none" } else { &delivers }));

        // ---- eviction race ----
        if !restricted && st == "closed" && committed && !superseded {
            let lc = self.d.exec.exec("spira-lc", &s(&["show", &id]), None, None);
            let row: serde_json::Value = if lc.success() { serde_json::from_str(lc.stdout.trim()).unwrap_or(serde_json::Value::Null) } else { serde_json::Value::Null };
            let fld = |k: &str| row.get("bead").and_then(|b| b.get(k)).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let (state, reason, tip) = (fld("state"), fld("reason"), fld("tip"));
            let cap_at = self.conf.n("SPIRA_EVICTION_ESCALATE_AT", 3);
            if decide::eviction_reopen(&state, &reason, &tip, "", 0, cap_at) != Eviction::None {
                let seen_f = self.run_dir().join(format!("{id}.evict-seen"));
                let seen = std::fs::read_to_string(&seen_f).unwrap_or_default();
                let key = format!("{tip} {reason}");
                if !seen.is_empty() && seen == key {
                    self.log(&format!("{f}: {id} closed with lifecycle state={state} ({reason}) — tip+reason unchanged since the last eviction-race reopen, duplicate skipped"));
                } else {
                    let cur = self.d.git.git(&self.s.repo, &["rev-parse", &branch]);
                    let cur = if cur.success() { cur.text() } else { String::new() };
                    let count = {
                        let o = self.d.exec.exec(&self.claim_bin, &s(&["requeues", &id, "--json"]), None, None);
                        if o.success() {
                            eviction_race_count(&o.stdout)
                        } else {
                            self.log(&format!("{f}: {id} spira-claim could not count prior eviction-race reopens (rc={}) — reading it as 0", o.code));
                            0
                        }
                    };
                    match decide::eviction_reopen(&state, &reason, &tip, &cur, count, cap_at) {
                        Eviction::Stale => self.log(&format!("{f}: {id} closed with lifecycle state={state} — row tip {tip} ≠ branch tip {cur}, stale record, close stands")),
                        Eviction::Cap => {
                            let ask = self.conf.ask_label();
                            let _ = self.d.bd.bd(&s(&["label", "add", &id, &ask]));
                            let _ = self.d.exec.exec("spira-lc", &s(&["hold", &id, "ask", &format!("eviction-race guard capped: reopened {count} time(s) already"), &f]), None, None);
                            self.note(&format!("Eviction-race guard capped: reopened {count} time(s) already. Recertify the branch by hand and clear the {ask} label; the guard will not reopen it again on its own."));
                            self.log(&format!("{f}: {id} eviction-race escalated — {count} prior requeue(s) ≥ {cap_at}, labeled {ask} instead of reopening"));
                            let _ = std::fs::write(&seen_f, &key);
                        }
                        Eviction::Reopen => {
                            self.bead_reopen(EVICTION_RACE, &format!("Reopened by aeon.sh: bead closed while its lifecycle state is {state} ({reason}) — the branch was evicted from the batch while this session was in flight. The close is valid but the work cannot re-enter the queue while the bead is closed. Recertify the branch to re-enter the merge queue."));
                            self.log(&format!("{f}: {id} REOPENED — closed with lifecycle state={state} (eviction race)"));
                            st = "open".into();
                            self.requeue(EVICTION_RACE, "Batch evicted the branch while this aeon was in flight; the bead was re-closed on a stale pass. Recertify the branch.".into());
                            let _ = std::fs::write(&seen_f, &key);
                        }
                        Eviction::None => {}
                    }
                }
            }
        }

        // ---- close_verdict: the same decision sentinel CHECK 5 makes, natively in each crate ----
        let delivers_timeout = self.conf.n("SPIRA_DELIVERS_CHECK_TIMEOUT", 60).max(1) as u64;
        let cv = close_verdict(self.d.bd, self.d.exec, delivers_timeout, &id, &st, superseded, &delivers, committed, self.s.session_epoch);
        match (cv.outcome.as_str(), cv.reason.as_str()) {
            ("keep", "delivers") => self.log(&format!("{f}: {id} closed with nothing committed and NOT reopened — {}", cv.msg)),
            ("keep", "superseded") => self.log(&format!("{f}: {id} closed with nothing committed and NOT reopened — superseded, so its work landed under another id")),
            ("reopen", "delivers-mismatch") => {
                let producer = row.as_ref().and_then(|r| r.created_by.clone()).unwrap_or_default();
                let pm = if producer.is_empty() { String::new() } else { format!(" Escalate to {producer} if the criterion cannot be met — the delivers: label may not be removed.") };
                self.bead_reopen("delivers-mismatch", &format!("Reopened by aeon.sh: {}. Set delivers:TYPE labels that match the evidence actually produced.{pm}", cv.msg));
                self.log(&format!("{f}: {id} REOPENED — delivers not verified: {}", cv.msg));
            }
            ("reopen", "closed-without-commit") => {
                if self.sv("bead_is_work_type", &s(&[&issue_type])).success() {
                    self.log(&format!("{f}: {id} closed with nothing committed — work type, left to the submitted conversion at teardown"));
                } else if self.sv("bead_is_decision_type", &s(&[&issue_type])).success() {
                    self.log(&format!("{f}: {id} closed with nothing committed — decision type, no commit expected, close stands"));
                } else {
                    self.bead_reopen("closed-without-commit", &format!("Reopened by aeon.sh: closed without a commit naming {id} on {branch}. Closed is not landed."));
                    self.log(&format!("{f}: {id} REOPENED — closed with nothing committed"));
                }
            }
            _ => {}
        }

        // ---- own-worktree dirty guard ----
        let work = self.s.work.clone().unwrap_or_default();
        if st == "closed" && committed && !self.conf.set_nonempty("SPIRA_ALLOW_PROD_DIRTY") {
            let dirty = self.d.git.git(&work, &["status", "--porcelain", "--untracked-files=no"]).stdout.trim_end().to_string();
            if !dirty.is_empty() {
                let spd_base = if let Some(b) = spira_config::repos::landref(&self.conf.repos, &work.display().to_string()) {
                    b
                } else {
                    let h = self.d.git.git(&work, &["rev-parse", "--abbrev-ref", "HEAD"]);
                    if h.success() { h.text() } else { "HEAD".into() }
                };
                let names: Vec<String> = self.d.git.git(&work, &["diff", "--name-only", "HEAD"]).stdout.lines().filter(|l| !l.is_empty()).map(String::from).collect();
                let identical: Vec<&String> = names.iter().filter(|p| self.d.git.git(&work, &["diff", "--quiet", &spd_base, "--", p]).success()).collect();
                let w = work.display().to_string();
                let mut n = format!(
                    "Reopened by aeon.sh: bead closed while the aeon's own worktree ({w}) carried uncommitted tracked modifications. Staged but uncommitted code is invisible to the commit graph — commit it or restore the file.\n\nModified paths:\n{}",
                    dirty.lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
                );
                if !identical.is_empty() {
                    let ids = identical.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
                    n.push_str(&format!("\n\nPaths byte-for-byte identical to {spd_base} (hand-applied, not genuinely new):\n  {ids}\nRemedy: git -C {w} checkout -- {ids}"));
                }
                n.push_str("\n\nOverride (only when the modification is intentional and will be committed separately): SPIRA_ALLOW_PROD_DIRTY=1");
                self.bead_reopen(PROD_DIRTY, &n);
                st = "open".into();
                let first5 = names.iter().take(5).map(|s| format!("{s} ")).collect::<String>();
                self.log(&format!("{f}: {id} REOPENED — own worktree dirty: {first5}"));
                self.requeue(PROD_DIRTY, format!("Bead closed while the aeon's own worktree ({w}) carried uncommitted tracked modifications. Commit or restore the staged/modified files, then resume this bead."));
            }
        }

        // ---- description changed since claim, unacknowledged ----
        if st == "closed" && !superseded {
            let shown = bd::json(self.d.bd, &["show", &id]);
            let claimed = bead::claimdesc::metadata_value(&shown, bead::claimdesc::HASH_KEY);
            if !claimed.is_empty() {
                let now = bead::claimdesc::desc_hash(&shown);
                if now.is_some_and(|n| n != claimed) {
                    self.bead_reopen(DESC_CHANGED_SINCE_CLAIM, "Reopened by aeon: this bead's description changed after it was claimed and before it closed, and the change was never acknowledged (no --force-claimed re-stamp). The session worked from whatever it read at claim time, not from what the bead says now. Re-verify the close against the current description before re-closing.");
                    self.log(&format!("{f}: {id} REOPENED — description changed since claim, unacknowledged"));
                    st = "open".into();
                    self.requeue(DESC_CHANGED_SINCE_CLAIM, "This bead's description changed after this session claimed it and before it closed, unacknowledged. The session's work reflects the description as claimed, not as it now reads — re-verify against the current text before re-closing.".into());
                }
            }
        }

        // ---- close-reason fence (law-no-close-reason-admits-unfinished) ----
        if st == "closed" && committed {
            let cr = bd::show(self.d.bd, &id).and_then(|r| r.close_reason).unwrap_or_default();
            if !cr.is_empty() && !self.conf.set_nonempty("SPIRA_CLOSE_REASON_OVERRIDE") {
                let o = self.d.exec.exec("close-reason-flags.py", &s(&[&cr]), None, None);
                let hit = if o.success() { o.text() } else { String::new() };
                if !hit.is_empty() {
                    self.bead_reopen(UNFINISHED_REASON, &format!("Reopened by aeon.sh: close reason contains a statute phrase (\"{hit}\") that says the work is not done (law-no-close-reason-admits-unfinished). A remainder is a bead, not a sentence in the close reason. Two endings: (a) file the remainder with bead.sh, cite its id in the reason, then close; (b) groomer depends-on-fix {id} --fix <blocker-bead> if a fix is already in flight (law-a-bug-with-a-fix-in-flight-depends-on-it)."));
                    self.ts_print(&format!("{f}: {id} REOPENED — close reason contains statute phrase: {hit}. Override: SPIRA_CLOSE_REASON_OVERRIDE=<why>"));
                    st = "open".into();
                    self.requeue(UNFINISHED_REASON, format!("Close reason contained a statute phrase (\"{hit}\"). File the remainder as a bead, cite its id in the reason, then re-close."));
                }
            }
        }

        // ---- the groom escalation rule ----
        let mut groom_silent = false;
        if self.fayth.groom_escalation_check && st == "closed" && !superseded {
            let text = std::fs::read_to_string(self.run_dir().join("groom.log")).unwrap_or_default();
            let new: String = text.lines().skip(self.s.groom_lines_before).map(|l| format!("{l}\n")).collect();
            let new = new.trim_end_matches('\n').to_string();
            let re = regex::Regex::new(r"(?i)ESCALATED|inquiry|flagged").unwrap();
            if !new.is_empty() && re.is_match(&new) {
                let ask = self.conf.ask_label();
                let aj = self.d.bd.bd(&s(&["list", "--type", "decision", "--label", &ask, "--json"]));
                let aj = if aj.success() { aj.text() } else { String::new() };
                let aj = if aj.is_empty() { "[]".to_string() } else { aj };
                let unproven = trace::groom_claims_verified(&new, &aj, self.s.session_epoch);
                if !unproven.is_empty() {
                    self.bead_reopen("no-groom-ask", &format!("Reopened and poisoned: groom log claimed ESCALATED for {unproven} but no ask bead was filed in this session naming those beads. A log claim is not an escalation. File the ask via mail send operator --kind question, then re-run the pass."));
                    let _ = self.d.bd.bd(&s(&["label", "add", &id, "spira-poison"]));
                    self.ts_print(&format!("{f}: {id} REOPENED and POISONED — groom log claimed ESCALATED for {unproven} but no ask bead found in this session"));
                    groom_silent = true;
                    if committed {
                        self.requeue(GROOM_SILENT, format!("Groom log claimed escalation for {unproven} without a matching ask bead; the close was undone and the trigger poisoned."));
                    }
                } else {
                    self.ts_print(&format!("{f}: {id} groom-escalation-check: all claimed escalations verified"));
                }
            }
        }

        // ---- close-time workflow-run fence (law-a-workflow-lands-on-its-own-run) ----
        if st == "closed" && committed {
            if self.conf.set_nonempty("SPIRA_WORKFLOW_RUN_CONSIDERED") {
                self.log(&format!("{f}: {id} workflow-run fence skipped (SPIRA_WORKFLOW_RUN_CONSIDERED={})", self.conf.s("SPIRA_WORKFLOW_RUN_CONSIDERED")));
            } else {
                // aeon.sh passed SPIRA_DB="$DB" under `set -u`; with DB unset the check never
                // ran and read as OK. It runs here exactly when bash's would (DESIGN.md §8).
                let r = match self.d.env.original.get("DB") {
                    Some(db) => {
                        let wf = self.conf.or("SPIRA_WORKFLOW_ONLY_PATHS", "spira/acceptance-ci.sh spira/acceptance-agent.sh spira/build-tarball.sh");
                        let a = s(&[
                            &format!("BEAD_ID={id}"),
                            &format!("SPIRA_BD={}", self.conf.or("SPIRA_BD", "bd")),
                            &format!("SPIRA_DB={db}"),
                            &format!("SPIRA_REPO={repo}"),
                            &format!("BRANCH={branch}"),
                            &format!("BASE={base}"),
                            &format!("SPIRA_GH_API={}", self.conf.or("SPIRA_GH_API", "https://api.github.com")),
                            &format!("SPIRA_WORKFLOW_ONLY_PATHS={wf}"),
                            "workflow-run-check.py",
                        ]);
                        let o = self.d.exec.exec("env", &a, None, None);
                        if o.success() { o.text() } else { String::new() }
                    }
                    None => String::new(),
                };
                self.workflow_fence(&r, &mut st);
            }
        }

        // ---- closed behind the base is not finished ----
        // A superseded close is not judged by whether its stale branch still replays: its
        // work landed under the successor's id, so a conflict is expected (sp-dz39p).
        if st == "closed" && committed && !superseded && !groom_silent {
            if !self.s.base_remote.is_empty() && !self.d.git.git(&self.s.repo, &["fetch", "-q", &self.s.base_remote]).success() {
                self.log(&format!("{f}: fetch of {} failed — judging currency against a possibly stale {base}", self.s.base_remote));
            }
            let fq = self.s.base_fq.clone();
            if self.d.git.git(&self.s.repo, &["merge-base", "--is-ancestor", &fq, &format!("refs/heads/{branch}")]).success() {
                self.log(&format!("{f}: {id} closed current with {base}"));
            } else {
                let rb = self.d.seam.call("_aeon_rebase", &s(&[&branch, &fq, &repo, &self.s.repo_name]));
                for l in rb.stderr.lines() {
                    self.d.sink.out(l);
                }
                if rb.success() {
                    self.log(&format!("{f}: {id} closed behind {base} — rebased by the harness after close (the session did not)"));
                    self.note(&format!("Rebased onto {base} by aeon.sh after the session closed the bead without doing so. The replay was clean; the landing gate judges the rebased tree."));
                } else {
                    self.s.rebase_conflicts = rb.stdout.trim_end().to_string();
                    let conflicts = if self.s.rebase_conflicts.is_empty() { "unknown".to_string() } else { self.s.rebase_conflicts.clone() };
                    let cited = if restricted {
                        String::new()
                    } else {
                        let o = self.d.exec.exec("landing-pass", &s(&["cited-commit", &id, &repo, &fq]), None, None);
                        if o.success() { o.text() } else { String::new() }
                    };
                    if !cited.is_empty() {
                        self.log(&format!("{f}: {id} closed behind {base} but notes cite {cited} on {base} — retiring as landed"));
                        // A failed mark is logged, never discarded: the bead reads landed-on-main with no
                        // delivery record to show it. SPIRA_RUN is passed explicitly (law-a-binary-resolves-the-config-it-reads).
                        let mo = self.d.exec.exec(
                            "env",
                            &s(&[&format!("SPIRA_RUN={}", self.conf.run.display()), "landing-pass", "mark", &id, "LANDED", &cited, "cited-on-main"]),
                            None,
                            None,
                        );
                        if !mo.success() {
                            self.log(&format!("{f}: {id} landing-pass mark LANDED {cited} cited-on-main FAILED (rc={}): {}", mo.code, mo.first_err_line()));
                        }
                        if self.sdo("spira_destroy_branch", &s(&[&id, &branch, &repo, &format!("fix on {base} cited in notes as {cited}"), "cited-landed"])) != 0 {
                            self.log(&format!("{f}: {id} branch retire failed"));
                        }
                    } else {
                        let others = self.sv("other_beads_on_conflicts", &s(&[&repo, &branch, &base, &self.s.rebase_conflicts])).text();
                        let mut n = format!("Reopened by aeon.sh: closed behind {base} and {branch} does not rebase onto it — conflicts in {conflicts}. The brief asked for this rebase before closing.");
                        if !others.is_empty() {
                            n.push_str(&format!(" Those files were changed on {base} by {others} — check whether this work is already landed before resolving."));
                        } else {
                            n.push_str(" A merge conflict is not an escalation — the next aeon is handed the rebase and must resolve it.");
                        }
                        self.bead_reopen(REBASE_CONFLICT, &n);
                        self.requeue(REBASE_CONFLICT, format!("{branch} would not rebase onto {base} (conflicts in {conflicts}); the next aeon is handed the rebase."));
                        self.log(&format!("{f}: {id} REOPENED — closed behind {base}, conflicts in {conflicts}"));
                    }
                }
            }
        }
    }

    fn workflow_fence(&mut self, r: &str, st: &mut String) {
        let id = self.s.bead.clone();
        let f = self.fayth.name.clone();
        let branch = self.s.branch.clone();
        let (cause, note, log, why) = if r.is_empty() || r == "OK" {
            return;
        } else if r == "NO_URL" {
            (
                WORKFLOW_RUN_MISSING,
                format!("Reopened by aeon.sh: branch touches CI workflow files but close reason has no GitHub Actions run URL (law-a-workflow-lands-on-its-own-run). Include https://github.com/.../actions/runs/<id> for a run on {branch} where the changed step executed. Override: SPIRA_WORKFLOW_RUN_CONSIDERED=<reason>; or: bd note {id} \"WORKFLOW_RUN_CONSIDERED: <reason>\"."),
                format!("{f}: {id} REOPENED — close reason has no workflow run URL (law-a-workflow-lands-on-its-own-run)"),
                format!("Close reason has no GitHub Actions run URL for the changed workflow. Include a run URL on branch {branch} where the changed step executed, then re-close."),
            )
        } else if let Some(b) = r.strip_prefix("WRONG_BRANCH:") {
            (
                WORKFLOW_RUN_WRONG_BRANCH,
                format!("Reopened by aeon.sh: cited run is on branch \"{b}\", not \"{branch}\" (law-a-workflow-lands-on-its-own-run). The run must be dispatched on the bead's branch. Override: SPIRA_WORKFLOW_RUN_CONSIDERED=<reason>."),
                format!("{f}: {id} REOPENED — cited workflow run is on branch {b}, not {branch}"),
                format!("Cited run is on branch \"{b}\", not \"{branch}\". Dispatch on the bead branch, then re-close."),
            )
        } else if let Some(sha) = r.strip_prefix("SHA_NOT_ANCESTOR:") {
            (
                WORKFLOW_RUN_STALE_SHA,
                format!("Reopened by aeon.sh: cited run is at commit {sha} which is not an ancestor of the current branch tip — the run tested a stale tree (law-a-workflow-lands-on-its-own-run). Ask the concierge to push the current tip and re-dispatch. Override: SPIRA_WORKFLOW_RUN_CONSIDERED=<reason>."),
                format!("{f}: {id} REOPENED — cited workflow run SHA {sha} is not an ancestor of {branch}"),
                format!("Cited run is at stale commit {sha}. Push the current tip and re-dispatch on {branch}, then re-close."),
            )
        } else if let Some(p) = r.strip_prefix("WRONG_WORKFLOW:") {
            (
                WORKFLOW_RUN_WRONG_FILE,
                format!("Reopened by aeon.sh: cited run is for workflow \"{p}\" but the changed workflow files are different (law-a-workflow-lands-on-its-own-run). Override: SPIRA_WORKFLOW_RUN_CONSIDERED=<reason>."),
                format!("{f}: {id} REOPENED — cited workflow run is for {p}, not the changed workflow"),
                format!("Cited run is for workflow \"{p}\", not the changed workflow. Dispatch the correct workflow on {branch}, then re-close."),
            )
        } else if r.starts_with("RUN_NOT_FOUND:") || r.starts_with("API_FAIL:") {
            (
                WORKFLOW_RUN_UNVERIFIABLE,
                format!("Reopened by aeon.sh: could not verify the cited workflow run URL ({r}) (law-a-workflow-lands-on-its-own-run). Override: SPIRA_WORKFLOW_RUN_CONSIDERED=<reason>."),
                format!("{f}: {id} REOPENED — workflow run URL could not be verified ({r})"),
                format!("Cited workflow run URL could not be verified ({r}). Use SPIRA_WORKFLOW_RUN_CONSIDERED=<reason> if this is a transient network issue."),
            )
        } else {
            return;
        };
        self.bead_reopen(cause, &note);
        self.log(&log);
        *st = "open".into();
        self.requeue(cause, why);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Out;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[test]
    fn eviction_count_reads_only_eviction_race_returns() {
        let j = r#"{"bead":"sp-a","returns":[{"at":"t","cause":"eviction-race","class":"harness-return"},{"at":"t","cause":"cert-gate-red","class":"judged"},{"at":"t","cause":"eviction-race","class":"harness-return"}]}"#;
        assert_eq!(eviction_race_count(j), 2);
        assert_eq!(eviction_race_count("not json"), 0);
        assert_eq!(eviction_race_count("{}"), 0);
    }

    // ---- verdict_committed -----------------------------------------------------------

    /// Fakes git log's subject output by ref name: args are `log --format=... -n <window>
    /// <ref>...`, so everything from index 4 on is the ref list to look up.
    struct FakeGit(BTreeMap<&'static str, &'static str>);
    impl Git for FakeGit {
        fn git(&self, _dir: &Path, args: &[&str]) -> Out {
            let text: String = args[4..].iter().filter_map(|r| self.0.get(*r)).map(|t| format!("{t}\n")).collect();
            Out::ok(text)
        }
    }
    #[test]
    fn verdict_committed_walks_branch_then_landrefs() {
        let mut log = BTreeMap::new();
        log.insert("mybranch", "sp-a — the work");
        let git = FakeGit(log);
        assert!(verdict_committed(&git, "", Path::new("/repo"), "mybranch", "sp-a", 400), "committed on the branch itself");
        assert!(!verdict_committed(&git, "", Path::new("/repo"), "mybranch", "sp-x", 400), "not committed anywhere");

        let mut log2 = BTreeMap::new();
        log2.insert("mybranch", "unrelated");
        log2.insert("local/main", "sp-a — landed earlier");
        let git2 = FakeGit(log2);
        assert!(verdict_committed(&git2, "local/main", Path::new("/repo"), "mybranch", "sp-a", 400), "not on the branch, but on the landing ref");

        // Empty landrefs short-circuits without a second git call that would need a ref.
        assert!(!verdict_committed(&git2, "", Path::new("/repo"), "mybranch", "sp-a", 400));
    }

    // ---- delivers_verdict / close_verdict ----------------------------------------------

    struct FakeBd(Mutex<String>);
    impl Bd for FakeBd {
        fn bd(&self, _args: &[String]) -> Out {
            Out::ok(self.0.lock().unwrap().clone())
        }
    }
    struct FakeExec;
    impl Exec for FakeExec {
        fn exec(&self, prog: &str, args: &[String], _stdin: Option<Vec<u8>>, _cwd: Option<&Path>) -> Out {
            assert_eq!(prog, "timeout");
            match args[3].as_str() {
                "true" => Out::ok(""),
                "false" => Out::fail(1, ""),
                "sleep 999" => Out::fail(124, ""),
                other => panic!("unexpected check command: {other}"),
            }
        }
    }

    #[test]
    fn delivers_verdict_beads() {
        let bd = FakeBd(Mutex::new(r#"[{"id":"sp-child","issue_type":"task"}]"#.into()));
        let exec = FakeExec;
        let (ok, _) = delivers_verdict(&bd, &exec, 5, "sp-a", "beads", 0);
        assert!(ok);
        *bd.0.lock().unwrap() = "[]".into();
        let (ok, fail) = delivers_verdict(&bd, &exec, 5, "sp-a", "beads", 0);
        assert!(!ok);
        assert!(fail.contains("no child beads name sp-a"), "{fail}");
    }

    #[test]
    fn delivers_verdict_note_and_report() {
        let d = testkit::TempDir::new("aeon-delivers");
        let bd = FakeBd(Mutex::new(String::new()));
        let exec = FakeExec;
        let (ok, fail) = delivers_verdict(&bd, &exec, 5, "sp-a", "note", 0);
        assert!(!ok);
        assert!(fail.contains("has no file path"), "{fail}");

        let (ok, fail) = delivers_verdict(&bd, &exec, 5, "sp-a", &format!("note:{}/missing", d.display()), 0);
        assert!(!ok);
        assert!(fail.contains("does not exist"), "{fail}");

        let p = d.join("report.md");
        std::fs::write(&p, "x").unwrap();
        let now = crate::util::now_epoch();
        let (ok, fail) = delivers_verdict(&bd, &exec, 5, "sp-a", &format!("report:{}", p.display()), now + 1000);
        assert!(!ok, "{fail}");
        assert!(fail.contains("predates the window"), "{fail}");
        let (ok, _) = delivers_verdict(&bd, &exec, 5, "sp-a", &format!("report:{}", p.display()), now - 1000);
        assert!(ok);

        let applied = d.join("applied.jsonl");
        std::fs::write(&applied, "{\"bead\": \"sp-other\"}\n").unwrap();
        let (ok, fail) = delivers_verdict(&bd, &exec, 5, "sp-a", &format!("report:{}", applied.display()), 0);
        assert!(!ok);
        assert!(fail.contains("no record naming bead sp-a"), "{fail}");
        std::fs::write(&applied, "{\"bead\": \"sp-a\"}\n").unwrap();
        let (ok, _) = delivers_verdict(&bd, &exec, 5, "sp-a", &format!("report:{}", applied.display()), 0);
        assert!(ok);
    }

    #[test]
    fn delivers_verdict_check_and_action() {
        let bd = FakeBd(Mutex::new(String::new()));
        let exec = FakeExec;
        assert!(delivers_verdict(&bd, &exec, 5, "sp-a", "check:true", 0).0);
        let (ok, fail) = delivers_verdict(&bd, &exec, 5, "sp-a", "check:false", 0);
        assert!(!ok);
        assert!(fail.contains("exited non-zero"), "{fail}");
        let (ok, fail) = delivers_verdict(&bd, &exec, 5, "sp-a", "check:sleep 999", 0);
        assert!(!ok);
        assert!(fail.contains("timed out"), "{fail}");
        assert!(delivers_verdict(&bd, &exec, 5, "sp-a", "action", 0).0);
        let (ok, fail) = delivers_verdict(&bd, &exec, 5, "sp-a", "weird:x", 0);
        assert!(!ok);
        assert!(fail.contains("not a recognised type"), "{fail}");
    }

    #[test]
    fn close_verdict_precedence() {
        let bd = FakeBd(Mutex::new(String::new()));
        let exec = FakeExec;
        // Not closed, or committed: keep|committed, regardless of anything else.
        let c = close_verdict(&bd, &exec, 5, "sp-a", "open", false, "", true, 0);
        assert_eq!((c.outcome.as_str(), c.reason.as_str()), ("keep", "committed"));
        let c = close_verdict(&bd, &exec, 5, "sp-a", "closed", false, "", true, 0);
        assert_eq!((c.outcome.as_str(), c.reason.as_str()), ("keep", "committed"));
        // Superseded wins over a missing commit.
        let c = close_verdict(&bd, &exec, 5, "sp-a", "closed", true, "", false, 0);
        assert_eq!((c.outcome.as_str(), c.reason.as_str()), ("keep", "superseded"));
        // No delivers label at all: reopen.
        let c = close_verdict(&bd, &exec, 5, "sp-a", "closed", false, "", false, 0);
        assert_eq!((c.outcome.as_str(), c.reason.as_str()), ("reopen", "closed-without-commit"));
        // delivers:action always verifies.
        let c = close_verdict(&bd, &exec, 5, "sp-a", "closed", false, "action", false, 0);
        assert_eq!((c.outcome.as_str(), c.reason.as_str()), ("keep", "delivers"));
        // delivers:check failing: reopen with the failure as the message.
        let c = close_verdict(&bd, &exec, 5, "sp-a", "closed", false, "check:false", false, 0);
        assert_eq!((c.outcome.as_str(), c.reason.as_str()), ("reopen", "delivers-mismatch"));
        assert!(c.msg.contains("exited non-zero"));
    }
}
