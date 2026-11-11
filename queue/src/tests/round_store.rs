//! `queue round` with the batch machine on the control store: every verb reads the open rounds
//! from the store's `batches/<repo>/<id>` records, and an ended round answers none of them.

use super::round::{batch_of, marker, round_world};
use super::*;
use crate::batches::{self, mem::MemDoor};
use crate::records::Kv;

/// A store holding, oldest first, the OPEN round `spira-open` (sp-a) and three rounds it ended:
/// one the reconciler's requeue-orphans abandoned, one LANDED and one SETTLED.
fn store_world() -> T {
    let t = round_world();
    let d = t.lc.store.get_or_init(MemDoor::default);
    d.put_round("spira", "spira-open", "OPEN", &[("sp-a", "ta")], 10);
    d.put_round("spira", "spira-orphaned", "OPEN", &[("sp-x", "tx")], 20);
    let mut kv = batches::batch(d, "spira", "spira-orphaned")
        .unwrap()
        .unwrap()
        .kv;
    kv.push("actor", "reconciler");
    batches::save(d, "spira", &kv).unwrap();
    d.set_state("spira", "spira-orphaned", "ABANDONED");
    d.put_round("spira", "spira-landed", "LANDED", &[("sp-y", "ty")], 30);
    d.put_round("spira", "spira-settled", "SETTLED", &[("sp-z", "tz")], 40);
    t
}

const ENDED: [&str; 3] = ["spira-orphaned", "spira-landed", "spira-settled"];

fn status(t: &T) -> String {
    let from = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    t.out()[from..].to_string()
}

#[test]
fn with_the_store_status_names_only_the_open_round_and_parses_unchanged() {
    let t = store_world();
    let status = status(&t);
    let open = release::round_recert::open_rounds(&status);
    assert_eq!(open.len(), 1, "{status}");
    assert_eq!(
        (
            open[0].batch.as_str(),
            open[0].phase.as_str(),
            open[0].head.as_str()
        ),
        ("spira-open", "opened", "head-spira-open")
    );
    assert!(
        status.starts_with("rounds=1\nopen_batches=spira-open\nround=open\n"),
        "{status}"
    );
    assert!(!t.qfile("round").exists());
}

/// A store imported from an older one holds every round the repo ever had, ended, in the
/// spelling spira-lc's `BatchRecord` writes (`Landed`, `Abandoned`). Status names the one open
/// round, asks the lifecycle for its phase once, and reads O(1) round records — never the
/// history (law-list-reads-run-on-indexed-tables, law-list-reads-paginate-in-the-database).
#[test]
fn with_a_thousand_ended_rounds_status_reads_only_the_open_one() {
    let t = round_world();
    let d = t.lc.store.get_or_init(MemDoor::default);
    for n in 0..1000 {
        let state = if n % 3 == 0 { "ABANDONED" } else { "LANDED" };
        d.put_round(
            "spira",
            &format!("r-auto-{n}"),
            state,
            &[("sp-old", "to")],
            n,
        );
    }
    d.put_round(
        "spira",
        "spira-open",
        "CI_RUNNING",
        &[("sp-a", "ta"), ("sp-b", "tb")],
        5000,
    );
    let stored = d
        .store
        .get("batches/spira/r-auto-0")
        .unwrap()
        .unwrap()
        .value
        .data["state"]
        .clone();
    assert_eq!(
        stored, "Abandoned",
        "the fixture writes what spira-lc writes"
    );
    d.items_read.set(0);
    let shows_before =
        t.lc.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("show-batch"))
            .count();
    let status = status(&t);
    assert!(
        status.starts_with("rounds=1\nopen_batches=spira-open\nround=open\nphase=certifying\n"),
        "{}",
        &status[..status.len().min(400)]
    );
    assert!(status.contains("\nmembers=sp-a:ta sp-b:tb\n"), "{status}");
    assert!(
        !status.contains("r-auto-"),
        "an ended round is never listed as open"
    );
    let read = d.items_read.get();
    assert!(
        read <= 4,
        "status read {read} round records for one open round"
    );
    let shows =
        t.lc.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("show-batch"))
            .count()
            - shows_before;
    assert!(shows <= 1, "status asked the lifecycle {shows} times");
}

