//! After the session (DESIGN.md §4.6): the wiki commit and the verdict fences. Closed is not
//! landed — every fence reads the commit graph or the store, never the session's word.
//!
//! The reopen causes are consts: census.sh folds `sp-requeue-<cause>` for every cause that is
//! both a bead_reopen cause and a REQUEUE_CAUSE here (test-census-events.sh sp-ytw2h scans
//! for them).

use std::path::Path;

use crate::bd;
use crate::decide::{self, Eviction, SopVerdict};
use crate::ports::s;
use crate::run::Run;
use crate::util;

pub const EVICTION_RACE: &str = "eviction-race";
pub const PROD_DIRTY: &str = "prod-dirty";
pub const SOP_SILENT: &str = "sop-silent";
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
        let logf = self.s.logf.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let writes = self.sv("wiki_write_paths", &s(&[&logf, &wiki])).text();
        let new = self.sv("wiki_commit_paths", &s(&[&writes, &dirty.join("\n")])).text();
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
        let committed = self.sv("verdict_committed", &s(&[&repo, &branch, &id])).text() == "yes";
        self.s.committed = committed;
        let cy = if committed { "yes" } else { "no" };
        let sup = if superseded { "1" } else { "0" };
        self.log(&format!("{f}: {id} status={st} committed={cy} superseded={sup} delivers={}", if delivers.is_empty() { "none" } else { &delivers }));

        // ---- eviction race ----
        if !restricted && st == "closed" && committed && !superseded {
            let ls = self.sv("land_state", &s(&[&id]));
            let ls = if ls.success() { ls.text() } else { String::new() };
            let reasons = self.conf.s("LAND_EVICTION_REASONS");
            let cap_at = self.conf.n("SPIRA_EVICTION_ESCALATE_AT", 3);
            if !ls.is_empty() && decide::eviction_reopen(&ls, "", 0, &reasons, cap_at) != Eviction::None {
                let mut it = ls.split_whitespace();
                let state = it.next().unwrap_or("").to_string();
                let tip = it.next().unwrap_or("").to_string();
                let _at = it.next();
                let reason = it.collect::<Vec<_>>().join(" ");
                let seen_f = self.conf.landstate().join(format!("{id}.evict-seen"));
                let seen = std::fs::read_to_string(&seen_f).unwrap_or_default();
                let key = format!("{tip} {reason}");
                if !seen.is_empty() && seen == key {
                    self.log(&format!("{f}: {id} closed with landstate={state} — tip+reason unchanged since the last eviction-race reopen, duplicate skipped"));
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
                    match decide::eviction_reopen(&ls, &cur, count, &reasons, cap_at) {
                        Eviction::Stale => self.log(&format!("{f}: {id} closed with landstate={state} — record tip {tip} ≠ branch tip {cur}, stale record, close stands")),
                        Eviction::Cap => {
                            let ask = self.conf.ask_label();
                            let _ = self.d.bd.bd(&s(&["label", "add", &id, &ask]));
                            // spira-lc's caller verb: lc.sh's lc_hold was never sourced into this seam, so
                            // the allowlisted name answered "command not found" (sp-arpjt).
                            let _ = self.d.exec.exec("spira-lc", &s(&["hold", &id, "ask", &format!("eviction-race guard capped: reopened {count} time(s) already"), &f]), None, None);
                            self.note(&format!("Eviction-race guard capped: reopened {count} time(s) already. Recertify the branch by hand and clear the {ask} label; the guard will not reopen it again on its own."));
                            self.log(&format!("{f}: {id} eviction-race escalated — {count} prior requeue(s) ≥ {cap_at}, labeled {ask} instead of reopening"));
                            let _ = std::fs::write(&seen_f, &key);
                        }
                        Eviction::Reopen => {
                            self.bead_reopen(EVICTION_RACE, &format!("Reopened by aeon.sh: bead closed while landstate is {state} — the branch was evicted from the batch while this session was in flight. The close is valid but the work cannot re-enter the queue while the bead is closed. Recertify the branch to re-enter the merge queue."));
                            self.log(&format!("{f}: {id} REOPENED — closed with landstate={state} (eviction race)"));
                            st = "open".into();
                            self.requeue(EVICTION_RACE, "Batch evicted the branch while this aeon was in flight; the bead was re-closed on a stale pass. Recertify the branch.".into());
                            let _ = std::fs::write(&seen_f, &key);
                        }
                        Eviction::None => {}
                    }
                }
            }
        }

        // ---- close_verdict: shared with sentinel CHECK 5 ----
        let cv = self.sv("close_verdict", &s(&[&id, &st, sup, &delivers, cy, &self.s.session_epoch.to_string()])).stdout;
        let cv = decide::parse_close_verdict(&cv);
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
                let lb = self.sv("_aeon_base", &s(&[&work.display().to_string()]));
                let spd_base = if lb.success() {
                    lb.stdout.lines().next().unwrap_or("HEAD").to_string()
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

        // ---- the closing rule: an incident resolved without a runbook is not resolved ----
        let mut sop_silent = false;
        if self.fayth.sop_required && st == "closed" && !superseded {
            let sop = "sop";
            let after = self.d.exec.exec(&sop, &s(&["digest"]), None, None);
            let applied = self.d.exec.exec(&sop, &s(&["log", "--bead", &id, "--check", "pass", "--since", &self.s.session_epoch.to_string()]), None, None).code;
            let before = self.s.sop_before.clone();
            let (wrote, v) = decide::sop_rule_verdict(before.is_some(), before.as_deref().unwrap_or(""), after.success(), &after.text(), applied);
            self.ts_print(&format!("{f}: {id} closing-rule wrote={wrote} applied={applied}"));
            match v {
                SopVerdict::Satisfied => {}
                SopVerdict::Decline => self.ts_print(&format!(
                    "{f}: {id} closing rule NOT judged — the shelf or the applications ledger could not be read (wrote={wrote} applied={applied}). Absence is not proven, so nothing is poisoned."
                )),
                SopVerdict::Poison => {
                    self.bead_reopen("no-sop", "Reopened and poisoned by aeon.sh: this incident was closed and no runbook came out of it. The session recorded neither an SOP written or amended (sop write) nor a runbook whose CHECK confirmed (sop applied --check pass), so nothing on the shelf is any better for this incident having happened and the next occurrence costs exactly as much. The closing rule is not optional: an incident resolved without an SOP must produce one. To clear this, write the runbook this incident should have left — or, if one already fitted and held, record it — then remove the spira-poison label.");
                    let _ = self.d.bd.bd(&s(&["label", "add", &id, "spira-poison"]));
                    self.ts_print(&format!("{f}: {id} REOPENED and POISONED — closed with no runbook written and no SOP application recorded"));
                    sop_silent = true;
                    if committed {
                        self.requeue(SOP_SILENT, "The incident was closed with no runbook behind it, so the close was undone and the bead poisoned; that poison is the verdict and this counter is not.".into());
                    }
                }
            }
        }

        // ---- close-reason fence (law-no-close-reason-admits-unfinished) ----
        if st == "closed" && committed && !sop_silent {
            let cr = bd::show(self.d.bd, &id).and_then(|r| r.close_reason).unwrap_or_default();
            if !cr.is_empty() && !self.conf.set_nonempty("SPIRA_CLOSE_REASON_OVERRIDE") {
                let o = self.d.exec.exec("close-reason-flags.py", &s(&[&cr]), None, None);
                let hit = if o.success() { o.text() } else { String::new() };
                if !hit.is_empty() {
                    self.bead_reopen(UNFINISHED_REASON, &format!("Reopened by aeon.sh: close reason contains a statute phrase (\"{hit}\") that says the work is not done (law-no-close-reason-admits-unfinished). A remainder is a bead, not a sentence in the close reason. Two endings: (a) file the remainder with bead.sh, cite its id in the reason, then close; (b) groomer.sh depends-on-fix {id} --fix <blocker-bead> if a fix is already in flight (law-a-bug-with-a-fix-in-flight-depends-on-it)."));
                    self.ts_print(&format!("{f}: {id} REOPENED — close reason contains statute phrase: {hit}. Override: SPIRA_CLOSE_REASON_OVERRIDE=<why>"));
                    st = "open".into();
                    self.requeue(UNFINISHED_REASON, format!("Close reason contained a statute phrase (\"{hit}\"). File the remainder as a bead, cite its id in the reason, then re-close."));
                }
            }
        }

        // ---- the groom escalation rule ----
        let mut groom_silent = false;
        if self.fayth.groom_escalation_check && st == "closed" && !superseded && !sop_silent {
            let text = std::fs::read_to_string(self.run_dir().join("groom.log")).unwrap_or_default();
            let new: String = text.lines().skip(self.s.groom_lines_before).map(|l| format!("{l}\n")).collect();
            let new = new.trim_end_matches('\n').to_string();
            let re = regex::Regex::new(r"(?i)ESCALATED|inquiry|flagged").unwrap();
            if !new.is_empty() && re.is_match(&new) {
                let ask = self.conf.ask_label();
                let aj = self.d.bd.bd(&s(&["list", "--type", "decision", "--label", &ask, "--json"]));
                let aj = if aj.success() { aj.text() } else { String::new() };
                let aj = if aj.is_empty() { "[]".to_string() } else { aj };
                let unproven = self.sv("groom_claims_verified", &s(&[&new, &aj, &self.s.session_epoch.to_string()])).text();
                if !unproven.is_empty() {
                    self.bead_reopen("no-groom-ask", &format!("Reopened and poisoned: groom log claimed ESCALATED for {unproven} but no ask bead was filed in this session naming those beads. A log claim is not an escalation. File the ask via mail.sh send operator --kind question, then re-run the pass."));
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
        if st == "closed" && committed && !sop_silent {
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
        if st == "closed" && committed && !superseded && !sop_silent && !groom_silent {
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
                        let o = self.sv("bead_cited_commit_on_base", &s(&[&id, &repo, &fq]));
                        if o.success() { o.text() } else { String::new() }
                    };
                    if !cited.is_empty() {
                        self.log(&format!("{f}: {id} closed behind {base} but notes cite {cited} on {base} — retiring as landed"));
                        self.sdo("land_mark", &s(&[&id, "LANDED", &cited, "cited-on-main"]));
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

    #[test]
    fn eviction_count_reads_only_eviction_race_returns() {
        let j = r#"{"bead":"sp-a","returns":[{"at":"t","cause":"eviction-race","class":"harness-return"},{"at":"t","cause":"cert-gate-red","class":"judged"},{"at":"t","cause":"eviction-race","class":"harness-return"}]}"#;
        assert_eq!(eviction_race_count(j), 2);
        assert_eq!(eviction_race_count("not json"), 0);
        assert_eq!(eviction_race_count("{}"), 0);
    }
}
