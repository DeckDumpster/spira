//! Unit tests over a `Fake` [`crate::ports::World`] — one scenario per decision point
//! census.sh's own orchestration made (the clock-skew guard, the watermark fallback,
//! suppression by open/closed remedy, orphan annotation, `--with-suppressed` gating).
//! The six `census/*.py` scripts and the SQL itself are treated as already-correct,
//! unchanged dependencies (DESIGN.md §2) — the `Fake` stands in for their output directly.

use crate::ports::World;
use crate::*;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

pub struct Fake {
    pub env: RefCell<std::collections::BTreeMap<String, String>>,
    pub events_sql: RefCell<std::collections::BTreeMap<String, Result<String, String>>>, // key: since as string, "" = all-time
    pub count_out: RefCell<std::collections::BTreeMap<String, String>>, // key: the tabular input
    pub merge_out: RefCell<String>,
    pub covers_out: RefCell<String>,
    pub covers_closed_out: RefCell<String>,
    pub handwritten_out: RefCell<String>,
    pub deliberate_out: RefCell<String>,
    pub fold_map: RefCell<String>,
    pub repo_root: RefCell<Option<String>>,
    pub landed: RefCell<std::collections::BTreeMap<String, i32>>,
    pub branches: RefCell<Vec<String>>, // bead ids with a matching branch
    pub host_epoch: RefCell<i64>,
    pub utc_row: RefCell<Result<String, String>>,
    pub parsed_epoch: RefCell<std::collections::BTreeMap<String, i64>>,
    pub files: RefCell<std::collections::BTreeMap<PathBuf, String>>,
    pub stdout: RefCell<Vec<String>>,
    pub stderr: RefCell<Vec<String>>,
    /// Every remedy bd holds (`covers:*`), as bd's JSON — bd's own status included.
    pub remedies: RefCell<String>,
    /// What covers.py / covers_closed.py were fed.
    pub covers_in: RefCell<String>,
    pub covers_closed_in: RefCell<String>,
    /// `spira-lc list`'s rows.
    pub lc: RefCell<Result<Vec<spira_config::lc_state::Row>, String>>,
}

impl Default for Fake {
    fn default() -> Self {
        Fake {
            env: RefCell::new(Default::default()),
            events_sql: RefCell::new(Default::default()),
            count_out: RefCell::new(Default::default()),
            merge_out: RefCell::new(String::new()),
            covers_out: RefCell::new(String::new()),
            covers_closed_out: RefCell::new(String::new()),
            handwritten_out: RefCell::new(String::new()),
            deliberate_out: RefCell::new(String::new()),
            fold_map: RefCell::new(String::new()),
            repo_root: RefCell::new(Some("/repo".into())),
            landed: RefCell::new(Default::default()),
            branches: RefCell::new(Vec::new()),
            host_epoch: RefCell::new(1_000_000),
            utc_row: RefCell::new(Ok("2026-09-30 12:00:00 |".into())),
            parsed_epoch: RefCell::new(Default::default()),
            files: RefCell::new(Default::default()),
            stdout: RefCell::new(Vec::new()),
            stderr: RefCell::new(Vec::new()),
            remedies: RefCell::new("[]".into()),
            covers_in: RefCell::new(String::new()),
            covers_closed_in: RefCell::new(String::new()),
            lc: RefCell::new(Ok(Vec::new())),
        }
    }
}

impl Fake {
    fn set_lc(&self, rows: &[(&str, &str)]) {
        *self.lc.borrow_mut() = Ok(rows
            .iter()
            .map(|(id, st)| spira_config::lc_state::Row { bead_id: id.to_string(), state: st.to_string(), ..Default::default() })
            .collect());
    }
    fn set(&self, k: &str, v: &str) {
        self.env.borrow_mut().insert(k.to_string(), v.to_string());
    }
    fn stdout_joined(&self) -> String {
        self.stdout.borrow().join("\n")
    }
    /// Wires a clean clock (substrate epoch == host epoch) so tests that aren't about the
    /// clock-skew guard don't have to think about it.
    fn clean_clock(&self) {
        let epoch = *self.host_epoch.borrow();
        self.parsed_epoch.borrow_mut().insert("2026-09-30 12:00:00".into(), epoch);
    }
    fn set_events(&self, since: Option<i64>, rows: &str) {
        let key = since.map(|s| s.to_string()).unwrap_or_default();
        self.events_sql.borrow_mut().insert(key, Ok(rows.to_string()));
    }
    fn set_count(&self, tabular: &str, counted: &str) {
        self.count_out.borrow_mut().insert(tabular.to_string(), counted.to_string());
    }
}

