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
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms, ..Default::default() }
}

fn round(id: &str, members: &[&str], head: &str, age_secs: u64) -> Round {
    Round { id: id.into(), members: members.iter().map(|m| m.to_string()).collect(), tips: vec![], head: head.into(), worktree: format!("w/{id}"), driven: true, red: false, red_suites: vec![], ejected: vec![], ejected_by: vec![], age_secs, unfit: None, phase: String::new(), gate_red: vec![] }
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
    let assembled = Assembled { merged: cut.members.clone(), wt: format!("w/{batch}"), base: "b1".into(), suites: true, ..Assembled::default() };
    let (carry, acts) = on_outcome(&carry, ASSEMBLE, true, &serde_json::to_string(&assembled).unwrap(), &cut, snapshot.now_ms);
    let open = leg_of(&acts, ROUND_OPEN);
    let opened = Opened { batch: batch.into(), ..Opened::default() };
    on_outcome(&carry, ROUND_OPEN, true, &serde_json::to_string(&opened).unwrap(), &open, snapshot.now_ms)
}

fn two_opened_rounds() -> Carry {
    let p = small(2);
    let (carry, acts) = cut_and_open(&p, &Carry::default(), &snap(vec![], NOW), "r1");
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
    let (carry, acts) = cut_and_open(&p, &carry, &snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 60)], NOW + 60_000), "r2");
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
    carry
}

fn certifying_two() -> Carry {
    let mut carry = two_opened_rounds();
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

#[test_id(t3m8g2p)]
#[test]
fn two_rounds_are_in_flight_at_once_the_second_cut_on_the_head_of_the_first() {
    let carry = two_opened_rounds();
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref(), ids(&rc.members)), (State::Certifying, Some("r1"), ["sp-a", "sp-b"].to_vec()));
    assert_eq!((rc.above.len(), rc.above[0].state, rc.above[0].batch.as_deref(), ids(&rc.above[0].members)), (1, State::Certifying, Some("r2"), ["sp-c", "sp-d"].to_vec()));
    assert_ne!(rc.leg, rc.above[0].leg, "each round runs its own leg");
    assert_ne!(rc.wt, rc.above[0].wt, "each round keeps its own tree");
}

#[test_id(tz44th8)]
#[test]
fn the_second_round_is_assembled_on_the_open_rounds_head_in_a_tree_of_its_own() {
    let p = small(2);
    let (carry, _) = cut_and_open(&p, &Carry::default(), &snap(vec![], NOW), "r1");
    let (_, acts) = decide_with(&p, &carry, &snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 60)], NOW + 60_000));
    assert_eq!(names(&acts), [NOTE_LEFT_OUT, FETCH, ASSEMBLE], "the first cut's left-out records are cleared: {acts:?}");
    let cut = leg_of(&acts, ASSEMBLE);
    assert_eq!((cut.onto.as_str(), ids(&cut.members)), ("h1", ["sp-c", "sp-d"].to_vec()), "the pool minus the open round's members, on its head");
}

#[test_id(t1fa79k)]
#[test]
fn with_one_round_open_allowed_a_running_round_cuts_nothing_more() {
    let p = small(1);
    let (carry, _) = cut_and_open(&p, &Carry::default(), &snap(vec![], NOW), "r1");
    let (next, acts) = decide_with(&p, &carry, &snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 60)], NOW + 60_000));
    assert!(acts.is_empty(), "{acts:?}");
    let mut seen = carry.clone();
    let rc = seen.repos.get_mut("spira").unwrap();
    (rc.head, rc.leg_head) = ("h1".into(), "h1".into());
    rc.admissible.clear();
    assert_eq!(next, seen, "only the head the round was first seen at is learned");
    let (_, acts) = decide_with(&Policy { max_members: 2, ..Policy::default() }, &carry, &snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 60)], NOW + 60_000));
    assert!(acts.is_empty(), "the default is one round: {acts:?}");
}

