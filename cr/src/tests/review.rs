//! `cr open | seal | unseal | status` through `cli::run`: an in-memory tree, a scripted
//! review repository and a temp run directory.

use super::gh::{unbase64, FakeGh, Shared, NAME, OWNER};
use super::{cfg, fixture, go, ledger_rows, ports_with, seeded, DAY, NOW};
use crate::ledger::{ClosedAs, LedgerFile, Row};
use crate::cli::Outcome;
use crate::ports::{CommitInfo, FixtureBeads, FixtureRegistry, Ports};
use crate::record::{self, State};
use sha2::{Digest, Sha256};
use std::rc::Rc;
use testkit::TempDir;

const DOES: &str = "Opens the store and hands back a handle.";
const LOOK: &str = "Start with how open builds the handle.";
const HEADINGS: [&str; 7] = ["## What it does", "## What to look at first", "## Files", "## Who calls it", "## How often it changed", "## Incidents that touched it", "## Tests that exercise it"];

struct World {
    run: TempDir,
    gh: Rc<FakeGh>,
    ports: Ports,
}

fn prior_row() -> Row {
    Row { review: "900".into(), kind: "persona".into(), unit_id: "persona:other".into(), commit: "older".into(), files: vec![LedgerFile { path: "other.md".into(), blob: "b".into() }], opened_at: 5, closed_at: None, closed_as: None, attention: None }
}