impl World for Fake {
    fn env(&self, k: &str) -> Option<String> {
        self.env.borrow().get(k).cloned()
    }
    fn census_events_run_sql(&self, since: Option<i64>) -> Result<String, String> {
        let key = since.map(|s| s.to_string()).unwrap_or_default();
        self.events_sql.borrow().get(&key).cloned().unwrap_or_else(|| Ok(String::new()))
    }
    fn census_handwritten_run_sql(&self) -> String {
        String::new()
    }
    fn census_deliberate_run_sql(&self, _since: Option<i64>) -> String {
        String::new()
    }
    fn census_class_fold_map(&self) -> String {
        self.fold_map.borrow().clone()
    }
    fn deliberate_cause_names(&self) -> Vec<String> {
        vec!["work-close-converted".to_string(), "eject".to_string()]
    }
    fn repo_root(&self) -> Option<String> {
        self.repo_root.borrow().clone()
    }
    fn lc_landed(&self, id: &str) -> i32 {
        self.landed.borrow().get(id).copied().unwrap_or(2)
    }
    fn count_py(&self, tabular: &str) -> Result<String, String> {
        Ok(self.count_out.borrow().get(tabular).cloned().unwrap_or_default())
    }
    fn merge_py(&self, _all_time: &str, _since_wm: &str) -> String {
        self.merge_out.borrow().clone()
    }
    fn covers_py(&self, bdq_json: &str, _fold_map: &str) -> String {
        *self.covers_in.borrow_mut() = bdq_json.to_string();
        self.covers_out.borrow().clone()
    }
    fn covers_closed_py(&self, bdq_json: &str, _fold_map: &str) -> String {
        *self.covers_closed_in.borrow_mut() = bdq_json.to_string();
        self.covers_closed_out.borrow().clone()
    }
    fn handwritten_py(&self, _tabular: &str) -> String {
        self.handwritten_out.borrow().clone()
    }
    fn deliberate_py(&self, _tabular: &str) -> String {
        self.deliberate_out.borrow().clone()
    }
    fn bd_list_all_json(&self, _label_pattern: &str) -> String {
        self.remedies.borrow().clone()
    }
    fn lc_rows(&self) -> Result<Vec<spira_config::lc_state::Row>, String> {
        self.lc.borrow().clone()
    }
    fn git_branch_exists_matching(&self, _repo: &str, pattern: &str) -> bool {
        self.branches.borrow().iter().any(|b| pattern.contains(b))
    }
    fn host_utc_epoch(&self) -> i64 {
        *self.host_epoch.borrow()
    }
    fn bd_sql_utc_now_row(&self) -> Result<String, String> {
        self.utc_row.borrow().clone()
    }
    fn parse_utc_to_epoch(&self, s: &str) -> Option<i64> {
        self.parsed_epoch.borrow().get(s).copied()
    }
    fn format_epoch_utc(&self, epoch: i64) -> String {
        format!("epoch:{epoch}")
    }
    fn read_to_string(&self, p: &Path) -> Option<String> {
        self.files.borrow().get(p).cloned()
    }
    fn out(&self, s: &str) {
        self.stdout.borrow_mut().push(s.to_string());
    }
    fn err(&self, s: &str) {
        self.stderr.borrow_mut().push(s.to_string());
    }
}

// ============================================================================ clock skew guard

