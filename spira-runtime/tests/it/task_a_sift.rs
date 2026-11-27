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
    test_ids: [ #[test_id(txinj2e)] #[test_id(tbx3n40)] #[test_id(t5amf7d)] #[test_id(txnwxcn)] #[test_id(tk5kn95)] ],
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

#[test_id(t8qgbtc)]
#[test]
fn t3_every_legal_move_of_the_sifted_machine_is_taken() {
    for (from, to) in MOVES {
        assert_eq!(record("sp-a", *from, *to), Ok(*to), "{from:?} → {to:?}");
    }
}

#[test_id(tkxwefz)]
#[test]
fn t3_capped_to_sent_back_and_a_skipped_count_are_refused_naming_the_state() {
    let capped = record("sp-a", Sifted::Capped, Sifted::SentBack(1)).expect_err("capped never sends back");
    assert!(capped.to_lowercase().contains("capped"), "{capped}");
    let skipped = record("sp-a", Sifted::SentBack(1), Sifted::SentBack(3)).expect_err("a send-back counts one at a time");
    assert!(skipped.to_lowercase().contains("sent_back") || skipped.to_lowercase().contains("sentback"), "{skipped}");
}

#[test_id(t0rk2rb)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `rs:138-141`: an empty pool fetches nothing; the fetch follows the pool read.
#[test_id(timdnxw)]
#[test]
fn t5_an_empty_pool_fetches_nothing() {
    let (_, acts) = decide(&Carry::default(), &idle());
    assert!(acts.is_empty(), "{acts:?}");
}

/// `sl:133-262`: a clean candidate records a pass at its tip; one already sifted at that tip records none.
#[test_id(tpetem7)]
#[test]
fn t5_a_clean_candidate_records_one_pass_at_its_tip() {
    let (_, acts) = decide(&Carry::default(), &busy());
    assert_eq!(acts.iter().filter(|a| a.effect == RECORD_PASS).map(|a| a.key.as_str()).collect::<Vec<_>>(), ["sp-a:t1:Sifted"]);
    let mut done = busy();
    done.repos.get_mut("spira").unwrap().rows[0].sifted_tip = Some("t1".into());
    assert!(decide(&Carry::default(), &done).1.iter().all(|a| a.effect != RECORD_PASS));
}

/// `sl:158-170`: a duplicate patch-id supersedes, the keeper preferring the open-round member.
#[test_id(tvtsy8r)]
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
#[test_id(t1isows)]
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

/// The same submission, rebuilt at a new tip at every read: a send-back is made once per tip.
fn sent_back_twice() -> TaskRig {
    let reads = std::sync::atomic::AtomicUsize::new(0);
    let mut r = rig_with(1, move |_| {
        let tip = format!("t{}", reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let mut s = without_a_target();
        let repo = s.repos.get_mut("spira").unwrap();
        repo.rows[0].tip = tip.clone();
        repo.branch_tips.insert("sp-a".into(), tip);
        Ok(serde_json::to_string(&s).unwrap())
    });
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test_id(twfhnvk)]
#[test]
fn the_sifted_machines_and_the_waiting_notes_are_kept_a_row_each() {
    let r = sent_back_twice();
    let rows = kit::assert_kept_as_rows(&r, NAME);
    assert!(rows.iter().any(|k| k == "beads/sp-a"), "{rows:?}");
    assert!(rows.iter().any(|k| k == "sent/sp-a"), "{rows:?}");
}

#[test_id(tz575z8)]
#[test]
fn a_boot_repeats_no_send_back_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(sent_back_twice, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(trqpkfl)]
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

/// A CERTIFIED bead whose tip no longer merges onto the landing ref.
fn certified_conflict() -> Snapshot {
    let mut s = busy();
    let repo = s.repos.get_mut("spira").unwrap();
    repo.rows[0].state = "CERTIFIED".into();
    repo.candidates.get_mut("sp-a").unwrap().conflicts = vec!["src/lib.rs".into()];
    s
}

fn sending(world: Snapshot, send_back: Result<String, String>) -> TaskRig {
    let world = std::sync::Arc::new(std::sync::Mutex::new(world));
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(SEND_BACK, move |_| send_back.clone());
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, SEND_BACK].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r
}

fn asked(r: &TaskRig, effect: &str) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == effect).map(|q| q.input.clone()).collect()
}

fn told(r: &TaskRig, what: &str) -> Vec<String> {
    asked(r, INBOX_APPEND).into_iter().filter(|l| l.contains(what)).collect()
}

