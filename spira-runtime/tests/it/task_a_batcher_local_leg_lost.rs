//! A round batcher.local opened is always driven to an outcome: a leg whose unit never starts,
//! or is gone with no `unit.ran`, is started once more, then the round is abandoned with the
//! reason — it is never left open with nothing driving it.

use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_793_000_000_000;
const TICK: u64 = CADENCE_MS;

fn corpus(leg: &str, at: u64) -> Carry {
    let rc = RepoCarry {
        state: State::Corpus,
        round_key: Some("k".into()),
        batch: Some("r9".into()),
        members: vec![("sp-a".into(), "t1".into())],
        wt: "w".into(),
        base: "b1".into(),
        leg: Some(leg.into()),
        leg_at_ms: at,
        run: 1,
        ..RepoCarry::default()
    };
    Carry {
        repos: [("spira".to_string(), rc)].into(),
    }
}

fn snap(now_ms: u64, live: Option<Vec<String>>) -> Snapshot {
    let repo = Repo {
        open_round: Some(Round {
            id: "r9".into(),
            worktree: "w".into(),
            ..Round::default()
        }),
        live_legs: live,
        ..Repo::default()
    };
    Snapshot {
        repos: [("spira".to_string(), repo)].into(),
        now_ms,
        ..Default::default()
    }
}

