//! A queue.forge repo with the batch machine on the control store: `batch open` writes the batch
//! record before its PR, `batch sweep` settles what a crashed open left, and every forge verb
//! finds the open batch in the store, with no `open` file.

use super::*;
use crate::batches::{self, mem::MemDoor};
use crate::records::Kv;

const BRANCH: &str = "spira/queue/20260929T010203Z";

/// A queue.forge repo on the store whose sp-a is CERTIFIED at ta, with a head h1 assembled from it.
fn forge_world() -> T {
    let t = T::new(LandMode::Queue);
    t.lc.store.get_or_init(MemDoor::default);
    t.git.set("origin/main", "b0");
    t.git.set("h1", "h1");
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta".into())];
    t.lc_row("sp-a", "CERTIFIED", "ta", 1);
    t.var("SPIRA_ROUND_LEASE_TTL_SECS", "300");
    t
}

fn door(t: &T) -> &MemDoor {
    t.lc.store.get().unwrap()
}

/// The repo's batch record `id`: its machine state and the queue's fields.
fn record(t: &T, id: &str) -> (String, Kv) {
    let b = batches::batch(door(t), "spira", id).unwrap().unwrap();
    (b.state, b.kv)
}

/// A forge batch record as `batch open` leaves it at `phase`, opened at `opened`.
fn seed(t: &T, id: &str, phase: &str, opened: u64, pr: Option<&str>) {
    let mut kv = Kv::default();
    for (k, v) in [
        ("batch_id", id),
        ("head", "h1"),
        ("base", "b0"),
        ("members", "sp-a:ta"),
        ("opened", &opened.to_string()),
        ("branch", &format!("spira/queue/{id}")),
        ("owner", "batcher"),
        ("forge.phase", phase),
    ] {
        kv.push(k, v);
    }
    if let Some(pr) = pr {
        kv.push("pr", pr);
    }
    door(t).cut_round(
        "spira",
        id,
        "OPEN",
        &[("sp-a", "ta")],
        opened as i64,
        Some(&kv),
    );
}

fn open_args() -> Vec<&'static str> {
    vec![
        "batch",
        "open",
        "--repo",
        "spira",
        "--members",
        "sp-a@ta",
        "--head",
        "h1",
        "--owner",
        "batcher",
        "--skip-pregate",
    ]
}

fn forge_calls(t: &T) -> Vec<String> {
    t.forge.calls.borrow().clone()
}

#[test]
fn batch_open_records_the_batch_then_opens_and_arms_its_pr_with_no_open_file() {
    let t = forge_world();
    assert_eq!(t.run(&open_args()), 0, "{}", t.err());
    let (state, kv) = record(&t, "spira-20260929T010203Z");
    assert_eq!(state, "OPEN");
    assert_eq!(
        (
            kv.get("forge.phase"),
            kv.get("pr"),
            kv.get("head"),
            kv.get("branch"),
            kv.get("members"),
            kv.get("owner")
        ),
        (
            Some("armed"),
            Some("77"),
            Some("h1"),
            Some(BRANCH),
            Some("sp-a:ta"),
            Some("batcher")
        ),
        "{kv:?}"
    );
    let calls = forge_calls(&t);
    let created = calls
        .iter()
        .position(|c| c.starts_with(&format!("pr-create {BRANCH} main queue: 1 beads for spira")))
        .unwrap();
    let armed = calls.iter().position(|c| c == "pr-automerge 77").unwrap();
    assert!(created < armed, "{calls:?}");
    assert_eq!(
        door(&t).kinds.borrow().as_slice(),
        ["queue.round-fields", "queue.round-fields"],
        "open, then armed"
    );
    assert!(!t.qfile("open").exists());
    assert!(
        t.out()
            .contains("\nbatch_id=spira-20260929T010203Z\npr=77\n"),
        "{}",
        t.out()
    );
}

#[test]
fn a_batch_open_whose_record_cannot_be_written_pushes_nothing_and_opens_no_pr() {
    let t = forge_world();
    t.lc.cut_refused.set(Some("refused: IllegalTransition"));
    assert_eq!(t.run(&open_args()), 1);
    assert!(
        !forge_calls(&t).iter().any(|c| c.starts_with("pr-create")),
        "{:?}",
        forge_calls(&t)
    );
    assert!(
        !t.lib.has(&format!("push origin h1:refs/heads/{BRANCH}")),
        "{:?}",
        t.lib.calls.borrow()
    );
}

