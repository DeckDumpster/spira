//! Which bead, and the atomic claim of it (DESIGN.md §4.1).
//!
//! Ranking is `spira-claim`'s: the ready set goes to it on STDIN and the epic lookup and the
//! resumable set in FILES — never argv (sp-o4trx: the claim outage was argv E2BIG). The one
//! thing left here is resumability, which needs the repo map and git and which spira-claim
//! by design leaves to its caller. Nothing in this module knows about briefs, chambers or
//! lifecycle_enforce (sp-f0qhr).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::bd::{self, BeadRow};
use crate::ports::{Bd, Exec, Git};
use crate::util;

/// `claim_retry <bd args…>`: the query with `--json`, retried; Ok(json_only(stdout)) — an
/// empty Ok is a real empty result — or Err(the one diagnostic line claim_retry printed).
pub fn claim_retry(bd: &dyn Bd, args: &[String], tries: u32, delay: Duration) -> Result<String, String> {
    let mut a = args.to_vec();
    a.push("--json".into());
    let tries = tries.max(1);
    let mut last = util::Out::default();
    for i in 1..=tries {
        last = bd.bd(&a);
        if last.success() {
            return Ok(util::json_only(&last.stdout));
        }
        if i < tries {
            std::thread::sleep(delay);
        }
    }
    Err(format!("claim_retry: query failed after {tries} attempt(s): {}", last.first_err_line()))
}

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
    pub fayth: &'a str,
    pub scratch: &'a Path,
    pub pid: u32,
    /// spira_config::repos (sp-o88bx, "wave 4.12") in-process, instead of the
    /// `_aeon_repo_info` bash seam this used to shell into.
    pub repos: &'a spira_config::repos::Registry,
}