#[test_id(tm4yqnb)]
#[test]
fn a_third_round_is_not_cut_past_the_limit() {
    let carry = two_opened_rounds();
    let open = vec![round("r1", &["sp-a", "sp-b"], "h1", 120), round("r2", &["sp-c", "sp-d"], "h2", 60)];
    let (_, acts) = decide_with(&small(2), &carry, &snap(open, NOW + 120_000));
    assert!(acts.is_empty(), "{acts:?}");
}

#[test_id(tj22eep)]
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
    assert_eq!(names(&acts), [SUPERSEDE_REDS, spira_runtime::tasks::a::queue_step::PUBLISH_WANTED], "the landing closes the repo's local reds and wakes publish: {acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref(), rc.above.len()), (State::Certified, Some("r2"), 0), "the upper is now the lowest round");

    let open = vec![round("r2", &["sp-c", "sp-d"], "h2", 70)];
    let (carry, acts) = decide_with(&small(2), &carry, &snap(open, NOW + 140_000));
    assert_eq!(names(&acts), [LC_CERTIFY, LAND], "{acts:?}");
    assert_eq!(leg_of(&acts, LAND).batch, "r2");
    assert_eq!(carry.repos["spira"].state, State::Landing);
}

#[test_id(tmcv9h9)]
#[test]
fn a_green_lower_with_the_upper_still_certifying_lands_at_once() {
    let (_, acts) = ran(&certifying_two(), "spira.lower.certify1", 0, "");
    assert_eq!(names(&acts), [LC_CERTIFY, LAND], "{acts:?}");
}

#[test_id(tvx8w8u)]
#[test]
fn a_red_lower_round_leaves_the_upper_cut_again_on_the_new_base() {
    let mut carry = certifying_two();
    carry.repos.get_mut("spira").unwrap().attributed = true;
    carry.repos.get_mut("spira").unwrap().confirmed = true;
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

#[test_id(trhm7h8)]
#[test]
fn a_stacked_round_just_opened_is_not_voided_by_a_snapshot_that_predates_it() {
    let carry = two_opened_rounds();
    let open = vec![round("r1", &["sp-a", "sp-b"], "h1", 60)];
    let (next, acts) = decide_with(&small(2), &carry, &snap(open, NOW + 61_000));
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(next.repos["spira"].above.len(), 1);
}

#[test_id(t4qn154)]
#[test]
fn a_round_the_queue_holds_that_this_task_does_not_drive_blocks_a_second_cut() {
    let (_, acts) = decide_with(&small(2), &Carry::default(), &snap(vec![round("rx", &["sp-a"], "hx", 10)], NOW));
    assert!(!names(&acts).contains(&ASSEMBLE), "{acts:?}");
}

#[test_id(t40gizn)]
#[test]
fn a_red_open_round_is_not_stacked_on() {
    let p = small(2);
    let (carry, _) = cut_and_open(&p, &Carry::default(), &snap(vec![], NOW), "r1");
    let red = Round { red: true, ..round("r1", &["sp-a", "sp-b"], "h1", 60) };
    let (_, acts) = decide_with(&p, &carry, &snap(vec![red], NOW + 60_000));
    assert!(acts.is_empty(), "{acts:?}");
}

#[test_id(t0gx51z)]
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
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref()), (State::Certifying, Some("r1-staged")));
    assert_eq!((rc.above[0].state, rc.above[0].batch.as_deref()), (State::Certifying, Some("r2")), "the upper is as it was");
}

#[test_id(tiit3dw)]
#[test]
fn the_end_of_a_leg_of_a_round_the_queue_voided_moves_nothing() {
    let mut carry = certifying_two();
    carry.repos.get_mut("spira").unwrap().attributed = true;
    carry.repos.get_mut("spira").unwrap().confirmed = true;
    let (carry, _) = ran(&carry, "spira.lower.certify1", 1, "red=test-a.sh\n");
    let mut gone = snap(vec![], NOW + 300_000);
    gone.repos.get_mut("spira").unwrap().pool.clear();
    let (carry, _) = decide_with(&small(2), &carry, &gone);
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref(), rc.leg.as_deref(), rc.above.len()), (State::Idle, None, None, 0), "the voided round is forgotten");
    let late = UnitRan { handle: UnitHandle::of(NAME, "spira.upper.certify1").unwrap(), exit: Exit::Code(1), duration_ms: 1 };
    assert!(on_ran_with(&carry, &late, &RanOutput::default(), NOW + 310_000).is_none());
}

