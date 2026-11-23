//! `cr thread` and `cr reply` through `cli::run`: an in-memory tree and reader.

use super::{attach_collection, fixture, go, ports_with, cfg};
use crate::cli::Outcome;
use crate::ports::Ports;
use crate::record::{self, Review};
use margin_client::{Anchor, FakeMargin};
use testkit::TempDir;

struct World {
    run: TempDir,
    margin: FakeMargin,
    ports: Ports,
}

fn world() -> World {
    let run = TempDir::new("cr-thread");
    let margin = FakeMargin::default();
    let mut ports = ports_with(fixture(), vec![], cfg());
    ports.run_dir = run.path().to_path_buf();
    ports.margin = Box::new(margin.clone());
    World { run, margin, ports }
}

impl World {
    fn cr(&self, args: &[&str]) -> Outcome {
        go(&self.ports, args)
    }
    fn opened(&self, aim: &str) -> Review {
        assert_eq!(self.cr(&["open", "--aim", aim, "--seed", "3"]).code, 0);
        attach_collection(&self.margin, &fixture(), self.run.path(), 1)
    }
    fn thread_on(&self, r: &Review, path: &str, (first, last): (u32, u32), body: &str) -> String {
        let (document, version) = r.documents[path].clone();
        self.margin.thread_by(r.collection.as_deref().unwrap(), Anchor { document, version, first, last }, "ryan", body, 100)
    }
}

#[test_id(tjpkt42)]
#[test]
fn thread_prints_the_anchors_range_its_source_and_every_comment_in_order() {
    let w = world();
    let r = w.opened("demo/src/store.rs");
    let t = w.thread_on(&r, "demo/src/store.rs", (6, 8), "why open here?");
    w.thread_on(&r, "demo/src/store.rs", (3, 3), "unrelated");
    w.margin.reply_as(&t, "agent:concierge", "it is the only opener", 200);
    w.margin.reply_as(&t, "ryan", "and the error path?", 300);
    let o = w.cr(&["thread", "--review", "001", "--thread", &t]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let out = o.stdout;
    assert!(out.starts_with("demo/src/store.rs:6-8\n"), "{out}");
    for n in [6, 7, 8] {
        assert!(out.contains(&format!(">{n:>5}  ")), "line {n} is marked: {out}");
    }
    assert!(out.contains("     1  //! The store.\n"), "the window reaches line 1: {out}");
    let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("{needle:?} missing in {out}"));
    assert!(at("why open here?") < at("it is the only opener") && at("it is the only opener") < at("and the error path?"), "{out}");
    assert!(!out.contains("unrelated"), "{out}");
}

#[test_id(tywukne)]
#[test]
fn thread_of_a_thread_the_reader_does_not_have_exits_one_with_the_readers_words() {
    let w = world();
    w.opened("demo/src/store.rs");
    let o = w.cr(&["thread", "--review", "001", "--thread", "nope"]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("the reader at"), "{}", o.stderr);
}

#[test_id(t7ssrir)]
#[test]
fn a_thread_on_line_n_of_a_large_module_shows_source_line_n() {
    let w = world();
    let r = w.opened("demo/src/big.rs");
    let t = w.thread_on(&r, "demo/src/big.rs", (27, 27), "which filler?");
    let o = w.cr(&["thread", "--review", "001", "--thread", &t]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains(">   27  fn filler_0() -> u32 {\n"), "{}", o.stdout);
}

#[test_id(t2wvwvw)]
#[test]
fn reply_with_an_empty_stdin_exits_two_and_posts_nothing() {
    let mut w = world();
    let r = w.opened("demo/src/store.rs");
    let t = w.thread_on(&r, "demo/src/store.rs", (6, 6), "why?");
    w.ports.stdin = " \n".into();
    assert_eq!(w.cr(&["reply", "--review", "001", "--thread", &t]).code, 2);
    assert!(w.margin.comments_posted().is_empty());
}

#[test_id(tj0x3ys)]
#[test]
fn reply_posts_stdin_in_the_thread_not_held_and_prints_the_new_url() {
    let mut w = world();
    let r = w.opened("demo/src/store.rs");
    let t = w.thread_on(&r, "demo/src/store.rs", (6, 6), "why?");
    w.ports.stdin = "it is the only opener".into();
    let o = w.cr(&["reply", "--review", "001", "--thread", &t]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.starts_with("https://margin.invalid/c/col1"), "{}", o.stdout);
    let posts = w.margin.comments_posted();
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].body.as_str(), posts[0].held, posts[0].in_reply_to.as_deref()), ("it is the only opener", false, Some(t.as_str())));
    assert_eq!((posts[0].anchor.first, posts[0].anchor.last), (6, 6));
    assert_eq!(record::load(w.run.path(), 1).unwrap().collection.as_deref(), Some("col1"));
}
