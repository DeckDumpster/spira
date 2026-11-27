// covers: batcher-cut/src/pace.rs
//! The batcher's round pages, a real writer of the inbox, against a real broker.
#[macro_use]
extern crate spira_test_id;
#[path = "../../inbox-rules/tests/support/announced.rs"]
mod support;

use support::{announced, sink};
use batcher_cut::pace::{self, Files, Seams};
use batcher_cut::screen::tell_concierge;
use testkit::broker::Broker;

#[test_id(tkasbxp)]
#[test]
fn a_stale_pause_page_is_announced_as_the_batcher() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("batcher-stale-pause"));
    let inbox = sink(&d, &broker, "batcher");
    let files = Files::new(d.path(), "a");
    std::fs::create_dir_all(files.pause_file().parent().unwrap()).unwrap();
    std::fs::write(files.pause_file(), "red: the round was red\n").unwrap();
    let since = files.pause_since().unwrap();
    let stamp = |_| "T1".to_string();
    let sm = Seams { repo: "a", now: since + 3601, inbox: &inbox, stamp: &stamp, redrive_after: 1, pause_lift_after: 1, landing_head: None };

    pace::page_stale_pause(&files, &sm, 3600, || true).unwrap();

    let line = announced(&broker, &inbox);
    assert!(line.starts_with("T1 [watch:rounds] PAUSE UNATTENDED a: a red round, held 3601s"), "{line}");
}

#[test_id(tmvqt0y)]
#[test]
fn a_held_repo_page_is_announced_and_marked_as_said() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("batcher-held"));
    let inbox = sink(&d, &broker, "batcher");

    pace::page_held(d.path(), Some(&inbox), "a", &["sp-a", "sp-b"], "T1");

    assert_eq!(announced(&broker, &inbox), "T1 [watch:rounds] BATCHES HELD a: SPIRA_BATCHER_FORGE_ENABLE=0 with CERTIFIED work waiting: sp-a,sp-b");
    assert_eq!(std::fs::read_to_string(d.join("rounds-held/a")).unwrap(), "sp-a,sp-b");
}

#[test_id(t161eh5)]
#[test]
fn a_held_page_that_misses_the_inbox_is_not_marked_as_said() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("batcher-held-unwritable"));
    let mut inbox = sink(&d, &broker, "batcher");
    inbox.inbox = d.path().to_path_buf();

    pace::page_held(d.path(), Some(&inbox), "a", &["sp-a"], "T1");

    assert!(!d.join("rounds-held/a").exists());
}

#[test_id(t19k45b)]
#[test]
fn a_repeated_refusal_page_is_announced_as_the_batcher() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("batcher-repeat"));
    let inbox = sink(&d, &broker, "batcher");

    for now in [100, 101] {
        pace::stop_on_repeat(d.path(), Some(&inbox), "a", "b-1", "refused: no binaries", now, "T1");
    }

    let line = announced(&broker, &inbox);
    assert!(line.starts_with("T1 [watch:rounds] ROUNDS STOPPED a: 2 consecutive rounds refused"), "{line}");
}

#[test_id(t1cj4pw)]
#[test]
fn a_sift_line_is_announced_as_the_batcher() {
    let (broker, d) = (Broker::up(), testkit::TempDir::new("batcher-sift"));
    let inbox = sink(&d, &broker, "batcher");

    tell_concierge(Ok(inbox.clone()), "sp-a sent back", 0);

    assert_eq!(announced(&broker, &inbox), "19700101T000000Z [watch:sift] sp-a sent back");
}