#[test]
fn a_crash_after_the_pr_and_before_open_leaves_an_opening_record_the_sweep_adopts() {
    let t = forge_world();
    door(&t).lost_kind.set(Some("queue.round-fields"));
    assert_eq!(t.run(&open_args()), 1);
    assert!(t.err().contains("stays forge.phase=opening"), "{}", t.err());
    let id = "spira-20260929T010203Z";
    assert_eq!(record(&t, id).1.get("forge.phase"), Some("opening"));
    door(&t).lost_kind.set(None);
    t.forge
        .queue_prs
        .borrow_mut()
        .push(("77".into(), BRANCH.into()));

    assert_eq!(
        t.run(&["batch", "sweep", "--repo", "spira"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        record(&t, id).1.get("forge.phase"),
        Some("opening"),
        "inside one TTL the opener keeps it"
    );
    t.clock.now.set(t.clock.now() + 300);
    assert_eq!(
        t.run(&["batch", "sweep", "--repo", "spira"]),
        0,
        "{}",
        t.err()
    );
    let (state, kv) = record(&t, id);
    assert_eq!(
        (state.as_str(), kv.get("forge.phase"), kv.get("pr")),
        ("OPEN", Some("open"), Some("77")),
        "{kv:?}"
    );
    let calls = forge_calls(&t);
    assert!(
        !calls.iter().any(|c| c.starts_with("pr-close")),
        "the adopted PR stays open: {calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("pr-automerge"))
            .count(),
        0,
        "the sweep never merges: {calls:?}"
    );
}

#[test]
fn a_queue_pr_no_record_names_is_closed_and_its_branch_deleted_never_merged() {
    let t = forge_world();
    seed(&t, "kept", "armed", 900, Some("12"));
    t.forge.queue_prs.borrow_mut().extend([
        ("12".into(), "spira/queue/kept".into()),
        ("90".into(), "spira/queue/stray".into()),
    ]);
    assert_eq!(
        t.run(&["batch", "sweep", "--repo", "spira"]),
        0,
        "{}",
        t.err()
    );
    let calls = forge_calls(&t);
    assert!(calls.contains(&"pr-close 90".to_string()), "{calls:?}");
    assert!(!calls.contains(&"pr-close 12".to_string()), "{calls:?}");
    assert!(
        !calls.iter().any(|c| c.starts_with("pr-automerge")),
        "{calls:?}"
    );
    assert!(
        t.lib.has("push origin :refs/heads/spira/queue/stray"),
        "{:?}",
        t.lib.calls.borrow()
    );
    assert!(t
        .git
        .calls
        .borrow()
        .contains(&"branch -D sanctioned spira/queue/stray".to_string()));
}

#[test]
fn an_opening_record_whose_pr_does_not_exist_is_released_past_one_ttl_unless_leased() {
    let t = forge_world();
    seed(&t, "lost", "opening", 500, None);
    seed(&t, "leased", "opening", 500, None);
    t.lc.leases.borrow_mut().insert(
        "spira/leased".into(),
        Ok(Some(RoundLease {
            holder: Some("rc-1".into()),
            token: "9-0".into(),
            ttl_left_secs: 60,
        })),
    );
    t.clock.now.set(900);
    assert_eq!(
        t.run(&["batch", "sweep", "--repo", "spira"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(record(&t, "lost").0, "ABANDONED");
    assert!(t.lc.has("abandon-batch lost"), "{:?}", t.lc.calls.borrow());
    assert_eq!(record(&t, "leased").0, "OPEN");
    assert!(
        t.out()
            .contains("batch leased is opening under rc-1's live lease"),
        "{}",
        t.out()
    );
    assert!(t
        .git
        .calls
        .borrow()
        .contains(&"branch -D sanctioned spira/queue/lost".to_string()));
}

#[test]
fn verdict_finds_the_open_batch_in_the_store_with_no_open_file() {
    let t = forge_world();
    seed(&t, "deck-1", "armed", 900, Some("12"));
    assert!(!t.qfile("open").exists());
    t.run(&["verdict", "spira"]);
    let calls = forge_calls(&t);
    assert!(
        calls.contains(&"check-status 12 spira/queue/deck-1".to_string()),
        "{calls:?}\n{}",
        t.err()
    );
    assert!(
        !calls.contains(&"pr-close 12".to_string()),
        "the recorded PR is no stray: {calls:?}"
    );
}

#[test]
fn verdict_leaves_an_opening_record_to_the_sweep() {
    let t = forge_world();
    seed(&t, "deck-1", "opening", 900, None);
    t.forge
        .queue_prs
        .borrow_mut()
        .push(("12".into(), "spira/queue/deck-1".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(
        t.out().contains("batch record deck-1 is still opening"),
        "{}",
        t.out()
    );
    let calls = forge_calls(&t);
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("check-status") || c.starts_with("pr-close")),
        "{calls:?}"
    );
}

#[test]
fn abandon_and_claim_act_on_the_store_record() {
    let t = forge_world();
    seed(&t, "deck-1", "armed", 900, Some("12"));
    assert_eq!(t.run(&["claim", "--reason", "hand edit"]), 0, "{}", t.err());
    assert_eq!(record(&t, "deck-1").1.get("owner"), Some("concierge"));
    assert_eq!(t.run(&["release"]), 0, "{}", t.err());
    assert_eq!(t.run(&["abandon", "--reason", "dirty"]), 0, "{}", t.err());
    let (state, kv) = record(&t, "deck-1");
    assert_eq!(
        (state.as_str(), kv.get("reason")),
        ("ABANDONED", Some("dirty")),
        "{kv:?}"
    );
    assert!(forge_calls(&t).contains(&"pr-close 12".to_string()));
    assert_eq!(t.run(&["abandon", "--reason", "again"]), 1);
    assert!(t.err().contains("no open batch for spira"), "{}", t.err());
}

#[test]
fn batch_open_refuses_while_a_record_is_opening_or_the_store_cannot_say() {
    let t = forge_world();
    seed(&t, "deck-1", "opening", 900, None);
    assert_eq!(t.run(&open_args()), 1);
    assert!(
        t.err()
            .contains("a batch is already open for spira (owner=batcher)"),
        "{}",
        t.err()
    );
    let t = forge_world();
    door(&t).down.set(true);
    assert_eq!(t.run(&open_args()), 1);
    assert!(
        t.err()
            .contains("cannot tell whether spira has an open batch"),
        "{}",
        t.err()
    );
    assert!(!forge_calls(&t).iter().any(|c| c.starts_with("pr-create")));
}

#[test]
fn the_sweep_refuses_while_the_open_file_of_before_the_store_remains() {
    let t = forge_world();
    t.open_record("pr=5\nbranch=spira/queue/old\nmembers=sp-a:ta\n");
    t.forge
        .queue_prs
        .borrow_mut()
        .push(("5".into(), "spira/queue/old".into()));
    assert_eq!(t.run(&["batch", "sweep", "--repo", "spira"]), 1);
    assert!(
        t.err().contains("still holds a batch recorded before"),
        "{}",
        t.err()
    );
    assert!(!forge_calls(&t).iter().any(|c| c.starts_with("pr-close")));
}

#[test]
fn batch_show_prints_the_store_record_and_refuses_when_the_store_cannot_say() {
    let t = forge_world();
    assert_eq!(t.run(&["batch", "show", "--repo", "spira"]), 0);
    assert_eq!(t.out(), "", "no batch open, nothing printed");
    seed(&t, "deck-1", "armed", 900, Some("12"));
    assert_eq!(
        t.run(&["batch", "show", "--repo", "spira"]),
        0,
        "{}",
        t.err()
    );
    let shown = Kv::parse(&t.out());
    assert_eq!(
        (
            shown.get("batch_id"),
            shown.get("pr"),
            shown.get("forge.phase")
        ),
        (Some("deck-1"), Some("12"), Some("armed")),
        "{}",
        t.out()
    );
    door(&t).down.set(true);
    assert_eq!(t.run(&["batch", "show", "--repo", "spira"]), 1);
    assert!(
        t.err()
            .contains("cannot tell whether spira has an open batch"),
        "{}",
        t.err()
    );
}

#[test]
fn batch_show_ended_lists_the_closed_forge_batches_on_the_store_and_the_archives_newest_first() {
    let t = forge_world();
    seed(&t, "deck-1", "armed", 900, Some("12"));
    let mut abandoned = Kv::default();
    for (k, v) in [
        ("batch_id", "deck-0"),
        ("pr", "11"),
        ("opened", "800"),
        ("members", "sp-a:ta"),
        ("forge.phase", "armed"),
        ("reason", "red"),
    ] {
        abandoned.push(k, v);
    }
    door(&t).cut_round(
        "spira",
        "deck-0",
        "ABANDONED",
        &[("sp-a", "ta")],
        800,
        Some(&abandoned),
    );
    let mut ended = abandoned.clone();
    ended.remove("batch_id");
    ended.lines.insert(0, ("batch_id".into(), "deck-2".into()));
    ended.remove("pr");
    ended.push("pr", "13");
    ended.remove("opened");
    ended.push("opened", "950");
    ended.push("ended", "abandoned");
    door(&t).cut_round(
        "spira",
        "deck-2",
        "OPEN",
        &[("sp-a", "ta")],
        950,
        Some(&ended),
    );
    door(&t).cut_round(
        "spira",
        "spira-9",
        "ABANDONED",
        &[("sp-a", "ta")],
        990,
        None,
    );
    fs::write(
        t.qfile("closed-pr7-20260901T000000Z"),
        "pr=7\nopened=100\nbranch=spira/queue/old\nreason=gone\n",
    )
    .unwrap();

    assert_eq!(
        t.run(&["batch", "show", "--repo", "spira", "--ended", "5"]),
        0,
        "{}",
        t.err()
    );
    let out = t.out();
    let records: Vec<Kv> = out
        .split("\n\n")
        .filter(|r| !r.trim().is_empty())
        .map(Kv::parse)
        .collect();
    let prs: Vec<_> = records.iter().map(|r| r.get("pr").unwrap_or("")).collect();
    assert_eq!(
        prs,
        ["13", "11", "7"],
        "ended forge batches newest first, the open one and a local round left out: {out}"
    );
    assert_eq!(records[1].get("reason"), Some("red"));
    t.io.out.borrow_mut().clear();
    assert_eq!(
        t.run(&["batch", "show", "--repo", "spira", "--ended", "1"]),
        0
    );
    assert_eq!(
        t.out()
            .split("\n\n")
            .filter(|r| !r.trim().is_empty())
            .count(),
        1
    );
    door(&t).down.set(true);
    assert_eq!(
        t.run(&["batch", "show", "--repo", "spira", "--ended", "5"]),
        1
    );
    assert!(
        t.err().contains("cannot read spira's ended batches"),
        "{}",
        t.err()
    );
}

#[test]
fn batch_show_carries_the_forge_state_file_as_forge_fields_while_the_batch_is_the_file() {
    let t = T::new(LandMode::Queue);
    t.open_record("batch_id=deck-1\npr=12\nmembers=sp-a:ta\n");
    fs::write(
        t.qfile("forge-state"),
        "batch_id=deck-1\npr=12\npr_state=open\nci=pending\nstep=unit\n",
    )
    .unwrap();
    assert_eq!(
        t.run(&["batch", "show", "--repo", "spira"]),
        0,
        "{}",
        t.err()
    );
    let shown = Kv::parse(&t.out());
    assert_eq!(
        (
            shown.get("pr"),
            shown.get("forge.ci"),
            shown.get("forge.pr_state"),
            shown.get("forge.step"),
            shown.get("forge.pr")
        ),
        (
            Some("12"),
            Some("pending"),
            Some("open"),
            Some("unit"),
            None
        ),
        "{}",
        t.out()
    );

    let stale = T::new(LandMode::Queue);
    stale.open_record("batch_id=deck-2\npr=13\n");
    fs::write(stale.qfile("forge-state"), "batch_id=deck-1\nci=red\n").unwrap();
    assert_eq!(
        stale.run(&["batch", "show", "--repo", "spira"]),
        0,
        "{}",
        stale.err()
    );
    assert_eq!(
        Kv::parse(&stale.out()).get("forge.ci"),
        None,
        "a record naming another batch is not this one's"
    );
}

#[test]
fn a_failed_push_or_create_leaves_the_record_opening_and_a_create_the_forge_misreported_is_adopted()
{
    let id = "spira-20260929T010203Z";
    let t = forge_world();
    t.lib.push_ok.set(false);
    assert_eq!(t.run(&open_args()), 1);
    assert!(
        t.err().contains(&format!(
            "could not push {BRANCH} — batch {id} stays forge.phase=opening"
        )),
        "{}",
        t.err()
    );
    assert!(!forge_calls(&t).iter().any(|c| c.starts_with("pr-create")));

    let t = forge_world();
    *t.forge.pr.borrow_mut() = None;
    assert_eq!(t.run(&open_args()), 1);
    assert!(t.err().contains("forge pr-create failed"), "{}", t.err());
    assert_eq!(record(&t, id).1.get("forge.phase"), Some("opening"));

    let t = forge_world();
    *t.forge.create_then_err.borrow_mut() = Some(("81".into(), "rc=1: timed out".into()));
    assert_eq!(t.run(&open_args()), 0, "{}", t.err());
    assert!(
        t.err().contains("but PR 81 is open on it — adopting it"),
        "{}",
        t.err()
    );
    let kv = record(&t, id).1;
    assert_eq!(
        (kv.get("forge.phase"), kv.get("pr")),
        (Some("armed"), Some("81"))
    );
}

#[test]
fn a_pr_whose_auto_merge_cannot_be_armed_stays_open_and_the_open_fails_naming_it() {
    let t = forge_world();
    t.forge.arm_fails.set(true);
    assert_eq!(t.run(&open_args()), 1);
    assert_eq!(
        record(&t, "spira-20260929T010203Z").1.get("forge.phase"),
        Some("open")
    );
    assert!(t.out().contains("\npr=77\n"), "{}", t.out());
    assert!(
        t.err().contains("auto-merge could not be armed"),
        "{}",
        t.err()
    );
}

#[test]
fn a_named_batch_open_is_refused_while_another_holder_leases_it() {
    let t = forge_world();
    t.lc.leases.borrow_mut().insert(
        "spira/deck-9".into(),
        Ok(Some(RoundLease {
            holder: Some("rc-1".into()),
            token: "9-0".into(),
            ttl_left_secs: 60,
        })),
    );
    let mut args = open_args();
    args.extend(["--name", "deck-9"]);
    assert_eq!(t.run(&args), 1);
    assert!(
        t.err().contains("round deck-9 is leased by rc-1"),
        "{}",
        t.err()
    );
    args.extend(["--lease-token", "9-0"]);
    assert_eq!(t.run(&args), 0, "{}", t.err());
    assert_eq!(record(&t, "deck-9").1.get("forge.phase"), Some("armed"));
    assert_eq!(
        t.run(&["batch", "open", "--lease-token", "9-0"]),
        2,
        "a token names the batch it is on"
    );
}

#[test]
fn the_sweep_refuses_when_it_cannot_tell_and_names_a_refused_release() {
    let t = forge_world();
    t.var("SPIRA_ROUND_LEASE_TTL_SECS", "5m");
    assert_eq!(t.run(&["batch", "sweep", "--repo", "spira"]), 1);
    assert!(
        t.err()
            .contains("SPIRA_ROUND_LEASE_TTL_SECS does not parse as seconds"),
        "{}",
        t.err()
    );

    let t = forge_world();
    door(&t).down.set(true);
    assert_eq!(t.run(&["batch", "sweep", "--repo", "spira"]), 1);
    assert!(
        t.err().contains("cannot tell which batches spira has open"),
        "{}",
        t.err()
    );

    let t = forge_world();
    seed(&t, "unread", "opening", 500, None);
    seed(&t, "refused", "opening", 500, None);
    t.lc.leases
        .borrow_mut()
        .insert("spira/unread".into(), Err("lc-serve did not answer".into()));
    t.lc.abandon_refused.set(Some("refused: IllegalTransition"));
    t.clock.now.set(900);
    assert_eq!(t.run(&["batch", "sweep", "--repo", "spira"]), 1);
    assert!(
        t.err()
            .contains("cannot tell whether batch unread is leased"),
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("the release of batch refused was refused"),
        "{}",
        t.err()
    );
    assert_eq!(record(&t, "unread").0, "OPEN");

    let t = forge_world();
    t.lib.r.borrow_mut().mode = LandMode::QueueLocal;
    assert_eq!(t.run(&["batch", "sweep", "--repo", "spira"]), 1);
    assert!(t.err().contains("repo is not in queue mode"), "{}", t.err());
}

#[test]
fn the_sweep_is_refused_while_the_batch_machine_is_not_on_the_store() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["batch", "sweep", "spira"]), 1);
    assert!(
        t.err().contains("the batch machine is not on it"),
        "{}",
        t.err()
    );
}

#[test]
fn a_settled_batch_the_machine_has_not_ended_is_ended_on_its_record_and_reads_as_none() {
    let t = forge_world();
    seed(&t, "deck-1", "armed", 900, Some("12"));
    t.with_world(|w| {
        let c = crate::ops::resolve(w, "test", None).unwrap();
        let open = crate::ops::forge_batch::read(w, &c).unwrap().unwrap();
        assert_eq!(open.place(), "batch record deck-1");
        open.rewrite(w, &c, &["owner"], &[("owner", "concierge")])
            .unwrap();
        open.remove(w, &c, "settled").unwrap();
        open.remove(w, &c, "again").unwrap();
        assert!(crate::ops::forge_batch::read(w, &c).unwrap().is_none());
    });
    let (state, kv) = record(&t, "deck-1");
    assert_eq!(
        (state.as_str(), kv.get("ended"), kv.get("owner")),
        ("OPEN", Some("settled"), Some("concierge")),
        "{kv:?}"
    );
}

#[test]
fn a_queue_local_verdict_releases_a_stale_forge_batch_still_opening_when_no_pr_exists() {
    let t = forge_world();
    seed(&t, "left", "opening", 1, None);
    t.lib.r.borrow_mut().mode = LandMode::QueueLocal;
    t.clock.now.set(1_000_000);
    t.run(&["verdict", "spira"]);
    assert!(t.lc.has("abandon-batch left"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn a_queue_local_verdict_adopts_a_stale_opening_record_whose_pr_the_forge_lists() {
    let t = forge_world();
    seed(&t, "left", "opening", 1, None);
    t.forge.queue_prs.borrow_mut().push(("12".into(), "spira/queue/left".into()));
    t.lib.r.borrow_mut().mode = LandMode::QueueLocal;
    t.clock.now.set(1_000_000);
    t.run(&["verdict", "spira"]);
    assert!(!t.lc.has("abandon-batch"), "{:?}", t.lc.calls.borrow());
    assert_eq!(record(&t, "left").1.get("forge.phase"), Some("open"));
}

#[test]
fn a_queue_local_verdict_leaves_a_stale_opening_record_when_the_forge_cannot_list_its_prs() {
    let t = forge_world();
    seed(&t, "lost", "opening", 1, None);
    t.forge.queue_prs_unreadable.set(true);
    t.lib.r.borrow_mut().mode = LandMode::QueueLocal;
    t.clock.now.set(1_000_000);
    assert_ne!(t.run(&["verdict", "spira"]), 0);
    assert!(t.err().contains("the forge cannot list its spira/queue/* PRs"), "{}", t.err());
    assert_eq!(record(&t, "lost").0, "OPEN");
    assert!(!t.lc.has("abandon-batch"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn the_sweep_changes_nothing_when_the_forge_cannot_list_its_prs() {
    let t = forge_world();
    seed(&t, "lost", "opening", 1, None);
    t.forge.queue_prs_unreadable.set(true);
    t.clock.now.set(900);
    assert_eq!(t.run(&["batch", "sweep", "--repo", "spira"]), 1);
    assert!(
        t.err()
            .contains("the forge cannot list spira's spira/queue/* PRs — nothing swept"),
        "{}",
        t.err()
    );
    assert_eq!(record(&t, "lost").0, "OPEN", "no PR list, so no release");
    assert!(!t.lc.has("abandon-batch"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn the_sweep_changes_nothing_when_the_lease_ttl_does_not_resolve() {
    let t = forge_world();
    seed(&t, "lost", "opening", 1, None);
    t.env
        .unresolved
        .borrow_mut()
        .insert("SPIRA_ROUND_LEASE_TTL_SECS".into());
    t.clock.now.set(900);
    assert_eq!(t.run(&["batch", "sweep", "--repo", "spira"]), 1);
    assert!(t.err().contains("SPIRA_ROUND_LEASE_TTL_SECS does not resolve (SPIRA_ROUND_LEASE_TTL_SECS is not declared) — nothing swept"), "{}", t.err());
    assert_eq!(record(&t, "lost").0, "OPEN");
}

#[test]
fn an_adoption_another_writer_beat_is_left_and_one_that_cannot_be_written_fails_the_sweep() {
    let t = forge_world();
    seed(&t, "deck-1", "opening", 1, None);
    t.forge
        .queue_prs
        .borrow_mut()
        .push(("12".into(), "spira/queue/deck-1".into()));
    t.clock.now.set(900);
    t.lc.cas_lost.set(true);
    assert_eq!(
        t.run(&["batch", "sweep", "--repo", "spira"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out().contains(
            "batch deck-1 left forge.phase=opening while the sweep read it — left as it is"
        ),
        "{}",
        t.out()
    );
    assert_eq!(record(&t, "deck-1").1.get("forge.phase"), Some("opening"));

    t.lc.cas_lost.set(false);
    door(&t).lost_kind.set(Some("queue.round-fields"));
    assert_eq!(t.run(&["batch", "sweep", "--repo", "spira"]), 1);
    assert!(
        t.err()
            .contains("batch deck-1's adoption of PR 12 could not be written"),
        "{}",
        t.err()
    );
    assert!(!forge_calls(&t)
        .iter()
        .any(|c| c.starts_with("pr-close") || c.starts_with("pr-automerge")));
}

fn local_world() -> T {
    let t = T::new(LandMode::QueueLocal);
    t.lc.store.get_or_init(MemDoor::default);
    t.var("SPIRA_ROUND_LEASE_TTL_SECS", "300");
    t
}

#[test]
fn a_stale_opening_record_on_a_queue_local_repo_is_released_by_the_verdict() {
    let t = local_world();
    seed(&t, "stale", "opening", 900, None);
    t.clock.now.set(900 + 301);
    t.run(&["verdict", "spira"]);
    assert_eq!(record(&t, "stale").0, "ABANDONED", "{}{}", t.out(), t.err());
}

#[test]
fn an_opening_record_inside_one_lease_ttl_on_a_queue_local_repo_is_left_to_its_opener() {
    let t = local_world();
    seed(&t, "fresh", "opening", 900, None);
    t.clock.now.set(900 + 10);
    t.run(&["verdict", "spira"]);
    assert_eq!(record(&t, "fresh").0, "OPEN", "{}{}", t.out(), t.err());
}

#[test]
fn a_queue_local_verdict_releases_every_stale_opening_record_but_a_leased_one() {
    let t = local_world();
    seed(&t, "a-lost", "opening", 500, None);
    seed(&t, "b-leased", "opening", 500, None);
    seed(&t, "c-lost", "opening", 500, None);
    t.lc.leases.borrow_mut().insert(
        "spira/b-leased".into(),
        Ok(Some(RoundLease { holder: Some("rc-1".into()), token: "9-0".into(), ttl_left_secs: 60 })),
    );
    t.clock.now.set(900);
    t.run(&["verdict", "spira"]);
    let states: Vec<_> = ["a-lost", "b-leased", "c-lost"].iter().map(|i| record(&t, i).0).collect();
    assert_eq!(states, ["ABANDONED", "OPEN", "ABANDONED"], "{}{}", t.out(), t.err());
}

#[test]
fn a_queue_local_verdict_with_an_unparsable_lease_ttl_releases_nothing_and_says_which_command() {
    let t = local_world();
    t.var("SPIRA_ROUND_LEASE_TTL_SECS", "soon");
    seed(&t, "stale", "opening", 500, None);
    t.clock.now.set(900);
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert_eq!(record(&t, "stale").0, "OPEN");
    assert!(t.err().contains("queue.sh verdict: SPIRA_ROUND_LEASE_TTL_SECS does not parse"), "{}", t.err());
}

#[test]
fn a_queue_local_verdict_on_a_repo_with_no_batch_store_does_not_consult_the_forge_or_records() {
    let t = T::new(LandMode::QueueLocal);
    t.var("SPIRA_ROUND_LEASE_TTL_SECS", "soon");
    t.forge.queue_prs_unreadable.set(true);
    t.run(&["verdict", "spira"]);
    assert!(!t.err().contains("LEASE_TTL"), "{}", t.err());
    assert!(!t.err().contains("cannot list"), "{}", t.err());
    assert!(!t.lc.has("abandon-batch"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn settling_forge_leftovers_never_releases_a_stale_opening_record_whose_pr_the_forge_may_hold() {
    let t = local_world();
    seed(&t, "lost", "opening", 500, None);
    t.clock.now.set(900);
    t.with_world(|w| {
        let c = crate::ops::resolve(w, "test", None).unwrap();
        let path = c.r.path.clone().unwrap();
        crate::ops::verdict::settle_forge_leftovers(w, &c, &path);
    });
    assert!(!t.lc.has("abandon-batch lost"), "{:?}", t.lc.calls.borrow());
    assert_eq!(record(&t, "lost").0, "OPEN");
}
