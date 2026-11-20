//! `cr open | seal | unseal | status` through `cli::run`: an in-memory tree, a scripted
//! review repository and a temp run directory.

use super::gh::{unbase64, FakeGh, Shared, NAME, OWNER};
use super::{cfg, fixture, go, ports_with, NOW};
use crate::cli::Outcome;
use crate::ports::Ports;
use crate::record::{self, State};
use sha2::{Digest, Sha256};
use std::rc::Rc;
use testkit::TempDir;

struct World {
    run: TempDir,
    gh: Rc<FakeGh>,
    ports: Ports,
}

fn world() -> World {
    let run = TempDir::new("cr-review");
    let gh = Rc::new(FakeGh::default());
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
        format!(r#"[{{"path":"{path}","line":1,"body":"first look"}}]"#)
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
fn open_count_two_stages_two_distinct_reviews_and_two_ledger_rows_without_a_forge_call() {
    let w = world();
    let o = w.cr(&["open", "--count", "2", "--seed", "5"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let all = record::all(w.run.path()).unwrap();
    assert_eq!(all.iter().map(|r| r.number).collect::<Vec<_>>(), [1, 2]);
    assert!(all.iter().all(|r| r.state == State::Staged && r.kind == record::Kind::OnDemand));
    assert_ne!(all[0].unit.id, all[1].unit.id);
    let ledger = std::fs::read_to_string(w.run.path().join("cr/ledger.jsonl")).unwrap();
    assert_eq!(ledger.lines().count(), 2);
    assert_eq!(w.gh.count(), 0);
    for n in ["001", "002"] {
        assert!(o.stdout.contains(&format!("seal with: cr seal --review {n} < comments.json")), "{}", o.stdout);
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
fn seal_stores_the_unit_bytes_and_review_md_in_blobs_tree_and_pull_request() {
    let mut w = world();
    let o = w.open_and_seal();
    assert_eq!(o.code, 0, "{}", o.stderr);
    let r = record::load(w.run.path(), 1).unwrap();
    let tree = fixture();
    let blobs = w.gh.posted("/git/blobs");
    let mut got: Vec<Vec<u8>> = blobs.iter().map(|b| unbase64(b["content"].as_str().unwrap())).collect();
    let mut want: Vec<Vec<u8>> = r.unit.files.iter().map(|f| crate::ports::SourceTree::read(&tree, &f.path).unwrap()).collect();
    assert_eq!(got.len(), want.len() + 1);
    let note = got.pop().unwrap();
    assert!(String::from_utf8(note).unwrap().starts_with("# Review 001"));
    got.sort();
    want.sort();
    assert_eq!(got, want);
    assert!(blobs.iter().all(|b| b["encoding"] == "base64"));

    let trees = w.gh.posted("/git/trees");
    assert_eq!(trees.len(), 1);
    let mut paths: Vec<String> = trees[0]["tree"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap().to_string()).collect();
    paths.sort();
    let mut expect: Vec<String> = r.unit.files.iter().map(|f| f.path.clone()).collect();
    expect.push("REVIEW.md".into());
    expect.sort();
    assert_eq!(paths, expect);
    assert_eq!(trees[0]["base_tree"], "base-tree");
    assert_eq!(w.gh.posted("/git/commits")[0]["parents"][0], "base-head");
    assert_eq!(w.gh.posted("/git/refs")[0]["ref"], format!("refs/heads/{}", r.branch));

    let pr = &w.gh.posted("/pulls")[0];
    let body = pr["body"].as_str().unwrap();
    let sealed = std::fs::read(w.sealed_path(1)).unwrap();
    assert!(body.contains(&format!("sealed-sha256: {}", sha256(&sealed))), "{body}");
    assert!(body.contains("c0ffee") && body.contains("src"), "{body}");
    assert!(body.contains("Add single comment"), "{body}");
    assert_eq!(pr["title"], r.unit.title);
    assert_eq!((r.state, r.pr), (State::Open, Some(7)));
    assert!(o.stdout.contains("/pull/7"), "{}", o.stdout);
}

#[test]
fn seal_cover_note_carries_facts_and_no_critique() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "1"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    let note_b64 = w.gh.posted("/git/blobs").last().unwrap()["content"].as_str().unwrap().to_string();
    let note = String::from_utf8(unbase64(&note_b64)).unwrap();
    for want in ["> The store.", "alpha, beta", "demo/src/store/tests.rs", "demo/tests/it.rs", "Commits in the last 60 days: 2", "c0ffee"] {
        assert!(note.contains(want), "missing {want:?} in:\n{note}");
    }
    assert!(!note.contains("first look"), "the sealed review must not reach the cover note");
}

#[test]
fn seal_writes_an_oversized_file_as_regions_joined_by_a_marker_naming_the_elided_range() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--aim", "demo/src/big.rs", "--seed", "2"]).code, 0);
    let r = record::load(w.run.path(), 1).unwrap();
    let regions = r.unit.files[0].regions.clone().expect("big.rs is oversized");
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let blobs = w.gh.posted("/git/blobs");
    let text = String::from_utf8(unbase64(blobs[0]["content"].as_str().unwrap())).unwrap();
    assert!(text.contains("// [cr: source lines "), "{text}");
    assert!(text.contains(" elided]"), "{text}");
    let source = crate::ports::SourceTree::read(&fixture(), "demo/src/big.rs").unwrap();
    assert!(text.len() < source.len());
    let first = source.split(|b| *b == b'\n').nth(regions[0].0 as usize - 1).unwrap();
    assert!(text.contains(&String::from_utf8_lossy(first).into_owned()));
    let note = String::from_utf8(unbase64(blobs.last().unwrap()["content"].as_str().unwrap())).unwrap();
    assert!(note.contains("elided source lines"), "{note}");
}

#[test]
fn seal_refuses_an_empty_array_a_foreign_path_and_a_line_past_the_end_before_any_forge_call() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let path = record::load(w.run.path(), 1).unwrap().unit.files[0].path.clone();
    for bad in [
        "[]".to_string(),
        r#"[{"path":"nowhere/else.rs","line":1,"body":"x"}]"#.to_string(),
        format!(r#"[{{"path":"{path}","line":9999,"body":"x"}}]"#),
        format!(r#"[{{"path":"{path}","line":0,"body":"x"}}]"#),
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
    w.gh.fail("/pulls");
    let c = w.comments(1);
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("pulls"), "{}", o.stderr);
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Staged);
    let blobs = w.gh.posted("/git/blobs").len();
    w.gh.heal();
    let before = w.gh.count();
    let o = w.seal_with("001", &c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(w.gh.posted("/git/blobs").len(), blobs);
    assert_eq!(w.gh.posted("/git/refs").len(), 1);
    assert_eq!(w.gh.posted("/git/trees").len(), 1);
    assert_eq!(w.gh.count() - before, 1);
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
    let posts = w.gh.posted("/pulls/7/reviews");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["event"], "COMMENT");
    let sealed: serde_json::Value = serde_json::from_slice(&std::fs::read(w.sealed_path(1)).unwrap()).unwrap();
    let want: Vec<serde_json::Value> = sealed.as_array().unwrap().iter().map(|c| serde_json::json!({"path": c["path"], "line": c["line"], "side": "RIGHT", "body": c["body"]})).collect();
    assert_eq!(posts[0]["comments"].as_array().unwrap(), &want);
    assert_eq!(w.cr(&["unseal", "--review", "001"]).code, 2);
    assert_eq!(w.gh.posted("/pulls/7/reviews").len(), 1);
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
    assert!(w.gh.posted("/reviews").is_empty());
}

#[test]
fn unseal_said_publishes_only_for_a_person_commenting_on_this_pull_request() {
    let mut w = world();
    assert_eq!(w.open_and_seal().code, 0);
    let issue = format!("https://forge.invalid/repos/{OWNER}/{NAME}/issues/7");
    w.gh.answer("/issues/comments/55", &format!(r#"{{"user":{{"type":"Bot"}},"issue_url":"{issue}"}}"#));
    assert_eq!(w.cr(&["unseal", "--review", "001", "--said", "55"]).code, 2);
    w.gh.answer("/issues/comments/56", &format!(r#"{{"user":{{"type":"User"}},"issue_url":"{issue}-other/issues/8"}}"#));
    assert_eq!(w.cr(&["unseal", "--review", "001", "--said", "56"]).code, 2);
    assert!(w.gh.posted("/reviews").is_empty());
    assert_eq!(record::load(w.run.path(), 1).unwrap().state, State::Open);
    w.gh.answer("/issues/comments/57", &format!(r#"{{"user":{{"type":"User"}},"issue_url":"{issue}"}}"#));
    let o = w.cr(&["unseal", "--review", "001", "--said", "57"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let r = record::load(w.run.path(), 1).unwrap();
    assert!(r.state == State::Published && r.published_at.is_some());
    assert_eq!(w.gh.posted("/reviews").len(), 1);
}

#[test]
fn status_lists_every_review_with_state_age_url_and_title() {
    let mut w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let c = w.comments(1);
    assert_eq!(w.seal_with("001", &c).code, 0);
    assert_eq!(w.cr(&["open", "--seed", "4"]).code, 0);
    let o = w.cr(&["status"]);
    let lines: Vec<&str> = o.stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{}", o.stdout);
    assert!(lines[0].starts_with("001 on-demand open 0h https://forge.invalid/"), "{}", lines[0]);
    assert!(lines[1].starts_with("002 on-demand staged 0h - "), "{}", lines[1]);
    let j: serde_json::Value = serde_json::from_str(&w.cr(&["status", "--json"]).stdout).unwrap();
    assert_eq!(j.as_array().unwrap().len(), 2);
}

#[test]
fn a_set_state_stamps_the_matching_time_and_keeps_the_rest() {
    let w = world();
    assert_eq!(w.cr(&["open", "--seed", "3"]).code, 0);
    let r = record::set_state(w.run.path(), 1, State::Closed, 99).unwrap();
    assert_eq!((r.state, r.closed_at), (State::Closed, Some(99)));
    assert_eq!(record::load(w.run.path(), 1).unwrap(), r);
}