impl Selector<'_> {
    fn file(&self, stem: &str) -> PathBuf {
        self.scratch.join(format!(".{stem}.{}", self.pid))
    }

    pub fn select(&self, ready_json: &str) -> Selection {
        let ready = ready_json.as_bytes().to_vec();
        let lk = self.exec.exec(self.claim_bin, &["epics".to_string()], Some(ready.clone()), None);
        if !lk.success() {
            return Selection::ClaimError {
                log: format!(
                    "{}: claim-error epic_parent_lookup failed (rc={}) — not reporting idle for a lookup that never completed",
                    self.fayth, lk.code
                ),
                ledger: format!("claim-error epic_parent_lookup failed rc={}", lk.code),
            };
        }
        let lk_file = self.file("epic-lookup");
        let rs_file = self.file("resumable");
        let _ = std::fs::write(&lk_file, lk.stdout.as_bytes());
        let lkf = lk_file.display().to_string();

        // The top tier only: resumability costs one git process per candidate.
        let tier = self.exec.exec(self.claim_bin, &crate::ports::s(&["select", "--fayth", self.fayth, "--epics", &lkf, "--top-tier"]), Some(ready.clone()), None);
        let band = if tier.success() { parse_tier(&tier.stdout) } else { Vec::new() };
        let (resumable, tier_label) = self.resumable(&band);

        let _ = std::fs::write(&rs_file, resumable.join(","));
        let rsf = rs_file.display().to_string();
        let ranked = self.exec.exec(self.claim_bin, &crate::ports::s(&["select", "--fayth", self.fayth, "--epics", &lkf, "--resumable", &rsf]), Some(ready), None);
        let _ = std::fs::remove_file(&lk_file);
        let _ = std::fs::remove_file(&rs_file);
        if !ranked.success() {
            return Selection::ClaimError {
                log: format!(
                    "{}: claim-error epic_rank_rows failed (rc={}) — not reporting idle for a rank that never completed",
                    self.fayth, ranked.code
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
        // `_aeon_repo_info <names…>`: root + base ref per name, skipping any name whose
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

/// One `bd update --claim` per ranked candidate, until one returns a row. Returns the
/// claimed bead (or None: idle) and the log lines, in order.
pub fn claim_loop(
    bd: &dyn Bd,
    ids: &[String],
    resumable: &[String],
    tier: Option<&str>,
    who: &str,
    tries: u32,
    delay: Duration,
) -> (Option<Claimed>, Vec<String>) {
    let mut logs = Vec::new();
    for id in ids {
        match claim_retry(bd, &bd::args(&["update", id, "--claim"]), tries, delay) {
            Err(e) => {
                let e = if e.is_empty() { "bd gave no reason".to_string() } else { e };
                logs.push(format!("{who}: claim query failed for ranked candidate {id}: {e} — trying the next ranked candidate"));
            }
            Ok(json) if !json.trim().is_empty() => {
                if resumable.iter().any(|r| r == id) {
                    logs.push(format!("{who}: resuming {id} ({}) — it already has work on its branch", tier.unwrap_or("top rank")));
                } else {
                    logs.push(format!("{who}: claiming {id} (epic-first rank)"));
                }
                let row = bd::first_row(&json);
                return match row {
                    Some(r) if !r.id.is_empty() => {
                        let repo = r.label_value("repo:").unwrap_or_default();
                        (Some(Claimed { id: r.id.clone(), repo, row: r, raw: json }), logs)
                    }
                    _ => (None, logs),
                };
            }
            Ok(_) => logs.push(format!(
                "{who}: ranked candidate {id} was claimed by another aeon between read and claim — trying the next ranked candidate"
            )),
        }
    }
    (None, logs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Out;
    use std::sync::Mutex;

    struct FakeExec {
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
        testkit::TempDir::new(&format!("aeon-claim-{name}"))
    }
    struct FakeGit(i64);
    impl Git for FakeGit {
        fn git(&self, _d: &Path, args: &[&str]) -> Out {
            assert_eq!(args[0], "rev-list");
            Out::ok(format!("{}\n", self.0))
        }
    }

    /// A registry mapping `svc` onto a REAL, if tiny, git checkout — `resumable()`
    /// (sp-o88bx, "wave 4.12") now resolves the base ref in-process through
    /// `spira_config::repos::landref`, which always shells to real git, so this is a real
    /// checkout rather than a canned string. Its declared base is empty on purpose: rung 4
    /// (no remote at all) resolves it to the checkout's own current branch, "main".
    fn svc_registry(dir: &Path) -> spira_config::repos::Registry {
        let repo = dir.join("svc-repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "x"]);
        let map = format!("svc | {}\n", repo.display());
        spira_config::repos::Registry::new(Some(&map), &BTreeMap::new(), Path::new(""))
    }

    fn sel<'a>(e: &'a FakeExec, repos: &'a spira_config::repos::Registry, g: &'a FakeGit, dir: &'a Path) -> Selector<'a> {
        Selector { exec: e, git: g, claim_bin: "spira-claim", fayth: "builder", scratch: dir, pid: 7, repos }
    }

    const READY: &str = r#"[{"id":"sp-a","priority":1,"labels":["repo:svc"]},{"id":"sp-b","priority":1,"labels":["repo:svc","branch:spira/x"]}]"#;

    #[test]
    fn ready_set_goes_on_stdin_and_files_carry_the_rest() {
        let dir = scratch("t1");
        let e = FakeExec {
            calls: Mutex::new(vec![]),
            epics: Out::ok("{\"prio\":{},\"started\":[]}"),
            tier: Out::ok("sp-a||svc|1|1|1\nsp-b|spira/x|svc|1|1|1\n"),
            rank: Out::ok("1\t1\t1\t0\tt\tsp-b\t\n1\t1\t1\t1\tt\tsp-a\t\n"),
        };
        let repos = svc_registry(dir.path());
        let g = FakeGit(2);
        let r = sel(&e, &repos, &g, &dir).select(READY);
        assert_eq!(r, Selection::Ranked { ids: vec!["sp-b".into(), "sp-a".into()], resumable: vec!["sp-a".into(), "sp-b".into()], tier: Some("P1/P1".into()) });
        let calls = e.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        for (prog, args, stdin) in calls.iter() {
            assert_eq!(prog, "spira-claim");
            assert_eq!(stdin.as_deref(), Some(READY), "the ready set is on stdin");
            assert!(args.iter().all(|a| !a.contains("sp-a")), "no bead payload in argv: {args:?}");
        }
        assert!(calls[2].1.contains(&"--resumable".to_string()));
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
    fn a_branch_not_ahead_is_not_resumable() {
        let dir = scratch("t3");
        let e = FakeExec { calls: Mutex::new(vec![]), epics: Out::ok("{}"), tier: Out::ok("sp-a||svc|0|0|2\n"), rank: Out::ok("0\t0\t2\t1\tt\tsp-a\t\n") };
        let repos = svc_registry(dir.path());
        let g = FakeGit(0);
        assert_eq!(sel(&e, &repos, &g, &dir).select("[]"), Selection::Ranked { ids: vec!["sp-a".into()], resumable: vec![], tier: None });
    }

    struct ClaimBd {
        answers: Mutex<Vec<Out>>,
        seen: Mutex<Vec<Vec<String>>>,
    }
    impl Bd for ClaimBd {
        fn bd(&self, args: &[String]) -> Out {
            self.seen.lock().unwrap().push(args.to_vec());
            self.answers.lock().unwrap().remove(0)
        }
    }

    // test-aeon-resume-collision.sh: a lost race falls through to the next ranked candidate.
    #[test]
    fn claim_loop_falls_through_lost_races_and_errors() {
        let bd = ClaimBd {
            answers: Mutex::new(vec![Out::ok(""), Out::fail(1, "dolt lock\n"), Out::ok("warning\n[{\"id\":\"sp-c\",\"labels\":[\"repo:svc\"]}]")]),
            seen: Mutex::new(vec![]),
        };
        let ids = vec!["sp-a".to_string(), "sp-b".into(), "sp-c".into()];
        let (c, logs) = claim_loop(&bd, &ids, &["sp-c".into()], Some("P0/started/P1"), "builder/ifrit", 1, Duration::ZERO);
        let c = c.unwrap();
        assert_eq!((c.id.as_str(), c.repo.as_str()), ("sp-c", "svc"));
        assert!(logs[0].contains("sp-a was claimed by another aeon between read and claim — trying the next ranked candidate"));
        assert!(logs[1].contains("claim query failed for ranked candidate sp-b: claim_retry: query failed after 1 attempt(s): dolt lock"));
        assert_eq!(logs[2], "builder/ifrit: resuming sp-c (P0/started/P1) — it already has work on its branch");
        assert_eq!(bd.seen.lock().unwrap()[0], vec!["update", "sp-a", "--claim", "--json"]);
    }

    #[test]
    fn nothing_claimable_is_idle() {
        let bd = ClaimBd { answers: Mutex::new(vec![]), seen: Mutex::new(vec![]) };
        let (c, logs) = claim_loop(&bd, &[], &[], None, "b/x", 3, Duration::ZERO);
        assert!(c.is_none() && logs.is_empty());
    }

    #[test]
    fn claim_retry_retries_then_reports_one_line() {
        let bd = ClaimBd { answers: Mutex::new(vec![Out::fail(1, "e1\nmore"), Out::fail(1, "e2\nmore")]), seen: Mutex::new(vec![]) };
        assert_eq!(claim_retry(&bd, &bd::args(&["ready"]), 2, Duration::ZERO), Err("claim_retry: query failed after 2 attempt(s): e2".into()));
        let ok = ClaimBd { answers: Mutex::new(vec![Out::ok("")]), seen: Mutex::new(vec![]) };
        assert_eq!(claim_retry(&ok, &bd::args(&["ready"]), 2, Duration::ZERO), Ok(String::new()), "a clean empty result is not an error");
    }
}
