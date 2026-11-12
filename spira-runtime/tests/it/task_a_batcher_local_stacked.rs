//! sp-s55moh: with `SPIRA_ROUND_MAX_OPEN` above one, batcher.local keeps that many rounds of a
//! repo in flight: a further round is cut on the head of the open one, lands only after it, and
//! is cut again from the pool when the round below it closes without landing.

use std::collections::BTreeMap;

use runtime::units::{Exit, RanOutput, UnitHandle, UnitRan};
use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_793_000_000_000;

fn policy(max_open: usize) -> Policy {
    Policy::pinned(&BTreeMap::from([("SPIRA_ROUND_MAX_OPEN".to_string(), max_open.to_string())]))
}

fn member(id: &str, tip: &str, priority: u8) -> Member {
    Member { id: id.into(), state: "SUBMITTED".into(), version: 5, tip: tip.into(), sifted_tip: Some(tip.into()), title: format!("{id} work"), priority, ..Member::default() }
}

/// Four sifted submissions and a round size of two: one round does not take the pool.
fn pool() -> Vec<Member> {
    vec![member("sp-a", "ta", 1), member("sp-b", "tb", 1), member("sp-c", "tc", 2), member("sp-d", "td", 2)]
}

fn snap(open: Vec<Round>, now_ms: u64) -> Snapshot {
    let pool = pool();
    let branch_tips = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    let mut open = open.into_iter();
    let repo = Repo { pool, branch_tips, base_sha: "b1".into(), open_round: open.next(), open_above: open.collect(), ..Repo::default() };
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms }
}

fn round(id: &str, members: &[&str], head: &str, age_secs: u64) -> Round {
    Round { id: id.into(), members: members.iter().map(|m| m.to_string()).collect(), head: head.into(), worktree: format!("/w/{id}"), driven: true, red: false, age_secs, unfit: None, phase: String::new() }
}

fn names(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

fn leg_of(acts: &[Act], effect: &str) -> Leg {
    serde_json::from_str(&acts.iter().find(|a| a.effect == effect).unwrap_or_else(|| panic!("no {effect}: {acts:?}")).input).unwrap()
}

fn ids(members: &[(String, String)]) -> Vec<&str> {
    members.iter().map(|(id, _)| id.as_str()).collect()
}

fn small(max_open: usize) -> Policy {
    Policy { max_members: 2, ..policy(max_open) }
}

/// A repo cut, opened and under its corpus leg, round by round, as the task's own events drive it.
fn cut_and_open(p: &Policy, carry: &Carry, snapshot: &Snapshot, batch: &str) -> (Carry, Vec<Act>) {
    let (carry, acts) = decide_with(p, carry, snapshot);
    let cut = leg_of(&acts, ASSEMBLE);
    let assembled = Assembled { merged: cut.members.clone(), wt: format!("/w/{batch}"), base: "b1".into(), suites: true, ..Assembled::default() };
    let (carry, acts) = on_outcome(&carry, ASSEMBLE, true, &serde_json::to_string(&assembled).unwrap(), &cut, snapshot.now_ms);
    let open = leg_of(&acts, ROUND_OPEN);
    let opened = Opened { batch: batch.into(), ..Opened::default() };
    on_outcome(&carry, ROUND_OPEN, true, &serde_json::to_string(&opened).unwrap(), &open, snapshot.now_ms)
}

fn two_corpus_rounds() -> Carry {
    let p = small(2);
    let (carry, acts) = cut_and_open(&p, &Carry::default(), &snap(vec![], NOW), "r1");
    assert_eq!(names(&acts), [CORPUS], "{acts:?}");
    let (carry, acts) = cut_and_open(&p, &carry, &snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 60)], NOW + 60_000), "r2");
    assert_eq!(names(&acts), [CORPUS], "{acts:?}");
    carry
}