fn names(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

/// Observes at `NOW + after` ms with no live leg, `n` times a tick apart.
fn observe_gone(mut carry: Carry, from: u64, n: u64) -> (Carry, Vec<Act>) {
    let mut acts = vec![];
    for i in 1..=n {
        let (c, a) = decide(&carry, &snap(from + i * TICK, Some(vec![])));
        carry = c;
        acts.extend(a);
    }
    (carry, acts)
}

#[test_id(tca5wav)]
#[test]
fn a_corpus_leg_that_never_started_is_started_once_more_within_a_minute_of_the_open() {
    let first = "spira.r9.corpus.1";
    let (carry, acts) = observe_gone(corpus(first, NOW), NOW, 2);
    assert_eq!(
        names(&acts),
        [CORPUS],
        "the missing leg is started again: {acts:?}"
    );
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_ne!(leg.unit, first, "a new run, so a new unit");
    assert_eq!(carry.repos["spira"].state, State::Corpus);
    assert_eq!(carry.repos["spira"].leg.as_deref(), Some(leg.unit.as_str()));
}

#[test_id(tmgoafv)]
#[test]
fn a_leg_the_unit_manager_lists_is_left_alone() {
    let mut carry = corpus("spira.r9.corpus.1", NOW);
    for i in 1..=6 {
        let (c, acts) = decide(
            &carry,
            &snap(NOW + i * TICK, Some(vec!["spira.r9.corpus.1".into()])),
        );
        assert!(acts.is_empty(), "{acts:?}");
        carry = c;
    }
}

#[test_id(tcq9fv8)]
#[test]
fn a_snapshot_that_could_not_list_units_judges_nothing() {
    let mut carry = corpus("spira.r9.corpus.1", NOW);
    for i in 1..=6 {
        let (c, acts) = decide(&carry, &snap(NOW + i * TICK, None));
        assert!(acts.is_empty(), "{acts:?}");
        carry = c;
    }
}

#[test_id(t30ort6)]
#[test]
fn a_corpus_leg_that_is_gone_twice_abandons_the_round_naming_why() {
    let (carry, _) = observe_gone(corpus("spira.r9.corpus.1", NOW), NOW, 2);
    let second = carry.repos["spira"].leg.clone().unwrap();
    let (carry, acts) = observe_gone(carry, NOW + 2 * TICK, 2);
    assert_eq!(names(&acts), [ABANDON], "{acts:?}");
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(leg.batch, "r9");
    assert!(
        leg.why.contains("corpus")
            && leg.why.contains(&second)
            && leg.why.contains("started again"),
        "the reason names the leg: {}",
        leg.why
    );
    let rc = &carry.repos["spira"];
    assert_eq!(
        (rc.state, rc.batch.clone(), rc.pace.infra),
        (State::Idle, None, 1),
        "an infrastructure fault, round forgotten"
    );
}

#[test_id(t5zin9w)]
#[test]
fn a_corpus_leg_that_could_not_be_started_is_started_once_more_then_abandoned() {
    let first = corpus("spira.r9.corpus.1", NOW);
    let leg = Leg {
        unit: "spira.r9.corpus.1".into(),
        batch: "r9".into(),
        ..Leg::on("spira")
    };
    let (again, acts) = on_outcome(
        &first,
        CORPUS,
        false,
        "no unit manager configured",
        &leg,
        NOW + 1000,
    );
    assert_eq!(names(&acts), [CORPUS], "{acts:?}");
    assert_eq!(
        again.repos["spira"].state,
        State::Corpus,
        "a failed start is not a red corpus"
    );
    let second = again.repos["spira"].leg.clone().unwrap();
    let leg2 = Leg {
        unit: second,
        batch: "r9".into(),
        ..Leg::on("spira")
    };
    let (done, acts) = on_outcome(
        &again,
        CORPUS,
        false,
        "no unit manager configured",
        &leg2,
        NOW + 2000,
    );
    assert_eq!(names(&acts), [ABANDON], "{acts:?}");
    let abandon: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert!(
        abandon.why.contains("no unit manager configured"),
        "{}",
        abandon.why
    );
    assert_eq!(done.repos["spira"].state, State::Idle);
}

#[test_id(trolsnz)]
#[test]
fn a_round_open_with_no_driver_after_its_one_redrive_is_abandoned_not_paged() {
    let mut carry = Carry::default();
    let wait = Policy::default().orphan_wait_s * 1000;
    let mut all: Vec<String> = vec![];
    let mut t = NOW;
    let mut abandoned = false;
    while t < NOW + 3 * wait {
        let mut snapshot = snap(t, None);
        if abandoned {
            snapshot.repos.get_mut("spira").unwrap().open_round = None;
        }
        let (c, acts) = decide(&carry, &snapshot);
        carry = c;
        abandoned |= names(&acts).contains(&ABANDON);
        all.extend(names(&acts).iter().map(|s| s.to_string()));
        t += TICK;
    }
    assert_eq!(
        all.iter().filter(|a| *a == REDRIVE).count(),
        1,
        "re-driven once: {all:?}"
    );
    assert_eq!(
        all.iter().filter(|a| *a == ABANDON).count(),
        1,
        "then abandoned: {all:?}"
    );
    assert!(
        !all.contains(&INBOX_APPEND.to_string()) && !all.contains(&FILE.to_string()),
        "an abandoned round pages nobody as open: {all:?}"
    );
}

#[test_id(tee5d5k)]
#[test]
fn each_leg_gets_its_own_restart_a_corpus_restart_does_not_spend_the_certify_leg_s() {
    let (carry, _) = observe_gone(corpus("spira.r9.corpus.1", NOW), NOW, 2);
    let leg = carry.repos["spira"].leg.clone().unwrap();
    assert_eq!(carry.repos["spira"].leg_restarts, 1);
    let ran = runtime::units::UnitRan {
        handle: runtime::units::UnitHandle::of(NAME, &leg).unwrap(),
        exit: runtime::units::Exit::Code(0),
        duration_ms: 1,
    };
    let (carry, acts) = on_ran(&carry, &ran, NOW + 5 * TICK).unwrap();
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
    let certify_at = NOW + 5 * TICK;
    let (_, acts) = observe_gone(carry, certify_at, 2);
    assert_eq!(
        names(&acts),
        [CERTIFY],
        "the certify leg is started again before the round is abandoned: {acts:?}"
    );
}

#[test_id(tofny1s)]
#[test]
fn a_round_certify_would_refuse_is_abandoned_after_the_wait_not_left_open() {
    let mut repo = Repo {
        open_round: Some(Round {
            id: "r9".into(),
            worktree: "w".into(),
            ..Round::default()
        }),
        unfit: Some("the round worktree is moved".into()),
        ..Repo::default()
    };
    let mut acts = vec![];
    let (mut carry, mut t) = (Carry::default(), NOW);
    while t <= NOW + (Policy::default().orphan_wait_s + 60) * 1000 {
        let (c, a) = decide(
            &carry,
            &Snapshot {
                repos: [("spira".to_string(), repo.clone())].into(),
                now_ms: t,
                ..Default::default()
            },
        );
        carry = c;
        if names(&a).contains(&ABANDON) {
            repo.open_round = None;
        }
        acts.extend(a);
        t += TICK;
    }
    assert_eq!(names(&acts), [ABANDON], "{acts:?}");
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert!(
        leg.why.contains("cannot be re-driven") && leg.why.contains("moved"),
        "{}",
        leg.why
    );
}

/// Observes `id` open, undriven and unfit from `from` until it is abandoned, as the existing
/// abandon test does; the open round is gone once an ABANDON is asked.
fn abandon_undriven(mut carry: Carry, id: &str, from: u64) -> (Carry, Vec<Act>, u64) {
    let repo = Repo {
        open_round: Some(Round {
            id: id.into(),
            worktree: "w".into(),
            ..Round::default()
        }),
        unfit: Some("the round worktree is moved".into()),
        ..Repo::default()
    };
    let (mut acts, mut t) = (vec![], from);
    while t <= from + (Policy::default().orphan_wait_s + 60) * 1000 {
        let (c, a) = decide(
            &carry,
            &Snapshot {
                repos: [("spira".to_string(), repo.clone())].into(),
                now_ms: t,
                ..Default::default()
            },
        );
        carry = c;
        let done = names(&a).contains(&ABANDON);
        acts.extend(a);
        t += TICK;
        if done {
            break;
        }
    }
    (carry, acts, t)
}

#[test_id(ttpq7n2)]
#[test]
fn a_second_undriven_round_abandoned_in_a_row_writes_the_stop_before_it_pages() {
    let (carry, first, t) = abandon_undriven(Carry::default(), "r8", NOW);
    assert!(!names(&first).contains(&PAUSE), "{first:?}");
    let (_, second, _) = abandon_undriven(carry, "r9", t);
    let n = names(&second);
    let stop = second
        .iter()
        .position(|a| {
            a.effect == PAUSE
                && serde_json::from_str::<Leg>(&a.input).is_ok_and(|l| l.bead == "stop")
        })
        .unwrap_or_else(|| panic!("no stop written: {n:?}"));
    let page = second
        .iter()
        .position(|a| a.effect == INBOX_APPEND && a.input.contains("ROUNDS STOPPED spira:"))
        .unwrap_or_else(|| panic!("no page: {n:?}"));
    assert!(stop < page, "the stop is written before the page: {n:?}");
}

#[test_id(tf5r0a4)]
#[test]
fn one_round_that_faulted_then_was_abandoned_undriven_counts_once() {
    let mut rc = RepoCarry {
        state: State::Certifying,
        round_key: Some("k".into()),
        batch: Some("r9".into()),
        members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())],
        wt: "w".into(),
        base: "b1".into(),
        leg: Some("spira.c3".into()),
        run: 1,
        attributed: true,
        ..RepoCarry::default()
    };
    rc.leg_at_ms = NOW;
    let carry = Carry {
        repos: [("spira".to_string(), rc)].into(),
    };
    let ran = runtime::units::UnitRan {
        handle: runtime::units::UnitHandle::of(NAME, "spira.c3").unwrap(),
        exit: runtime::units::Exit::Code(1),
        duration_ms: 1,
    };
    let out =
        runtime::units::RanOutput::captured(b"queue.sh round certify: refused, nothing ran\n", b"");
    let (carry, faulted) = on_ran_with(&carry, &ran, &out, NOW + TICK).unwrap();
    assert!(!names(&faulted).contains(&ABANDON), "{faulted:?}");
    assert_eq!(carry.repos["spira"].pace.infra, 1);
    let (carry, acts, _) = abandon_undriven(carry, "r9", NOW + 2 * TICK);
    assert_eq!(names(&acts), [ABANDON], "{acts:?}");
    assert_eq!(carry.repos["spira"].pace.infra, 1);
}

