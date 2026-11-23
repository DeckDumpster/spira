//! `cr open | seal | unseal | close | status` through `cli::run`: an in-memory tree and reader, a
//! findings directory holding the ledger, a recording committer and a temp run directory.

use super::{cfg, fixture, go, ledger_rows, ports_with, Recorder, SharedRecorder, DAY, NOW};
use crate::cli::Outcome;
use crate::ledger::{ClosedAs, LedgerFile, Row};
use crate::ports::{CommitInfo, FixtureBeads, FixtureRegistry, Ports, SourceTree};
use crate::record::{self, State};
use margin_client::{Anchor, Doc, FakeMargin};
use sha2::{Digest, Sha256};
use std::rc::Rc;
use testkit::TempDir;

const DOES: &str = "Opens the store and hands back a handle.";
const LOOK: &str = "Start with how open builds the handle.";
const HEADINGS: [&str; 7] = ["## What it does", "## What to look at first", "## Files", "## Who calls it", "## How often it changed", "## Incidents that touched it", "## Tests that exercise it"];
const READER: &str = "https://margin.invalid/c/col1";

struct World {
    run: TempDir,
    margin: FakeMargin,
    commits: Rc<Recorder>,
    ports: Ports,
}

fn prior_row() -> Row {
    Row { review: "900".into(), kind: "persona".into(), unit_id: "persona:other".into(), commit: "older".into(), files: vec![LedgerFile { path: "other.md".into(), blob: "b".into() }], opened_at: 5, closed_at: None, closed_as: None, attention: None }
}

fn world() -> World {
    let run = TempDir::new("cr-review");
    let margin = FakeMargin::default();
    let commits = Rc::new(Recorder::default());
    let mut ports = ports_with(fixture().with_authors("demo/src/store.rs", &["alpha", "beta", "alpha"]), vec![prior_row()], cfg());
    ports.run_dir = run.path().to_path_buf();
    ports.margin = Box::new(margin.clone());
    ports.committer = Box::new(SharedRecorder(commits.clone()));
    World { run, margin, commits, ports }
}

