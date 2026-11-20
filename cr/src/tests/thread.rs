//! `cr thread` and `cr reply` through `cli::run`: a kept `comments.json`, an in-memory tree and a scripted forge.

use super::gh::{FakeGh, Shared, NAME, OWNER};
use super::{cfg, fixture, go, ports_with};
use crate::cli::Outcome;
use crate::ports::Ports;
use crate::record;
use crate::thread::{comments_path, source_line, Comment};
use std::rc::Rc;
use testkit::TempDir;

struct World {
    run: TempDir,
    gh: Rc<FakeGh>,
    ports: Ports,
}

fn world() -> World {
    let run = TempDir::new("cr-thread");
    let gh = Rc::new(FakeGh::default());
    let mut ports = ports_with(fixture(), vec![], cfg());
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
    fn opened(&self) {
        assert_eq!(self.cr(&["open", "--aim", "demo/src/store.rs", "--seed", "3"]).code, 0);
        let mut r = record::load(self.run.path(), 1).unwrap();
        r.pr = Some(7);
        record::save(self.run.path(), &r).unwrap();
    }
    fn keep(&self, comments: &[Comment]) {
        std::fs::write(comments_path(self.run.path(), 1), serde_json::to_string(comments).unwrap()).unwrap();
    }
}

fn comment(id: u64, in_reply_to: Option<u64>, author: &str, line: Option<u64>, body: &str, at: u64) -> Comment {
    Comment { id, in_reply_to, author: author.into(), bot: false, path: Some("demo/src/store.rs".into()), line, body: body.into(), created_at: at }
}

#[test]
fn thread_prints_the_code_around_the_line_and_the_thread_in_order() {
    let w = world();
    w.opened();
    w.keep(&[
        comment(30, Some(10), "ryan", Some(6), "and the error path?", 300),
        comment(10, None, "ryan", Some(6), "why open here?", 100),
        comment(99, None, "ryan", Some(3), "unrelated", 150),
        comment(20, Some(10), "concierge", Some(6), "it is the only opener", 200),
    ]);
    let o = w.cr(&["thread", "--review", "001", "--comment", "20"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let out = o.stdout;
    assert!(out.starts_with("demo/src/store.rs:6\n"), "{out}");
    assert!(out.contains(">    6      pub fn open() -> Store {\n"), "{out}");
    assert!(out.contains("     1  //! The store.\n"), "the window reaches line 1: {out}");
    let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("{needle:?} missing in {out}"));
    assert!(at("[10]") < at("[20]") && at("[20]") < at("[30]"), "{out}");
    assert!(!out.contains("unrelated"), "{out}");
}

#[test]
fn thread_of_a_conversation_comment_has_no_code() {
    let w = world();
    w.opened();
    w.keep(&[Comment { path: None, line: None, ..comment(5, None, "ryan", None, "publish it", 10) }]);
    let o = w.cr(&["thread", "--review", "001", "--comment", "5"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.starts_with("conversation\n") && o.stdout.contains("publish it"), "{}", o.stdout);
}

#[test]
fn thread_of_a_comment_the_watcher_never_kept_exits_two() {
    let w = world();
    w.opened();
    w.keep(&[]);
    assert_eq!(w.cr(&["thread", "--review", "001", "--comment", "1"]).code, 2);
}

#[test]
fn an_oversized_files_branch_line_maps_back_to_its_source_line() {
    let regions = [(3, 4), (10, 11)];
    let map = |l| source_line(Some(&regions), 20, l);
    assert_eq!((map(1), map(2), map(3)), (None, Some(3), Some(4)), "the marker for the elided head maps to nothing");
    assert_eq!((map(4), map(5), map(6)), (None, Some(10), Some(11)), "the marker between regions maps to nothing");
    assert_eq!(map(7), None, "the final gap's marker follows the regions");
    assert_eq!(source_line(None, 20, 7), Some(7));
}

#[test]
fn reply_with_an_empty_stdin_exits_two_and_calls_nothing() {
    let mut w = world();
    w.opened();
    w.ports.stdin = " \n".into();
    assert_eq!(w.cr(&["reply", "--review", "001", "--comment", "10"]).code, 2);
    assert_eq!(w.gh.count(), 0);
}

#[test]
fn reply_posts_stdin_in_the_comments_thread_and_prints_the_new_url() {
    let mut w = world();
    w.opened();
    w.gh.answer("/replies", r#"{"html_url":"https://forge.invalid/pull/7#r55"}"#);
    w.ports.stdin = "it is the only opener".into();
    let o = w.cr(&["reply", "--review", "001", "--comment", "10"]);
    assert_eq!((o.code, o.stdout.as_str()), (0, "https://forge.invalid/pull/7#r55\n"), "{}", o.stderr);
    assert_eq!(w.gh.count(), 1);
    let posts = w.gh.posted("pulls/7/comments/10/replies");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["body"], "it is the only opener");
}
