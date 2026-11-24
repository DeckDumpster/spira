//! `run`'s walk over a fake [`World`]: what it discovers, what it reports, what it prints.

use super::*;
use std::cell::RefCell;
use std::path::Path;
use testkit::TempDir;

struct Fake {
    root: TempDir,
    gates: &'static str,
    check: &'static str,
    discovered: RefCell<Vec<(String, String)>>,
    resolved: RefCell<Vec<String>>,
    printed: RefCell<String>,
    repos: Vec<&'static str>,
}

impl Fake {
    fn new(gates: &'static str, check: &'static str) -> Fake {
        Fake {
            root: TempDir::new("gate-check-fake"),
            gates,
            check,
            discovered: RefCell::new(Vec::new()),
            resolved: RefCell::new(Vec::new()),
            printed: RefCell::new(String::new()),
            repos: vec!["prrepo", "pushrepo"],
        }
    }
}

impl World for Fake {
    fn repo_names(&self) -> Vec<String> {
        self.repos.iter().map(|r| r.to_string()).collect()
    }
    fn repo_land(&self, name: &str) -> String {
        if name.starts_with("pr") { "pr" } else { "push" }.into()
    }
    fn repo_root(&self, name: &str) -> Option<PathBuf> {
        let dir = self.root.path().join(name);
        std::fs::create_dir_all(dir.join(if name == "prunborn" { "src" } else { ".git" })).unwrap();
        Some(dir)
    }
    fn home_repo(&self) -> String {
        "prrepo".into()
    }
    fn bd_gate_list_json(&self) -> String {
        self.gates.into()
    }
    fn bd_gate_discover(&self, repo: &str, branch: &str) {
        self.discovered.borrow_mut().push((repo.into(), branch.into()));
    }
    fn bd_gate_check(&self) -> String {
        self.check.into()
    }
    fn bd_show_json(&self, _: &str) -> String {
        r#"[{"description":"gate blocking zq-7aa until CI resolves"}]"#.into()
    }
    fn bd_gate_resolve(&self, id: &str) {
        self.resolved.borrow_mut().push(id.into());
    }
    fn spira_event(&self, _: &str, _: &str, _: &str, _: &str) {}
    fn bd_list_json(&self) -> String {
        "[]".into()
    }
    fn lc_rows(&self) -> Result<engine::Lc, String> {
        Err("no lifecycle in a unit test".into())
    }
    fn bd_priority(&self, _: &str, _: i64) {}
    fn bd_note(&self, _: &str, _: &str) {}
    fn file_bead(&self, _: &str, _: &str, _: i64, _: &str) {}
    fn flaky_repo(&self) -> Option<String> {
        None
    }
    fn gh_recent_run_ids(&self, _: &str, _: u32) -> Vec<String> {
        Vec::new()
    }
    fn gh_jobs_json(&self, _: &str, _: &str) -> String {
        String::new()
    }
    fn gh_annotations_json(&self, _: &str, _: &str) -> String {
        String::new()
    }
    fn gh_last_green_main_sha(&self, _: &str) -> Option<String> {
        None
    }
    fn gh_failed_main_runs(&self, _: &str, _: u32) -> Vec<(String, String)> {
        Vec::new()
    }
    fn gh_fail_lines(&self, _: &str, _: &str) -> String {
        String::new()
    }
    fn git_log_range(&self, _: &Path, _: &str, _: &str) -> String {
        String::new()
    }
    fn tsd_ingest(&self, _: &Path, _: &str, _: &str) {}
    fn print(&self, s: &str) {
        self.printed.borrow_mut().push_str(s);
    }
}

const TWO_GATES_SAME_BRANCH: &str = r#"[
    {"await_type":"gh:run","await_id":"","metadata":{"branch":"spira/sp-1"}},
    {"await_type":"gh:run","await_id":"","metadata":{"branch":"spira/sp-1"}}
]"#;

// covers: UC-landing-merge-queue-52
#[test_id(t9t7ces)]
#[test]
fn an_unbound_gate_branch_is_discovered_once_in_pr_repos_only_and_a_stuck_gate_is_reported() {
    let root = testkit::TempDir::new("gate-check-run");
    let w = Fake::new(
        TWO_GATES_SAME_BRANCH,
        "gate x-1: no run ID specified\n⚠ zq-9k2: ESCALATE red\n",
    );
    run(&w, &root.join("nonexistent"));

    let found = w.discovered.borrow();
    assert_eq!(found.len(), 1, "one discover per distinct branch, pr repos only: {found:?}");
    assert_eq!(found[0].1, "spira/sp-1");
    assert_eq!(found[0].0, "prrepo");
    assert!(w.printed.borrow().contains("1 stuck: no await_id"));
    assert!(w.printed.borrow().contains("ESCALATE red"), "gate check ran and its output is shown");
    assert_eq!(*w.resolved.borrow(), vec!["zq-9k2".to_string()], "any id prefix is parsed");
}

// covers: UC-landing-merge-queue-52
#[test_id(t5bemt6)]
#[test]
fn with_no_unbound_gate_nothing_is_discovered() {
    let root = testkit::TempDir::new("gate-check-run");
    let w = Fake::new("[]", "");
    run(&w, &root.join("nonexistent"));
    assert!(w.discovered.borrow().is_empty());
    assert!(!w.printed.borrow().contains("stuck"));
}

// covers: UC-landing-merge-queue-53
#[test_id(ty98wmg)]
#[test]
fn a_gates_branch_is_discovered_in_each_pr_repo_that_has_a_checkout_and_in_no_other() {
    let root = testkit::TempDir::new("gate-check-scoped");
    let mut w = Fake::new(TWO_GATES_SAME_BRANCH, "");
    w.repos = vec!["prrepo", "pushrepo", "prsecond", "prunborn"];
    run(&w, &root.join("nonexistent"));

    let found: Vec<(String, String)> = w.discovered.borrow().clone();
    assert_eq!(
        found,
        [("prrepo".to_string(), "spira/sp-1".to_string()), ("prsecond".to_string(), "spira/sp-1".to_string())],
        "each pr repo with a checkout is asked once, by name; a push repo and a repo with no checkout are not"
    );
}