#[test_id(twcevxi)]
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

#[test_id(tjd3wdk)]
#[test]
fn the_real_task_cuts_the_second_round_on_the_first_ones_head() {
    use runtime::event::EventKind;
    use runtime::taskrig::TaskRig;
    let p = BTreeMap::from([("SPIRA_ROUND_MAX_OPEN".to_string(), "2".to_string())]);
    let mut carry = Carry::default();
    carry.repos.insert("spira".into(), RepoCarry { state: State::Corpus, round_key: Some("k".into()), batch: Some("r1".into()), members: vec![("sp-a".into(), "ta".into()), ("sp-b".into(), "tb".into())], wt: "w/r1".into(), base: "b1".into(), leg: Some("spira.r1.corpus1".into()), leg_at_ms: NOW, run: 1, ..RepoCarry::default() });
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

/// `rx`, opened by another actor above `r1`: whole, fit, and with no driver.
fn another_actors(age_secs: u64) -> Round {
    Round { driven: false, phase: "opened".into(), tips: vec!["tc".into(), "td".into()], ..round("rx", &["sp-c", "sp-d"], "hx", age_secs) }
}

fn beside_ours(rx: Round) -> Snapshot {
    snap(vec![round("r1", &["sp-a", "sp-b"], "h1", 90), rx], NOW + 60_000)
}

fn ours_certifying(p: &Policy) -> Carry {
    cut_and_open(p, &Carry::default(), &snap(vec![], NOW), "r1").0
}

fn adopts_beside_ours(max_open: usize) -> Carry {
    let p = small(max_open);
    let (carry, acts) = decide_with(&p, &ours_certifying(&p), &beside_ours(another_actors(30)));
    assert!(names(&acts).contains(&CERTIFY), "{acts:?}");
    assert_eq!(leg_of(&acts, CERTIFY).batch, "rx");
    assert_eq!(leg_of(&acts, CERTIFY).wt, "w/rx");
    assert!([REDRIVE, ASSEMBLE, ABANDON].iter().all(|no| !names(&acts).contains(no)), "{acts:?}");
    let above = &carry.repos["spira"].above;
    assert_eq!((above.len(), above[0].batch.as_deref(), above[0].state, above[0].stacked), (1, Some("rx"), State::Certifying, true));
    assert_eq!(above[0].base, "b1");
    carry
}

#[test_id(toi1fpr)]
#[test]
fn a_round_another_actor_opens_above_ours_is_adopted_and_certified() {
    adopts_beside_ours(2);
}

#[test_id(th3jco2)]
#[test]
fn adopting_is_not_cutting_so_a_reached_cap_does_not_stop_it() {
    adopts_beside_ours(1);
}

#[test_id(tw9xjz3)]
#[test]
fn an_adopted_stacked_round_lands_once_after_the_round_below() {
    let carry = adopts_beside_ours(2);
    let upper = carry.repos["spira"].above[0].leg.clone().unwrap();
    let lower = carry.repos["spira"].leg.clone().unwrap();
    let (carry, acts) = ran(&carry, &upper, 0, "");
    assert!(!names(&acts).contains(&LAND), "{acts:?}");
    assert_eq!(carry.repos["spira"].above[0].state, State::Certified);

    let (carry, acts) = ran(&carry, &lower, 0, "");
    assert_eq!(names(&acts), [LC_CERTIFY, LAND], "{acts:?}");
    let landed = leg_of(&acts, LAND);
    assert_eq!(landed.batch, "r1");
    let (carry, _) = on_outcome(&carry, LAND, true, &serde_json::to_string(&Landed::default()).unwrap(), &landed, NOW + 130_000);

    let green = Round { phase: "green".into(), ..another_actors(100) };
    let (carry, acts) = decide_with(&small(2), &carry, &snap(vec![green.clone()], NOW + 140_000));
    assert_eq!(names(&acts), [LC_CERTIFY, LAND], "{acts:?}");
    assert_eq!(leg_of(&acts, LAND).batch, "rx");

    let mut in_flight = snap(vec![green], NOW + 170_000);
    in_flight.repos.get_mut("spira").unwrap().in_flight = Some(vec![LAND.into()]);
    let (_, acts) = decide_with(&small(2), &carry, &in_flight);
    assert!(!names(&acts).contains(&LAND), "{acts:?}");
}

#[test_id(tvggw9a)]
#[test]
fn a_round_still_within_its_openers_grace_or_unfit_is_not_adopted() {
    let p = small(2);
    let (_, acts) = decide_with(&p, &ours_certifying(&p), &beside_ours(another_actors(20)));
    assert!(names(&acts).contains(&CERTIFY), "control: at the grace it is adopted: {acts:?}");

    let (carry, acts) = decide_with(&p, &ours_certifying(&p), &beside_ours(another_actors(19)));
    assert!(!names(&acts).contains(&CERTIFY), "{acts:?}");
    assert!(carry.repos["spira"].above.is_empty());

    let unfit = Round { unfit: Some("the round worktree is not at the round's head".into()), ..another_actors(30) };
    let (carry, acts) = decide_with(&p, &ours_certifying(&p), &beside_ours(unfit));
    assert!(!names(&acts).contains(&CERTIFY), "{acts:?}");
    assert_eq!(names(&acts).iter().filter(|n| **n == ABANDON).count(), 1, "the orphan path ends it, naming why: {acts:?}");
    assert_eq!(leg_of(&acts, ABANDON).batch, "rx");
    assert!(carry.repos["spira"].above.is_empty());
}

#[test_id(tatqj8u)]
#[test]
fn a_stopped_or_hand_landing_repo_adopts_nothing() {
    let p = small(2);
    adopts_beside_ours(2);
    for hold in [|r: &mut Repo| r.stopped = true, |r: &mut Repo| r.cut_hold = Some("concierge (hand landing)".into())] {
        let mut world = beside_ours(another_actors(30));
        hold(world.repos.get_mut("spira").unwrap());
        let (carry, acts) = decide_with(&p, &ours_certifying(&p), &world);
        assert!(!names(&acts).contains(&CERTIFY), "{acts:?}");
        assert!(carry.repos["spira"].above.is_empty());
    }
}

#[test_id(tejifnt)]
#[test]
fn no_round_is_adopted_while_one_of_ours_is_unnamed_or_being_abandoned() {
    let p = small(2);
    let (carry, _) = decide_with(&p, &Carry::default(), &snap(vec![], NOW));
    assert_eq!(carry.repos["spira"].state, State::Assembling);
    let (_, acts) = decide_with(&p, &carry, &snap(vec![another_actors(30)], NOW + 30_000));
    assert!(!names(&acts).contains(&CERTIFY), "{acts:?}");

    let (_, acts) = decide_with(&p, &Carry::default(), &snap(vec![another_actors(30)], NOW));
    assert_eq!(names(&acts), [CERTIFY], "control: with nothing pending it is adopted: {acts:?}");

    for state in [State::Opened, State::Promoting] {
        let mut waiting = ours_certifying(&p);
        let rc = waiting.repos.get_mut("spira").unwrap();
        (rc.state, rc.batch, rc.waiting) = (state, None, None);
        let (_, acts) = decide_with(&p, &waiting, &snap(vec![another_actors(30)], NOW + 5_000));
        assert!(!names(&acts).contains(&CERTIFY), "{state:?}: {acts:?}");
    }

    let mut closing = snap(vec![another_actors(30)], NOW);
    closing.repos.get_mut("spira").unwrap().in_flight = Some(vec![ABANDON.into()]);
    let (_, acts) = decide_with(&p, &Carry::default(), &closing);
    assert!(!names(&acts).contains(&CERTIFY), "{acts:?}");
}

#[test_id(t5dsg38)]
#[test]
fn a_round_adopted_once_and_let_go_still_opened_is_not_adopted_again() {
    let p = small(1);
    let world = |t| snap(vec![another_actors(30)], t);
    let (carry, acts) = decide_with(&p, &Carry::default(), &world(NOW));
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
    let leg = carry.repos["spira"].leg.clone().unwrap();
    let signalled = UnitRan { handle: UnitHandle::of(NAME, &leg).unwrap(), exit: Exit::Signal(15), duration_ms: 1 };
    let (carry, _) = on_ran_with(&carry, &signalled, &RanOutput::default(), NOW).expect("its own leg");
    assert!(carry.repos.get("spira").is_none_or(|rc| rc.batch.is_none()), "the round is let go");
    let (_, acts) = decide_with(&p, &carry, &world(NOW + 30_000));
    assert!(!names(&acts).contains(&CERTIFY), "{acts:?}");
}

#[test_id(tnpitow)]
#[test]
fn a_stacked_foreign_round_that_is_not_adopted_is_left_alone_until_it_is_first() {
    let p = small(2);
    let far = NOW + (Policy::default().orphan_wait_s + 600) * 1000;
    for (phase, tips) in [("attributing", vec!["tc".to_string(), "td".to_string()]), ("certifying", vec![]), ("opened", vec![])] {
        let rx = Round { phase: phase.into(), tips, ..another_actors(1_400) };
        let (mut carry, mut all) = (ours_certifying(&p), vec![]);
        for t in [NOW + 60_000, far] {
            let mut world = beside_ours(rx.clone());
            world.now_ms = t;
            let (next, acts) = decide_with(&p, &carry, &world);
            carry = next;
            all.extend(acts.iter().map(|a| a.effect.clone()));
        }
        assert!([REDRIVE, ABANDON].iter().all(|no| !all.iter().any(|a| a == no)), "{phase}: {all:?}");
    }
}

#[test_id(ty6gk58)]
#[test]
fn a_round_above_one_this_task_does_not_carry_is_not_adopted_so_it_cannot_land_first() {
    let p = small(2);
    let by_hand = Round { driven: true, phase: "certifying".into(), ..round("rh", &["sp-a", "sp-b"], "h1", 90) };
    let (carry, acts) = decide_with(&p, &Carry::default(), &snap(vec![by_hand, another_actors(30)], NOW));
    assert!([CERTIFY, LAND].iter().all(|no| !names(&acts).contains(no)), "{acts:?}");
    assert!(carry.repos.get("spira").is_none_or(|rc| rc.batch.is_none()));
    adopts_beside_ours(2);
}

#[test_id(t847qgd)]
#[test]
fn a_round_opened_under_one_of_ours_is_adopted_below_it_and_lands_first() {
    let p = small(1);
    let under = snap(vec![another_actors(120), round("r1", &["sp-a", "sp-b"], "h1", 90)], NOW + 60_000);
    let (carry, acts) = decide_with(&p, &ours_certifying(&p), &under);
    assert_eq!(leg_of(&acts, CERTIFY).batch, "rx", "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.batch.as_deref(), rc.stacked), (Some("rx"), false));
    assert_eq!((rc.above[0].batch.as_deref(), rc.above[0].stacked), (Some("r1"), true));
    let ours = rc.above[0].leg.clone().unwrap();
    let (_, acts) = ran(&carry, &ours, 0, "");
    assert!(!names(&acts).contains(&LAND), "ours waits for the round now under it: {acts:?}");
}

#[test_id(tyk6gqt)]
#[test]
fn a_paused_repo_still_adopts_and_cuts_nothing() {
    let p = small(2);
    let mut world = snap(vec![another_actors(30)], NOW);
    world.repos.get_mut("spira").unwrap().paused = Some("hold: an operator".into());
    let (_, acts) = decide_with(&p, &Carry::default(), &world);
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
}