/// Round `id` certifying after its attribution, its certify leg ending with no verdict.
fn certify_faults(mut carry: Carry, id: &str, at: u64) -> (Carry, Vec<Act>) {
    let leg = format!("spira.{id}.c");
    let rc = RepoCarry {
        state: State::Certifying,
        round_key: Some("k".into()),
        batch: Some(id.into()),
        members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())],
        wt: "w".into(),
        base: "b1".into(),
        leg: Some(leg.clone()),
        run: 1,
        attributed: true,
        leg_at_ms: at,
        pace: carry
            .repos
            .get("spira")
            .map(|r| r.pace.clone())
            .unwrap_or_default(),
        ..RepoCarry::default()
    };
    carry.repos.insert("spira".into(), rc);
    let ran = runtime::units::UnitRan {
        handle: runtime::units::UnitHandle::of(NAME, &leg).unwrap(),
        exit: runtime::units::Exit::Code(1),
        duration_ms: 1,
    };
    on_ran_with(
        &carry,
        &ran,
        &runtime::units::RanOutput::captured(
            b"queue.sh round certify: refused, nothing ran\n",
            b"",
        ),
        at + TICK,
    )
    .unwrap()
}

#[test_id(tvqxvx3)]
#[test]
fn two_different_rounds_that_each_fault_still_write_the_stop_and_page() {
    let (carry, first) = certify_faults(Carry::default(), "r8", NOW);
    assert!(!names(&first).contains(&PAUSE), "{first:?}");
    let (_, second) = certify_faults(carry, "r9", NOW + 5 * TICK);
    let n = names(&second);
    assert!(
        second.iter().any(|a| a.effect == PAUSE
            && serde_json::from_str::<Leg>(&a.input).is_ok_and(|l| l.bead == "stop")),
        "{n:?}"
    );
    assert!(
        second
            .iter()
            .any(|a| a.effect == INBOX_APPEND && a.input.contains("ROUNDS STOPPED spira:")),
        "{n:?}"
    );
}

