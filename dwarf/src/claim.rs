//! Which bead, and the atomic claim of it (DESIGN.md §4.1).
//!
//! Ranking is `spira-claim`'s: the ready set goes to it on STDIN and the epic lookup and the
//! resumable set in FILES — never argv (sp-o4trx: the claim outage was argv E2BIG). The one
//! thing left here is resumability, which needs the repo map and git and which spira-claim
//! by design leaves to its caller. Nothing in this module knows about briefs or runes
//! (sp-f0qhr).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use crate::bd::BeadRow;
use crate::ports::{Exec, Git};

/// One `spira-claim select --top-tier` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierLine {
    pub id: String,
    pub branch: String,
    pub repo: String,
    pub eprio: String,
    pub estarted: String,
    pub bprio: String,
}

pub fn parse_tier(text: &str) -> Vec<TierLine> {
    text.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split('|').collect();
            if p.len() < 6 || p[0].is_empty() {
                return None;
            }
            Some(TierLine { id: p[0].into(), branch: p[1].into(), repo: p[2].into(), eprio: p[3].into(), estarted: p[4].into(), bprio: p[5].into() })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Ranked ids, best first, and which of them are resumable (and the tier label).
    Ranked { ids: Vec<String>, resumable: Vec<String>, tier: Option<String> },
    /// A lookup or rank that never completed: `awake <f> claim-error <why>`, exit 1.
    ClaimError { log: String, ledger: String },
}

pub struct Selector<'a> {
    pub exec: &'a dyn Exec,
    pub git: &'a dyn Git,
    pub claim_bin: &'a str,
    pub rune: &'a str,
    pub scratch: &'a Path,
    pub pid: u32,
    /// spira_config::repos (sp-o88bx, "wave 4.12") in-process, instead of the
    /// `_dwarf_repo_info` bash seam this used to shell into.
    pub repos: &'a spira_config::repos::Registry,
}

impl Selector<'_> {
    fn file(&self, stem: &str) -> PathBuf {
        self.scratch.join(format!(".{stem}.{}", self.pid))
    }

    fn select_args(&self, base: &[&str]) -> Vec<String> {
        // The ready set is the machine's: `select` judges blockers by its rows too
        // (`--blockers machine`), the rule the ready set was built with (sp-s9675.2).
        let mut a = crate::ports::s(base);
        a.extend(crate::ports::s(&["--blockers", "machine"]));
        a
    }

    pub fn select(&self, ready_json: &str) -> Selection {
        let ready = ready_json.as_bytes().to_vec();
        let lk = self.exec.exec(self.claim_bin, &["epics".to_string()], Some(ready.clone()), None);
        if !lk.success() {
            return Selection::ClaimError {
                log: format!(
                    "{}: claim-error epic_parent_lookup failed (rc={}) — not reporting idle for a lookup that never completed",
                    self.rune, lk.code
                ),
                ledger: format!("claim-error epic_parent_lookup failed rc={}", lk.code),
            };
        }
        let lk_file = self.file("epic-lookup");
        let rs_file = self.file("resumable");
        let _ = std::fs::write(&lk_file, lk.stdout.as_bytes());
        let lkf = lk_file.display().to_string();

        // The top tier only: resumability costs one git process per candidate.
        let tier = self.exec.exec(self.claim_bin, &self.select_args(&["select", "--rune", self.rune, "--epics", &lkf, "--top-tier"]), Some(ready.clone()), None);
        let band = if tier.success() { parse_tier(&tier.stdout) } else { Vec::new() };
        let (resumable, tier_label) = self.resumable(&band);

        let _ = std::fs::write(&rs_file, resumable.join(","));
        let rsf = rs_file.display().to_string();
        let ranked = self.exec.exec(self.claim_bin, &self.select_args(&["select", "--rune", self.rune, "--epics", &lkf, "--resumable", &rsf]), Some(ready), None);
        let _ = std::fs::remove_file(&lk_file);
        let _ = std::fs::remove_file(&rs_file);
        if !ranked.success() {
            return Selection::ClaimError {
                log: format!(
                    "{}: claim-error epic_rank_rows failed (rc={}) — not reporting idle for a rank that never completed",
                    self.rune, ranked.code
                ),
                ledger: format!("claim-error epic_rank_rows failed rc={}", ranked.code),
            };
        }
        let ids = ranked.stdout.lines().filter_map(|l| l.split('\t').nth(5)).filter(|s| !s.is_empty()).map(String::from).collect();
        Selection::Ranked { ids, resumable, tier: tier_label }
    }

    /// Every top-tier candidate whose branch is ahead of its base — all of them, not just the
    /// first, so a lost race falls through to the next resumable one (sp-3ntca).
    fn resumable(&self, band: &[TierLine]) -> (Vec<String>, Option<String>) {
        if band.is_empty() {
            return (Vec::new(), None);
        }
        let mut names: Vec<String> = band.iter().map(|t| t.repo.clone()).collect();
        names.sort();
        names.dedup();
        // `_dwarf_repo_info <names…>`: root + base ref per name, skipping any name whose
        // root does not resolve to a real checkout — same filter the bash composite made.
        let repos: BTreeMap<String, (String, String)> = names
            .iter()
            .filter_map(|n| {
                let root = self.repos.root(n)?;
                if !Path::new(&root).join(".git").exists() {
                    return None;
                }
                let base = spira_config::repos::landref(self.repos, &root).unwrap_or_default();
                Some((n.clone(), (root, base)))
            })
            .collect();
        let (mut ids, mut label) = (Vec::new(), None);
        for t in band {
            let br = if t.branch.is_empty() { format!("spira/{}", t.id) } else { t.branch.clone() };
            let Some((root, base)) = repos.get(&t.repo) else { continue };
            if base.is_empty() {
                continue;
            }
            let n = self.git.git(Path::new(root), &["rev-list", "--count", &format!("{base}..{br}")]);
            let n: i64 = if n.success() { n.text().trim().parse().unwrap_or(0) } else { 0 };
            if n > 0 {
                ids.push(t.id.clone());
                label = Some(format!("P{}{}/P{}", t.eprio, if t.estarted == "0" { "/started" } else { "" }, t.bprio));
            }
        }
        (ids, label)
    }
}

