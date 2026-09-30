//! The gate pipeline, ported from the retired suites `test-gate-touched.sh`
//! (UC-gate-verdict-08, -09), `test-certify-suites-off.sh`, `test-certify-critical-covers.sh`
//! and `test-gate-budget-select.sh` (sp-wx2tw), plus the refusals this port adds.

use super::*;
use crate::io::Git;
use std::cell::RefCell;
use std::path::Path;

/// Answers every read from canned text; a `None` read is a git failure.
#[derive(Default)]
struct FakeGit {
    name_status: Option<String>,
    raw: String,
    ls_tree: Option<Vec<String>>,
    calls: RefCell<Vec<String>>,
}

impl Git for FakeGit {
    fn diff_name_status(&self, _: &Path, b: &str, h: &str) -> Result<String, Refusal> {
        self.calls.borrow_mut().push(format!("name-status {b}...{h}"));
        self.name_status.clone().ok_or(Refusal("git diff failed".into()))
    }
    fn diff_raw(&self, _: &Path, _: &str, _: &str) -> Result<String, Refusal> {
        Ok(self.raw.clone())
    }
    fn diff_u0(&self, _: &Path, _: &str, _: &str, _: &str) -> Option<String> {
        None
    }
    fn show(&self, _: &Path, _: &str, _: &str) -> Option<String> {
        None
    }
    fn ls_tree(&self, _: &Path, rev: &str) -> Result<Vec<String>, Refusal> {
        self.calls.borrow_mut().push(format!("ls-tree {rev}"));
        self.ls_tree.clone().ok_or(Refusal(format!("cannot resolve {rev}")))
    }
    fn ls_files(&self, _: &Path) -> Result<Vec<String>, Refusal> {
        Ok(vec![])
    }
}

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(tag: &str, suites: &[(&str, &str)]) -> Dir {
    let d = std::env::temp_dir().join(format!("sp-wx2tw-gate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for (n, h) in suites {
        std::fs::write(d.join(n), h).unwrap();
    }
    Dir(d)
}

fn genv(d: &Path, extra: &[(&str, &str)]) -> GateEnv {
    let sd = d.display().to_string();
    let mut pairs: Vec<(&str, &str)> = vec![("SPIRA_BATCH_SUITE_DIR", &sd)];
    pairs.extend_from_slice(extra);
    GateEnv::from_env(&env_from_pairs(&pairs)).unwrap()
}

/// test-gate-touched.sh's fixture: a covers spira/changed.sh, a2 (added by the branch)
/// too, b covers something else, c always runs, meta covers the suites themselves.
fn fixture(tag: &str) -> Dir {
    dir(
        tag,
        &[
            ("test-a.sh", "# covers: spira/changed.sh\n"),
            ("test-a2.sh", "# covers: spira/changed.sh\n"),
            ("test-b.sh", "# covers: spira/unrelated.sh\n"),
            ("test-c.sh", "#!/bin/bash\nset -u\n"),
            ("test-meta.sh", "# covers: spira/test-*.sh\n"),
        ],
    )
}

fn diff_git() -> FakeGit {
    FakeGit {
        name_status: Some("M\tspira/changed.sh\nA\tspira/test-a2.sh\n".into()),
        ..FakeGit::default()
    }
}

#[test]
fn uc_gate_verdict_08_covers_selection_on_the_branch_tree() {
    let d = fixture("a");
    let o = run(&genv(&d.0, &[]), &diff_git(), "main", "br").unwrap();
    assert_eq!(o.suites, ["test-a.sh", "test-a2.sh", "test-c.sh", "test-meta.sh"]);
    assert!(!o.suites.contains(&"test-b.sh".to_string()));
}

#[test]
fn uc_gate_verdict_08_a_file_list_selects_from_the_base_corpus() {
    let d = fixture("c");
    let fl = d.0.join("flist");
    std::fs::write(&fl, "spira/changed.sh\n").unwrap();
    let fls = fl.display().to_string();
    let g = FakeGit {
        // The base has every suite but a2 (the branch added it).
        ls_tree: Some(
            ["spira/test-a.sh", "spira/test-b.sh", "spira/test-c.sh", "spira/test-meta.sh", "spira/lib.sh", "spira/sub/test-x.sh"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        ..FakeGit::default()
    };
    let o = run(&genv(&d.0, &[("SPIRA_GATE_FILES", &fls)]), &g, "main", "br").unwrap();
    assert_eq!(o.suites, ["test-a.sh", "test-c.sh"]);
    assert_eq!(*g.calls.borrow(), ["ls-tree main"], "the file list, not a diff");
}

#[test]
fn uc_gate_verdict_09_ejected_suites_are_always_added_once_and_only_if_present() {
    let d = fixture("d");
    let base = run(&genv(&d.0, &[]), &diff_git(), "main", "br").unwrap().suites;
    let with = |e: &str| run(&genv(&d.0, &[("SPIRA_GATE_EJECTED_SUITES", e)]), &diff_git(), "main", "br").unwrap().suites;
    assert!(with("test-b.sh").contains(&"test-b.sh".to_string()));
    let both = with("test-a.sh,test-b.sh");
    assert!(both.contains(&"test-a.sh".to_string()) && both.contains(&"test-b.sh".to_string()));
    assert_eq!(with("test-a.sh"), base, "ejecting a covered suite changes nothing");
    assert_eq!(with("test-gone.sh"), base, "an ejected suite the tree lacks is skipped");
    assert_eq!(with("../test-b.sh"), base, "a path is not a suite name");
    assert!(with("test-b.sh test-c.sh").contains(&"test-b.sh".to_string()), "blanks separate too");
}

#[test]
fn gate_all_is_the_whole_corpus() {
    let d = fixture("e");
    let o = run(&genv(&d.0, &[("SPIRA_GATE_ALL", "1")]), &diff_git(), "main", "br").unwrap();
    assert_eq!(o.suites.len(), 5);
    let ctrl = run(&genv(&d.0, &[("SPIRA_GATE_ALL", "0")]), &diff_git(), "main", "br").unwrap();
    assert!(!ctrl.suites.contains(&"test-b.sh".to_string()));
    // ALL overrides SUITES=off.
    let o = run(&genv(&d.0, &[("SPIRA_GATE_ALL", "1"), ("SPIRA_GATE_SUITES", "off")]), &diff_git(), "main", "br").unwrap();
    assert_eq!(o.suites.len(), 5);
}

fn crit_fixture(tag: &str) -> Dir {
    dir(
        tag,
        &[
            ("test-crit.sh", "# covers: spira/lib.sh#landed\n"),
            ("test-other.sh", "# covers: spira/other.sh\n"),
            ("test-nocov.sh", "set -u\n"),
        ],
    )
}

fn git_changing(path: &str) -> FakeGit {
    FakeGit {
        name_status: Some(format!("M\t{path}\n")),
        ..FakeGit::default()
    }
}

#[test]
fn suites_off_runs_only_what_covers_the_critical_file() {
    let d = crit_fixture("off");
    let off = [("SPIRA_GATE_SUITES", "off"), ("SPIRA_GATE_EJECTED_SUITES", "test-other.sh")];
    let o = run(&genv(&d.0, &off), &git_changing("spira/lib.sh"), "main", "br").unwrap();
    assert_eq!(o.suites, ["test-crit.sh"], "targeted: no always-run, no ejected");
    let o = run(&genv(&d.0, &off), &git_changing("spira/other.sh"), "main", "br").unwrap();
    assert!(o.suites.is_empty(), "{:?}", o);
    assert!(o.log[0].contains("SPIRA_GATE_SUITES=off"));
    // Positive control: on, the always-run suite is back.
    let o = run(&genv(&d.0, &[]), &git_changing("spira/lib.sh"), "main", "br").unwrap();
    assert!(o.suites.contains(&"test-nocov.sh".to_string()));
    // Configurable.
    let o = run(
        &genv(&d.0, &[("SPIRA_GATE_SUITES", "off"), ("SPIRA_CERTIFY_ALWAYS_COVERS", "spira/nonexistent.sh")]),
        &git_changing("spira/lib.sh"),
        "main",
        "br",
    )
    .unwrap();
    assert!(o.suites.is_empty());
}

#[test]
fn the_budget_cuts_most_specific_first_and_never_cuts_an_ejected_suite() {
    // 5 suites at 1s each (T1 caps, no timings); budget 3 keeps the 3 most specific.
    let mut suites: Vec<(String, String)> = Vec::new();
    for i in 1..=5 {
        let mut toks = vec!["spira/lib.sh".to_string()];
        toks.extend((2..=i).map(|j| format!("spira/filler-{j}.sh")));
        suites.push((format!("test-s{i}.sh"), format!("# covers: {}\n", toks.join(" "))));
    }
    let refs: Vec<(&str, &str)> = suites.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let d = dir("budget", &refs);
    let g = git_changing("spira/lib.sh");
    let o = run(&genv(&d.0, &[("SPIRA_GATE_BUDGET", "3")]), &g, "main", "br").unwrap();
    assert_eq!(o.suites, ["test-s1.sh", "test-s2.sh", "test-s3.sh"]);
    assert!(o.log.iter().any(|l| l.contains("dropped test-s5.sh")));
    let o = run(
        &genv(&d.0, &[("SPIRA_GATE_BUDGET", "3"), ("SPIRA_GATE_EJECTED_SUITES", "test-s5.sh")]),
        &g,
        "main",
        "br",
    )
    .unwrap();
    assert_eq!(o.suites, ["test-s1.sh", "test-s2.sh", "test-s3.sh", "test-s5.sh"]);
    // Width divides.
    let o = run(&genv(&d.0, &[("SPIRA_GATE_BUDGET", "3"), ("SPIRA_BATCH_MAXPAR", "2")]), &g, "main", "br").unwrap();
    assert_eq!(o.suites.len(), 5);
    // A pinned tier cap is what an unmeasured suite costs.
    let o = run(
        &genv(&d.0, &[("SPIRA_GATE_BUDGET", "3"), ("SPIRA_TIER_BUDGET_T1_MS", "1500")]),
        &g,
        "main",
        "br",
    )
    .unwrap();
    assert_eq!(o.suites, ["test-s1.sh", "test-s2.sh"]);
}

#[test]
fn a_measured_p90_replaces_the_tier_cap() {
    let d = dir("p90", &[("test-m.sh", "# covers: spira/lib.sh\n")]);
    let run_dir = d.0.join("run");
    std::fs::create_dir_all(run_dir.join("tsd")).unwrap();
    std::fs::write(
        run_dir.join("tsd/suite-timing.jsonl"),
        "{\"ts\":\"2026-09-28T00:00:01Z\",\"suite\":\"test-m.sh\",\"wall_secs\":2}\n",
    )
    .unwrap();
    let r = run_dir.display().to_string();
    let g = git_changing("spira/lib.sh");
    let o = run(&genv(&d.0, &[("SPIRA_RUN", &r), ("SPIRA_GATE_BUDGET", "1")]), &g, "main", "br").unwrap();
    assert!(o.suites.is_empty(), "2s P90 over a 1s budget");
    let o = run(&genv(&d.0, &[("SPIRA_RUN", &r), ("SPIRA_GATE_BUDGET", "2")]), &g, "main", "br").unwrap();
    assert_eq!(o.suites, ["test-m.sh"]);
}

// ------------------------------------------------------------------ failing closed

#[test]
fn what_it_cannot_read_is_a_refusal_never_an_empty_selection() {
    let d = fixture("refuse");
    let refused = |r: Result<GateOut, Fail>| matches!(r, Err(Fail::Refused(_)));
    // The diff fails.
    let g = FakeGit::default();
    assert!(refused(run(&genv(&d.0, &[]), &g, "main", "br")));
    // The base cannot be listed.
    let fl = d.0.join("flist");
    std::fs::write(&fl, "spira/changed.sh\n").unwrap();
    let fls = fl.display().to_string();
    assert!(refused(run(&genv(&d.0, &[("SPIRA_GATE_FILES", &fls)]), &g, "main", "br")));
    // The file list cannot be read.
    assert!(refused(run(&genv(&d.0, &[("SPIRA_GATE_FILES", "/nonexistent/flist")]), &diff_git(), "main", "br")));
    // No suite directory; an empty one.
    assert!(refused(run(&genv(Path::new("/nonexistent/spira"), &[]), &diff_git(), "main", "br")));
    let empty = dir("empty", &[]);
    assert!(refused(run(&genv(&empty.0, &[]), &diff_git(), "main", "br")));
    // A tier nobody can place.
    let bad = dir("badtier", &[("test-a.sh", "# tier: T7\n# covers: spira/changed.sh\n")]);
    assert!(refused(run(&genv(&bad.0, &[]), &diff_git(), "main", "br")));
    // A budget or tier cap that is not a number.
    let e = env_from_pairs(&[("SPIRA_GATE_BUDGET", "lots")]);
    assert!(GateEnv::from_env(&e).is_err());
    let e = env_from_pairs(&[("SPIRA_TIER_BUDGET_T3_MS", "x")]);
    assert!(GateEnv::from_env(&e).is_err());
}

#[test]
fn an_unclaimed_source_file_fails_the_gate_instead_of_emptying_it() {
    let d = fixture("unclaimed");
    let g = git_changing("spira/brand-new.sh");
    match run(&genv(&d.0, &[]), &g, "main", "br") {
        Err(Fail::Unclaimed { files, .. }) => assert_eq!(files, ["spira/brand-new.sh"]),
        o => panic!("{o:?}"),
    }
}

#[test]
fn width_and_defaults_read_like_the_bash() {
    let e = GateEnv::from_env(&env_from_pairs(&[("SPIRA_GATE_HOST_CORES", "8")])).unwrap();
    assert_eq!(e.width, 8);
    let e = GateEnv::from_env(&env_from_pairs(&[("SPIRA_BATCH_MAXPAR", "x"), ("SPIRA_GATE_HOST_CORES", "8")])).unwrap();
    assert_eq!(e.width, 1, "a set but unreadable MAXPAR is 1, as ${{MAXPAR:-…}} then the case");
    let e = GateEnv::from_env(&env_from_pairs(&[])).unwrap();
    assert_eq!((e.width, e.budget_secs, e.tiers.clone()), (1, 300.0, Some(vec!["T0".into(), "T1".into()])));
    assert_eq!(e.suite_dir, PathBuf::from("spira"));
    assert_eq!(e.always_covers, ["spira/lib.sh"]);
}

/// Part F of the retired test-gate-touched.sh: the real corpus's declaration — a
/// unit-installer-only diff selects test-install-hooks-artifact.sh.
#[test]
fn the_real_installer_suite_claims_the_unit_installer() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let c = Corpus::load_named(&dir, &["test-install-hooks-artifact.sh".into()]).unwrap();
    let s = select::select(
        &c,
        &[select::Change::modified("systemd/install.sh")],
        &mut |_| vec![],
        &Buckets::default(),
        &Options {
            no_all_fallback: true,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(s.suites, ["test-install-hooks-artifact.sh"]);
}