impl World {
    fn cr(&self, args: &[&str]) -> Outcome {
        go(&self.ports, args)
    }
    fn seal_with(&mut self, n: &str, stdin: &str) -> Outcome {
        self.ports.stdin = stdin.to_string();
        self.cr(&["seal", "--review", n])
    }
    fn comments(&self, n: u32) -> String {
        let r = record::load(self.run.path(), n).unwrap();
        let path = &r.unit.files[0].path;
        format!(r#"{{"does":"{DOES}","look_first":"{LOOK}","lens":[{{"path":"{path}","line":1,"body":"first look"}}],"fresh":[{{"path":"{path}","line":2,"body":"fresh look"}}]}}"#)
    }
    fn sealed_path(&self, n: u32) -> std::path::PathBuf {
        record::dir(self.run.path(), n).join("sealed.json")
    }
    fn open_and_seal(&mut self) -> Outcome {
        assert_eq!(self.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
        let c = self.comments(1);
        self.seal_with("001", &c)
    }
    fn cover(&self) -> String {
        self.margin.collection_posted(0).1
    }
    fn review_json(&self, n: u32) -> String {
        std::fs::read_to_string(record::dir(self.run.path(), n).join("review.json")).unwrap()
    }
    fn ledger_file(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.ports.findings_dir).join(crate::ledger::FILE)
    }
    fn his(&self, n: u32, line: u32, body: &str) -> String {
        let r = record::load(self.run.path(), n).unwrap();
        self.margin.his_thread(r.collection.as_deref().unwrap(), Anchor { path: r.unit.files[0].path.clone(), first: line, last: line }, body, 7)
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[test_id(tsduo7o)]
#[test]
fn open_count_two_stages_two_distinct_reviews_and_no_ledger_row_and_no_reader_post() {
    let w = world();
    let o = w.cr(&["open", "--count", "2", "--seed", "5"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let all = record::all(w.run.path()).unwrap();
    assert_eq!(all.iter().map(|r| r.number).collect::<Vec<_>>(), [1, 2]);
    assert!(all.iter().all(|r| r.state == State::Staged && r.kind == record::Kind::OnDemand));
    assert_ne!(all[0].unit.id, all[1].unit.id);
    assert_eq!(w.margin.collections_posted(), 0);
    assert_eq!(ledger_rows(&w.ports), vec![prior_row()]);
    for n in ["001", "002"] {
        assert!(o.stdout.contains(&format!("seal with: cr seal --review {n} < seal.json")), "{}", o.stdout);
    }
}

#[test_id(tejxosy)]
#[test]
fn open_scheduled_marks_the_review_scheduled() {
    let w = world();
    assert_eq!(w.cr(&["open", "--scheduled", "--seed", "1"]).code, 0);
    assert_eq!(record::load(w.run.path(), 1).unwrap().kind, record::Kind::Scheduled);
}

#[test_id(t0inynx)]
#[test]
fn open_with_a_bare_number_exits_two_naming_count() {
    let w = world();
    let o = w.cr(&["open", "2"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("--count"), "{}", o.stderr);
    assert!(record::all(w.run.path()).unwrap().is_empty());
}

#[test_id(t885p8b)]
#[test]
fn open_never_hands_two_reviews_one_number() {
    let w = world();
    let a = record::allocate(w.run.path()).unwrap();
    let b = record::allocate(w.run.path()).unwrap();
    assert_eq!((a, b), (1, 2));
}

#[test_id(txut26b)]
#[test]
fn seal_refuses_a_cover_missing_does_or_look_first_before_any_forge_call() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    let c = r#""lens":[{"path":"demo/src/store.rs","line":1,"body":"x"}],"fresh":[{"path":"demo/src/store.rs","line":1,"body":"y"}]"#.to_string();
    for (bad, field) in [
        (format!("[{c}]"), "does"),
        (format!(r#"{{"look_first":"{LOOK}",{c}}}"#), "does"),
        (format!(r#"{{"does":"{DOES}","look_first":"  ",{c}}}"#), "look_first"),
        (format!(r#"{{"does":"","look_first":"{LOOK}",{c}}}"#), "does"),
    ] {
        let o = w.seal_with("001", &bad);
        assert_eq!(o.code, 2, "{bad}: {}", o.stderr);
        assert!(o.stderr.contains(field), "{}", o.stderr);
    }
    assert_eq!(w.margin.collections_posted(), 0);
    assert!(!w.sealed_path(1).exists());
}

#[test_id(t12y7jd)]
#[test]
fn the_cover_lists_tests_by_call_path_not_by_name() {
    let mut w = world();
    let tree = fixture().with_file("demo/tests/names_only.rs", "use demo::store;\n\n#[test]\nfn unrelated() {\n    assert_eq!(1, 1);\n}\n");
    w.ports.tree = Box::new(tree);
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    let body = w.cover();
    assert!(body.contains("`demo/src/store/tests.rs::opens` calls `Store::open`"), "{body}");
    assert!(body.contains("`demo/tests/it.rs::it` calls `Store::open`"), "{body}");
    let tests_section = body.split("## Tests that exercise it").nth(1).unwrap();
    assert!(!tests_section.contains("names_only.rs"), "{body}");
}

#[test_id(tg2okig)]
#[test]
fn a_test_inside_a_cfg_test_module_that_calls_a_pub_function_is_listed() {
    let mut w = world();
    let src = "//! Util.\n\npub fn twice(n: u32) -> u32 {\n    n * 2\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn doubles() {\n        assert_eq!(twice(2), 4);\n    }\n}\n";
    w.ports.tree = Box::new(fixture().with_file("other/src/util.rs", src));
    assert_eq!(w.cr(&["open", "--aim", "other/src/util.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    let body = w.cover();
    assert!(body.contains("`other/src/util.rs::doubles` calls `twice`"), "{body}");
}

#[test_id(ta11sgp)]
#[test]
fn a_unit_with_no_calling_test_says_none_found_by_call_path() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/sink.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    let body = w.cover();
    assert!(body.contains("None found by call path."), "{body}");
}

#[test_id(tbkrts0)]
#[test]
fn seal_cover_note_carries_facts_and_no_critique() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    let note = w.cover();
    for want in ["alpha, beta", "demo/src/store/tests.rs", "demo/tests/it.rs", "Commits in the last 60 days: 2"] {
        assert!(note.contains(want), "missing {want:?} in:\n{note}");
    }
    assert!(!note.contains("first look"), "the sealed review must not reach the cover note");
}

#[test_id(tmseqna)]
#[test]
fn seal_refuses_an_empty_array_a_foreign_path_and_a_line_past_the_end_before_any_forge_call() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let path = record::load(w.run.path(), 1).unwrap().unit.files[0].path.clone();
    let wrap = |comments: String| format!(r#"{{"does":"{DOES}","look_first":"{LOOK}","lens":{comments},"fresh":{comments}}}"#);
    for bad in [
        wrap("[]".to_string()),
        wrap(r#"[{"path":"nowhere/else.rs","line":1,"body":"x"}]"#.to_string()),
        wrap(format!(r#"[{{"path":"{path}","line":9999,"body":"x"}}]"#)),
        wrap(format!(r#"[{{"path":"{path}","line":0,"body":"x"}}]"#)),
        "not json".to_string(),
    ] {
        let o = w.seal_with("001", &bad);
        assert_eq!(o.code, 2, "{bad}: {}", o.stderr);
    }
    assert_eq!(w.margin.collections_posted(), 0);
    assert!(!w.sealed_path(1).exists());
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
}

#[test_id(tny0eju)]
#[test]
fn status_lists_every_review_with_state_age_url_and_title() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    assert_eq!(w.cr(&["open", "--seed", "4"]).code, 0);
    let o = w.cr(&["status"]);
    let (reviews, _) = o.stdout.split_once("coverage at ").expect("a coverage section");
    let lines: Vec<&str> = reviews.lines().collect();
    assert_eq!(lines[0], "open reviews", "{}", o.stdout);
    assert!(lines[1].starts_with("001 on-demand open 0h https://margin.invalid/c/col1 "), "{}", lines[1]);
    assert!(lines[2].starts_with("002 on-demand staged 0h - "), "{}", lines[2]);
    let j: serde_json::Value = serde_json::from_str(&w.cr(&["status", "--json"]).stdout).unwrap();
    assert_eq!(j["reviews"].as_array().unwrap().len(), 2);
}

fn store_unit_files() -> Vec<String> {
    let ix = crate::index::Index::build(&fixture()).unwrap();
    let u = crate::units::all(&ix).into_iter().find(|u| u.id == "module:demo/src/store.rs").unwrap();
    u.files.into_iter().map(|f| f.path).collect()
}

fn published(review: &str, unit_id: &str, files: &[(String, String)], closed_as: ClosedAs) -> Vec<Row> {
    let open = Row { review: review.into(), kind: unit_id.split(':').next().unwrap().into(), unit_id: unit_id.into(), commit: "old".into(), files: files.iter().map(|(p, b)| LedgerFile { path: p.clone(), blob: b.clone() }).collect(), opened_at: NOW - 10 * DAY, closed_at: None, closed_as: None, attention: None };
    vec![open.clone(), open.closing(NOW - 5 * DAY, closed_as, None)]
}

fn at_today(tree: &crate::ports::FixtureTree, paths: &[String]) -> Vec<(String, String)> {
    use crate::ports::SourceTree;
    paths.iter().map(|p| (p.clone(), tree.blob(p).unwrap())).collect()
}

fn status_json(p: &Ports) -> serde_json::Value {
    let o = go(p, &["status", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    serde_json::from_str(&o.stdout).unwrap()
}

fn coverage_ports(tree: crate::ports::FixtureTree, rows: Vec<Row>, limit: usize) -> (TempDir, Ports) {
    let run = TempDir::new("cr-status");
    let mut p = ports_with(tree, rows, crate::ports::Config { status_limit: limit, ..cfg() });
    p.run_dir = run.path().to_path_buf();
    (run, p)
}

#[test_id(tagwoqt)]
#[test]
fn status_reports_coverage_by_area_and_kind() {
    let files = store_unit_files();
    let rows = published("001", "module:demo/src/store.rs", &at_today(&fixture(), &files), ClosedAs::Published);
    let (_run, p) = coverage_ports(fixture(), rows, 10);
    let c = &status_json(&p)["coverage"];
    assert_eq!(c["commit"], "c0ffee");
    assert_eq!(c["files"]["reviewed"], files.len());
    assert_eq!(c["files"]["current"], files.len());
    let area = |name: &str| c["areas"].as_array().unwrap().iter().find(|a| a["area"] == name).unwrap_or_else(|| panic!("no area {name}: {c}")).clone();
    assert!(area("demo")["reviewed"].as_u64().unwrap() > 0);
    assert_eq!(area("other")["reviewed"], 0);
    let module = c["kinds"].as_array().unwrap().iter().find(|k| k["kind"] == "module").unwrap().clone();
    assert_eq!(module["reviewed"], 1);
    let text = go(&p, &["status"]).stdout;
    assert!(text.contains("coverage at c0ffee: "), "{text}");
}

#[test_id(t59e7o4)]
#[test]
fn status_lists_the_oldest_never_reviewed_files_first_and_counts_the_rest() {
    let tree = fixture().with_commits("demo/src/net.rs", &[NOW - 50 * DAY, NOW - DAY]).with_commits("other/src/util.rs", &[NOW - 80 * DAY]);
    let (_run, p) = coverage_ports(tree, vec![], 2);
    let c = &status_json(&p)["coverage"];
    let listed = &c["never_reviewed"]["files"];
    let paths: Vec<&str> = listed.as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["demo/src/store.rs", "other/src/util.rs"]);
    assert_eq!(listed[0]["first_commit_ms"], NOW - 90 * DAY);
    let total = c["files"]["total"].as_u64().unwrap();
    assert_eq!(c["never_reviewed"]["more"], total - 2);
    assert!(go(&p, &["status"]).stdout.contains(&format!("… {} more", total - 2)));
}

#[test_id(tuxy59c)]
#[test]
fn status_lists_files_changed_since_review_with_lines_changed() {
    let files = store_unit_files();
    let old: Vec<(String, String)> = at_today(&fixture(), &files).into_iter().map(|(p, b)| if p == "demo/src/store.rs" { (p, "oldblob".to_string()) } else { (p, b) }).collect();
    let rows = published("001", "module:demo/src/store.rs", &old, ClosedAs::Published);
    let tree = fixture().with_changed("demo/src/store.rs", "oldblob", 4);
    let (_run, p) = coverage_ports(tree, rows, 10);
    let c = &status_json(&p)["coverage"];
    assert_eq!(c["changed"]["files"][0], serde_json::json!({"path": "demo/src/store.rs", "review": "001", "changed_lines": 4}));
    assert_eq!(c["files"]["reviewed"], files.len());
    assert_eq!(c["files"]["current"], files.len() - 1);
}

#[test_id(tuuha1h)]
#[test]
fn status_counts_a_file_reviewed_under_its_old_name() {
    let files = store_unit_files();
    let mut at = at_today(&fixture(), &files);
    at[0].0 = "demo/src/old_store.rs".into();
    let rows = published("001", "module:demo/src/store.rs", &at, ClosedAs::Published);
    let (_run, p) = coverage_ports(fixture().with_rename("old", "demo/src/old_store.rs", &files[0]), rows, 10);
    let c = &status_json(&p)["coverage"];
    assert_eq!(c["files"]["current"], files.len());
}

#[test_id(t155l3y)]
#[test]
fn a_review_closed_unpublished_adds_no_coverage() {
    let files = store_unit_files();
    let rows = published("001", "module:demo/src/store.rs", &at_today(&fixture(), &files), ClosedAs::Unpublished);
    let (_run, p) = coverage_ports(fixture(), rows, 10);
    let c = &status_json(&p)["coverage"];
    assert_eq!(c["files"]["reviewed"], 0);
    assert_eq!(c["kinds"].as_array().unwrap().iter().find(|k| k["kind"] == "module").unwrap()["reviewed"], 0);
}

#[test_id(tpm45gj)]
#[test]
fn a_set_state_stamps_the_matching_time_and_keeps_the_rest() {
    let w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let r = record::set_state(w.run.path(), 1, State::Closed, 99).unwrap();
    assert_eq!((r.state, r.closed_at), (State::Closed, Some(99)));
    assert_eq!(record::load(w.run.path(), 1).unwrap(), r);
}

#[test_id(tw0z9i4)]
#[test]
fn seal_rerun_with_different_comments_is_refused_not_silently_dropped() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    w.margin.fail_comment_number(1);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 1);
    let other = c.replace("first look", "second thoughts");
    let o = w.seal_with("001", &other);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("different comments"), "{}", o.stderr);
}

#[test_id(twk2jco)]
#[test]
fn seal_rerun_with_a_different_fresh_read_is_refused() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    w.margin.fail_comment_number(1);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 1);
    let o = w.seal_with("001", &c.replace("fresh look", "another fresh look"));
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("different comments"), "{}", o.stderr);
}

#[test_id(tpn9kae)]
#[test]
fn seal_rerun_with_a_different_cover_is_refused() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    w.margin.fail_comment_number(1);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 1);
    let o = w.seal_with("001", &c.replace(DOES, "Something else entirely."));
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("different cover"), "{}", o.stderr);
}

#[test_id(tsuv27m)]
#[test]
fn the_section_guard_names_a_missing_heading_and_a_blank_section() {
    use crate::review::missing_section;
    let full: String = HEADINGS.iter().map(|h| format!("{h}\n\ntext\n\n")).collect();
    assert_eq!(missing_section(&full), None);
    assert_eq!(missing_section(&full.replace("## Files", "## Fils")), Some("## Files"));
    assert_eq!(missing_section(&full.replace("## Who calls it\n\ntext", "## Who calls it\n\n")), Some("## Who calls it"));
}

#[test_id(tkq7fmz)]
#[test]
fn seal_refuses_a_prose_field_carrying_a_heading_line_and_a_bare_array_on_a_rerun() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let c = w.comments(1);
    let o = w.seal_with("001", &c.replace(DOES, "x\\n## Files\\ny"));
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("does"), "{}", o.stderr);
    w.margin.fail_comment_number(1);
    assert_eq!(w.seal_with("001", &c).code, 1);
    let o = w.seal_with("001", r#"[{"path":"x","line":1,"body":"y"}]"#);
    assert_eq!(o.code, 2, "{}", o.stderr);
}

fn commit(sha: &str, subject: &str, reverts: bool) -> CommitInfo {
    CommitInfo { sha: sha.into(), at_ms: NOW - DAY, subject: subject.into(), reverts }
}

fn sealed_body(w: &mut World) -> String {
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    w.cover()
}

#[test_id(te469fb)]
#[test]
fn the_cover_counts_every_commit_and_its_reverts_and_fixes() {
    let mut w = world();
    let log = vec![
        commit("a1", "spira: land sp-bug001 — stop the store losing a write", false),
        commit("a2", "sp-feat02: add a handle", false),
        commit("a3", "Revert \"tidy\"", true),
        commit("a4", "tidy", false),
        commit("a5", "wip", false),
    ];
    w.ports.tree = Box::new(fixture().with_log("demo/src/store.rs", log));
    w.ports.beads = Box::new(FixtureBeads { types: Ok([("sp-bug001", "bug"), ("sp-feat02", "feature")].map(|(a, b)| (a.to_string(), b.to_string())).into()), incidents: Ok(Default::default()) });
    let body = sealed_body(&mut w);
    assert!(body.contains("- Commits: 5 ever, 1 reverts, 1 fixes"), "{body}");
}

#[test_id(twwucb4)]
#[test]
fn the_cover_gives_the_rewritten_share_and_median_line_age() {
    let mut w = world();
    let now = 1000 * DAY;
    w.ports.clock = Box::new(crate::ports::VirtualClock::new(now));
    w.ports.tree = Box::new(fixture().with_line_times("demo/src/store.rs", &[now - 10 * DAY, now - 10 * DAY, now - 200 * DAY, now - 400 * DAY]));
    let body = sealed_body(&mut w);
    assert!(body.contains("50% of"), "{body}");
    assert!(body.contains("Rewritten in the last 45 days: 50% of"), "{body}");
    assert!(body.contains("median line age 105 days"), "{body}");
}

#[test_id(td5c8eq)]
#[test]
fn the_cover_names_tests_deleted_later_and_disabled_as_flaky() {
    let mut w = world();
    w.ports.tree = Box::new(fixture().with_removed_test("demo/src/store/tests.rs", "closes", "deadbeefcafe", NOW - 5 * DAY));
    w.ports.registry = Box::new(FixtureRegistry(Ok(vec![("unit:demo store::tests::opens".into(), 0)])));
    let body = sealed_body(&mut w);
    assert!(body.contains("- Deleted later: 1"), "{body}");
    assert!(body.contains("`demo/src/store/tests.rs::closes`") && body.contains("deadbee"), "{body}");
    assert!(body.contains("- Disabled as flaky: 1"), "{body}");
    assert!(body.contains("`unit:demo store::tests::opens` 1970-01-01"), "{body}");
}

#[test_id(te48p23)]
#[test]
fn an_unreadable_bead_store_or_registry_says_unknown_not_zero() {
    let mut w = world();
    w.ports.tree = Box::new(fixture().with_log("demo/src/store.rs", vec![commit("a1", "sp-bug001: fix", false)]));
    w.ports.beads = Box::new(FixtureBeads { types: Err("store down".into()), incidents: Ok(Default::default()) });
    w.ports.registry = Box::new(FixtureRegistry(Err("registry locked".into())));
    let body = sealed_body(&mut w);
    assert!(body.contains("fixes: unknown (store down)"), "{body}");
    assert!(body.contains("Disabled as flaky: unknown (registry locked)"), "{body}");
    assert!(!body.contains("0 fixes") && !body.contains("Disabled as flaky: 0"), "{body}");
}

#[test_id(tdux8bk)]
#[test]
fn a_staged_review_holds_its_unit_back_from_the_next_open() {
    let w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    let o = w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]);
    assert_eq!(o.code, 1, "{}", o.stderr);
}

#[test_id(tgwcdxf)]
#[test]
fn seal_refuses_a_sealed_review_missing_either_half_before_any_forge_call() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    let one = r#"[{"path":"demo/src/store.rs","line":1,"body":"x"}]"#;
    let head = format!(r#""does":"{DOES}","look_first":"{LOOK}""#);
    for (bad, half) in [
        (format!(r#"{{{head},"lens":[],"fresh":{one}}}"#), "lens"),
        (format!(r#"{{{head},"fresh":{one}}}"#), "lens"),
        (format!(r#"{{{head},"lens":{one}}}"#), "fresh"),
        (format!(r#"{{{head},"lens":{one},"fresh":[]}}"#), "fresh"),
    ] {
        let o = w.seal_with("001", &bad);
        assert_eq!(o.code, 2, "{bad}: {}", o.stderr);
        assert!(o.stderr.contains(half), "{}", o.stderr);
    }
    assert_eq!(w.margin.collections_posted(), 0);
    assert!(!w.sealed_path(1).exists());
}

#[test_id(ts2dkhu)]
#[test]
fn a_review_sealed_as_one_undivided_array_scores_all_lens_and_no_fresh() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    let path = record::load(w.run.path(), 1).unwrap().unit.files[0].path.clone();
    std::fs::write(w.sealed_path(1), format!(r#"[{{"path":"{path}","line":1,"body":"old"}}]"#)).unwrap();
    record::set_state(w.run.path(), 1, State::Published, NOW).unwrap();
    let j: serde_json::Value = serde_json::from_str(&w.cr(&["status", "--json"]).stdout).unwrap();
    assert_eq!(j["scores"][0]["fresh_points"], 0);
    assert_eq!(j["scores"][0]["lens_predicted"], 0);
}

#[test_id(t7sobt0)]
#[test]
fn the_cover_lists_the_incidents_whose_commits_touched_the_unit() {
    let mut w = world();
    let log = vec![commit("a1", "spira: land sp-inc001 — the store lost a write", false), commit("a2", "sp-feat02: add a handle", false)];
    w.ports.tree = Box::new(fixture().with_log("demo/src/store.rs", log));
    w.ports.beads = Box::new(FixtureBeads { types: Ok(Default::default()), incidents: Ok([("sp-inc001".to_string(), "the store lost a write".to_string())].into()) });
    let body = sealed_body(&mut w);
    let section = body.split("## Incidents that touched it").nth(1).unwrap().split("\n## ").next().unwrap();
    assert!(section.contains("- `sp-inc001`: the store lost a write"), "{body}");
    assert!(!section.contains("sp-feat02"), "{body}");
}

#[test_id(tmxn8u4)]
#[test]
fn a_unit_no_incident_touched_says_none_found() {
    let mut w = world();
    w.ports.tree = Box::new(fixture().with_log("demo/src/store.rs", vec![commit("a2", "sp-feat02: add a handle", false)]));
    let body = sealed_body(&mut w);
    let section = body.split("## Incidents that touched it").nth(1).unwrap().split("\n## ").next().unwrap();
    assert!(section.contains("None found."), "{body}");
}

#[test_id(toomx3g)]
#[test]
fn an_unreadable_incident_lookup_says_unknown_not_none() {
    let mut w = world();
    w.ports.tree = Box::new(fixture().with_log("demo/src/store.rs", vec![commit("a2", "sp-feat02: add a handle", false)]));
    w.ports.beads = Box::new(FixtureBeads { types: Ok(Default::default()), incidents: Err("store down".into()) });
    let body = sealed_body(&mut w);
    assert!(body.contains("Incidents: unknown (store down)") && !body.contains("None found.\n\n## Tests"), "{body}");
}

#[test_id(tjg38lz)]
#[test]
fn a_unit_whose_commits_name_no_bead_says_none_found_without_asking_the_store() {
    let mut w = world();
    w.ports.tree = Box::new(fixture().with_log("demo/src/store.rs", vec![commit("a1", "tidy", false)]));
    w.ports.beads = Box::new(FixtureBeads { types: Ok(Default::default()), incidents: Err("store down".into()) });
    let body = sealed_body(&mut w);
    let section = body.split("## Incidents that touched it").nth(1).unwrap().split("\n## ").next().unwrap();
    assert!(section.contains("None found.") && !body.contains("Incidents: unknown"), "{body}");
}

#[test_id(tfbsgr3)]
#[test]
fn seal_posts_one_collection_of_the_units_files_whole_then_the_held_labelled_comments() {
    let mut w = world();
    let o = w.open_and_seal();
    assert_eq!(o.code, 0, "{}", o.stderr);
    let r = record::load(w.run.path(), 1).unwrap();
    assert_eq!(w.margin.collections_posted(), 1);
    let (title, _, docs) = w.margin.collection_posted(0);
    let tree = fixture();
    let want: Vec<Doc> = r.unit.files.iter().map(|f| Doc { path: f.path.clone(), kind: "code".into(), text: String::from_utf8(tree.read(&f.path).unwrap()).unwrap() }).collect();
    assert_eq!((title, docs), (r.unit.title.clone(), want));
    let posts = w.margin.comments_posted();
    let first = &r.unit.files[0].path;
    let at = |line| Anchor { path: first.clone(), first: line, last: line };
    assert_eq!(posts.iter().map(|p| (p.body.as_str(), p.held, p.anchor.clone(), p.in_reply_to.clone())).collect::<Vec<_>>(), [("**His lens:** first look", true, at(1), None), ("**Fresh read:** fresh look", true, at(2), None)]);
    assert_eq!((r.state, r.collection.as_deref(), r.url.as_deref()), (State::Open, Some("col1"), Some(READER)));
    assert_eq!(o.stdout, format!("{READER}\n"));
}

#[test_id(t87pbnr)]
#[test]
fn seal_puts_the_cover_note_in_the_collection_and_no_pull_request_wording() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    let body = w.cover();
    for h in HEADINGS {
        assert!(body.contains(h), "missing {h:?} in:\n{body}");
    }
    let sealed = std::fs::read(w.sealed_path(1)).unwrap();
    for want in [DOES, LOOK, &format!("sealed-sha256: {}", sha256(&sealed))] {
        assert!(body.contains(want), "missing {want:?} in:\n{body}");
    }
    assert!(!body.contains("Source "), "{body}");
    assert!(!body.contains("Add single comment") && !body.contains("pull request"), "{body}");
}

#[test_id(tsrcpst)]
#[test]
fn seal_posts_the_source_url_and_commit_with_the_collection() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    assert_eq!(w.margin.source_posted(0), Some(margin_client::Source { repository: crate::tests::SOURCE_URL.into(), commit: "c0ffee".into() }));
}

#[test_id(tsrcerr)]
#[test]
fn seal_without_a_source_url_exits_one_with_its_text_posts_nothing_and_stays_staged() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    w.ports.source_url = Err("remote \"gone\" (SPIRA_PUBLISH_REMOTE) has no URL".into());
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("remote \"gone\" (SPIRA_PUBLISH_REMOTE)"), "{}", o.stderr);
    assert_eq!(w.margin.collections_posted(), 0);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
    assert!(!w.sealed_path(1).exists());
}

#[test_id(tnz1lbs)]
#[test]
fn seal_sends_a_large_module_whole_and_byte_identical() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/big.rs", "--seed", "2"]).code, 0);
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let source = String::from_utf8(fixture().read("demo/src/big.rs").unwrap()).unwrap();
    let (_, note, docs) = w.margin.collection_posted(0);
    assert_eq!(docs.iter().find(|d| d.path == "demo/src/big.rs").map(|d| d.text.as_str()), Some(source.as_str()));
    let lines = source.lines().count() as u32;
    let want = format!("- `demo/src/big.rs`: {lines} lines (about {} pages)\n- Total: {lines} lines", lines.div_ceil(10));
    assert!(note.contains(&want), "missing {want:?} in:\n{note}");
}

#[test_id(tmjnknl)]
#[test]
fn seal_failing_at_the_second_held_comment_stays_staged_and_a_rerun_posts_only_the_rest() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    w.margin.fail_comment_number(2);
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("did not answer"), "{}", o.stderr);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
    assert_eq!(w.margin.comments_posted().len(), 1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(w.margin.collections_posted(), 1);
    let bodies: Vec<String> = w.margin.comments_posted().into_iter().map(|p| p.body).collect();
    assert_eq!(bodies, ["**His lens:** first look", "**Fresh read:** fresh look"]);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Open);
}

#[test_id(tmrri4b)]
#[test]
fn seal_with_the_reader_down_names_the_reader_and_leaves_the_review_staged() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    w.margin.go_down();
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("the reader at") && o.stderr.contains("did not answer POST /api/collections"), "{}", o.stderr);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
    assert_eq!(ledger_rows(&w.ports), vec![prior_row()]);
}

#[test_id(tqvhqif)]
#[test]
fn unseal_on_an_open_review_the_reader_has_not_published_exits_two_and_changes_nothing() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    let (before, posted) = (w.review_json(1), w.margin.comments_posted().len());
    let o = w.cr(&["unseal", "--review", "001"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("review 001 is not published in the reader; it waits on his Publish"), "{}", o.stderr);
    assert_eq!((w.review_json(1), w.margin.comments_posted().len()), (before, posted));
}

#[test_id(t3qdrx2)]
#[test]
fn unseal_once_the_reader_shows_publish_records_the_time_and_posts_nothing() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    w.margin.publish("col1", NOW + 123);
    let posted = w.margin.comments_posted().len();
    let o = w.cr(&["unseal", "--review", "001"]);
    assert_eq!((o.code, o.stdout.as_str()), (0, "review 001 published\n"), "{}", o.stderr);
    let r = record::load(w.run.path(), 1).unwrap();
    assert_eq!((r.state, r.published_at), (State::Published, Some(NOW + 123)));
    assert_eq!((w.margin.comments_posted().len(), w.margin.collections_posted()), (posted, 1));
    let again = w.cr(&["unseal", "--review", "001"]);
    assert_eq!((again.code, again.stdout.as_str()), (0, "review 001 published\n"));
}

#[test_id(ti67dio)]
#[test]
fn unseal_of_a_staged_review_exits_two_and_has_no_said_flag() {
    let w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    assert_eq!(w.cr(&["unseal", "--review", "001"]).code, 2);
    let o = w.cr(&["unseal", "--review", "001", "--said", "5"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("--said"), "{}", o.stderr);
}

#[test_id(t1poskd)]
#[test]
fn open_scheduled_with_a_scheduled_review_not_closed_exits_one_naming_close_and_stages_nothing() {
    let w = world();
    assert_eq!(w.cr(&["open", "--scheduled", "--seed", "1"]).code, 0);
    let o = w.cr(&["open", "--scheduled", "--seed", "2"]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("scheduled review 001 is staged; it must close first (cr close --review 001)"), "{}", o.stderr);
    assert!(!record::dir(w.run.path(), 2).exists());
    assert_eq!(w.cr(&["open", "--seed", "4"]).code, 0, "an on-demand open is unaffected");
    assert!(record::dir(w.run.path(), 2).exists());
    assert_eq!(w.cr(&["close", "--review", "001"]).code, 0);
    assert_eq!(w.cr(&["open", "--scheduled", "--seed", "5"]).code, 0, "closing frees the next scheduled open");
}

#[test_id(t19ep42)]
#[test]
fn close_writes_one_close_row_commits_it_and_closes_the_collection_once() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    let o = w.cr(&["close", "--review", "001"]);
    assert_eq!((o.code, o.stdout.as_str()), (0, "review 001 closed\n"), "{}", o.stderr);
    let rows: Vec<Row> = ledger_rows(&w.ports).into_iter().filter(|r| r.review == "001").collect();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[1].closed_at, rows[1].closed_as, rows[1].attention.clone()), (Some(NOW), Some(ClosedAs::Unpublished), None));
    let log = w.commits.log.borrow().clone();
    assert_eq!(log.last().unwrap(), &(vec!["ledger.jsonl".to_string()], "ledger: 001 closed".to_string()));
    assert_eq!(w.margin.closed(), ["col1"]);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Closed);
    let again = w.cr(&["close", "--review", "001"]);
    assert_eq!((again.code, again.stdout.as_str()), (0, "review 001 already closed\n"));
    assert_eq!((ledger_rows(&w.ports).len(), w.commits.log.borrow().len(), w.margin.closed().len()), (3, log.len(), 1));
}

fn close_published(w: &mut World, his: &[(u32, &str)]) -> Row {
    assert_eq!(w.open_and_seal().code, 0);
    w.margin.publish("col1", NOW + 1);
    assert_eq!(w.cr(&["unseal", "--review", "001"]).code, 0);
    for (line, body) in his {
        w.his(1, *line, body);
    }
    let finding = r#"{"words":"w","exemplar":{"path":"demo/src/store.rs","line":3},"kind":"refactor","rule":"r","scope":"unit"}"#;
    std::fs::write(record::dir(w.run.path(), 1).join("findings.jsonl"), format!("{finding}\n")).unwrap();
    let o = w.cr(&["close", "--review", "001"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    ledger_rows(&w.ports).pop().unwrap()
}

#[test_id(tcljrl5)]
#[test]
fn a_published_reviews_close_row_counts_his_comments_by_path_from_the_threads_and_not_the_agents() {
    let mut w = world();
    let last = close_published(&mut w, &[(1, "h1"), (3, "h2")]);
    assert_eq!(last.closed_as, Some(ClosedAs::Published));
    let a = last.attention.expect("a published close records attention");
    assert_eq!(serde_json::to_value(&a.comments_by_path).unwrap(), serde_json::json!({"demo/src/store.rs": 2}));
    assert_eq!(serde_json::to_value(&a.finding_kinds).unwrap(), serde_json::json!({"refactor": 1}));
}

#[test_id(tl6hk4a)]
#[test]
fn comments_recorded_at_close_weigh_the_next_pick_in_their_area() {
    let mut w = world();
    close_published(&mut w, &[(1, "h1")]);
    let o = w.cr(&["pick", "--aim", "demo/src/net.rs", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
    assert!(v["weight"]["attention_share"].as_f64().unwrap() > 0.0, "{v}");
}

#[test_id(tp2umop)]
#[test]
fn closing_a_review_never_sealed_writes_no_ledger_row_and_closes_it() {
    let w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    let o = w.cr(&["close", "--review", "001"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(w.commits.log.borrow().is_empty() && w.margin.closed().is_empty());
    assert_eq!(ledger_rows(&w.ports), vec![prior_row()]);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Closed);
}

#[test_id(t64hsfy)]
#[test]
fn close_of_a_review_the_reader_published_but_unseal_never_saw_closes_it_as_published() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    w.margin.publish("col1", NOW + 9);
    assert_eq!(w.cr(&["close", "--review", "001"]).code, 0);
    let r = record::load(w.run.path(), 1).unwrap();
    assert_eq!((r.state, r.published_at), (State::Closed, Some(NOW + 9)));
    assert_eq!(ledger_rows(&w.ports).pop().unwrap().closed_as, Some(ClosedAs::Published));
}

#[test_id(t8tc6rt)]
#[test]
fn seal_appends_one_open_row_to_the_ledger_and_commits_that_file_alone() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    w.commits.fail.set(true);
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(o.stderr.contains("git commit failed"), "{}", o.stderr);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged, "a failed append leaves the review staged");
    assert_eq!(ledger_rows(&w.ports), vec![prior_row()], "a failed commit leaves the file as it was");
    w.commits.fail.set(false);
    w.ports.clock = Box::new(crate::ports::VirtualClock::new(NOW + 5_000));
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(w.margin.collections_posted(), 1, "the rerun makes no second collection");
    let rows = ledger_rows(&w.ports);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], prior_row());
    let r = &rows[1];
    let store = fixture();
    assert_eq!((r.review.as_str(), r.unit_id.as_str(), r.opened_at, r.closed_at, r.closed_as), ("001", "module:demo/src/store.rs", NOW + 5_000, None, None));
    let want: Vec<(String, String)> = record::load(w.run.path(), 1).unwrap().unit.files.iter().map(|f| (f.path.clone(), store.blob(&f.path).unwrap())).collect();
    assert_eq!(r.files.iter().map(|f| (f.path.clone(), f.blob.clone())).collect::<Vec<_>>(), want);
    assert_eq!(w.commits.log.borrow().last().unwrap(), &(vec!["ledger.jsonl".to_string()], "ledger: 001 opened".to_string()));
    assert_eq!(w.cr(&["seal", "--review", "001"]).code, 2);
    assert_eq!(ledger_rows(&w.ports).len(), 2);
}

#[test_id(t13iwtf)]
#[test]
fn with_no_ledger_file_and_no_history_status_exits_zero_with_zero_coverage() {
    let w = world();
    std::fs::remove_file(w.ledger_file()).unwrap();
    let j = status_json(&w.ports);
    assert_eq!(j["coverage"]["files"]["reviewed"], 0);
}

#[test_id(tin6b8t)]
#[test]
fn a_ledger_deleted_after_it_was_committed_makes_status_exit_one_naming_the_restore() {
    let w = world();
    std::fs::remove_file(w.ledger_file()).unwrap();
    w.commits.history.set(true);
    let o = w.cr(&["status"]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("ledger.jsonl was deleted from") && o.stderr.contains("restore it from git"), "{}", o.stderr);
}

#[test_id(tjh21c8)]
#[test]
fn status_shows_each_published_reviews_two_scores_in_order() {
    let mut w = world();
    for (n, seed) in [("001", "3"), ("002", "4"), ("003", "5")] {
        assert_eq!(w.cr(&["open", "--seed", seed]).code, 0);
        let c = w.comments(n.parse().unwrap());
        assert_eq!(w.seal_with(n, &c).code, 0);
    }
    let path = record::load(w.run.path(), 1).unwrap().unit.files[0].path.clone();
    w.his(1, 1, "h");
    w.his(1, 40, "h2");
    let dir = record::dir(w.run.path(), 1);
    let finding = format!(r#"{{"id":"001-1","words":"w","exemplar":{{"path":"{path}","line":1}},"kind":"question","rule":"r","scope":"unit","endorses":1}}"#);
    std::fs::write(dir.join("findings.jsonl"), format!("{finding}\n")).unwrap();
    record::set_state(w.run.path(), 3, State::Published, NOW).unwrap();
    record::set_state(w.run.path(), 1, State::Closed, NOW).unwrap();
    let j = status_json(&w.ports);
    let rows: Vec<(&str, u64, u64, u64, u64)> = j["scores"].as_array().unwrap().iter().map(|r| (r["review"].as_str().unwrap(), r["lens_predicted"].as_u64().unwrap(), r["his_comments"].as_u64().unwrap(), r["fresh_endorsed"].as_u64().unwrap(), r["fresh_points"].as_u64().unwrap())).collect();
    assert_eq!(rows, [("001", 1, 2, 1, 1), ("003", 0, 0, 0, 1)], "002 is still open and has no score row");
    let text = w.cr(&["status"]).stdout;
    let at = text.find("scores\n").expect("a scores section");
    let lines: Vec<&str> = text[at..].lines().skip(1).take(2).collect();
    assert_eq!(lines, ["  001 lens 1/2 fresh 1/1", "  003 lens 0/0 fresh 0/1"]);
}

fn review_of(number: u32, kind: record::Kind, state: State) -> record::Review {
    let unit = crate::units::Unit { id: "u".into(), kind: crate::units::Kind::Module, files: vec![], title: "t".into() };
    record::Review { number, kind, state, source_repo: "s".into(), source_sha: "c".into(), collection: None, url: None, unit, sealed_sha256: None, staged_at: 0, opened_at: None, published_at: None, closed_at: None }
}

#[test_id(tdoh0ml)]
#[test]
fn a_scheduled_review_not_closed_blocks_and_a_closed_or_on_demand_one_does_not() {
    use record::Kind::{OnDemand, Scheduled};
    for state in [State::Staged, State::Open, State::Published] {
        assert_eq!(record::scheduled_blocker(&[review_of(1, Scheduled, State::Closed), review_of(2, Scheduled, state)]), Some((2, state)));
    }
    assert_eq!(record::scheduled_blocker(&[review_of(1, Scheduled, State::Closed), review_of(2, OnDemand, State::Open)]), None);
    assert_eq!(record::scheduled_blocker(&[]), None);
}

#[test_id(ttw4958)]
#[test]
fn a_record_written_before_the_reader_still_loads() {
    let w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let path = record::dir(w.run.path(), 1).join("review.json");
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (k, val) in [("branch", serde_json::json!("cr/001-x")), ("pr", serde_json::json!(7)), ("pr_url", serde_json::json!("https://forge.invalid/pull/7"))] {
        v[k] = val;
    }
    for k in ["collection", "url"] {
        v.as_object_mut().unwrap().remove(k);
    }
    std::fs::write(&path, v.to_string()).unwrap();
    let r = record::load(w.run.path(), 1).unwrap();
    assert_eq!((r.collection, r.url), (None, None));
}

#[test_id(thqwf0k)]
#[test]
fn close_with_the_reader_down_or_the_ledger_commit_failing_leaves_the_review_unclosed_and_a_rerun_finishes() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    w.margin.go_down();
    let o = w.cr(&["close", "--review", "001"]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("the reader at"), "{}", o.stderr);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Open);
    assert!(ledger_rows(&w.ports).iter().all(|r| r.closed_at.is_none()), "no close row was written");
    w.margin.come_up();
    w.commits.fail.set(true);
    assert_eq!(w.cr(&["close", "--review", "001"]).code, 1);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Open);
    assert_eq!(ledger_rows(&w.ports).len(), 2, "a failed commit leaves the ledger as it was");
    w.commits.fail.set(false);
    assert_eq!(w.cr(&["close", "--review", "001"]).code, 0);
    assert_eq!(ledger_rows(&w.ports).iter().filter(|r| r.review == "001" && r.closed_at.is_some()).count(), 1);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Closed);
}

