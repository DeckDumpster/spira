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
    let mut kv = batches::batch(d, "spira", "spira-orphaned").unwrap().unwrap().kv;
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
        (open[0].batch.as_str(), open[0].phase.as_str(), open[0].head.as_str()),
        ("spira-open", "opened", "head-spira-open")
    );
    assert!(status.starts_with("rounds=1\nopen_batches=spira-open\nround=open\n"), "{status}");
    assert!(!t.qfile("round").exists());
}

#[test]
fn status_reads_the_same_from_the_store_as_from_the_round_file() {
    let record = "batch_id=spira-open\nrepo=spira\nhead=h1\nbase=b0\nmembers=sp-a:ta\nworktree=/wt/spira-open\nactor=overseer\nopened=900\nred=test-a.sh\n";
    let files = round_world();
    fs::write(files.qfile("round"), record).unwrap();
    files.lc.batch_states.borrow_mut().insert("spira-open".into(), ("ATTRIBUTING".into(), "4".into()));
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
                err.contains(&format!("the open round for spira is spira-open, not {batch}")),
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
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let rec = batches::batch(d, "spira", &batch).unwrap().unwrap();
    assert_eq!(
        (rec.state.as_str(), rec.kv.get("head"), rec.kv.get("base"), rec.kv.get("parent")),
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
        t.err().contains("a round is already open for spira (spira-open (phase opened))"),
        "{}",
        t.err()
    );
    t.lib.s.rounds_max = 2;
    t.git.ancestor("head-spira-open", "merged-tb");
    assert_eq!(t.run(&["round", "open", "--members", "sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let rec = batches::batch(t.lc.store.get().unwrap(), "spira", &batch).unwrap().unwrap();
    assert_eq!(
        (rec.kv.get("parent"), rec.kv.get("base")),
        (Some("spira-open"), Some("head-spira-open")),
        "{:?}",
        rec.kv
    );
    t.lc.store.get().unwrap().set_state("spira", &batch, "GREEN");
    assert_eq!(t.run(&["round", "land", &batch]), 1);
    assert!(
        t.err().contains(&format!("round {batch} is stacked on round spira-open, which has not landed")),
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
            err.contains("cannot tell which rounds are open for spira: cannot tell: lc-serve did not answer"),
            "{verb:?}: {err}"
        );
    }
    assert!(!t.lc.has("cut "), "nothing is cut on a store that cannot say what is open");
}

#[test]
fn with_the_store_eject_acts_on_the_open_round_and_keeps_its_fields_in_the_record() {
    let t = round_world();
    let d = t.lc.store.get_or_init(MemDoor::default);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red"]), 0, "{}", t.err());
    assert!(t.out().contains(&format!("ejected sp-b from round {batch}; 2 member(s) remain")), "{}", t.out());
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
    assert_eq!(t.run(&["round", "abandon", "spira-orphaned", "--reason", "orphaned"]), 0, "{}", t.err());
    assert!(
        fs::read_to_string(marker(&t, "spira-orphaned", "result")).unwrap().contains("abandoned: orphaned"),
        "the hand abandon finishes the round requeue-orphans ended"
    );
    assert_eq!(t.run(&["round", "abandon", "spira-landed", "--reason", "x"]), 1);
    assert!(t.err().contains("the open round for spira is spira-open, not spira-landed"), "{}", t.err());
    assert_eq!(t.run(&["round", "abandon", "spira-open", "--reason", "base moved"]), 0, "{}", t.err());
    assert_eq!(t.lc.store.get().unwrap().find("spira-open").unwrap().1, "ABANDONED");
    assert!(status(&t).contains("round=none"));
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
    assert_eq!(result, "ended: ABANDONED on the lifecycle; record swept\n");
    assert_eq!(t.run(&["round", "open", "--members", "sp-b:tb"]), 1);
    assert_eq!(fs::read_to_string(marker(&t, "spira-orphaned", "result")).unwrap(), result);
}

#[test]
fn with_the_store_stage_stages_behind_the_open_round_or_reports_none_open() {
    let t = round_world();
    let d = t.lc.store.get_or_init(MemDoor::default);
    d.put_round("spira", "spira-landed", "LANDED", &[("sp-y", "ty")], 1);
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 1);
    assert!(t.err().contains("no round is open for spira"), "{}", t.err());
    let t = store_world();
    t.git.ancestor("head-spira-open", "merged-tb");
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb", "--name", "spira-staged"]), 0, "{}", t.err());
    assert!(t.lc.has("stage spira-staged merged-tb head-spira-open sp-b:tb behind spira-open"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn with_the_store_publish_asks_only_the_open_round_whether_a_pass_holds_the_hypervisor() {
    let t = T::new(LandMode::QueueLocal);
    publishable(&t);
    *t.forge.pr.borrow_mut() = Some("78".into());
    let d = t.lc.store.get_or_init(MemDoor::default);
    d.put_round("spira", "b-1", "CI_RUNNING", &[("sp-a", "ta")], 1);
    for (b, s) in [("b-landed", "LANDED"), ("b-settled", "SETTLED"), ("b-orphaned", "ABANDONED")] {
        d.put_round("spira", b, "OPEN", &[], 2);
        d.set_state("spira", b, s);
    }
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(t.out().contains("a round pass holds the hypervisor"), "{}", t.out());
    d.set_state("spira", "b-1", "GREEN");
    assert_eq!(t.run(&["publish-settle"]), 0, "{}", t.err());
    assert!(
        t.forge.calls.borrow().iter().any(|c| c.starts_with("pr-create")),
        "ended rounds hold nothing: {:?}",
        t.forge.calls.borrow()
    );
}

#[test]
fn with_the_store_a_queue_forge_repos_batch_is_not_a_round() {
    let t = T::new(LandMode::Queue);
    let d = t.lc.store.get_or_init(MemDoor::default);
    d.put_round("spira", "spira-pr-batch", "CI_RUNNING", &[("sp-a", "ta")], 1);
    let status = status(&t);
    assert!(status.starts_with("round=none\nrepo=spira\n"), "{status}");
}