fn certifying_two() -> Carry {
    let mut carry = two_corpus_rounds();
    let rc = carry.repos.get_mut("spira").unwrap();
    rc.state = State::Certifying;
    rc.leg = Some("spira.lower.certify1".into());
    rc.above[0].state = State::Certifying;
    rc.above[0].leg = Some("spira.upper.certify1".into());
    carry
}

fn ran(carry: &Carry, leg: &str, code: i32, out: &str) -> (Carry, Vec<Act>) {
    let ran = UnitRan { handle: UnitHandle::of(NAME, leg).unwrap(), exit: Exit::Code(code), duration_ms: 1 };
    on_ran_with(carry, &ran, &RanOutput::captured(out.as_bytes(), b""), NOW + 120_000).expect("a carried leg's end moves the machine")
}

#[test]
fn two_rounds_are_in_flight_at_once_the_second_cut_on_the_head_of_the_first() {
    let carry = two_corpus_rounds();
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref(), ids(&rc.members)), (State::Corpus, Some("r1"), ["sp-a", "sp-b"].to_vec()));
    assert_eq!((rc.above.len(), rc.above[0].state, rc.above[0].batch.as_deref(), ids(&rc.above[0].members)), (1, State::Corpus, Some("r2"), ["sp-c", "sp-d"].to_vec()));
    assert_ne!(rc.leg, rc.above[0].leg, "each round runs its own leg");
    assert_ne!(rc.wt, rc.above[0].wt, "each round keeps its own tree");
}

#[test]
fn the_second_round_is_assembled_on_the_open_rounds_head_in_a_tree_of_its_own() {
    let p = small(2);
    let (carry, _) = cut_and_open(&p, &Carry::default(), &snap(vec![], NOW), "r1");
    let (_, acts) = decide_with(&p, &carry, &snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 60)], NOW + 60_000));
    assert_eq!(names(&acts), [FETCH, ASSEMBLE], "{acts:?}");
    let cut = leg_of(&acts, ASSEMBLE);
    assert_eq!((cut.onto.as_str(), ids(&cut.members)), ("h1", ["sp-c", "sp-d"].to_vec()), "the pool minus the open round's members, on its head");
}

#[test]
fn with_one_round_open_allowed_a_running_round_cuts_nothing_more() {
    let p = small(1);
    let (carry, _) = cut_and_open(&p, &Carry::default(), &snap(vec![], NOW), "r1");
    let (next, acts) = decide_with(&p, &carry, &snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 60)], NOW + 60_000));
    assert!(acts.is_empty(), "{acts:?}");
    let mut seen = carry.clone();
    let rc = seen.repos.get_mut("spira").unwrap();
    (rc.head, rc.leg_head) = ("h1".into(), "h1".into());
    assert_eq!(next, seen, "only the head the round was first seen at is learned");
    let (_, acts) = decide_with(&Policy { max_members: 2, ..Policy::default() }, &carry, &snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 60)], NOW + 60_000));
    assert!(acts.is_empty(), "the default is one round: {acts:?}");
}

#[test]
fn a_third_round_is_not_cut_past_the_limit() {
    let carry = two_corpus_rounds();
    let open = vec![round("r1", &["sp-a", "sp-b"], "h1", 120), round("r2", &["sp-c", "sp-d"], "h2", 60)];
    let (_, acts) = decide_with(&small(2), &carry, &snap(open, NOW + 120_000));
    assert!(acts.is_empty(), "{acts:?}");
}