fn world() -> World {
    let run = TempDir::new("cr-review");
    let gh = Rc::new(seeded(&[prior_row()]));
    let mut ports = ports_with(fixture().with_authors("demo/src/store.rs", &["alpha", "beta", "alpha"]), vec![], cfg());
    ports.run_dir = run.path().to_path_buf();
    ports.gh = Box::new(Shared(gh.clone()));
    ports.repo_owner = OWNER.into();
    ports.repo_name = NAME.into();
    World { run, gh, ports }
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
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn open_count_two_stages_two_distinct_reviews_and_no_ledger_row_and_no_forge_write() {
    let w = world();
    let o = w.cr(&["open", "--count", "2", "--seed", "5"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let all = record::all(w.run.path()).unwrap();
    assert_eq!(all.iter().map(|r| r.number).collect::<Vec<_>>(), [1, 2]);
    assert!(all.iter().all(|r| r.state == State::Staged && r.kind == record::Kind::OnDemand));
    assert_ne!(all[0].unit.id, all[1].unit.id);
    assert!(w.gh.put("contents/ledger.jsonl").is_empty() && w.gh.posted("git/blobs").is_empty());
    assert_eq!(ledger_rows(&w.gh), vec![prior_row()]);
    for n in ["001", "002"] {
        assert!(o.stdout.contains(&format!("seal with: cr seal --review {n} < seal.json")), "{}", o.stdout);
    }
}

#[test]
fn open_scheduled_marks_the_review_scheduled() {
    let w = world();
    assert_eq!(w.cr(&["open", "--scheduled", "--seed", "1"]).code, 0);
    assert_eq!(record::load(w.run.path(), 1).unwrap().kind, record::Kind::Scheduled);
}

#[test]
fn open_with_a_bare_number_exits_two_naming_count() {
    let w = world();
    let o = w.cr(&["open", "2"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("--count"), "{}", o.stderr);
    assert!(record::all(w.run.path()).unwrap().is_empty());
}

#[test]
fn open_never_hands_two_reviews_one_number() {
    let w = world();
    let a = record::allocate(w.run.path()).unwrap();
    let b = record::allocate(w.run.path()).unwrap();
    assert_eq!((a, b), (1, 2));
}

#[test]
fn seal_stores_the_unit_bytes_in_blobs_tree_and_pull_request() {
    let mut w = world();
    let o = w.open_and_seal();
    assert_eq!(o.code, 0, "{}", o.stderr);
    let r = record::load(w.run.path(), 1).unwrap();
    let tree = fixture();
    let blobs = w.gh.posted("git/blobs");
    let mut got: Vec<Vec<u8>> = blobs.iter().map(|b| unbase64(b["content"].as_str().unwrap())).collect();
    let mut want: Vec<Vec<u8>> = r.unit.files.iter().map(|f| crate::ports::SourceTree::read(&tree, &f.path).unwrap()).collect();
    got.sort();
    want.sort();
    assert_eq!(got, want);
    assert!(blobs.iter().all(|b| b["encoding"] == "base64"));
    assert_eq!(w.gh.posted("git/trees")[0]["base_tree"], "base-tree");
    assert_eq!(w.gh.posted("git/commits")[0]["parents"][0], "base-head");
    assert_eq!(w.gh.posted("git/refs")[0]["ref"], format!("refs/heads/{}", r.branch));
    assert_eq!(w.gh.posted("pulls")[0]["title"], r.unit.title);
    assert_eq!((r.state, r.pr), (State::Open, Some(7)));
    assert!(o.stdout.contains("pull/7"), "{}", o.stdout);
}

#[test]
fn seal_puts_the_cover_note_in_the_pull_request_description_and_only_unit_files_on_the_branch() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    let r = record::load(w.run.path(), 1).unwrap();
    let trees = w.gh.posted("git/trees");
    assert_eq!(trees.len(), 1);
    let mut paths: Vec<String> = trees[0]["tree"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap().to_string()).collect();
    paths.sort();
    let mut expect: Vec<String> = r.unit.files.iter().map(|f| f.path.clone()).collect();
    expect.sort();
    assert_eq!(paths, expect);
    let body = w.gh.posted("pulls")[0]["body"].as_str().unwrap().to_string();
    for h in HEADINGS {
        assert!(body.contains(h), "missing {h:?} in:\n{body}");
    }
    let sealed = std::fs::read(w.sealed_path(1)).unwrap();
    for want in [DOES, LOOK, "c0ffee", "Add single comment", &format!("sealed-sha256: {}", sha256(&sealed))] {
        assert!(body.contains(want), "missing {want:?} in:\n{body}");
    }
}

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
    assert_eq!(w.gh.count(), 0);
    assert!(!w.sealed_path(1).exists());
}

#[test]
fn the_cover_lists_tests_by_call_path_not_by_name() {
    let mut w = world();
    let tree = fixture().with_file("demo/tests/names_only.rs", "use demo::store;\n\n#[test]\nfn unrelated() {\n    assert_eq!(1, 1);\n}\n");
    w.ports.tree = Box::new(tree);
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    let body = w.gh.posted("pulls")[0]["body"].as_str().unwrap().to_string();
    assert!(body.contains("`demo/src/store/tests.rs::opens` calls `Store::open`"), "{body}");
    assert!(body.contains("`demo/tests/it.rs::it` calls `Store::open`"), "{body}");
    let tests_section = body.split("## Tests that exercise it").nth(1).unwrap();
    assert!(!tests_section.contains("names_only.rs"), "{body}");
}

#[test]
fn a_test_inside_a_cfg_test_module_that_calls_a_pub_function_is_listed() {
    let mut w = world();
    let src = "//! Util.\n\npub fn twice(n: u32) -> u32 {\n    n * 2\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn doubles() {\n        assert_eq!(twice(2), 4);\n    }\n}\n";
    w.ports.tree = Box::new(fixture().with_file("other/src/util.rs", src));
    assert_eq!(w.cr(&["open", "--aim", "other/src/util.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    let body = w.gh.posted("pulls")[0]["body"].as_str().unwrap().to_string();
    assert!(body.contains("`other/src/util.rs::doubles` calls `twice`"), "{body}");
}

#[test]
fn a_unit_with_no_calling_test_says_none_found_by_call_path() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/sink.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    let body = w.gh.posted("pulls")[0]["body"].as_str().unwrap().to_string();
    assert!(body.contains("None found by call path."), "{body}");
}

#[test]
fn seal_cover_note_carries_facts_and_no_critique() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    let note = w.gh.posted("pulls")[0]["body"].as_str().unwrap().to_string();
    for want in ["alpha, beta", "demo/src/store/tests.rs", "demo/tests/it.rs", "Commits in the last 60 days: 2", "c0ffee"] {
        assert!(note.contains(want), "missing {want:?} in:\n{note}");
    }
    assert!(!note.contains("first look"), "the sealed review must not reach the cover note");
}

#[test]
fn seal_sends_a_large_module_whole_and_byte_identical() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/big.rs", "--seed", "2"]).code, 0);
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let source = crate::ports::SourceTree::read(&fixture(), "demo/src/big.rs").unwrap();
    let blobs = w.gh.posted("git/blobs");
    assert!(!blobs.is_empty());
    for b in &blobs {
        assert!(!String::from_utf8_lossy(&unbase64(b["content"].as_str().unwrap())).contains("[cr: source lines"));
    }
    assert!(blobs.iter().any(|b| unbase64(b["content"].as_str().unwrap()) == source), "no posted blob equals the source");
    let lines = String::from_utf8_lossy(&source).lines().count() as u32;
    let note = w.gh.posted("pulls")[0]["body"].as_str().unwrap().to_string();
    let want = format!("- `demo/src/big.rs`: {lines} lines (about {} pages)\n- Total: {lines} lines", lines.div_ceil(10));
    assert!(note.contains(&want), "missing {want:?} in:\n{note}");
}

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
    assert_eq!(w.gh.count(), 0);
    assert!(!w.sealed_path(1).exists());
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
}

#[test]
fn seal_that_fails_at_the_pull_request_stays_staged_and_a_rerun_makes_only_the_pull_request() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    w.gh.fail("pulls");
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("pulls"), "{}", o.stderr);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
    let blobs = w.gh.posted("git/blobs").len();
    w.gh.heal();
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(w.gh.posted("git/blobs").len(), blobs);
    assert_eq!(w.gh.posted("git/refs").len(), 1);
    assert_eq!(w.gh.posted("git/trees").len(), 1);
    assert_eq!(w.gh.posted("pulls").len(), 2, "the failed attempt and the retry");
    let r = record::load(w.run.path(), 1).unwrap();
    assert_eq!((r.state, r.pr), (State::Open, Some(7)));
}