#[test]
fn status_reads_the_same_from_the_store_as_from_the_round_file() {
    let record = "batch_id=spira-open\nrepo=spira\nhead=h1\nbase=b0\nmembers=sp-a:ta\nworktree=/wt/spira-open\nactor=overseer\nopened=900\nred=test-a.sh\n";
    let files = round_world();
    fs::write(files.qfile("round"), record).unwrap();
    files
        .lc
        .batch_states
        .borrow_mut()
        .insert("spira-open".into(), ("ATTRIBUTING".into(), "4".into()));
    let store = round_world();
    let d = store.lc.store.get_or_init(MemDoor::default);
    d.put_round("spira", "spira-open", "ATTRIBUTING", &[("sp-a", "ta")], 10);
    batches::save(d, "spira", &Kv::parse(record)).unwrap();
    assert_eq!(status(&store), status(&files));
}

#[test]
fn with_the_store_a_verb_naming_an_ended_round_is_refused_naming_only_the_open_one() {
    let t = store_world();
    for batch in ENDED {
        for verb in [
            vec!["round", "certify", batch],
            vec!["round", "eject", batch, "sp-a", "--reason", "red"],
            vec!["round", "land", batch],
        ] {
            let from = t.io.err.borrow().len();
            assert_eq!(t.run(&verb), 1, "{verb:?}");
            let err = t.err()[from..].to_string();
            assert!(
                err.contains(&format!(
                    "the open round for spira is spira-open, not {batch}"
                )),
                "{verb:?}: {err}"
            );
        }
    }
    assert!(!t.lc.has("land "), "{:?}", t.lc.calls.borrow());
}

#[test]
fn with_the_store_open_certify_and_land_run_the_round_the_whole_way_with_no_round_file() {
    let t = round_world();
    let d = t.lc.store.get_or_init(MemDoor::default);
    for (n, ended) in ENDED.iter().enumerate() {
        d.put_round("spira", ended, "OPEN", &[("sp-x", "tx")], n as i64);
        d.set_state("spira", ended, ["ABANDONED", "LANDED", "SETTLED"][n]);
    }
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let rec = batches::batch(d, "spira", &batch).unwrap().unwrap();
    assert_eq!(
        (
            rec.state.as_str(),
            rec.kv.get("head"),
            rec.kv.get("base"),
            rec.kv.get("parent")
        ),
        ("OPEN", Some("merged-tb"), Some("b0"), None),
        "no ended round is a parent: {:?}",
        rec.kv
    );
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert_eq!(d.find(&batch).unwrap().1, "GREEN");
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
    assert_eq!(d.find(&batch).unwrap().1, "LANDED");
    assert!(status(&t).contains("round=none"));
    assert!(!t.qfile("round").exists() && !t.qfile(&format!("round.{batch}")).exists());
}

#[test]
fn with_the_store_no_ended_round_counts_toward_the_limit_or_is_a_parent() {
    let mut t = store_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-b:tb"]), 1);
    assert!(
        t.err()
            .contains("a round is already open for spira (spira-open (phase opened))"),
        "{}",
        t.err()
    );
    t.lib.s.rounds_max = 2;
    t.git.ancestor("head-spira-open", "merged-tb");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let rec = batches::batch(t.lc.store.get().unwrap(), "spira", &batch)
        .unwrap()
        .unwrap();
    assert_eq!(
        (rec.kv.get("parent"), rec.kv.get("base")),
        (Some("spira-open"), Some("head-spira-open")),
        "{:?}",
        rec.kv
    );
    t.lc.store
        .get()
        .unwrap()
        .set_state("spira", &batch, "GREEN");
    assert_eq!(t.run(&["round", "land", &batch]), crate::ops::LAND_STACKED);
    assert!(
        t.err().contains(&format!(
            "round {batch} is stacked on round spira-open, which has not landed"
        )),
        "{}",
        t.err()
    );
}

#[test]
fn with_the_store_unreadable_every_verb_refuses_saying_it_cannot_tell() {
    let t = store_world();
    t.lc.store.get().unwrap().down.set(true);
    for verb in [
        vec!["round", "status"],
        vec!["round", "open", "--members", "sp-b:tb"],
        vec!["round", "certify", "spira-open"],
        vec!["round", "stage", "--members", "sp-b:tb"],
    ] {
        let from = t.io.err.borrow().len();
        assert_eq!(t.run(&verb), 1, "{verb:?}");
        let err = t.err()[from..].to_string();
        assert!(
            err.contains(
                "cannot tell which rounds are open for spira: cannot tell: lc-serve did not answer"
            ),
            "{verb:?}: {err}"
        );
    }
    assert!(
        !t.lc.has("cut "),
        "nothing is cut on a store that cannot say what is open"
    );
}