#[test_id(t2sa4ug)]
#[test]
fn the_same_round_faulting_twice_counts_once_and_stops_nothing() {
    let (carry, _) = certify_faults(Carry::default(), "r9", NOW);
    let (carry, again) = certify_faults(carry, "r9", NOW + 5 * TICK);
    assert!(
        !names(&again).contains(&PAUSE) && !names(&again).contains(&INBOX_APPEND),
        "{again:?}"
    );
    assert_eq!(carry.repos["spira"].pace.infra, 1);
}

#[test_id(trs7cwb)]
#[test]
fn running_legs_are_this_task_s_active_units_by_key() {
    use runtime::units::{UnitDetail, UnitHandle, UnitState};
    let named = |task: &str, key: &str, state| UnitDetail {
        name: UnitHandle::of(task, key).unwrap().name,
        state,
        ..UnitDetail::default()
    };
    let listed = [
        named(NAME, "spira.r9.corpus1", UnitState::Active),
        named(NAME, "spira.r9.corpus2", UnitState::Failed),
        named("sentinel-call", "x.y", UnitState::Active),
    ];
    assert_eq!(running_legs(&listed), ["spira.r9.corpus1"]);
}

fn attributing(at: u64) -> Carry {
    let mut carry = corpus("spira.r9.attribute.2", at);
    carry.repos.get_mut("spira").unwrap().state = State::Attributing;
    carry
}

#[test_id(tykclr2)]
#[test]
fn an_attribution_leg_still_listed_past_one_walls_grace_is_not_judged_lost() {
    let past_one_wall = Policy::default().wall_ms + LEG_GRACE_MS + TICK;
    let (carry, acts) = decide(
        &attributing(NOW),
        &snap(
            NOW + past_one_wall,
            Some(vec!["spira.r9.attribute.2".into()]),
        ),
    );
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Attributing);
}

fn certifying(at: u64) -> Carry {
    let mut carry = corpus("spira.r9.certify1", at);
    carry.repos.get_mut("spira").unwrap().state = State::Certifying;
    carry
}