#[test]
fn seal_with_an_empty_owner_exits_two_naming_the_key() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    w.ports.repo_owner = String::new();
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("SPIRA_CR_REPO_OWNER"), "{}", o.stderr);
    assert_eq!(w.gh.count(), 0);
    w.ports.repo_owner = OWNER.into();
    w.ports.repo_name = String::new();
    assert!(w.seal_with("001", &c).stderr.contains("SPIRA_CR_REPO_NAME"));
}

#[test]
fn unseal_on_an_open_review_exits_two_and_posts_nothing() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    let before = w.gh.count();
    let o = w.cr(&["unseal", "--review", "001"]);
    assert_eq!(o.code, 2);
    assert_eq!(w.gh.count(), before);
}

#[test]
fn unseal_on_a_published_review_posts_the_sealed_comments_as_one_review_once() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    record::set_state(w.run.path(), 1, State::Published, NOW).unwrap();
    let o = w.cr(&["unseal", "--review", "001"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let posts = w.gh.posted("pulls/7/reviews");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["event"], "COMMENT");
    let sealed: serde_json::Value = serde_json::from_slice(&std::fs::read(w.sealed_path(1)).unwrap()).unwrap();
    let want: Vec<serde_json::Value> = sealed["lens"].as_array().unwrap().iter().chain(sealed["fresh"].as_array().unwrap()).map(|c| serde_json::json!({"path": c["path"], "line": c["line"], "side": "RIGHT", "body": format!("{}{}", if sealed["lens"].as_array().unwrap().contains(c) { "**His lens:** " } else { "**Fresh read:** " }, c["body"].as_str().unwrap())})).collect();
    assert_eq!(posts[0]["comments"].as_array().unwrap(), &want);
    assert_eq!(w.cr(&["unseal", "--review", "001"]).code, 2);
    assert_eq!(w.gh.posted("pulls/7/reviews").len(), 1);
}

#[test]
fn unseal_refuses_a_sealed_file_altered_after_sealing() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    record::set_state(w.run.path(), 1, State::Published, NOW).unwrap();
    let mut text = std::fs::read_to_string(w.sealed_path(1)).unwrap();
    text.push(' ');
    std::fs::write(w.sealed_path(1), text).unwrap();
    let o = w.cr(&["unseal", "--review", "001"]);
    assert_eq!(o.code, 2);
    assert!(w.gh.posted("reviews").is_empty());
}

#[test]
fn unseal_said_publishes_only_for_a_person_commenting_on_this_pull_request() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    let issue = format!("https://forge.invalid/repos/{OWNER}/{NAME}/issues/7");
    w.gh.answer("issues/comments/55", &format!(r#"{{"user":{{"type":"Bot"}},"issue_url":"{issue}"}}"#));
    assert_eq!(w.cr(&["unseal", "--review", "001", "--said", "55"]).code, 2);
    w.gh.answer("issues/comments/56", &format!(r#"{{"user":{{"type":"User"}},"issue_url":"{issue}-other/issues/8"}}"#));
    assert_eq!(w.cr(&["unseal", "--review", "001", "--said", "56"]).code, 2);
    assert!(w.gh.posted("reviews").is_empty());
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Open);
    w.gh.answer("issues/comments/57", &format!(r#"{{"user":{{"type":"User"}},"issue_url":"{issue}"}}"#));
    let o = w.cr(&["unseal", "--review", "001", "--said", "57"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let r = record::load(w.run.path(), 1).unwrap();
    assert!(r.state == State::Published && r.published_at.is_some());
    assert_eq!(w.gh.posted("reviews").len(), 1);
}

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
    assert!(lines[1].starts_with("001 on-demand open 0h https://forge.invalid/"), "{}", lines[1]);
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

#[test]
fn a_review_closed_unpublished_adds_no_coverage() {
    let files = store_unit_files();
    let rows = published("001", "module:demo/src/store.rs", &at_today(&fixture(), &files), ClosedAs::Unpublished);
    let (_run, p) = coverage_ports(fixture(), rows, 10);
    let c = &status_json(&p)["coverage"];
    assert_eq!(c["files"]["reviewed"], 0);
    assert_eq!(c["kinds"].as_array().unwrap().iter().find(|k| k["kind"] == "module").unwrap()["reviewed"], 0);
}

#[test]
fn a_set_state_stamps_the_matching_time_and_keeps_the_rest() {
    let w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let r = record::set_state(w.run.path(), 1, State::Closed, 99).unwrap();
    assert_eq!((r.state, r.closed_at), (State::Closed, Some(99)));
    assert_eq!(record::load(w.run.path(), 1).unwrap(), r);
}

#[test]
fn seal_rerun_with_different_comments_is_refused_not_silently_dropped() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    w.gh.fail("pulls");
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 1);
    w.gh.heal();
    let other = c.replace("first look", "second thoughts");
    let o = w.seal_with("001", &other);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("different comments"), "{}", o.stderr);
}

#[test]
fn seal_rerun_with_a_different_fresh_read_is_refused() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    w.gh.fail("pulls");
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 1);
    w.gh.heal();
    let o = w.seal_with("001", &c.replace("fresh look", "another fresh look"));
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("different comments"), "{}", o.stderr);
}