#[test]
fn clock_skew_within_tolerance_proceeds() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "");
    f.set_count("", "");
    assert_eq!(run(&f, false), 0);
    // No skew-guard complaint — the only stderr noise, if any, is the expected
    // "no watermark file" fallback notice, not a clock refusal.
    assert!(!f.stderr.borrow().iter().any(|l| l.contains("clock") || l.contains("refusing to rank blind")));
}

#[test]
fn clock_skew_beyond_tolerance_refuses() {
    let f = Fake::default();
    // substrate epoch 1_000_500 vs host 1_000_000 -> 500s skew, over the 120s default tolerance.
    f.parsed_epoch.borrow_mut().insert("2026-09-30 12:00:00".into(), 1_000_500);
    let rc = run(&f, false);
    assert_eq!(rc, 1);
    assert!(f.stderr.borrow().iter().any(|l| l.contains("clock skew is 500s")));
}

#[test]
fn clock_query_failure_refuses() {
    let f = Fake::default();
    *f.utc_row.borrow_mut() = Err("bd: connection refused".into());
    let rc = run(&f, false);
    assert_eq!(rc, 1);
    assert!(f.stderr.borrow().iter().any(|l| l.contains("refusing to rank blind")));
}

#[test]
fn clock_row_unparseable_refuses() {
    let f = Fake::default();
    *f.utc_row.borrow_mut() = Ok("garbage".into());
    let rc = run(&f, false);
    assert_eq!(rc, 1);
}

// ============================================================================ events substrate

#[test]
fn events_substrate_unreachable_refuses() {
    let f = Fake::default();
    f.clean_clock();
    f.events_sql.borrow_mut().insert(String::new(), Err("no server".into()));
    let rc = run(&f, false);
    assert_eq!(rc, 1);
    assert!(f.stderr.borrow().iter().any(|l| l.contains("events substrate is unreachable")));
}

// ============================================================================ watermark / all-time formatting

#[test]
fn no_watermark_file_falls_back_to_all_time_format() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "raw-rows");
    f.set_count("raw-rows", "3 18 sp-recur-suite-red\n");
    let rc = run(&f, false);
    assert_eq!(rc, 0);
    assert_eq!(f.stdout_joined(), "3 sp-recur-suite-red (18 detections)");
    assert!(f.stderr.borrow().iter().any(|l| l.contains("no watermark file")));
}

#[test]
fn unreadable_watermark_warns_and_falls_back() {
    let f = Fake::default();
    f.clean_clock();
    f.set("SPIRA_RUN", "/run");
    f.files.borrow_mut().insert(PathBuf::from("/run/maechen.watermark"), "not-a-number".into());
    f.set_events(None, "raw-rows");
    f.set_count("raw-rows", "1 2 sp-reclaim\n");
    run(&f, false);
    assert!(f.stderr.borrow().iter().any(|l| l.contains("unreadable")));
}

#[test]
fn valid_watermark_uses_merge_py() {
    let f = Fake::default();
    f.clean_clock();
    f.set("SPIRA_RUN", "/run");
    f.files.borrow_mut().insert(PathBuf::from("/run/maechen.watermark"), "1700000000".into());
    f.set_events(None, "all-rows");
    f.set_events(Some(1_700_000_000), "since-rows");
    f.set_count("all-rows", "5 9 sp-recur-suite-red\n");
    f.set_count("since-rows", "1 2 sp-recur-suite-red\n");
    *f.merge_out.borrow_mut() = "1 sp-recur-suite-red (2 detections, 5 all-time)".into();
    let rc = run(&f, false);
    assert_eq!(rc, 0);
    assert_eq!(f.stdout_joined(), "1 sp-recur-suite-red (2 detections, 5 all-time)");
}

// ============================================================================ suppression

#[test]
fn open_remedy_suppresses_by_default_and_shows_with_flag() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "rows");
    f.set_count("rows", "4 6 sp-recur-suite-red\n");
    *f.covers_out.borrow_mut() = "sp-recur-suite-red\n".into();

    let rc = run(&f, false);
    assert_eq!(rc, 0);
    assert!(f.stdout_joined().is_empty(), "suppressed by default: {}", f.stdout_joined());
}

