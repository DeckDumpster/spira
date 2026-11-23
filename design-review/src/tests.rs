use super::*;
use margin_client::FakeMargin;
use runtime::ports::VirtualClock;

const PAGE: &str = "wiki/projects/p/designs/d.md";

struct Fx {
    dir: testkit::TempDir,
    fake: FakeMargin,
}

fn fx() -> Fx {
    let dir = testkit::TempDir::new("design-review");
    std::fs::create_dir_all(dir.path().join("w/wiki/projects/p/designs")).unwrap();
    std::fs::write(dir.path().join("w").join(PAGE), "# The Design\n\nbody\n").unwrap();
    Fx { dir, fake: FakeMargin::default() }
}

fn go(f: &Fx, argv: &[&str], stdin: &str) -> Outcome {
    let ports = Ports { run_dir: f.dir.path().join("run"), wiki: f.dir.path().join("w"), margin: Box::new(f.fake.clone()), clock: Box::new(VirtualClock::new(7_000)), stdin: stdin.into() };
    run(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &ports)
}

#[test_id(tdr0001)]
#[test]
fn the_first_post_makes_one_collection_of_one_prose_document_holding_the_file_and_writes_the_record() {
    let f = fx();
    let o = go(&f, &["post", "--page", PAGE], "cover");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(f.fake.collections_posted(), 1);
    let (title, cover, docs) = f.fake.collection_posted(0);
    assert_eq!((title.as_str(), cover.as_str()), ("The Design", "cover"));
    assert_eq!(docs, vec![Doc { path: PAGE.into(), kind: "prose".into(), text: "# The Design\n\nbody\n".into() }]);
    let r = by_collection(&f.dir.path().join("run"), "col1").unwrap();
    assert_eq!((r.page.as_str(), r.version, r.state.as_str(), r.posted_at_ms), (PAGE, 1, "open", 7_000));
    assert_eq!(o.stdout, format!("{}\n", r.url));
}

#[test_id(tdr0002)]
#[test]
fn a_changed_page_posts_one_new_version_and_no_new_collection_and_an_unchanged_one_posts_nothing() {
    let f = fx();
    go(&f, &["post", "--page", PAGE], "cover");
    let same = go(&f, &["post", "--page", PAGE], "");
    assert_eq!(same.code, 0);
    std::fs::write(f.dir.path().join("w").join(PAGE), "# The Design\n\nnew body\n").unwrap();
    let o = go(&f, &["post", "--page", PAGE], "");
    assert!(o.stdout.trim_end().ends_with("version 2"), "{}", o.stdout);
    assert_eq!(f.fake.collections_posted(), 1);
    assert_eq!(f.fake.document_text("col1-d1", 2).unwrap(), "# The Design\n\nnew body\n");
    let r = by_collection(&f.dir.path().join("run"), "col1").unwrap();
    assert_eq!(r.version, 2);
    let again = go(&f, &["post", "--page", PAGE], "");
    assert_eq!(by_collection(&f.dir.path().join("run"), "col1").unwrap(), r);
    assert!(!again.stdout.contains("version"));
    assert!(f.fake.document_text("col1-d1", 3).is_err(), "an unchanged page posts no version");
}

#[test_id(tdr0003)]
#[test]
fn a_first_post_with_an_empty_cover_note_exits_2_and_posts_nothing() {
    let f = fx();
    let o = go(&f, &["post", "--page", PAGE], "  \n");
    assert_eq!(o.code, 2);
    assert_eq!(f.fake.collections_posted(), 0);
}

#[test_id(tdr0004)]
#[test]
fn a_page_outside_the_wiki_or_missing_exits_2_naming_the_path() {
    let f = fx();
    let o = go(&f, &["post", "--page", "../x.md"], "cover");
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("../x.md"), "{}", o.stderr);
    let o = go(&f, &["post", "--page", "wiki/projects/p/designs/none.md"], "cover");
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("none.md"), "{}", o.stderr);
    assert_eq!(f.fake.collections_posted(), 0);
}

#[test_id(tdr0005)]
#[test]
fn an_empty_reply_exits_2_and_a_reply_is_posted_unheld_in_the_thread() {
    let f = fx();
    go(&f, &["post", "--page", PAGE], "cover");
    let a = margin_client::Anchor { path: PAGE.into(), first: 1, last: 1 };
    let t = f.fake.his_thread("col1", a, "q", 1);
    assert_eq!(go(&f, &["reply", "--page", PAGE, "--thread", &t], "").code, 2);
    assert!(f.fake.comments_posted().is_empty());
    assert_eq!(go(&f, &["reply", "--page", PAGE, "--thread", &t], "answer").code, 0);
    let c = f.fake.comments_posted();
    assert_eq!((c.len(), c[0].held, c[0].in_reply_to.as_deref()), (1, false, Some(t.as_str())));
}

#[test_id(tdr0006)]
#[test]
fn a_closed_page_posted_again_opens_a_new_collection_and_a_bare_positional_is_refused() {
    let f = fx();
    go(&f, &["post", "--page", PAGE], "cover");
    assert_eq!(go(&f, &["close", "--page", PAGE], "").code, 0);
    assert_eq!(f.fake.closed(), vec!["col1".to_string()]);
    assert!(go(&f, &["status"], "").stdout.contains("closed"));
    go(&f, &["post", "--page", PAGE], "again");
    assert_eq!(f.fake.collections_posted(), 2);
    assert_eq!(go(&f, &["post", PAGE], "cover").code, 2);
}

#[test_id(tdr0007)]
#[test]
fn a_reply_names_a_thread_of_this_page_and_a_posted_page_and_help_lists_every_verb() {
    let f = fx();
    assert_eq!(go(&f, &["reply", "--page", PAGE, "--thread", "t"], "a").code, 2, "not posted yet");
    go(&f, &["post", "--page", PAGE], "cover");
    assert_eq!(go(&f, &["reply", "--page", PAGE], "a").code, 2, "no thread");
    assert_eq!(go(&f, &["reply", PAGE], "a").code, 2);
    assert_eq!(go(&f, &["close", PAGE], "").code, 2);
    let docs = ["a", "b"].map(|n| Doc { path: format!("wiki/projects/p/designs/{n}.md"), kind: "prose".into(), text: n.into() });
    f.fake.post_collection("two", "c", &docs, None).unwrap();
    let r = Record { page: PAGE.into(), collection: "col2".into(), document: "col2-d2".into(), version: 1, url: "u".into(), sha256: "s".into(), state: "open".into(), posted_at_ms: 1 };
    save(&f.dir.path().join("run"), &r).unwrap();
    let other = f.fake.his_thread("col2", margin_client::Anchor { path: docs[0].path.clone(), first: 1, last: 1 }, "q", 1);
    let o = go(&f, &["reply", "--page", PAGE, "--thread", &other], "a");
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(f.fake.comments_posted().is_empty());
    let help = go(&f, &["-h"], "").stdout;
    assert!(["post", "reply", "close", "status"].iter().all(|v| help.contains(v)), "{help}");
}