#[test]
fn seal_rerun_with_a_different_cover_is_refused() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    w.gh.fail("pulls");
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 1);
    w.gh.heal();
    let o = w.seal_with("001", &c.replace(DOES, "Something else entirely."));
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("different cover"), "{}", o.stderr);
}

#[test]
fn seal_rerun_after_a_lost_progress_file_adopts_the_ref_already_on_the_forge() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    w.gh.fail("pulls");
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 1);
    std::fs::remove_file(record::dir(w.run.path(), 1).join("progress.json")).unwrap();
    w.gh.heal();
    w.gh.fail("git/refs");
    let branch = record::load(w.run.path(), 1).unwrap().branch;
    w.gh.answer(&format!("git/ref/heads/{branch}"), r#"{"object":{"sha":"new-commit"}}"#);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(w.gh.posted("pulls").len(), 2);
}

#[test]
fn unseal_with_a_failing_review_post_leaves_it_retryable() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    record::set_state(w.run.path(), 1, State::Published, NOW).unwrap();
    w.gh.fail("pulls/7/reviews");
    assert_eq!(w.cr(&["unseal", "--review", "001"]).code, 1);
    w.gh.heal();
    assert_eq!(w.cr(&["unseal", "--review", "001"]).code, 0);
}

#[test]
fn the_section_guard_names_a_missing_heading_and_a_blank_section() {
    use crate::review::missing_section;
    let full: String = HEADINGS.iter().map(|h| format!("{h}\n\ntext\n\n")).collect();
    assert_eq!(missing_section(&full), None);
    assert_eq!(missing_section(&full.replace("## Files", "## Fils")), Some("## Files"));
    assert_eq!(missing_section(&full.replace("## Who calls it\n\ntext", "## Who calls it\n\n")), Some("## Who calls it"));
}

#[test]
fn seal_refuses_a_prose_field_carrying_a_heading_line_and_a_bare_array_on_a_rerun() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let c = w.comments(1);
    let o = w.seal_with("001", &c.replace(DOES, "x\\n## Files\\ny"));
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("does"), "{}", o.stderr);
    w.gh.fail("pulls");
    assert_eq!(w.seal_with("001", &c).code, 1);
    w.gh.heal();
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
    w.gh.posted("pulls")[0]["body"].as_str().unwrap().to_string()
}

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

fn put_rows(body: &serde_json::Value) -> Vec<Row> {
    crate::ledger::Ledger::parse(&String::from_utf8(unbase64(body["content"].as_str().unwrap())).unwrap()).unwrap().rows
}

#[test]
fn seal_appends_one_open_row_to_the_review_repositorys_ledger() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    w.gh.fail_ledger_puts();
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert!(o.stderr.contains("ledger write refused"), "{}", o.stderr);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged, "a failed append leaves the review staged");
    assert_eq!(w.gh.put("contents/ledger.jsonl").len(), 1, "an unmoved sha is not retried");
    w.gh.heal_puts();
    w.ports.clock = Box::new(crate::ports::VirtualClock::new(NOW + 5_000));
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let puts = w.gh.put("contents/ledger.jsonl");
    assert_eq!(puts.len(), 2, "one failed write, then the rerun's");
    let rows = put_rows(&puts[1]);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], prior_row());
    let r = &rows[1];
    let store = fixture();
    assert_eq!((r.review.as_str(), r.unit_id.as_str(), r.opened_at, r.closed_at, r.closed_as), ("001", "module:demo/src/store.rs", NOW + 5_000, None, None));
    assert_eq!(r.commit, store.commit_id);
    let want: Vec<(String, String)> = record::load(w.run.path(), 1).unwrap().unit.files.iter().map(|f| (f.path.clone(), crate::ports::SourceTree::blob(&store, &f.path).unwrap())).collect();
    assert_eq!(r.files.iter().map(|f| (f.path.clone(), f.blob.clone())).collect::<Vec<_>>(), want);
    assert_eq!(puts[1]["branch"], "main");
    assert_eq!(put_rows(&puts[1]), ledger_rows(&w.gh));
    let again = w.cr(&["seal", "--review", "001"]);
    assert_eq!(again.code, 2, "{}", again.stderr);
    assert_eq!(w.gh.put("contents/ledger.jsonl").len(), 2);
}

#[test]
fn a_staged_review_holds_its_unit_back_from_the_next_open() {
    let w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    let o = w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]);
    assert_eq!(o.code, 1, "{}", o.stderr);
}

