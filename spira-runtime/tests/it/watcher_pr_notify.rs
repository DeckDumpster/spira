use std::sync::{Arc, Mutex};

use runtime::event::EventKind;
use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::watchers::pr_notify::*;
use spira_runtime::watchers::INBOX_APPEND;

fn registration() -> runtime::Registration {
    PrNotify { interval_ms: 90_000, deploy_giveup_secs: 100 }.registration()
}

fn forge(obs: RepoObs) -> Snapshot {
    Snapshot { repos: vec![("r".into(), obs)] }
}

fn pr_seven_open() -> Snapshot {
    forge(RepoObs { open: Some(vec![OpenPr { number: 7, status: "pending".into(), title: "t7".into(), fail: String::new() }]), ..RepoObs::default() })
}

fn pr_seven_merged() -> Snapshot {
    let finals = [(7, Final { state: "MERGED".into(), sha: "0123456789abcdef".into(), beads: vec!["- sp-aaa one".into()] })].into();
    forge(RepoObs { open: Some(vec![]), finals, ..RepoObs::default() })
}

fn pr_tracked_then_merged() -> TaskRig {
    let world = Arc::new(Mutex::new(pr_seven_open()));
    let seen = world.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*seen.lock().unwrap()).unwrap())).double(INBOX_APPEND, |l| Ok(l.into())).double(OPERATOR_NOTE, |n| Ok(n.into()));
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    *world.lock().unwrap() = pr_seven_merged();
    r.clock.advance(1);
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&pr_tracked_then_merged(), NAME);
    assert!(rows.iter().any(|k| k == "repos/r"), "{rows:?}");
}

#[test]
fn a_boot_reports_the_merge_of_a_pr_tracked_before_it() {
    kit::assert_boot_matches_an_unstopped_run(pr_tracked_then_merged, registration, STREAM, EventKind::new(TICK, ""));
}