#[test]
fn open_remedy_suppressed_shown_with_suppressed_flag() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "rows");
    f.set_count("rows", "4 6 sp-recur-suite-red\n");
    *f.covers_out.borrow_mut() = "sp-recur-suite-red\n".into();

    run(&f, true);
    assert!(f.stdout_joined().contains("[suppressed]"), "{}", f.stdout_joined());
}

#[test]
fn closed_remedy_with_live_branch_is_suppressed() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "rows");
    f.set_count("rows", "2 3 sp-recur-doctor-gh-intake-skip\n");
    *f.covers_closed_out.borrow_mut() = "sp-4w1pp sp-recur-doctor-gh-intake-skip\n".into();
    f.landed.borrow_mut().insert("sp-4w1pp".into(), 1);
    f.branches.borrow_mut().push("sp-4w1pp".into());

    run(&f, true);
    assert!(f.stdout_joined().contains("[suppressed: remedy closed, not landed]"), "{}", f.stdout_joined());
}

#[test]
fn closed_remedy_with_no_branch_is_orphaned() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "rows");
    f.set_count("rows", "2 3 sp-recur-doctor-gh-intake-skip\n");
    *f.covers_closed_out.borrow_mut() = "sp-4w1pp sp-recur-doctor-gh-intake-skip\n".into();
    f.landed.borrow_mut().insert("sp-4w1pp".into(), 1);
    // no branch registered

    let rc = run(&f, false);
    assert_eq!(rc, 0);
    // orphan annotation shows even WITHOUT --with-suppressed (it names a real, unresolved gap).
    assert!(f.stdout_joined().contains("[orphaned remedy sp-4w1pp: closed, nothing in flight]"), "{}", f.stdout_joined());
}

#[test]
fn closed_remedy_landed_is_neither_suppressed_nor_orphaned() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "rows");
    f.set_count("rows", "2 3 sp-recur-doctor-gh-intake-skip\n");
    *f.covers_closed_out.borrow_mut() = "sp-4w1pp sp-recur-doctor-gh-intake-skip\n".into();
    f.landed.borrow_mut().insert("sp-4w1pp".into(), 0); // landed — the fix already shipped

    run(&f, false);
    let out = f.stdout_joined();
    assert!(out.contains("2 sp-recur-doctor-gh-intake-skip"));
    assert!(!out.contains("suppressed") && !out.contains("orphaned"));
}

#[test]
fn closed_remedy_unknown_land_status_is_not_suppressed_and_warns() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "rows");
    f.set_count("rows", "2 3 sp-recur-doctor-gh-intake-skip\n");
    *f.covers_closed_out.borrow_mut() = "sp-4w1pp sp-recur-doctor-gh-intake-skip\n".into();
    f.landed.borrow_mut().insert("sp-4w1pp".into(), 2);

    run(&f, false);
    assert!(f.stderr.borrow().iter().any(|l| l.contains("land status unknown")));
    let out = f.stdout_joined();
    assert!(out.contains("2 sp-recur-doctor-gh-intake-skip"));
}

// ============================================================================ --with-suppressed extras

#[test]
fn with_suppressed_prints_handwritten_and_deliberate() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "");
    f.set_count("", "");
    *f.handwritten_out.borrow_mut() = "hand-written (not ranked): sp-recur-x 1 beads (actor operator)\n".into();
    *f.deliberate_out.borrow_mut() = "deliberate, not ranked: sp-reopen-eject 2 beads (3 events)\n".into();

    run(&f, true);
    let out = f.stdout_joined();
    assert!(out.contains("hand-written (not ranked): sp-recur-x"));
    assert!(out.contains("deliberate, not ranked: sp-reopen-eject"));
}

#[test]
fn without_with_suppressed_extras_are_silent() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "");
    f.set_count("", "");
    *f.handwritten_out.borrow_mut() = "hand-written (not ranked): sp-recur-x 1 beads (actor operator)\n".into();

    run(&f, false);
    assert!(!f.stdout_joined().contains("hand-written"));
}