#[test]
fn with_the_store_eject_acts_on_the_open_round_and_keeps_its_fields_in_the_record() {
    let t = round_world();
    let d = t.lc.store.get_or_init(MemDoor::default);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out().contains(&format!(
            "ejected sp-b from round {batch}; 2 member(s) remain"
        )),
        "{}",
        t.out()
    );
    let rec = batches::batch(d, "spira", &batch).unwrap().unwrap();
    assert_eq!(
        (rec.kv.get("members"), rec.kv.get("ejected")),
        (Some("sp-a:ta sp-c:tc"), Some("sp-b ")),
        "{:?}",
        rec.kv
    );
}

#[test]
fn with_the_store_abandon_closes_the_open_round_and_finishes_one_the_reconciler_abandoned() {
    let t = store_world();
    assert_eq!(
        t.run(&["round", "abandon", "spira-orphaned", "--reason", "orphaned"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        fs::read_to_string(marker(&t, "spira-orphaned", "result"))
            .unwrap()
            .contains(" abandoned:hand orphaned"),
        "the hand abandon finishes the round requeue-orphans ended"
    );
    assert_eq!(
        t.run(&["round", "abandon", "spira-landed", "--reason", "x"]),
        1
    );
    assert!(
        t.err()
            .contains("the open round for spira is spira-open, not spira-landed"),
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "abandon", "spira-open", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.lc.store.get().unwrap().find("spira-open").unwrap().1,
        "ABANDONED"
    );
    assert!(status(&t).contains("round=none"));
}

#[test]
fn with_the_store_a_second_abandon_of_a_closed_round_writes_no_second_audit_line() {
    let t = store_world();
    for reason in ["first", "second"] {
        assert_eq!(
            t.run(&["round", "abandon", "spira-orphaned", "--reason", reason]),
            0,
            "{}",
            t.err()
        );
    }
    assert_eq!(t.landing_log().matches("ROUND-ABANDON").count(), 1, "{}", t.landing_log());
}

#[test]
fn with_the_store_a_round_the_lifecycle_ended_is_swept_once_under_the_lock() {
    let t = store_world();
    let running = marker(&t, "spira-orphaned", "running");
    fs::create_dir_all(running.parent().unwrap()).unwrap();
    fs::write(&running, "").unwrap();
    assert_eq!(t.run(&["round", "open", "--members", "sp-b:tb"]), 1);
    assert!(!running.exists());
    let result = fs::read_to_string(marker(&t, "spira-orphaned", "result")).unwrap();
    assert!(
        result.ends_with(" abandoned:swept ended: ABANDONED on the lifecycle; record swept\n"),
        "{result}"
    );
    assert_eq!(t.run(&["round", "open", "--members", "sp-b:tb"]), 1);
    assert_eq!(
        fs::read_to_string(marker(&t, "spira-orphaned", "result")).unwrap(),
        result
    );
}

#[test]
fn with_the_store_stage_stages_behind_the_open_round_or_reports_none_open() {
    let t = round_world();
    let d = t.lc.store.get_or_init(MemDoor::default);
    d.put_round("spira", "spira-landed", "LANDED", &[("sp-y", "ty")], 1);
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 1);
    assert!(
        t.err().contains("no round is open for spira"),
        "{}",
        t.err()
    );
    let t = store_world();
    t.git.ancestor("head-spira-open", "merged-tb");
    assert_eq!(
        t.run(&[
            "round",
            "stage",
            "--members",
            "sp-b:tb",
            "--name",
            "spira-staged"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has("stage spira-staged merged-tb head-spira-open sp-b:tb behind spira-open"),
        "{:?}",
        t.lc.calls.borrow()
    );
}

#[test]
fn with_the_store_publish_asks_only_the_open_round_whether_a_pass_holds_the_hypervisor() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    *t.forge.pr.borrow_mut() = Some("78".into());
    let d = t.lc.store.get_or_init(MemDoor::default);
    d.put_round("spira", "b-1", "CI_RUNNING", &[("sp-a", "ta")], 1);
    for (b, s) in [
        ("b-landed", "LANDED"),
        ("b-settled", "SETTLED"),
        ("b-orphaned", "ABANDONED"),
    ] {
        d.put_round("spira", b, "OPEN", &[], 2);
        d.set_state("spira", b, s);
    }
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.out().contains("a round pass holds the hypervisor"),
        "{}",
        t.out()
    );
    d.set_state("spira", "b-1", "GREEN");
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.forge
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("pr-create")),
        "ended rounds hold nothing: {:?}",
        t.forge.calls.borrow()
    );
}