#[test]
fn ledger_close_writes_once_with_how_it_closed() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    record::set_state(w.run.path(), 1, State::Published, NOW + 1).unwrap();
    let puts = w.gh.put("contents/ledger.jsonl").len();
    let o = w.cr(&["ledger-close", "--review", "001", "--at", "777"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let rows = ledger_rows(&w.gh);
    let last = rows.last().unwrap();
    assert_eq!((last.review.as_str(), last.closed_at, last.closed_as), ("001", Some(777), Some(ClosedAs::Published)));
    assert_eq!(rows.iter().filter(|r| r.review == "001").count(), 2);
    assert_eq!(w.gh.put("contents/ledger.jsonl").len(), puts + 1);
    let o = w.cr(&["ledger-close", "--review", "001", "--at", "999"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(w.gh.put("contents/ledger.jsonl").len(), puts + 1, "a second close makes no PUT");
}

#[test]
fn ledger_close_of_an_unpublished_review_says_unpublished() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    assert_eq!(w.cr(&["ledger-close", "--review", "001", "--at", "5"]).code, 0);
    assert_eq!(ledger_rows(&w.gh).last().unwrap().closed_as, Some(ClosedAs::Unpublished));
}

#[test]
fn a_missing_ledger_file_is_refused_naming_the_step() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    w.gh = Rc::new(FakeGh::default());
    w.ports.gh = Box::new(Shared(w.gh.clone()));
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("ledger.jsonl") && o.stderr.contains("create it empty"), "{}", o.stderr);
    assert!(w.gh.put("contents/ledger.jsonl").is_empty());
}

#[test]
fn a_ledger_put_conflict_rereads_and_retries() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    let other = Row { review: "901".into(), ..prior_row() };
    w.gh.conflict_next_ledger_put(&serde_json::to_string(&other).unwrap());
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let puts = w.gh.put("contents/ledger.jsonl");
    assert_eq!(puts.len(), 2);
    assert_ne!(puts[0]["sha"], puts[1]["sha"]);
    let rows = put_rows(&puts[1]);
    assert_eq!(rows.iter().map(|r| r.review.as_str()).collect::<Vec<_>>(), ["900", "901", "001"]);
}

#[test]
fn a_ledger_that_moves_under_every_write_gives_up_after_three_puts() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    w.gh.conflict_every_ledger_put();
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("moved under 3 writes"), "{}", o.stderr);
    assert_eq!(w.gh.put("contents/ledger.jsonl").len(), 3);
}

#[test]
fn closing_a_review_that_was_never_sealed_writes_nothing_and_succeeds() {
    let w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
    let o = w.cr(&["ledger-close", "--review", "001", "--at", "5"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(w.gh.put("contents/ledger.jsonl").is_empty());
}

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
    assert_eq!(w.gh.count(), 0);
    assert!(!w.sealed_path(1).exists());
}

#[test]
fn unseal_labels_each_comment_with_its_half() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    record::set_state(w.run.path(), 1, State::Published, NOW).unwrap();
    assert_eq!(w.cr(&["unseal", "--review", "001"]).code, 0);
    let posts = w.gh.posted("pulls/7/reviews");
    let bodies: Vec<&str> = posts[0]["comments"].as_array().unwrap().iter().map(|c| c["body"].as_str().unwrap()).collect();
    assert_eq!(bodies, ["**His lens:** first look", "**Fresh read:** fresh look"]);
}

