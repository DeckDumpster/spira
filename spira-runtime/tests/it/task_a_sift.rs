//! Acceptance T1–T6 for `sift`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/sift.md`, Acceptance) on `TaskRig`. Red until the port
//! lands: `decide` and `record` are `unimplemented!`, and the row waits in `tasks::a::PENDING`.
//! T6 runs two virtual days (the contract asks seven).

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: sift,
    registration: Sift::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
}

/// The contract's idle fixture: no SUBMITTED or CERTIFIED rows in any repo.
fn idle() -> Snapshot {
    Snapshot { repos: [("spira".to_string(), Repo { fetched: true, identity_pages: Some(vec![]), ..Repo::default() })].into() }
}

fn row(id: &str, tip: &str) -> PoolRow {
    PoolRow { id: id.into(), state: "SUBMITTED".into(), version: 4, tip: tip.into(), ..PoolRow::default() }
}

fn clean(patch: &str) -> Candidate {
    Candidate { base: "b1".into(), patch_id: Some(patch.into()), ..Candidate::default() }
}

/// One clean submission at its branch tip.
fn busy() -> Snapshot {
    let repo = Repo {
        rows: vec![row("sp-a", "t1")],
        branch_tips: [("sp-a".to_string(), "t1".to_string())].into(),
        candidates: [("sp-a".to_string(), clean("p1"))].into(),
        items: [("sp-a".to_string(), "Outcome: x".to_string())].into(),
        ..idle().repos["spira"].clone()
    };
    Snapshot { repos: [("spira".to_string(), repo)].into() }
}

#[test]
fn t3_every_legal_move_of_the_sifted_machine_is_taken() {
    for (from, to) in MOVES {
        assert_eq!(record("sp-a", *from, *to), Ok(*to), "{from:?} → {to:?}");
    }
}

#[test]
fn t3_capped_to_sent_back_and_a_skipped_count_are_refused_naming_the_state() {
    let capped = record("sp-a", Sifted::Capped, Sifted::SentBack(1)).expect_err("capped never sends back");
    assert!(capped.to_lowercase().contains("capped"), "{capped}");
    let skipped = record("sp-a", Sifted::SentBack(1), Sifted::SentBack(3)).expect_err("a send-back counts one at a time");
    assert!(skipped.to_lowercase().contains("sent_back") || skipped.to_lowercase().contains("sentback"), "{skipped}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `rs:138-141`: an empty pool fetches nothing; the fetch follows the pool read.
#[test]
fn t5_an_empty_pool_fetches_nothing() {
    let (_, acts) = decide(&Carry::default(), &idle());
    assert!(acts.is_empty(), "{acts:?}");
}

/// `sl:133-262`: a clean candidate records a pass at its tip; one already sifted at that tip records none.
#[test]
fn t5_a_clean_candidate_records_one_pass_at_its_tip() {
    let (_, acts) = decide(&Carry::default(), &busy());
    assert_eq!(acts.iter().filter(|a| a.effect == RECORD_PASS).map(|a| a.key.as_str()).collect::<Vec<_>>(), ["sp-a:t1:Sifted"]);
    let mut done = busy();
    done.repos.get_mut("spira").unwrap().rows[0].sifted_tip = Some("t1".into());
    assert!(decide(&Carry::default(), &done).1.iter().all(|a| a.effect != RECORD_PASS));
}

/// `sl:158-170`: a duplicate patch-id supersedes, the keeper preferring the open-round member.
#[test]
fn t5_a_duplicate_patch_is_superseded_by_the_open_round_member() {
    let mut s = busy();
    let repo = s.repos.get_mut("spira").unwrap();
    repo.rows.push(row("sp-b", "t2"));
    repo.branch_tips.insert("sp-b".into(), "t2".into());
    repo.candidates.insert("sp-b".into(), clean("p1"));
    repo.open_round = vec!["sp-b:t2".into()];
    let (_, acts) = decide(&Carry::default(), &s);
    assert!(acts.iter().any(|a| a.effect == SUPERSEDE && a.key == "sp-a:superseded-by:sp-b"), "{acts:?}");
}

/// `sl:229-249`: a conflict sends back after its note and is never counted toward the cap.
#[test]
fn t5_a_conflict_is_noted_then_sent_back_and_not_counted() {
    let mut s = busy();
    s.repos.get_mut("spira").unwrap().candidates.get_mut("sp-a").unwrap().conflicts = vec!["src/lib.rs".into()];
    let (carry, acts) = decide(&Carry::default(), &s);
    let order: Vec<&str> = acts.iter().filter(|a| a.effect == NOTE || a.effect == SEND_BACK).map(|a| a.effect.as_str()).collect();
    assert_eq!(order, [NOTE, SEND_BACK], "the note precedes the send-back");
    assert_ne!(carry.beads.get("sp-a").map(|b| b.0), Some(Sifted::SentBack(1)), "a conflict is never counted");
}

/// A submission whose target names no outcome: sent back, and again at each tick until the cap.
fn without_a_target() -> Snapshot {
    let mut s = busy();
    s.repos.get_mut("spira").unwrap().items.insert("sp-a".into(), "Target: wall under 10 s".into());
    s
}

fn sent_back_twice() -> TaskRig {
    let mut r = rig(1, without_a_target());
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test]
fn the_sifted_machines_and_the_waiting_notes_are_kept_a_row_each() {
    let r = sent_back_twice();
    let rows = kit::assert_kept_as_rows(&r, NAME);
    assert!(rows.iter().any(|k| k == "beads/sp-a"), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_send_back_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(sent_back_twice, registration, STREAM, EventKind::new(TICK, ""));
}

#[test]
fn a_boot_does_not_start_the_send_back_count_again() {
    let mut r = sent_back_twice();
    r.reopen(registration()).unwrap();
    for _ in 0..6 {
        r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    }
    let sent = r.requested().iter().filter(|q| q.name == SEND_BACK).count();
    assert_eq!(sent, sift::SEND_BACK_CAP as usize, "send-backs across a restart: {sent}");
}
