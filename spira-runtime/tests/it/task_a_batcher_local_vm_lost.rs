//! sp-e30jhh: a round whose round VM is lost under its certify (`round-vm run: INCOMPLETE
//! reason=vm-lost`, `queue round certify` exit 75) is certified again on the same head with a
//! new VM, so the suites the lost pass finished resume. It is not judged red, abandoned or left
//! for attribution, which only an abandon and a re-cut (a new tree, no resume) got out of.

use runtime::units::{Exit, RanOutput, UnitHandle, UnitRan};
use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_793_000_000_000;

/// The repo `spira` certifying round `r9` at head `h9`, its certify leg `leg`.
fn certifying(leg: &str) -> Carry {
    let rc = RepoCarry { state: State::Certifying, round_key: Some("k".into()), batch: Some("r9".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "/w".into(), base: "b1".into(), leg: Some(leg.into()), leg_at_ms: NOW, run: 1, ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

/// The carried certify leg ended with exit 75, as `queue round certify` ends a pass whose VM died.
fn vm_lost(carry: &Carry) -> (Carry, Vec<Act>) {
    let leg = carry.repos["spira"].leg.clone().expect("a carried leg");
    let ran = UnitRan { handle: UnitHandle::of(NAME, &leg).unwrap(), exit: Exit::Code(75), duration_ms: 1 };
    on_ran_with(carry, &ran, &RanOutput::captured(b"", b"queue.sh round certify: certify rc=75: INCOMPLETE reason=vm-lost: the round VM died under the pass\n"), NOW).expect("the carried leg's end moves the machine")
}

fn names(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn a_round_whose_vm_is_lost_is_certified_again_on_the_same_head_never_abandoned() {
    let first = certifying("spira.r9.certify.1");
    let (again, acts) = vm_lost(&first);
    assert_eq!(names(&acts), [CERTIFY], "a lost VM re-certifies, it does not judge: {acts:?}");
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!((leg.batch.as_str(), leg.members.len(), leg.wt.as_str()), ("r9", 2, "/w"), "the same round, members and worktree (so the same head and tree)");
    let rc = &again.repos["spira"];
    assert_eq!(rc.state, State::Certifying);
    assert_ne!(rc.leg.as_deref(), first.repos["spira"].leg.as_deref(), "a new certify leg, so a new VM");

    // Lost again: once more on the same head.
    let (twice, acts) = vm_lost(&again);
    assert_eq!(names(&acts), [CERTIFY], "{acts:?}");

    // Past the budget the loss is the host's: no member is charged and the round stays open.
    let (spent, acts) = vm_lost(&twice);
    assert!(!names(&acts).contains(&ABANDON) && !names(&acts).contains(&EJECT) && !names(&acts).contains(&CERTIFY), "{acts:?}");
    let rc = spent.repos.get("spira").cloned().unwrap_or_default();
    assert_eq!((rc.state, rc.batch, rc.pace.infra), (State::Idle, None, 1), "an Infra fault, not a red");
}

#[test]
fn a_red_certify_is_still_judged_red() {
    let carry = certifying("spira.r9.certify.1");
    let ran = UnitRan { handle: UnitHandle::of(NAME, "spira.r9.certify.1").unwrap(), exit: Exit::Code(1), duration_ms: 1 };
    let (_, acts) = on_ran_with(&carry, &ran, &RanOutput::captured(b"verdict=red\n", b""), NOW).unwrap();
    assert!(names(&acts).contains(&ABANDON), "{acts:?}");
}
