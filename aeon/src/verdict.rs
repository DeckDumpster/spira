//! After the session (DESIGN.md §4.6): the wiki commit and the verdict fences. Closed is not
//! landed — every fence reads the commit graph or the store, never the session's word.
//!
use std::path::Path;

use crate::bd;
use crate::decide;
use crate::ports::{s, Git};
use crate::run::Run;
use crate::trace;
use crate::util;

pub const PROD_DIRTY: &str = "prod-dirty";
pub const UNFINISHED_REASON: &str = "unfinished-reason";
pub const GROOM_SILENT: &str = "groom-silent";

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

impl Run<'_> {
    /// This bead's lifecycle row (`spira-lc show`), through the exec port every other
    /// spira-lc read here uses; `None` when the machine has no row or cannot answer.
    pub(crate) fn lc_bead(&self, id: &str) -> Option<spira_config::lc_state::Row> {
        let o = self.d.exec.exec("spira-lc", &s(&["show", id]), None, None);
        if !o.success() {
            return None;
        }
        spira_config::lc_state::parse_show(&o.stdout).ok().flatten()
    }

    fn ts_print(&self, msg: &str) {
        // aeon.sh printf'd these lines itself: `<ts> spira: <fayth>: <id> …`.
        self.d.sink.out(&util::log_line(self.now(), msg));
    }

    /// A guard refusing a restricted session's submission: the row it handed on goes back to
    /// REWORK. A restricted session never closes in bd, so `bead_reopen` alone (bd status, the
    /// submitted label) left the row SUBMITTED and the batcher would deliver refused work. The
    /// verdict is the guard's, recorded as the certifier's PolicyViolation red on the submitted tip.
    fn lc_rework(&self, id: &str) {
        let o = self.d.exec.exec("spira-lc", &s(&["show", id]), None, None);
        let v: serde_json::Value = if o.success() { serde_json::from_slice(o.stdout.as_bytes()).unwrap_or_default() } else { serde_json::Value::Null };
        let b = &v["bead"];
        let txt = |k: &str| match &b[k] { serde_json::Value::String(x) => x.clone(), serde_json::Value::Number(n) => n.to_string(), _ => String::new() };
        let (state, tip, version) = (txt("state"), txt("tip"), txt("version"));
        if state != "SUBMITTED" || tip.is_empty() || version.is_empty() {
            self.log(&format!("{}: {id} lifecycle row not returned to REWORK (state={state:?}, tip={tip:?}, version={version:?})", self.f()));
            return;
        }
        let kind = serde_json::json!({"GateRed": {"tip": tip, "reason": "policy-violation"}}).to_string();
        let r = self.d.exec.exec("spira-lc", &s(&["event", "bead", id, "--expect", &state, "--version", &version, "--actor", self.f(), "--kind", &kind]), None, None);
        if !r.success() {
            self.log(&format!("{}: {id} lifecycle row not returned to REWORK: spira-lc refused the event", self.f()));
        }
    }

    /// `bead_reopen`, plus the lifecycle half when the session handed on by submitting.
    fn refuse(&mut self, submitted: bool, cause: &str, note: &str) {
        self.bead_reopen(cause, note);
        if submitted {
            let id = self.s.bead.clone();
            self.lc_rework(&id);
        }
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

        let row = bd::show(self.d.bd, &id);
        let lc = self.lc_bead(&id);
        let submitted = decide::builder_submitted(self.s.lc_model_restricted, lc.as_ref());
        let st = decide::ledger_word(lc.as_ref());
        let superseded = row.as_ref().is_some_and(|r| r.superseded());
        let window = self.conf.i("SPIRA_VERDICT_WINDOW");
        let land_refs = match spira_config::repos::landrefs(&self.conf.repos, &repo) {
            Some((base, Some(local))) => format!("{base} {local}"),
            Some((base, None)) => base,
            None => String::new(),
        };
        let committed = verdict_committed(self.d.git, &land_refs, &self.s.repo, &branch, &id, window);
        self.s.committed = committed;
        let cy = if committed { "yes" } else { "no" };
        let sup = if superseded { "1" } else { "0" };
        self.log(&format!("{f}: {id} status={st} committed={cy} superseded={sup}"));

        // ---- own-worktree dirty guard ----
        let work = self.s.work.clone().unwrap_or_default();
        if submitted && committed && !self.conf.set_nonempty("SPIRA_ALLOW_PROD_DIRTY") {
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
                self.refuse(submitted, PROD_DIRTY, &n);
                let first5 = names.iter().take(5).map(|s| format!("{s} ")).collect::<String>();
                self.log(&format!("{f}: {id} REOPENED — own worktree dirty: {first5}"));
                self.requeue(PROD_DIRTY, format!("Bead closed while the aeon's own worktree ({w}) carried uncommitted tracked modifications. Commit or restore the staged/modified files, then resume this bead."));
            }
        }

        // ---- close-reason fence (law-no-close-reason-admits-unfinished) ----
        if submitted && committed {
            let cr = bd::show(self.d.bd, &id).and_then(|r| r.close_reason).unwrap_or_default();
            if !cr.is_empty() && !self.conf.set_nonempty("SPIRA_CLOSE_REASON_OVERRIDE") {
                let o = self.d.exec.exec("close-reason-flags.py", &s(&[&cr]), None, None);
                let hit = if o.success() { o.text() } else { String::new() };
                if !hit.is_empty() {
                    self.refuse(submitted, UNFINISHED_REASON, &format!("Reopened by aeon.sh: close reason contains a statute phrase (\"{hit}\") that says the work is not done (law-no-close-reason-admits-unfinished). A remainder is a bead, not a sentence in the close reason. Two endings: (a) file the remainder with bead.sh, cite its id in the reason, then close; (b) groomer depends-on-fix {id} --fix <blocker-bead> if a fix is already in flight (law-a-bug-with-a-fix-in-flight-depends-on-it)."));
                    self.ts_print(&format!("{f}: {id} REOPENED — close reason contains statute phrase: {hit}. Override: SPIRA_CLOSE_REASON_OVERRIDE=<why>"));
                        self.requeue(UNFINISHED_REASON, format!("Close reason contained a statute phrase (\"{hit}\"). File the remainder as a bead, cite its id in the reason, then re-close."));
                }
            }
        }

        // ---- the groom escalation rule ----
        if self.fayth.groom_escalation_check && submitted && !superseded {
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
                    self.refuse(submitted, "no-groom-ask", &format!("Reopened and poisoned: groom log claimed ESCALATED for {unproven} but no ask bead was filed in this session naming those beads. A log claim is not an escalation. File the ask via mail send operator --kind question, then re-run the pass."));
                    let _ = self.d.bd.bd(&s(&["label", "add", &id, "spira-poison"]));
                    let _ = self.d.exec.exec("spira-lc", &s(&["hold", &id, "poison", &format!("groom log claimed ESCALATED for {unproven} with no ask bead"), &f]), None, None);
                    self.ts_print(&format!("{f}: {id} REOPENED and POISONED — groom log claimed ESCALATED for {unproven} but no ask bead found in this session"));
                    if committed {
                        self.requeue(GROOM_SILENT, format!("Groom log claimed escalation for {unproven} without a matching ask bead; the close was undone and the trigger poisoned."));
                    }
                } else {
                    self.ts_print(&format!("{f}: {id} groom-escalation-check: all claimed escalations verified"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Out;
    use std::collections::BTreeMap;

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
}
