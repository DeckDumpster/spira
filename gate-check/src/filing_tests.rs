//! UC-test-infrastructure-37: annotation → bead filing, over a fake whose bead list is
//! stateful, so a rescan sees what the first pass filed.

use super::*;
use std::cell::RefCell;
use std::path::Path;

struct Beads {
    list: RefCell<Vec<(String, String, i64)>>,
    notes: RefCell<Vec<(String, String)>>,
}

impl Beads {
    fn with(rows: &[(&str, &str, i64)]) -> Beads {
        Beads {
            list: RefCell::new(rows.iter().map(|(i, t, p)| (i.to_string(), t.to_string(), *p)).collect()),
            notes: RefCell::new(Vec::new()),
        }
    }
    fn count(&self, title: &str) -> usize {
        self.list.borrow().iter().filter(|b| b.1 == title).count()
    }
    fn priority(&self, title: &str) -> Option<i64> {
        self.list.borrow().iter().find(|b| b.1 == title).map(|b| b.2)
    }
}

impl World for Beads {
    fn repo_root(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn home_repo(&self) -> String {
        "home".into()
    }
    fn bd_gate_check(&self) -> String {
        String::new()
    }
    fn bd_show_json(&self, _: &str) -> String {
        String::new()
    }
    fn bd_gate_resolve(&self, _: &str) {}
    fn spira_event(&self, _: &str, _: &str, _: &str, _: &str) {}
    fn bd_list_json(&self) -> String {
        let rows: Vec<String> = self
            .list
            .borrow()
            .iter()
            .map(|(id, t, p)| format!(r#"{{"id":"{id}","title":"{t}","priority":{p}}}"#))
            .collect();
        format!("[{}]", rows.join(","))
    }
    fn lc_rows(&self) -> Result<engine::Lc, String> {
        Ok(self
            .list
            .borrow()
            .iter()
            .map(|(id, ..)| (id.clone(), spira_store::lc_state::Row { bead_id: id.clone(), state: "WORKING".into(), ..Default::default() }))
            .collect())
    }
    fn bd_priority(&self, id: &str, p: i64) {
        if let Some(b) = self.list.borrow_mut().iter_mut().find(|b| b.0 == id) {
            b.2 = p;
        }
    }
    fn bd_note(&self, id: &str, text: &str) {
        self.notes.borrow_mut().push((id.into(), text.into()));
    }
    fn file_bead(&self, title: &str, _: &str, priority: i64, _: &str) {
        let id = format!("sp-{}", self.list.borrow().len());
        self.list.borrow_mut().push((id, title.into(), priority));
    }
    fn flaky_repo(&self) -> Option<String> {
        Some("org/repo".into())
    }
    fn gh_recent_run_ids(&self, _: &str, _: u32) -> Vec<String> {
        vec!["54".into()]
    }
    fn gh_jobs_json(&self, _: &str, run_id: &str) -> String {
        match run_id {
            "55" => r#"{"jobs":[{"id":201},{"id":202}]}"#.into(),
            "54" => r#"{"jobs":[{"id":301}]}"#.into(),
            _ => String::new(),
        }
    }
    fn gh_annotations_json(&self, _: &str, job_id: &str) -> String {
        let red_twice = |s: &str| format!(r#"[{{"annotation_level":"failure","title":"red-twice suite","message":"{s}"}}]"#);
        match job_id {
            "201" => red_twice("test-alpha.sh"),
            "202" => red_twice("test-beta.sh"),
            "301" => r#"[{"annotation_level":"warning","title":"flaky suite","message":"test-gamma.sh was red, then green on a serial re-run"}]"#.into(),
            _ => String::new(),
        }
    }
    fn gh_last_green_main_sha(&self, _: &str) -> Option<String> {
        Some("aabbcc".into())
    }
    fn gh_failed_main_runs(&self, _: &str, _: u32) -> Vec<(String, String)> {
        vec![("55".into(), "aabbdd".into())]
    }
    fn gh_fail_lines(&self, _: &str, _: &str) -> String {
        String::new()
    }
    fn git_log_range(&self, _: &Path, _: &str, _: &str) -> String {
        String::new()
    }
    fn tsd_ingest(&self, _: &Path, _: &str, _: &str) {}
    fn print(&self, _: &str) {}
}

// covers: UC-test-infrastructure-37
#[test_id(tjzip5d)]
#[test]
fn a_flaky_annotation_files_one_bead_and_a_red_twice_one_p1_deduplicated_raised_and_emitted_by_gate_retry() {
    let root = testkit::TempDir::new("gate-check-filing");
    let w = Beads::with(&[]);
    run(&w, &root.join("nonexistent"));
    assert_eq!(w.priority("suite red on main: test-alpha.sh"), Some(1));
    assert_eq!(w.priority("suite red on main: test-beta.sh"), Some(1));
    assert_eq!(w.priority("flaky suite: test-gamma.sh"), Some(2));
    let filed = w.list.borrow().len();
    assert_eq!(filed, 3);
    run(&w, &root.join("nonexistent"));
    assert_eq!(w.list.borrow().len(), filed, "a rescan files nothing while the beads are open");

    let low = Beads::with(&[("sp-low", "suite red on main: test-alpha.sh", 3)]);
    run(&low, &root.join("nonexistent"));
    assert_eq!(low.priority("suite red on main: test-alpha.sh"), Some(1), "an existing lower-priority bead is raised");
    assert_eq!(low.count("suite red on main: test-alpha.sh"), 1);
    assert!(low.notes.borrow().iter().any(|(id, _)| id == "sp-low"));

    // The strings parsed above are the ones the producer emits.
    let producer = std::fs::read_to_string(spira_config::resolve::locate_home_for_process().unwrap().join("gate-retry.sh")).unwrap();
    assert!(producer.contains("::warning title=flaky suite::%s was red"), "gate-retry.sh no longer emits the flaky annotation gate-check reads");
    assert!(producer.contains("::error title=red-twice suite::"), "gate-retry.sh no longer emits the red-twice annotation gate-check reads");
}
