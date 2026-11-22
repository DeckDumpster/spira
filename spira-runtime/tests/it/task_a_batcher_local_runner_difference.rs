//! A red tier that is green on the host it last ran green on is a runner difference: the
//! attribution leg says so, no member is charged, and the round is certified again with its
//! next pass pinned to the host (`queue round certify --place host`), once.

use batcher_cut::attribute::Attribution;
use runtime::units::{Exit, RanOutput, UnitHandle, UnitRan};
use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_793_000_000_000;

fn attributing(leg: &str) -> Carry {
    let rc = RepoCarry { state: State::Attributing, round_key: Some("k".into()), batch: Some("r9".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "w".into(), base: "b1".into(), leg: Some(leg.into()), leg_at_ms: NOW, run: 1, ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn difference_line() -> String {
    Attribution::RunnerDifference { tests: vec!["FAIL testenv::test_sandbox around_x".into()], red_on: "ec2".into(), green_on: "host".into() }.line()
}

fn attribution_ended(carry: &Carry, out: &str) -> (Carry, Vec<Act>) {
    let leg = carry.repos["spira"].leg.clone().expect("a carried leg");
    let ran = UnitRan { handle: UnitHandle::of(NAME, &leg).unwrap(), exit: Exit::Code(0), duration_ms: 1 };
    on_ran_with(carry, &ran, &RanOutput::captured(out.as_bytes(), b""), NOW).expect("the carried leg's end moves the machine")
}

fn argv_of(act: &Act) -> Vec<String> {
    let leg: Leg = serde_json::from_str(&act.input).unwrap();
    certify_argv("queue", &leg)
}

#[test]
fn a_runner_difference_moves_attributing_to_certifying_and_pins_that_certify_to_the_host() {
    let (next, acts) = attribution_ended(&attributing("spira.r9.attribute1"), &difference_line());
    let rc = &next.repos["spira"];
    assert_eq!(rc.state, State::Certifying, "{acts:?}");
    assert_eq!(rc.members.len(), 2, "no member is ejected");
    let certify: Vec<&Act> = acts.iter().filter(|a| a.effect == CERTIFY).collect();
    assert_eq!(certify.len(), 1, "{acts:?}");
    let argv = argv_of(certify[0]);
    assert!(argv.windows(2).any(|w| w[0] == "--place" && w[1] == "host"), "{argv:?}");
    assert!(!acts.iter().any(|a| a.effect == ABANDON || a.effect == EJECT), "{acts:?}");
}

#[test]
fn the_host_pin_is_for_one_certify_and_the_pass_after_it_uses_the_ordinary_rule() {
    let (next, _) = attribution_ended(&attributing("spira.r9.attribute1"), &difference_line());
    let leg = next.repos["spira"].leg.clone().unwrap();
    let ran = UnitRan { handle: UnitHandle::of(NAME, &leg).unwrap(), exit: Exit::Code(75), duration_ms: 1 };
    let (_, acts) = on_ran_with(&next, &ran, &RanOutput::captured(b"", b"queue.sh round certify: certify rc=75: INCOMPLETE reason=vm-lost\n"), NOW).unwrap();
    let certify: Vec<&Act> = acts.iter().filter(|a| a.effect == CERTIFY).collect();
    assert_eq!(certify.len(), 1, "{acts:?}");
    assert!(!argv_of(certify[0]).contains(&"--place".to_string()), "{:?}", argv_of(certify[0]));
}

#[test]
fn a_certify_not_after_a_runner_difference_carries_no_place() {
    let leg = Leg { batch: "r9".into(), repo: "spira".into(), ..Leg::default() };
    assert!(!certify_argv("queue", &leg).contains(&"--place".to_string()));
}