#[test_id(t2jv87n)]
#[test]
fn a_certified_conflict_is_sent_back_once_and_told_after_the_store_applied_it() {
    let mut r = sending(certified_conflict(), Ok("sent sp-a back".into()));
    let sends: Vec<_> = r.requested().iter().filter(|q| q.name == SEND_BACK).collect();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].key, "sp-a:t1:GateRed");
    assert!(sends[0].input.contains("no-rebase"), "{}", sends[0].input);
    assert_eq!(told(&r, "SIFT sent sp-a to REWORK").len(), 1);
    let order: Vec<String> = r.requested().iter().map(|q| q.name.clone()).collect();
    let (sb, line) = (order.iter().position(|n| n == SEND_BACK).unwrap(), order.iter().position(|n| n == INBOX_APPEND).unwrap());
    assert!(sb < line, "the line follows the send-back: {order:?}");
    let before = r.requested().len();
    r.advance(CADENCE_MS).unwrap();
    let later: Vec<String> = r.requested()[before..].iter().map(|q| q.name.clone()).filter(|n| [NOTE, SEND_BACK, INBOX_APPEND].contains(&n.as_str())).collect();
    assert!(later.is_empty(), "{later:?}");
}

#[test_id(tpzkqgp)]
#[test]
fn a_send_back_the_store_refused_is_reported_once_and_never_as_sent() {
    let mut r = sending(certified_conflict(), Err("sp-a moved since screening (state CERTIFIED)".into()));
    r.advance(3 * CADENCE_MS).unwrap();
    assert!(told(&r, "SIFT sent").is_empty(), "{:?}", asked(&r, INBOX_APPEND));
    let said = told(&r, "could not send");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("sp-a") && said[0].contains("t1") && said[0].contains("moved since screening"), "{said:?}");
    assert_eq!(asked(&r, SEND_BACK).len(), 1);
    assert_eq!(asked(&r, NOTE).len(), 1);
}

#[test_id(tl6jnfh)]
#[test]
fn a_failed_evidence_note_holds_the_send_back_and_says_so_once() {
    let world = std::sync::Arc::new(std::sync::Mutex::new(certified_conflict()));
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(NOTE, |_| Err("store down".into()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, NOTE].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(2 * CADENCE_MS).unwrap();
    assert!(asked(&r, SEND_BACK).is_empty());
    let said = told(&r, "SIFT held sp-a at t1: evidence note failed: store down");
    assert_eq!(said.len(), 1, "{:?}", asked(&r, INBOX_APPEND));
    assert_eq!(asked(&r, NOTE).len(), 1);
}

#[test_id(t2y5v3b)]
#[test]
fn a_moved_base_is_attempted_again_after_a_failed_send_back() {
    let world = std::sync::Arc::new(std::sync::Mutex::new(certified_conflict()));
    let w = world.clone();
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*w.lock().unwrap()).unwrap())).double(SEND_BACK, |_| Err("refused".into()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, SEND_BACK].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS).unwrap();
    assert_eq!((asked(&r, NOTE).len(), asked(&r, SEND_BACK).len()), (1, 1));
    world.lock().unwrap().repos.get_mut("spira").unwrap().candidates.get_mut("sp-a").unwrap().base = "b2".into();
    r.advance(CADENCE_MS).unwrap();
    assert_eq!((asked(&r, NOTE).len(), asked(&r, SEND_BACK).len()), (2, 2));
}

#[test_id(t6kkiiq)]
#[test]
fn an_attempt_is_forgotten_once_its_bead_leaves_the_pool() {
    let (carry, _) = decide(&Carry::default(), &certified_conflict());
    assert!(carry.sent.contains_key("sp-a"));
    let (carry, acts) = decide(&carry, &idle());
    assert!(carry.sent.is_empty(), "{:?}", carry.sent);
    assert!(acts.is_empty());
}

#[test_id(tfxhmh0)]
#[test]
fn a_send_back_lost_at_a_restart_is_not_reported_as_refused() {
    let mut r = TaskRig::new(1, registration()).unwrap().hanging(SEND_BACK).double(OBSERVE, |_| Ok(serde_json::to_string(&certified_conflict()).unwrap()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    assert_eq!(asked(&r, SEND_BACK).len(), 1, "{:?}", r.requested());
    r.reopen(registration()).unwrap();
    r.run_until_idle().unwrap();
    assert!(told(&r, "could not send").is_empty(), "{:?}", r.requested());
}