/// The claim loop's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Claimed {
    pub id: String,
    pub repo: String,
    pub row: BeadRow,
    pub raw: String,
}

/// One attempt's answer, spira-lc's own exit codes: 0 applied, 3 refused, else cannot tell.
pub const LC_APPLIED: i32 = 0;
pub const LC_REFUSED: i32 = 3;

/// The lifecycle claim loop: one Claim event per ranked candidate until one applies. The row
/// is the claim, so a refusal is the bead no longer READY/REWORK — another dwarf won it — and
/// the next candidate is tried. Returns the claimed id, the log lines, and whether every
/// attempt was "cannot tell" (a machine that never answered is not an idle queue).
pub fn lc_claim_loop(
    ids: &[String],
    resumable: &[String],
    tier: Option<&str>,
    who: &str,
    mut attempt: impl FnMut(&str) -> i32,
) -> (Option<String>, Vec<String>, bool) {
    let mut logs = Vec::new();
    let mut answered = false;
    for id in ids {
        match attempt(id) {
            LC_APPLIED => {
                if resumable.iter().any(|r| r == id) {
                    logs.push(format!("{who}: resuming {id} ({}) — it already has work on its branch", tier.unwrap_or("top rank")));
                } else {
                    logs.push(format!("{who}: claiming {id} (epic-first rank)"));
                }
                return (Some(id.clone()), logs, false);
            }
            LC_REFUSED => {
                answered = true;
                logs.push(format!(
                    "{who}: the lifecycle machine refused ranked candidate {id} (no longer READY/REWORK, or its stack exceeds stack_max_depth) — trying the next ranked candidate"
                ));
            }
            rc => logs.push(format!("{who}: the lifecycle machine could not be reached for ranked candidate {id} (rc={rc}) — trying the next ranked candidate")),
        }
    }
    (None, logs, !ids.is_empty() && !answered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Out;
    use std::sync::Mutex;

    struct FakeExec {
        #[allow(clippy::type_complexity)]
        calls: Mutex<Vec<(String, Vec<String>, Option<String>)>>,
        epics: Out,
        tier: Out,
        rank: Out,
    }
    impl Exec for FakeExec {
        fn exec(&self, prog: &str, args: &[String], stdin: Option<Vec<u8>>, _cwd: Option<&Path>) -> Out {
            let sin = stdin.map(|b| String::from_utf8(b).unwrap());
            self.calls.lock().unwrap().push((prog.to_string(), args.to_vec(), sin));
            if args[0] == "epics" {
                return self.epics.clone();
            }
            if args.iter().any(|a| a == "--top-tier") {
                return self.tier.clone();
            }
            // The resumable file must exist while spira-claim reads it.
            let i = args.iter().position(|a| a == "--resumable").unwrap();
            let f = std::fs::read_to_string(&args[i + 1]).unwrap();
            let mut o = self.rank.clone();
            o.stdout = o.stdout.replace("{RES}", &f);
            o
        }
    }
    /// Each test its own scratch dir: the selector names its files `.<stem>.<pid>`, and
    /// parallel tests share one pid, so a shared dir let one test delete another's file.
    fn scratch(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("dwarf-claim-{name}"))
    }
    struct FakeGit(i64);
    impl Git for FakeGit {
        fn git(&self, _d: &Path, args: &[&str]) -> Out {
            assert_eq!(args[0], "rev-list");
            Out::ok(format!("{}\n", self.0))
        }
    }

    fn sel<'a>(e: &'a FakeExec, repos: &'a spira_config::repos::Registry, g: &'a FakeGit, dir: &'a Path) -> Selector<'a> {
        Selector { exec: e, git: g, claim_bin: "spira-claim", rune: "builder", scratch: dir, pid: 7, repos }
    }

    #[test]
    fn lookup_or_rank_failure_is_claim_error_not_idle() {
        let dir = scratch("t2");
        let e = FakeExec { calls: Mutex::new(vec![]), epics: Out::fail(2, "x"), tier: Out::ok(""), rank: Out::ok("") };
        let repos = spira_config::repos::Registry::default();
        let g = FakeGit(0);
        match sel(&e, &repos, &g, &dir).select("[]") {
            Selection::ClaimError { ledger, .. } => assert_eq!(ledger, "claim-error epic_parent_lookup failed rc=2"),
            o => panic!("{o:?}"),
        }
        let e2 = FakeExec { calls: Mutex::new(vec![]), epics: Out::ok("{}"), tier: Out::fail(2, ""), rank: Out::fail(7, "") };
        match sel(&e2, &repos, &g, &dir).select("[]") {
            Selection::ClaimError { ledger, log } => {
                assert_eq!(ledger, "claim-error epic_rank_rows failed rc=7");
                assert!(log.contains("not reporting idle"));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn lc_claim_loop_takes_the_first_applied_and_never_steals_a_refused_one() {
        let ids = vec!["sp-held".to_string(), "sp-down".into(), "sp-ok".into(), "sp-never".into()];
        let mut tried = Vec::new();
        let (c, logs, unreachable) = lc_claim_loop(&ids, &["sp-ok".into()], Some("P1/P2"), "builder/eitri", |id| {
            tried.push(id.to_string());
            match id {
                "sp-held" => LC_REFUSED,
                "sp-down" => 2,
                _ => LC_APPLIED,
            }
        });
        assert_eq!(c.as_deref(), Some("sp-ok"));
        assert!(!unreachable);
        assert_eq!(tried, vec!["sp-held", "sp-down", "sp-ok"], "stops at the first applied claim");
        assert!(logs[0].contains("refused ranked candidate sp-held"), "{logs:?}");
        assert!(logs[1].contains("could not be reached for ranked candidate sp-down (rc=2)"), "{logs:?}");
        assert_eq!(logs[2], "builder/eitri: resuming sp-ok (P1/P2) — it already has work on its branch");
    }

    #[test]
    fn lc_claim_loop_that_never_heard_the_machine_is_not_idle() {
        let ids = vec!["sp-a".to_string(), "sp-b".into()];
        let (c, _, unreachable) = lc_claim_loop(&ids, &[], None, "b/x", |_| 2);
        assert!(c.is_none() && unreachable);
        let (c, _, unreachable) = lc_claim_loop(&ids, &[], None, "b/x", |_| LC_REFUSED);
        assert!(c.is_none() && !unreachable, "refusals are an answer: idle");
        let (c, _, unreachable) = lc_claim_loop(&[], &[], None, "b/x", |_| unreachable!());
        assert!(c.is_none() && !unreachable);
    }
}
