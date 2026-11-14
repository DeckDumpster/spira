//! A round the attribution rebuilt is certified for the head it was rebuilt at, so a green there
//! lands it: the certify leg names that head and no head-following guard discards the leg.

use batcher_cut::attribute::Attribution;
use runtime::units::{Exit, RanOutput, UnitHandle, UnitRan};
use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_794_294_300_000;
const ATTRIBUTE: &str = "spira.r9.attribute2";
const OPENED: &str = "H1";
const REBUILT: &str = "H2";

fn attributing() -> Carry {
    let rc = RepoCarry {
        state: State::Attributing,
        round_key: Some("k".into()),
        batch: Some("r9".into()),
        members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into()), ("sp-c".into(), "t3".into())],
        wt: "/w".into(),
        base: "b1".into(),
        leg: Some(ATTRIBUTE.into()),
        leg_at_ms: NOW - 60_000,
        run: 2,
        head: OPENED.into(),
        leg_head: OPENED.into(),
        attributed: true,
        ..RepoCarry::default()
    };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn snapshot(head: &str) -> Snapshot {
    let round = Round { id: "r9".into(), members: vec!["sp-a".into(), "sp-b".into()], head: head.into(), worktree: "/w".into(), driven: true, phase: "certifying".into(), ..Round::default() };
    let repo = Repo { open_round: Some(round), base_sha: "b1".into(), live_legs: Some(vec![]), ..Repo::default() };
    Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms: NOW }
}

fn ran(unit: &str, exit: Exit) -> UnitRan {
    UnitRan { handle: UnitHandle::of(NAME, unit).unwrap(), exit, duration_ms: 1 }
}

fn land_line(head: &str) -> RanOutput {
    let land = Attribution::Land { members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], attribution_secs: None, mended: false, head: head.into() };
    RanOutput::captured(land.line().as_bytes(), b"")
}

fn certify_leg(acts: &[Act]) -> Leg {
    let act = acts.iter().find(|a| a.effect == CERTIFY).unwrap_or_else(|| panic!("no certify act: {acts:?}"));
    serde_json::from_str(&act.input).unwrap()
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn a_round_the_attribution_rebuilt_is_certified_for_the_rebuilt_head_and_its_green_lands() {
    let (held, acts) = on_ran_with(&attributing(), &ran(ATTRIBUTE, Exit::Code(0)), &land_line(REBUILT), NOW).expect("the attribution's own leg");
    assert!(acts.is_empty(), "{acts:?}");

    let (certifying, acts) = decide(&held, &snapshot(OPENED));
    let leg = certify_leg(&acts);
    assert_eq!(leg.head, REBUILT, "{acts:?}");

    let (before_write, acts) = decide(&certifying, &snapshot(OPENED));
    assert!(acts.is_empty(), "the store still names the pre-eject head: {acts:?}");
    assert_eq!(before_write.repos["spira"].leg.as_deref(), Some(leg.unit.as_str()));

    let (after_write, acts) = decide(&before_write, &snapshot(REBUILT));
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(after_write.repos["spira"].leg.as_deref(), Some(leg.unit.as_str()));

    for (carry, seen) in [(&before_write, OPENED), (&after_write, REBUILT)] {
        let head_of = move |_: &str, _: &str| Some(seen.to_string());
        let (landing, acts) = on_ran_seeing(carry, &ran(&leg.unit, Exit::Code(0)), &RanOutput::captured(b"", b""), NOW, &head_of).unwrap_or_else(|| panic!("a green certify seen at {seen} was discarded"));
        assert_eq!(effects(&acts), [LC_CERTIFY, LAND], "{acts:?}");
        assert_eq!(landing.repos["spira"].state, State::Landing);
    }
}

#[test]
fn a_certify_for_the_rebuilt_head_is_discarded_when_the_head_moves_to_another() {
    let (held, _) = on_ran_with(&attributing(), &ran(ATTRIBUTE, Exit::Code(0)), &land_line(REBUILT), NOW).unwrap();
    let (certifying, acts) = decide(&held, &snapshot(OPENED));
    let leg = certify_leg(&acts);
    let (moved, _) = decide(&certifying, &snapshot("H3"));
    assert_eq!(moved.repos["spira"].leg, None);
    let head_of = |_: &str, _: &str| Some("H3".to_string());
    assert!(on_ran_seeing(&certifying, &ran(&leg.unit, Exit::Code(0)), &RanOutput::captured(b"", b""), NOW, &head_of).is_none());
}

#[test]
fn a_certify_leg_names_no_head_when_the_attribution_did_not_rebuild_the_tree() {
    for head in ["", OPENED] {
        let (held, _) = on_ran_with(&attributing(), &ran(ATTRIBUTE, Exit::Code(0)), &land_line(head), NOW).unwrap();
        let (_, acts) = decide(&held, &snapshot(OPENED));
        assert_eq!(certify_leg(&acts).head, "", "{head:?}");
    }
}

#[test]
fn certify_argv_carries_rehead_only_for_a_leg_that_names_a_head() {
    let leg = Leg { batch: "r9".into(), repo: "spira".into(), head: REBUILT.into(), ..Leg::default() };
    assert_eq!(certify_argv("q", &leg), ["q", "round", "certify", "r9", "spira", "--rehead", REBUILT]);
    let bare = Leg { head: String::new(), ..leg };
    assert_eq!(certify_argv("q", &bare), ["q", "round", "certify", "r9", "spira"]);
}

#[test]
fn a_preempted_certify_leaves_the_rebuilt_head_for_the_certify_that_replaces_it() {
    let (held, _) = on_ran_with(&attributing(), &ran(ATTRIBUTE, Exit::Code(0)), &land_line(REBUILT), NOW).unwrap();
    let (certifying, acts) = decide(&held, &snapshot(OPENED));
    let leg = certify_leg(&acts);
    let preempted = Exit::Code(queue::ops::round::PREEMPTED);
    let (after, _) = on_ran_with(&certifying, &ran(&leg.unit, preempted), &RanOutput::captured(b"", b""), NOW).expect("its own leg");
    assert_eq!(after.repos["spira"].certify_head, REBUILT);
}
