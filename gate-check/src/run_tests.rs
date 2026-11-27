//! `run`'s walk over a fake [`World`]: what it reports, resolves and prints.

use super::*;
use std::cell::RefCell;
use std::path::Path;

#[derive(Default)]
struct Fake {
    check: &'static str,
    resolved: RefCell<Vec<String>>,
    printed: RefCell<String>,
}

impl World for Fake {
    fn repo_root(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn home_repo(&self) -> String {
        "prrepo".into()
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

// covers: UC-landing-merge-queue-52
#[test_id(t23sotu)]
#[test]
fn a_stuck_gate_is_reported_and_an_escalated_one_is_resolved() {
    let w = Fake { check: "gate x-1: no run ID specified\n⚠ zq-9k2: ESCALATE red\n", ..Fake::default() };
    run(&w, Path::new("nonexistent"));

    assert!(w.printed.borrow().contains("1 stuck: no await_id"));
    assert!(w.printed.borrow().contains("ESCALATE red"), "gate check ran and its output is shown");
    assert_eq!(*w.resolved.borrow(), vec!["zq-9k2".to_string()], "any id prefix is parsed");
}
