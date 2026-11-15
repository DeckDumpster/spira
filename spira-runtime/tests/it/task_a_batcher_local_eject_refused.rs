//! A refused eject ends the round Red with the verb's refusal as the reason, and a certify
//! killed inside a red fence is attributed from that fence, not run again.

use runtime::units::{Exit, RanOutput, UnitHandle, UnitRan};
use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_794_294_300_000;
const UNIT: &str = "spira.r9.certify.1";

fn certifying(wt: &str) -> Carry {
    let rc = RepoCarry {
        state: State::Certifying,
        round_key: Some("k".into()),
        batch: Some("r9".into()),
        members: vec![("sp-a".into(), "t1".into())],
        wt: wt.into(),
        base: "b1".into(),
        leg: Some(UNIT.into()),
        leg_at_ms: NOW,
        run: 1,
        ..RepoCarry::default()
    };
    Carry {
        repos: [("spira".to_string(), rc)].into(),
    }
}

fn names(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn a_refused_eject_ends_the_round_red_with_the_refusal_and_runs_nothing_else() {
    let carry = certifying("/w");
    let leg = Leg {
        bead: "sp-a".into(),
        batch: "r9".into(),
        reds: vec!["fence-x".into()],
        ..Leg::on("spira")
    };
    let said = "--suites names gate which testenv does not know — nothing changed";
    let (next, acts) = on_outcome(&carry, EJECT, false, said, &leg, NOW);
    let abandon: Leg = serde_json::from_str(
        &acts
            .iter()
            .find(|a| a.effect == ABANDON)
            .expect("abandon")
            .input,
    )
    .unwrap();
    assert!(abandon.why.contains(said), "{}", abandon.why);
    assert!(
        !names(&acts).contains(&CERTIFY) && !names(&acts).contains(&CORPUS),
        "{acts:?}"
    );
    let rc = &next.repos["spira"];
    assert_eq!(
        (rc.state, rc.paused_on.is_some()),
        (State::Idle, true),
        "a Red pause"
    );
}

#[test]
fn a_certify_killed_inside_a_red_fence_attributes_from_that_fence_not_a_fresh_pass() {
    let dir = testkit::TempDir::new("eject-refused");
    let wt = dir.path().to_path_buf();
    let results = round_results(wt.to_str().unwrap(), UNIT);
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(results.join("fence-round-image-preflight.result"), "red\n").unwrap();
    let carry = certifying(wt.to_str().unwrap());
    let ran = UnitRan {
        handle: UnitHandle::of(NAME, UNIT).unwrap(),
        exit: Exit::Code(137),
        duration_ms: 1,
    };
    let (_, acts) = on_ran_with(&carry, &ran, &RanOutput::captured(b"", b"killed\n"), NOW).unwrap();
    let leg: Leg = serde_json::from_str(
        &acts
            .iter()
            .find(|a| a.effect == CORPUS)
            .expect("an attribute leg")
            .input,
    )
    .unwrap();
    assert_eq!(leg.after_pass, results.display().to_string());
}

#[test]
fn a_certify_that_printed_a_refused_eject_ends_the_round_red_and_runs_nothing_else() {
    let carry = certifying("/w");
    let ran = UnitRan { handle: UnitHandle::of(NAME, UNIT).unwrap(), exit: Exit::Code(1), duration_ms: 1 };
    let said = b"eject-refused=sp-a: eject for the red fence-round-image-preflight refused (rc=1)\n";
    let (next, acts) = on_ran_with(&carry, &ran, &RanOutput::captured(said, b""), NOW).unwrap();
    let abandon: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ABANDON).expect("abandon").input).unwrap();
    assert!(abandon.why.contains("sp-a: eject for the red fence-round-image-preflight refused"), "{}", abandon.why);
    assert!(!names(&acts).contains(&CERTIFY) && !names(&acts).contains(&CORPUS), "{acts:?}");
    assert!(next.repos["spira"].paused_on.is_some(), "a Red pause");
}