#[test_id(tuz02fi)]
#[test]
fn a_live_certify_leg_past_one_walls_grace_starts_no_attribution() {
    let past_one_wall = Policy::default().wall_ms + LEG_GRACE_MS + TICK;
    let (carry, acts) = decide(
        &certifying(NOW),
        &snap(NOW + past_one_wall, Some(vec!["spira.r9.certify1".into()])),
    );
    assert!(acts.is_empty(), "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!(rc.state, State::Certifying);
    assert_eq!(rc.leg.as_deref(), Some("spira.r9.certify1"));
    assert!(!rc.attributed);
}

#[test_id(tqqxf1v)]
#[test]
fn a_certify_leg_the_manager_does_not_list_is_started_once_more() {
    let (_, acts) = observe_gone(
        certifying(NOW),
        NOW + LEG_START_GRACE_MS,
        LEG_MISSING_OBSERVES as u64,
    );
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
}

#[test_id(tgopl7v)]
#[test]
fn an_attribution_leg_is_judged_lost_past_its_iterations_deadline() {
    let p = Policy::default();
    let past_all = p.wall_ms * 3 + LEG_GRACE_MS + TICK;
    let (_, acts) = decide(
        &attributing(NOW),
        &snap(NOW + past_all, Some(vec!["spira.r9.attribute.2".into()])),
    );
    assert_eq!(
        names(&acts).first(),
        Some(&PAUSE),
        "a lost leg judged nothing, so the round stays open: {acts:?}"
    );
    assert!(!names(&acts).contains(&ABANDON), "{acts:?}");
}

#[test_id(ti8krv1)]
#[test]
fn a_corpus_leg_is_judged_lost_one_wall_and_grace_after_it_started() {
    let past_one_wall = Policy::default().wall_ms + LEG_GRACE_MS + TICK;
    let (_, acts) = decide(
        &corpus("spira.r9.corpus.1", NOW),
        &snap(NOW + past_one_wall, Some(vec!["spira.r9.corpus.1".into()])),
    );
    assert_eq!(
        names(&acts),
        [CORPUS],
        "lost, so the round goes to attribution: {acts:?}"
    );
}

#[test_id(twakt60)]
#[test]
fn an_attribution_leg_deadline_follows_the_member_count() {
    let wall = Policy::default().wall_ms;
    let mut carry = attributing(NOW);
    carry.repos.get_mut("spira").unwrap().members = (0..3)
        .map(|i| (format!("sp-{i}"), format!("t{i}")))
        .collect();
    let listed = Some(vec!["spira.r9.attribute.2".to_string()]);
    let (_, acts) = decide(&carry, &snap(NOW + wall * 5 - 1, listed.clone()));
    assert!(acts.is_empty(), "three members allow five runs: {acts:?}");
    let (_, acts) = decide(&carry, &snap(NOW + wall * 5 + LEG_GRACE_MS + 1, listed));
    assert_eq!(names(&acts).first(), Some(&PAUSE), "{acts:?}");
    assert!(!names(&acts).contains(&ABANDON), "{acts:?}");
}

/// A leg that ended while the runtime was down left no `unit.ran`: the tick reads each leg in
/// flight, in every state that has one, through `unit.facts`.
#[test_id(tc8d9zo)]
#[test]
fn a_tick_reads_every_leg_in_flight_through_unit_facts() {
    use runtime::units::{Exit, UnitFacts, UnitState, UNIT_FACTS};
    let facts = serde_json::to_string(&UnitFacts {
        state: UnitState::Inactive,
        main_pid: None,
        exit: Some(Exit::Code(0)),
        since_ms: Some(1),
    })
    .unwrap();
    for state in [State::Corpus, State::Attributing, State::Certifying] {
        let mut carry = corpus("spira.r9.leg.1", NOW);
        carry.repos.get_mut("spira").unwrap().state = state;
        let facts = facts.clone();
        let mut r =
            runtime::taskrig::TaskRig::new(1, BatcherLocal::new(Default::default()).registration())
                .unwrap()
                .double(UNIT_FACTS, move |_| Ok(facts.clone()));
        r.tick(
            STREAM,
            runtime::event::EventKind::new(
                spira_runtime::tasks::a::batcher_local::STATE,
                &serde_json::to_string(&carry).unwrap(),
            ),
        )
        .unwrap();
        r.tick(
            STREAM,
            runtime::event::EventKind::new(spira_runtime::tasks::a::batcher_local::TICK, ""),
        )
        .unwrap();
        let asked: Vec<&str> = r.requested().iter().map(|q| q.name.as_str()).collect();
        assert!(asked.contains(&UNIT_FACTS), "{state:?}: {asked:?}");
        assert!(
            asked.iter().any(|n| *n != UNIT_FACTS && *n != OBSERVE),
            "{state:?}: the exit read from facts moved the machine: {asked:?}"
        );
    }
}

const RED_MARK: &str = "suites-failed; failing step: test-a.sh";

fn certifying_three(below: bool) -> Carry {
    let members = vec![
        ("sp-a".into(), "t1".into()),
        ("sp-b".into(), "t2".into()),
        ("sp-c".into(), "t3".into()),
    ];
    let rc = RepoCarry {
        state: State::Certifying,
        round_key: Some("k".into()),
        batch: Some("r9".into()),
        members,
        wt: "w".into(),
        base: "b1".into(),
        leg: Some("spira.r9.certify1".into()),
        leg_at_ms: NOW - 180_000,
        run: 1,
        head: "h0".into(),
        leg_head: "h0".into(),
        ..RepoCarry::default()
    };
    if below {
        let lower = RepoCarry {
            batch: Some("r8".into()),
            members: vec![("sp-z".into(), "t9".into())],
            wt: "w8".into(),
            leg: Some("spira.r8.certify1".into()),
            head: "g1".into(),
            leg_head: "g1".into(),
            leg_at_ms: NOW,
            round_key: Some("kz".into()),
            ..rc.clone()
        };
        return Carry {
            repos: [(
                "spira".to_string(),
                RepoCarry {
                    above: vec![rc],
                    ..lower
                },
            )]
            .into(),
        };
    }
    Carry {
        repos: [("spira".to_string(), rc)].into(),
    }
}

fn r9(head: &str, phase: &str, driven: bool, gate_red: Vec<(String, String, String)>) -> Round {
    Round {
        id: "r9".into(),
        members: vec!["sp-a".into(), "sp-b".into(), "sp-c".into()],
        head: head.into(),
        worktree: "w".into(),
        driven,
        phase: phase.into(),
        gate_red,
        ..Round::default()
    }
}

fn snap_rounds(now_ms: u64, live: Option<Vec<String>>, below: Option<Round>, r: Round) -> Snapshot {
    let (open_round, open_above) = match below {
        Some(b) => (Some(b), vec![r]),
        None => (Some(r), vec![]),
    };
    let repo = Repo {
        open_round,
        open_above,
        live_legs: live,
        base_sha: "b1".into(),
        ..Repo::default()
    };
    Snapshot {
        repos: [("spira".to_string(), repo)].into(),
        now_ms,
        ..Default::default()
    }
}

/// Carries the round through the gate-red eject into a started certify leg on head h1, then lets
/// the store record green with the leg unlisted and no `unit.ran`.
fn green_with_leg_gone(below: Option<Round>) -> (Carry, Vec<Act>, Vec<Act>) {
    let red = vec![("sp-a".to_string(), "t1".to_string(), RED_MARK.to_string())];
    let (carry, acts) = decide(
        &certifying_three(below.is_some()),
        &snap_rounds(
            NOW,
            Some(vec!["spira.r9.certify1".into(), "spira.r8.certify1".into()]),
            below.clone(),
            r9("h0", "certifying", true, red),
        ),
    );
    assert!(acts.is_empty(), "{acts:?}");
    let ran = runtime::units::UnitRan {
        handle: runtime::units::UnitHandle::of(NAME, "spira.r9.certify1").unwrap(),
        exit: runtime::units::Exit::Code(1),
        duration_ms: 1,
    };
    let out = runtime::units::RanOutput::captured(b"verdict=red\n", b"");
    let (carry, acts) = on_ran_with(&carry, &ran, &out, NOW).expect("the carried leg ended");
    assert_eq!(names(&acts), [EJECT_GATE_RED], "{acts:?}");
    let eject: Leg = serde_json::from_str(&acts[0].input).unwrap();
    let (carry, acts) = on_outcome(&carry, EJECT_GATE_RED, true, "{}", &eject, NOW);
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    let live = || {
        let mut l = vec![leg.unit.clone()];
        if below.is_some() {
            l.push("spira.r8.certify1".into());
        }
        Some(l)
    };
    let (carry, _) = decide(
        &carry,
        &snap_rounds(
            NOW + TICK,
            live(),
            below.clone(),
            r9("h1", "certifying", true, vec![]),
        ),
    );
    let (mut carry, mut all) = (carry, vec![]);
    for i in 2..=3 {
        let (c, a) = decide(
            &carry,
            &snap_rounds(
                NOW + i * TICK,
                Some(if below.is_some() {
                    vec!["spira.r8.certify1".into()]
                } else {
                    vec![]
                }),
                below.clone(),
                r9("h1", "green", true, vec![]),
            ),
        );
        carry = c;
        all.extend(a);
    }
    (carry, all, acts)
}

#[test_id(tznbo04)]
#[test]
fn a_green_round_whose_certify_unit_ran_has_not_arrived_lands_not_abandoned_and_a_late_unit_ran_changes_nothing(
) {
    let (carry, acts, started) = green_with_leg_gone(None);
    let n = names(&acts);
    assert!(n.contains(&LAND) && n.contains(&LC_CERTIFY), "{acts:?}");
    assert!(!n.contains(&ABANDON) && !n.contains(&CERTIFY), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Landing);
    let leg: Leg = serde_json::from_str(&started[0].input).unwrap();
    let ran = runtime::units::UnitRan {
        handle: runtime::units::UnitHandle::of(NAME, &leg.unit).unwrap(),
        exit: runtime::units::Exit::Code(0),
        duration_ms: 1,
    };
    assert!(on_ran(&carry, &ran, NOW + 9 * TICK).is_none());
}

#[test_id(tw9fgjw)]
#[test]
fn a_green_stacked_round_whose_certify_unit_ran_has_not_arrived_is_certified_and_held_not_abandoned(
) {
    let below = Round {
        id: "r8".into(),
        members: vec!["sp-z".into()],
        head: "g1".into(),
        worktree: "w8".into(),
        driven: true,
        phase: "certifying".into(),
        ..Round::default()
    };
    let (carry, acts, _) = green_with_leg_gone(Some(below));
    let n = names(&acts);
    assert!(!n.contains(&LAND) && !n.contains(&ABANDON), "{acts:?}");
    assert_eq!(carry.repos["spira"].above[0].state, State::Certified);
}

#[test_id(tct390h)]
#[test]
fn a_pass_started_after_a_gate_red_eject_begins_with_its_restart_allowance_unspent() {
    let mut carry = certifying(NOW);
    {
        let rc = carry.repos.get_mut("spira").unwrap();
        rc.leg_restarts = 1;
        rc.members = vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())];
        rc.head = "H0".into();
        rc.leg_head = "H0".into();
    }
    let at = |head: &str| {
        let mut snapshot = snap(NOW, Some(vec!["spira.r9.certify1".into()]));
        let round = snapshot
            .repos
            .get_mut("spira")
            .unwrap()
            .open_round
            .as_mut()
            .unwrap();
        round.members = vec!["sp-a".into(), "sp-b".into()];
        round.head = head.into();
        round.driven = true;
        round.phase = "certifying".into();
        round.gate_red = vec![(
            "sp-a".into(),
            "t1".into(),
            "suites-failed; failing step: x".into(),
        )];
        snapshot
    };
    let (carry, acts) = decide(&carry, &at("H0"));
    assert!(acts.is_empty(), "{acts:?}");
    let (carry, acts) = decide(&carry, &at("H1"));
    assert!(names(&acts).contains(&EJECT_GATE_RED), "{acts:?}");
    let eject = acts.iter().find(|a| a.effect == EJECT_GATE_RED).unwrap();
    let leg: Leg = serde_json::from_str(&eject.input).expect("a leg");

    let (carry, acts) = on_outcome(&carry, EJECT_GATE_RED, true, "{}", &leg, NOW);
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");
    assert_eq!(carry.repos["spira"].leg_restarts, 0);
    let started = carry.repos["spira"].leg.clone().expect("a leg");

    let from = NOW + LEG_START_GRACE_MS;
    let (carry, acts) = observe_gone(carry, from, LEG_MISSING_OBSERVES as u64);
    assert_eq!(
        names(&acts),
        [CERTIFY],
        "started once more, not abandoned: {acts:?}"
    );
    let restarted = carry.repos["spira"].leg.clone().expect("a leg");
    assert_ne!(restarted, started);

    let (_, acts) = observe_gone(
        carry,
        from + LEG_START_GRACE_MS + 10 * TICK,
        LEG_MISSING_OBSERVES as u64,
    );
    assert_eq!(names(&acts), [ABANDON], "{acts:?}");
    let why = serde_json::from_str::<Leg>(&acts[0].input).unwrap().why;
    assert!(
        why.contains(&restarted) && why.contains("started again once"),
        "{why}"
    );
}