#[test_id(tiqle0c)]
#[test]
fn seal_against_a_dead_reader_address_names_the_address_and_route_and_leaves_the_review_staged() {
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let token = TempDir::new("cr-dead-token");
    std::fs::write(token.path().join("t"), "tok").unwrap();
    let mut w = world();
    w.ports.margin = Box::new(margin_client::HttpMargin { base: base.clone(), token_file: token.path().join("t").to_str().unwrap().to_string(), timeout: std::time::Duration::from_secs(1) });
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains(&base) && o.stderr.contains("POST /api/collections") && !o.stderr.contains("tok\n"), "{}", o.stderr);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
}

#[test_id(t1grunh)]
#[test]
fn a_verb_that_never_reaches_the_reader_runs_with_the_reader_unconfigured() {
    let mut w = world();
    w.ports.margin = Box::new(margin_client::Unconfigured("SPIRA_MARGIN_URL is empty; name it in the config, it has no default".into()));
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    assert_eq!(w.cr(&["status"]).code, 0);
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("SPIRA_MARGIN_URL"), "{}", o.stderr);
}

#[test_id(t7pd1ov)]
#[test]
fn a_progress_file_and_a_record_that_still_carry_documents_load_and_the_seal_resumes_on_one_collection() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    w.margin.fail_comment_number(2);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 1);
    let legacy = serde_json::json!({"demo/src/store.rs": ["doc-1", 1]});
    for name in ["progress.json", "review.json"] {
        let path = record::dir(w.run.path(), 1).join(name);
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["documents"] = legacy.clone();
        std::fs::write(&path, v.to_string()).unwrap();
    }
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(w.margin.collections_posted(), 1);
    assert_eq!(w.margin.comments_posted().len(), 2);
}