#[test]
fn with_the_store_a_queue_forge_repos_batch_is_not_a_round() {
    let t = T::new(LandMode::Queue);
    let d = t.lc.store.get_or_init(MemDoor::default);
    d.put_round(
        "spira",
        "spira-pr-batch",
        "CI_RUNNING",
        &[("sp-a", "ta")],
        1,
    );
    let status = status(&t);
    assert!(status.starts_with("round=none\nrepo=spira\n"), "{status}");
}

/// A store round `name` opened through `round open` (members `members`).
fn opened(t: &T, name: &str, members: &str) -> String {
    t.lc.store.get_or_init(MemDoor::default);
    assert_eq!(
        t.run(&["round", "open", "--members", members, "--name", name]),
        0,
        "{}",
        t.err()
    );
    name.to_string()
}

#[test]
fn with_the_store_an_emptied_round_whose_abandon_is_refused_is_ended_visibly_and_abandoned_later() {
    let t = round_world();
    let batch = opened(&t, "spira-1", "sp-a:ta");
    t.lc.abandon_refused.set(Some("refused: StaleVersion"));
    t.run(&["round", "eject", &batch, "sp-a", "--reason", "red"]);
    let d = t.lc.store.get().unwrap();
    assert_eq!(
        d.find(&batch).unwrap().1,
        "OPEN",
        "the lifecycle refused the end"
    );
    let from = t.io.err.borrow().len();
    let status = status(&t);
    assert!(
        status.starts_with("round=none\n"),
        "an ended round is no open round: {status}"
    );
    assert!(
        t.err()[from..].contains(&format!(
            "round {batch} ended (emptied) but spira-lc holds it OPEN"
        )),
        "{}",
        t.err()
    );
    t.lc.abandon_refused.set(None);
    let next = opened(&t, "spira-2", "sp-b:tb");
    let rec = batches::batch(d, "spira", &next).unwrap().unwrap();
    assert_eq!(
        rec.kv.get("parent"),
        None,
        "an ended round is no parent: {:?}",
        rec.kv
    );
    assert_eq!(
        d.find(&batch).unwrap().1,
        "ABANDONED",
        "round open retried the abandon"
    );
}

#[test]
fn with_the_store_a_round_whose_landing_record_is_refused_is_ended_and_recorded_at_the_next_open() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    let batch = opened(&t, "spira-1", "sp-a:ta,sp-b");
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    t.lc.land_refused.set(Some("store unreachable"));
    assert_eq!(t.run(&["round", "land", &batch]), 1);
    let d = t.lc.store.get().unwrap();
    assert_eq!(
        d.find(&batch).unwrap().1,
        "GREEN",
        "the landing record was refused"
    );
    assert!(
        status(&t).starts_with("round=none\n"),
        "a landed round is no open round"
    );
    t.lc.land_refused.set(None);
    // The fake git compares names; the landed head is on the landing ref.
    t.git.ancestor("merged-tb", "local/main");
    let next = opened(&t, "spira-2", "sp-c:tc");
    let rec = batches::batch(d, "spira", &next).unwrap().unwrap();
    assert_eq!(
        rec.kv.get("parent"),
        None,
        "a landed round is no parent: {:?}",
        rec.kv
    );
    assert_eq!(
        d.find(&batch).unwrap().1,
        "LANDED",
        "round open recorded the landing"
    );
}

#[test]
fn with_the_store_the_round_fields_ride_in_the_lifecycle_events_not_a_write_of_their_own() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() =
        vec![("test-b.sh".into(), "red 1 2 fp p e 1".into())];
    let d = t.lc.store.get_or_init(MemDoor::default);
    d.lost_kind.set(Some("queue.round-fields"));
    let batch = opened(&t, "spira-1", "sp-a:ta,sp-b:tb");
    let rec = batches::batch(d, "spira", &batch).unwrap().unwrap();
    assert!(
        rec.kv.get("worktree").is_some_and(|w| !w.is_empty()) && rec.kv.get("actor").is_some(),
        "the cut carried the worktree and actor: {:?}",
        rec.kv
    );
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    let rec = batches::batch(d, "spira", &batch).unwrap().unwrap();
    assert_eq!(rec.state, "ATTRIBUTING");
    assert_eq!(
        rec.kv.get("red"),
        Some("test-b.sh"),
        "the red verdict carried the red: {:?}",
        rec.kv
    );
    assert!(
        !d.kinds.borrow().iter().any(|k| k == "queue.round-fields"),
        "{:?}",
        d.kinds.borrow()
    );
}