#[test]
fn the_upper_round_lands_only_after_the_lower() {
    let carry = certifying_two();
    let (carry, acts) = ran(&carry, "spira.upper.certify1", 0, "");
    assert!(acts.is_empty(), "the upper's green waits for the lower: {acts:?}");
    assert_eq!(carry.repos["spira"].above[0].state, State::Certified);

    let (carry, acts) = ran(&carry, "spira.lower.certify1", 0, "");
    assert_eq!(names(&acts), [LC_CERTIFY, LAND], "{acts:?}");
    assert_eq!(leg_of(&acts, LAND).batch, "r1");

    let landed = leg_of(&acts, LAND);
    let (carry, acts) = on_outcome(&carry, LAND, true, &serde_json::to_string(&Landed::default()).unwrap(), &landed, NOW + 130_000);
    assert!(acts.is_empty(), "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref(), rc.above.len()), (State::Certified, Some("r2"), 0), "the upper is now the lowest round");

    let open = vec![round("r2", &["sp-c", "sp-d"], "h2", 70)];
    let (carry, acts) = decide_with(&small(2), &carry, &snap(open, NOW + 140_000));
    assert_eq!(names(&acts), [LC_CERTIFY, LAND], "{acts:?}");
    assert_eq!(leg_of(&acts, LAND).batch, "r2");
    assert_eq!(carry.repos["spira"].state, State::Landing);
}

#[test]
fn a_green_lower_with_the_upper_still_certifying_lands_at_once() {
    let (_, acts) = ran(&certifying_two(), "spira.lower.certify1", 0, "");
    assert_eq!(names(&acts), [LC_CERTIFY, LAND], "{acts:?}");
}

#[test]
fn a_red_lower_round_leaves_the_upper_cut_again_on_the_new_base() {
    let mut carry = certifying_two();
    carry.repos.get_mut("spira").unwrap().attributed = true;
    let (carry, acts) = ran(&carry, "spira.lower.certify1", 1, "queue.sh round certify: round r1 is RED at abc: test-a.sh\nred=test-a.sh\n");
    assert!(names(&acts).contains(&ABANDON), "{acts:?}");
    assert_eq!(leg_of(&acts, ABANDON).batch, "r1");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref()), (State::Certifying, Some("r2")), "the upper stands alone until the queue's void is seen");

    let mut gone = snap(vec![], NOW + 300_000);
    gone.repos.get_mut("spira").unwrap().base_sha = "b2".into();
    let (carry, acts) = decide_with(&small(2), &carry, &gone);
    assert!(acts.iter().all(|a| a.effect != ABANDON), "a voided round is not abandoned again: {acts:?}");
    let cut = leg_of(&acts, ASSEMBLE);
    assert_eq!((cut.base.as_str(), cut.onto.as_str(), ids(&cut.members)), ("b2", "", ["sp-a", "sp-b"].to_vec()), "cut again from the pool on the landing ref");
    assert_eq!(carry.repos["spira"].state, State::Assembling);
    assert!(!carry.repos["spira"].stacked && carry.repos["spira"].above.is_empty());
}

#[test]
fn a_stacked_round_just_opened_is_not_voided_by_a_snapshot_that_predates_it() {
    let carry = two_corpus_rounds();
    let open = vec![round("r1", &["sp-a", "sp-b"], "h1", 60)];
    let (next, acts) = decide_with(&small(2), &carry, &snap(open, NOW + 61_000));
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(next.repos["spira"].above.len(), 1);
}

#[test]
fn a_round_the_queue_holds_that_this_task_does_not_drive_blocks_a_second_cut() {
    let (_, acts) = decide_with(&small(2), &Carry::default(), &snap(vec![round("rx", &["sp-a"], "hx", 10)], NOW));
    assert!(!names(&acts).contains(&ASSEMBLE), "{acts:?}");
}

#[test]
fn a_red_open_round_is_not_stacked_on() {
    let p = small(2);
    let (carry, _) = cut_and_open(&p, &Carry::default(), &snap(vec![], NOW), "r1");
    let red = Round { red: true, ..round("r1", &["sp-a", "sp-b"], "h1", 60) };
    let (_, acts) = decide_with(&p, &carry, &snap(vec![red], NOW + 60_000));
    assert!(acts.is_empty(), "{acts:?}");
}