#[test]
fn plain_class_with_no_remedy_is_unsuppressed() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "rows");
    f.set_count("rows", "7 12 sp-reopen-unrecorded\n");
    run(&f, false);
    assert_eq!(f.stdout_joined(), "7 sp-reopen-unrecorded (12 detections)");
}

/// sp-oqf8c: `Real::landed` asks the lifecycle record (`spira-lc state`), never the retired
/// landing-pass subject oracle. Stubs for both sit in the release's `bin/` (the parent of
/// `home`, which `child_path_env` puts first on PATH); the landing-pass stub always says
/// "landed", so only a reader of spira-lc gets the CERTIFIED, no-row and cannot-tell rows right.
#[test]
fn real_landed_reads_the_lifecycle_record_not_the_landing_pass_oracle() {
    let root = testkit::TempDir::new("census-landed");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(root.join("spira")).unwrap();
    testkit::write_exe(bin.join("landing-pass"), "#!/bin/sh\necho deadbeef; exit 0\n");
    testkit::write_exe(
        bin.join("spira-lc"),
        "#!/bin/sh\n[ \"$1\" = state ] || exit 2\ncase \"$2\" in sp-l) echo LANDED ;; sp-c) echo CERTIFIED ;; sp-n) exit 1 ;; *) exit 2 ;; esac\n",
    );
    let r = crate::real::Real::new(root.join("spira"));
    assert_eq!(r.lc_landed("sp-l"), 0, "LANDED in the lifecycle record is landed");
    assert_eq!(r.lc_landed("sp-c"), 1, "CERTIFIED is not landed, whatever landing-pass says");
    assert_eq!(r.lc_landed("sp-n"), 1, "no row (spira-lc NO_ROW) is not landed");
    assert_eq!(r.lc_landed("sp-x"), 2, "the record cannot answer: cannot tell");
}

/// sp-mve9i: a remedy is open or closed by its lifecycle row, never by bd's status (design
/// §3.4). A remedy still WORKING whose bd row someone closed by hand still suppresses its
/// class; one SUBMITTED whose bd row was reopened by hand is a closed remedy, checked for a
/// landing; a remedy with no lifecycle row is neither, and says so.
#[test]
fn remedies_are_split_by_the_lifecycle_row_not_bd_status() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "rows");
    f.set_count("rows", "4 6 sp-recur-suite-red\n");
    *f.remedies.borrow_mut() = r#"[{"id":"sp-w","status":"closed","labels":["covers:a"]},
        {"id":"sp-s","status":"open","labels":["covers:b"]},
        {"id":"sp-n","status":"open","labels":["covers:c"]}]"#
        .into();
    f.set_lc(&[("sp-w", "WORKING"), ("sp-s", "SUBMITTED")]);
    run(&f, false);
    let ids = |j: &str| -> Vec<String> {
        let v: Vec<serde_json::Value> = serde_json::from_str(j).unwrap_or_default();
        v.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(ids(&f.covers_in.borrow()), vec!["sp-w"], "open remedies");
    assert_eq!(ids(&f.covers_closed_in.borrow()), vec!["sp-s"], "closed remedies");
    assert!(f.stderr.borrow().iter().any(|l| l.contains("sp-n") && l.contains("no lifecycle row")), "{:?}", f.stderr.borrow());
}

/// A lifecycle machine that cannot answer suppresses nothing and says why: no remedy is
/// split by a guess.
#[test]
fn an_unreachable_lifecycle_machine_suppresses_nothing() {
    let f = Fake::default();
    f.clean_clock();
    f.set_events(None, "rows");
    f.set_count("rows", "4 6 sp-recur-suite-red\n");
    *f.remedies.borrow_mut() = r#"[{"id":"sp-w","status":"open","labels":["covers:a"]}]"#.into();
    *f.lc.borrow_mut() = Err("spira-lc list exited 2".into());
    run(&f, false);
    assert_eq!(f.covers_in.borrow().trim(), "[]");
    assert!(f.stderr.borrow().iter().any(|l| l.contains("lifecycle") && l.contains("not suppressing")), "{:?}", f.stderr.borrow());
}