#[test]
fn with_the_store_unreadable_a_passes_members_are_an_error_not_none() {
    let t = store_world();
    let d = t.lc.store.get().unwrap();
    t.with_world(|w| {
        let c = crate::ops::resolve(w, "certify", Some("spira")).unwrap();
        let ids = crate::ops::round::pass_members(w, &c, "spira-open");
        assert_eq!(ids, Ok(vec!["sp-a".to_string()]));
        d.down.set(true);
        let e = crate::ops::round::pass_members(w, &c, "spira-open").unwrap_err();
        assert!(e.contains("cannot tell"), "{e}");
    });
}

/// sp-av82ny: every round on a repo opens at `.round-<repo>` by default, so an abandoned round's
/// record names the very tree the next round stands in. Its sweep under certify's lock never
/// removes a tree an unended round records.
#[test]
fn with_the_store_an_abandoned_rounds_sweep_spares_the_next_rounds_tree_at_the_same_path() {
    let t = round_world();
    let d = t.lc.store.get_or_init(MemDoor::default);
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    let first = opened(&t, "spira-stale", "sp-a:ta");
    assert_eq!(
        t.run(&["round", "abandon", &first, "--reason", "stale"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(d.find(&first).unwrap().1, "ABANDONED");
    let second = opened(&t, "spira-live", "sp-b:tb");
    let tree = |b: &str| {
        batches::batch(d, "spira", b)
            .unwrap()
            .unwrap()
            .kv
            .get("worktree")
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(
        tree(&first),
        tree(&second),
        "both rounds took the default path"
    );
    let wt = PathBuf::from(tree(&second));
    fs::create_dir_all(&wt).unwrap();
    t.git.calls.borrow_mut().clear();
    t.run(&["round", "certify", &second]);
    let removes: Vec<String> = t
        .git
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("worktree-remove"))
        .cloned()
        .collect();
    assert!(
        !removes.contains(&format!("worktree-remove {}", wt.display())),
        "the abandoned round's sweep removed the live round's tree: {removes:?}\n{}",
        t.err()
    );
}

#[test]
fn open_whose_cut_reply_is_lost_never_leaves_a_round_without_its_worktree() {
    let t = round_world();
    t.lc.store.get_or_init(MemDoor::default);
    t.git.materialise.set(true);
    t.lc.cut_reply_lost.set(true);
    let rc = t.run(&["round", "open", "--members", "sp-a,sp-b"]);
    let d = t.lc.store.get().unwrap();
    let held = batches::open_batches(d, "spira").unwrap();
    assert!(
        !t.err().contains("cut refused"),
        "a lost reply is not a refusal: {}",
        t.err()
    );
    assert_eq!(
        held.len(),
        1,
        "the cut wrote the round and open reports it: {}",
        t.err()
    );
    assert_eq!(rc, 0, "a round that exists is reported opened: {}", t.err());
    let wt = std::path::PathBuf::from(held[0].kv.get("worktree").unwrap_or(""));
    assert!(
        wt.exists(),
        "round {} is recorded and its worktree is gone",
        held[0].id
    );
    assert_eq!(
        held[0].kv.get("head"),
        t.out().lines().find_map(|l| l.strip_prefix("head="))
    );
}

#[test]
fn the_sweep_reclaims_an_open_round_whose_worktree_is_missing() {
    let t = round_world();
    let d = t.lc.store.get_or_init(MemDoor::default);
    d.put_round("spira", "spira-lost", "OPEN", &[("sp-a", "ta")], 10);
    let mut kv = batches::batch(d, "spira", "spira-lost")
        .unwrap()
        .unwrap()
        .kv;
    kv.push("worktree", "/wt/spira-lost");
    batches::save(d, "spira", &kv).unwrap();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b"]),
        0,
        "{}",
        t.err()
    );
    let (state, _) = t.lc.batch_state("spira-lost").unwrap();
    assert_eq!(state, "ABANDONED", "{}", t.err());
}

#[test]
fn the_sweep_stops_the_legs_of_a_round_the_lifecycle_ended_before_it_frees_the_tree() {
    let t = store_world();
    let d = t.lc.store.get().unwrap();
    let mut kv = batches::batch(d, "spira", "spira-orphaned").unwrap().unwrap().kv;
    let wt = t.dir.path().join("orphan-tree");
    fs::create_dir_all(&wt).unwrap();
    kv.push("worktree", &wt.display().to_string());
    batches::save(d, "spira", &kv).unwrap();
    t.scripts.round_legs.borrow_mut().push(("spira".into(), "spira-orphaned".into(), "leg.service".into()));
    t.run(&["round", "open", "--members", "sp-b:tb"]);
    assert!(
        !t.scripts.round_legs_live("spira", "spira-orphaned"),
        "a leg of an ended round still holds its tree: {}",
        t.err()
    );
}