#[test]
fn a_promotion_moves_the_round_that_landed_not_the_one_above_it() {
    let mut carry = certifying_two();
    let rc = carry.repos.get_mut("spira").unwrap();
    rc.state = State::Promoting;
    rc.round_key = Some("lower-key".into());
    rc.above[0].round_key = Some("upper-key".into());
    let leg = Leg { batch: "r1-staged".into(), key: "r1-staged".into(), round_key: "lower-key".into(), ..Leg::on("spira") };
    let promoted = Promoted { batch: "r1-staged".into(), members: vec![("sp-e".into(), "te".into())], attested: false };
    let (carry, acts) = on_outcome(&carry, PROMOTE, true, &serde_json::to_string(&promoted).unwrap(), &leg, NOW);
    assert_eq!(names(&acts), [CORPUS], "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref()), (State::Corpus, Some("r1-staged")));
    assert_eq!((rc.above[0].state, rc.above[0].batch.as_deref()), (State::Certifying, Some("r2")), "the upper is as it was");
}

#[test]
fn the_end_of_a_leg_of_a_round_the_queue_voided_moves_nothing() {
    let mut carry = certifying_two();
    carry.repos.get_mut("spira").unwrap().attributed = true;
    let (carry, _) = ran(&carry, "spira.lower.certify1", 1, "red=test-a.sh\n");
    let mut gone = snap(vec![], NOW + 300_000);
    gone.repos.get_mut("spira").unwrap().pool.clear();
    let (carry, _) = decide_with(&small(2), &carry, &gone);
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref(), rc.leg.as_deref(), rc.above.len()), (State::Idle, None, None, 0), "the voided round is forgotten");
    let late = UnitRan { handle: UnitHandle::of(NAME, "spira.upper.certify1").unwrap(), exit: Exit::Code(1), duration_ms: 1 };
    assert!(on_ran_with(&carry, &late, &RanOutput::default(), NOW + 310_000).is_none());
}

#[test]
fn an_unattended_open_round_is_driven_to_the_same_acts_whatever_the_limit() {
    let acts_at = |max_open: usize| {
        let (mut carry, mut t, mut all) = (Carry::default(), NOW, vec![]);
        let undriven = Round { driven: false, ..round("rx", &["sp-a"], "hx", 10) };
        while t <= NOW + (Policy::default().orphan_wait_s + 60) * 1000 {
            let (c, acts) = decide_with(&small(max_open), &carry, &snap(vec![undriven.clone()], t));
            carry = c;
            all.extend(acts.iter().map(|a| a.effect.clone()));
            t += 30_000;
        }
        all
    };
    assert_eq!(acts_at(1), acts_at(2));
    assert_eq!(acts_at(1), [REDRIVE], "{:?}", acts_at(1));
}

#[test]
fn the_real_task_cuts_the_second_round_on_the_first_ones_head() {
    use runtime::event::EventKind;
    use runtime::taskrig::TaskRig;
    let p = BTreeMap::from([("SPIRA_ROUND_MAX_OPEN".to_string(), "2".to_string())]);
    let mut carry = Carry::default();
    carry.repos.insert("spira".into(), RepoCarry { state: State::Corpus, round_key: Some("k".into()), batch: Some("r1".into()), members: vec![("sp-a".into(), "ta".into()), ("sp-b".into(), "tb".into())], wt: "/w/r1".into(), base: "b1".into(), leg: Some("spira.r1.corpus1".into()), leg_at_ms: NOW, run: 1, ..RepoCarry::default() });
    let world = snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 60)], NOW + 30_000);
    let mut r = TaskRig::new(1, BatcherLocal::new(p).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world).unwrap()));
    for e in EFFECTS.iter().filter(|e| **e != OBSERVE) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(STATE, &serde_json::to_string(&carry).unwrap())).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let cut = r.requested().iter().find(|q| q.name == ASSEMBLE).map(|q| serde_json::from_str::<Leg>(&q.input).unwrap()).expect("the second round is assembled");
    assert_eq!((cut.onto.as_str(), ids(&cut.members)), ("h1", ["sp-c", "sp-d"].to_vec()));
}