#[test]
fn status_shows_each_published_reviews_two_scores_in_order() {
    let mut w = world();
    for (n, seed) in [("001", "3"), ("002", "4"), ("003", "5")] {
        assert_eq!(w.cr(&["open", "--seed", seed]).code, 0);
        let c = w.comments(n.parse().unwrap());
        assert_eq!(w.seal_with(n, &c).code, 0);
    }
    let path = record::load(w.run.path(), 1).unwrap().unit.files[0].path.clone();
    let dir = record::dir(w.run.path(), 1);
    std::fs::write(dir.join("comments.json"), format!(r#"[{{"path":"{path}","line":1,"body":"h","user":{{"type":"User"}}}},{{"path":"{path}","line":40,"body":"h2","user":{{"type":"User"}}}}]"#)).unwrap();
    let finding = format!(r#"{{"id":"001-1","words":"w","exemplar":{{"path":"{path}","line":1}},"kind":"question","rule":"r","scope":"unit","endorses":1}}"#);
    std::fs::write(dir.join("findings.jsonl"), format!("{finding}\n")).unwrap();
    record::set_state(w.run.path(), 3, State::Published, NOW).unwrap();
    record::set_state(w.run.path(), 1, State::Closed, NOW).unwrap();
    let j: serde_json::Value = serde_json::from_str(&w.cr(&["status", "--json"]).stdout).unwrap();
    let rows: Vec<(&str, u64, u64, u64, u64)> = j["scores"].as_array().unwrap().iter().map(|r| (r["review"].as_str().unwrap(), r["lens_predicted"].as_u64().unwrap(), r["his_comments"].as_u64().unwrap(), r["fresh_endorsed"].as_u64().unwrap(), r["fresh_points"].as_u64().unwrap())).collect();
    assert_eq!(rows, [("001", 1, 2, 1, 1), ("003", 0, 0, 0, 1)], "002 is still open and has no score row");
    let text = w.cr(&["status"]).stdout;
    let at = text.find("scores\n").expect("a scores section");
    let lines: Vec<&str> = text[at..].lines().skip(1).take(2).collect();
    assert_eq!(lines, ["  001 lens 1/2 fresh 1/1", "  003 lens 0/0 fresh 0/1"]);
}

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

fn close_with(w: &mut World, comments: &str, findings: &str, publish: bool) -> Row {
    assert_eq!(w.open_and_seal().code, 0);
    if publish {
        record::set_state(w.run.path(), 1, State::Published, NOW + 1).unwrap();
    }
    let dir = record::dir(w.run.path(), 1);
    std::fs::write(dir.join("comments.json"), comments).unwrap();
    std::fs::write(dir.join("findings.jsonl"), findings).unwrap();
    assert_eq!(
        w.cr(&["ledger-close", "--review", "001", "--at", "777"])
            .code,
        0
    );
    ledger_rows(&w.gh).pop().unwrap()
}

fn comment(id: u64, bot: bool, path: Option<&str>) -> String {
    let path = path.map_or("null".to_string(), |p| format!("\"{p}\""));
    format!(
        r#"{{"id":{id},"in_reply_to":null,"author":"x","bot":{bot},"path":{path},"line":1,"body":"b","created_at":1}}"#
    )
}

#[test]
fn ledger_close_records_his_comments_by_path_and_findings_by_kind() {
    let mut w = world();
    let comments = format!(
        "[{},{},{},{}]",
        comment(1, false, Some("demo/src/store.rs")),
        comment(2, false, Some("demo/src/store.rs")),
        comment(3, true, Some("demo/src/store.rs")),
        comment(4, false, None)
    );
    let finding = r#"{"words":"w","exemplar":{"path":"demo/src/store.rs","line":3},"kind":"refactor","rule":"r","scope":"unit"}"#;
    let last = close_with(&mut w, &comments, &format!("{finding}\n"), true);
    let a = last.attention.expect("a published close records attention");
    assert_eq!(
        serde_json::to_value(&a.comments_by_path).unwrap(),
        serde_json::json!({"demo/src/store.rs": 2})
    );
    assert_eq!(
        serde_json::to_value(&a.finding_kinds).unwrap(),
        serde_json::json!({"refactor": 1})
    );
}

#[test]
fn a_review_closed_unpublished_records_no_attention() {
    let mut w = world();
    let comments = format!("[{}]", comment(1, false, Some("demo/src/store.rs")));
    let last = close_with(&mut w, &comments, "", false);
    assert_eq!(
        (last.closed_as, last.attention),
        (Some(ClosedAs::Unpublished), None)
    );
}

#[test]
fn comments_recorded_at_close_weigh_the_next_pick_in_their_area() {
    let mut w = world();
    let comments = format!("[{}]", comment(1, false, Some("demo/src/store.rs")));
    close_with(&mut w, &comments, "", true);
    let o = w.cr(&["pick", "--aim", "demo/src/net.rs", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
    assert!(v["weight"]["attention_share"].as_f64().unwrap() > 0.0, "{v}");
}

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

#[test]
fn a_unit_no_incident_touched_says_none_found() {
    let mut w = world();
    w.ports.tree = Box::new(fixture().with_log("demo/src/store.rs", vec![commit("a2", "sp-feat02: add a handle", false)]));
    let body = sealed_body(&mut w);
    let section = body.split("## Incidents that touched it").nth(1).unwrap().split("\n## ").next().unwrap();
    assert!(section.contains("None found."), "{body}");
}

#[test]
fn an_unreadable_incident_lookup_says_unknown_not_none() {
    let mut w = world();
    w.ports.tree = Box::new(fixture().with_log("demo/src/store.rs", vec![commit("a2", "sp-feat02: add a handle", false)]));
    w.ports.beads = Box::new(FixtureBeads { types: Ok(Default::default()), incidents: Err("store down".into()) });
    let body = sealed_body(&mut w);
    assert!(body.contains("Incidents: unknown (store down)") && !body.contains("None found.\n\n## Tests"), "{body}");
}

#[test]
fn a_unit_whose_commits_name_no_bead_says_none_found_without_asking_the_store() {
    let mut w = world();
    w.ports.tree = Box::new(fixture().with_log("demo/src/store.rs", vec![commit("a1", "tidy", false)]));
    w.ports.beads = Box::new(FixtureBeads { types: Ok(Default::default()), incidents: Err("store down".into()) });
    let body = sealed_body(&mut w);
    let section = body.split("## Incidents that touched it").nth(1).unwrap().split("\n## ").next().unwrap();
    assert!(section.contains("None found.") && !body.contains("Incidents: unknown"), "{body}");
}

// ───────────────────────────── adopt ─────────────────────────────

const PR9: &str = r#"{"state":"open","html_url":"https://forge.invalid/fixture-owner/fixture-reviews/pull/9","head":{"ref":"hand/sink"}}"#;
const PR9_URL: &str = "https://forge.invalid/fixture-owner/fixture-reviews/pull/9";
const SINK: &str = "demo/src/sink.rs";
const FILE_SINK: &str = "demo/src/file_sink.rs";
const SINK_UNIT: &str = "trait:demo/src/sink.rs::Sink";

fn blob(path: &str) -> String {
    crate::ports::blob_of(&crate::ports::SourceTree::read(&fixture(), path).unwrap())
}

fn pr_files(entries: &[(&str, &str)]) -> String {
    format!("[{}]", entries.iter().map(|(f, s)| format!(r#"{{"filename":"{f}","sha":"{s}"}}"#)).collect::<Vec<_>>().join(","))
}

fn comments_on(path: &str) -> String {
    format!(r#"[{{"path":"{path}","line":1,"body":"hand-made look"}}]"#)
}

impl World {
    fn hand_pr(&self, state_json: &str, files: &[(&str, &str)]) {
        self.gh.answer("pulls/9", state_json);
        self.gh.answer("pulls/9/files?per_page=100&page=1", &pr_files(files));
    }
    fn adopt(&mut self, stdin: &str, sha: &str, args: &[&str]) -> Outcome {
        self.ports.stdin = stdin.to_string();
        let mut a = vec!["adopt"];
        a.extend_from_slice(args);
        a.extend_from_slice(&["--pr", "9", "--sha256", sha]);
        self.cr(&a)
    }
    fn good_pr(&self) {
        self.hand_pr(PR9, &[(FILE_SINK, &blob(FILE_SINK)), (SINK, &blob(SINK))]);
    }
    fn review_json(&self, n: u32) -> String {
        std::fs::read_to_string(record::dir(self.run.path(), n).join("review.json")).unwrap()
    }
}

fn staged_sink_world() -> (World, String, String) {
    let w = world();
    assert_eq!(w.cr(&["open", "--aim", "Sink", "--seed", "3"]).code, 0);
    w.good_pr();
    let stdin = comments_on(SINK);
    let sha = sha256(stdin.as_bytes());
    (w, stdin, sha)
}

#[test]
fn adopt_records_a_staged_review_as_open_on_the_pull_request_with_its_forge_files() {
    let (mut w, stdin, sha) = staged_sink_world();
    let o = w.adopt(&stdin, &sha, &["--review", "001"]);
    assert_eq!((o.code, o.stdout.as_str()), (0, format!("{PR9_URL}\n").as_str()), "{}", o.stderr);
    let r = record::load(w.run.path(), 1).unwrap();
    assert_eq!((r.state, r.pr, r.branch.as_str(), r.pr_url.as_deref()), (State::Open, Some(9), "hand/sink", Some(PR9_URL)));
    assert_eq!(r.sealed_sha256.as_deref(), Some(sha.as_str()));
    assert_eq!(r.unit.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), [SINK, FILE_SINK]);
    assert_eq!(std::fs::read(w.sealed_path(1)).unwrap(), stdin.as_bytes());
    let status = w.cr(&["status"]).stdout;
    assert!(status.lines().any(|l| l.starts_with("001 on-demand open ") && l.contains(PR9_URL)), "{status}");
    let rows: Vec<Row> = ledger_rows(&w.gh).into_iter().filter(|r| r.review == "001").collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].files, vec![LedgerFile { path: SINK.into(), blob: blob(SINK) }, LedgerFile { path: FILE_SINK.into(), blob: blob(FILE_SINK) }]);
    assert!(w.gh.posted("").is_empty());
}

#[test]
fn an_adopted_review_unseals_and_takes_findings_only_on_the_pull_requests_files() {
    let (mut w, stdin, sha) = staged_sink_world();
    assert_eq!(w.adopt(&stdin, &sha, &["--review", "001"]).code, 0);
    let issue = format!("https://forge.invalid/repos/{OWNER}/{NAME}/issues/9");
    w.gh.answer("issues/comments/60", &format!(r#"{{"user":{{"type":"User"}},"issue_url":"{issue}"}}"#));
    let o = w.cr(&["unseal", "--review", "001", "--said", "60"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let posted = w.gh.posted("pulls/9/reviews");
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0]["comments"][0]["path"], SINK);
    let finding = |path: &str| format!(r#"{{"words":"w","exemplar":{{"path":"{path}","line":1}},"kind":"question","rule":"r","scope":"unit"}}"#);
    w.ports.stdin = finding(FILE_SINK);
    w.ports.findings_dir = TempDir::new("cr-adopt-find").path().to_str().unwrap().to_string();
    assert_eq!(w.cr(&["finding", "--review", "001"]).code, 0);
    w.ports.stdin = finding("demo/src/router.rs");
    assert_eq!(w.cr(&["finding", "--review", "001"]).code, 2);
}

#[test]
fn adopt_with_a_unit_and_commit_allocates_an_on_demand_review() {
    let mut w = world();
    w.good_pr();
    let (stdin, sha) = (comments_on(SINK), sha256(comments_on(SINK).as_bytes()));
    let o = w.adopt(&stdin, &sha, &["--unit", "trait:nope", "--commit", "c0ffee"]);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(record::all(w.run.path()).unwrap().is_empty());
    let o = w.adopt(&stdin, &sha, &["--unit", SINK_UNIT, "--commit", "c0ffee"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let r = record::load(w.run.path(), 1).unwrap();
    assert_eq!((r.kind, r.state, r.pr, r.source_sha.as_str(), r.unit.id.as_str()), (record::Kind::OnDemand, State::Open, Some(9), "c0ffee", SINK_UNIT));
    let again = w.adopt(&stdin, &sha, &["--unit", SINK_UNIT, "--commit", "c0ffee"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert_eq!(record::all(w.run.path()).unwrap().len(), 1);
}

#[test]
fn adopt_refusals_exit_two_and_write_nothing() {
    let (mut w, stdin, sha) = staged_sink_world();
    let before_record = w.review_json(1);
    let before_ledger = w.gh.ledger_text();
    let check = |w: &mut World, label: &str, stdin: &str, sha: &str, args: &[&str]| {
        let o = w.adopt(stdin, sha, args);
        assert_eq!(o.code, 2, "{label}: {}", o.stderr);
        assert_eq!(w.review_json(1), before_record, "{label}");
        assert_eq!(w.gh.ledger_text(), before_ledger, "{label}");
        assert!(!w.sealed_path(1).exists(), "{label}");
    };
    check(&mut w, "hash", &stdin, &sha256(b"other"), &["--review", "001"]);
    w.hand_pr(PR9, &[(SINK, "0000000000000000"), (FILE_SINK, &blob(FILE_SINK))]);
    check(&mut w, "blob", &stdin, &sha, &["--review", "001"]);
    w.hand_pr(&PR9.replace("open", "closed"), &[(SINK, &blob(SINK))]);
    check(&mut w, "closed", &stdin, &sha, &["--review", "001"]);
    w.hand_pr(PR9, &[(FILE_SINK, &blob(FILE_SINK))]);
    check(&mut w, "primary", &stdin, &sha, &["--review", "001"]);
    w.good_pr();
    let outside = comments_on("demo/src/router.rs");
    check(&mut w, "comment", &outside, &sha256(outside.as_bytes()), &["--review", "001"]);
    let mut r = record::load(w.run.path(), 1).unwrap();
    r.state = State::Closed;
    record::save(w.run.path(), &r).unwrap();
    let closed_record = w.review_json(1);
    let o = w.adopt(&stdin, &sha, &["--review", "001"]);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert_eq!((w.review_json(1), w.gh.ledger_text()), (closed_record, before_ledger));
    r.state = State::Staged;
    record::save(w.run.path(), &r).unwrap();
    assert_eq!(w.adopt(&stdin, &sha, &["--review", "001"]).code, 0);
}

#[test]
fn adopt_again_is_a_no_op_and_another_pull_request_is_refused() {
    let (mut w, stdin, sha) = staged_sink_world();
    assert_eq!(w.adopt(&stdin, &sha, &["--review", "001"]).code, 0);
    let (record_after, ledger_after) = (w.review_json(1), w.gh.ledger_text());
    let o = w.adopt(&stdin, &sha, &["--review", "001"]);
    assert_eq!((o.code, o.stdout.as_str()), (0, format!("{PR9_URL}\n").as_str()), "{}", o.stderr);
    assert_eq!((w.review_json(1), w.gh.ledger_text()), (record_after.clone(), ledger_after.clone()));
    w.ports.stdin = stdin.clone();
    let o = w.cr(&["adopt", "--review", "001", "--pr", "8", "--sha256", &sha]);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert_eq!((w.review_json(1), w.gh.ledger_text()), (record_after, ledger_after));
}

#[test]
fn adopt_rejects_review_with_unit_and_an_incomplete_form_before_anything_runs() {
    let (mut w, stdin, sha) = staged_sink_world();
    for args in [vec!["--review", "001", "--unit", SINK_UNIT], vec!["--review", "001", "--commit", "c0ffee"], vec!["--unit", SINK_UNIT], vec!["--commit", "c0ffee"], vec![]] {
        let o = w.adopt(&stdin, &sha, &args);
        assert_eq!(o.code, 2, "{args:?}: {}", o.stderr);
    }
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
    assert_eq!(w.adopt(&stdin, &sha, &["--review", "001"]).code, 0);
}
