//! A round whose last member has left ends quietly, from every state a pass runs in, and the
//! eject the batcher asks is one the queue CLI parses as the batcher's.

use spira_runtime::tasks::a::batcher_local::*;
use spira_runtime::tasks::a::kit::Act;

const NOW: u64 = 1_794_294_300_000;

fn carried(state: State) -> Carry {
    let rc = RepoCarry {
        state,
        round_key: Some("k".into()),
        batch: Some("r9".into()),
        members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())],
        wt: "w".into(),
        base: "b1".into(),
        leg: Some("spira.r9.corpus1".into()),
        leg_at_ms: NOW - 180_000,
        run: 1,
        head: "H".into(),
        leg_head: "H".into(),
        ..RepoCarry::default()
    };
    Carry {
        repos: [("spira".to_string(), rc)].into(),
    }
}

#[test]
fn the_eject_the_batcher_asks_is_one_the_queue_cli_parses_as_the_batchers_with_its_reason_on_stdin()
{
    let argv: Vec<String> = std::iter::once("round".to_string())
        .chain(batcher_cut::io::eject_args(
            "r9",
            "spira",
            "sp-a",
            "test-b.sh",
        ))
        .collect();
    match queue::cli::parse(&argv) {
        Ok(queue::cli::Cmd::Round(queue::cli::Round::Eject {
            by,
            batch,
            id,
            reason,
            ..
        })) => {
            assert_eq!(
                (by.as_str(), batch.as_str(), id.as_str()),
                ("batcher", "r9", "sp-a")
            );
            assert!(matches!(reason, queue::cli::Text::Stdin), "{reason:?}");
        }
        other => panic!("not an eject: {other:?}"),
    }
}

fn one_member(state: State) -> Carry {
    let mut carry = carried(state);
    carry.repos.get_mut("spira").unwrap().members = vec![("sp-a".into(), "t1".into())];
    carry
}

fn names(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn a_corpus_leg_ending_red_on_a_round_with_no_member_left_neither_attributes_nor_pauses() {
    let mut carry = one_member(State::Corpus);
    carry.repos.get_mut("spira").unwrap().members.clear();
    let ran = runtime::units::UnitRan {
        handle: runtime::units::UnitHandle::of(NAME, "spira.r9.corpus1").unwrap(),
        exit: runtime::units::Exit::Code(1),
        duration_ms: 1,
    };
    let (next, acts) = on_ran(&carry, &ran, NOW).expect("the carried leg ended");
    assert!(
        !names(&acts).contains(&CORPUS)
            && !names(&acts).contains(&PAUSE)
            && !names(&acts).contains(&INBOX_APPEND),
        "{acts:?}"
    );
    let rc = next.repos.get("spira").cloned().unwrap_or_default();
    assert_eq!((rc.state, rc.pace.infra), (State::Idle, 0));
}

#[test]
fn an_emptied_round_ends_quietly_from_every_state_a_pass_runs_in() {
    for (state, leg) in [
        (State::Attributing, "spira.r9.attribute1"),
        (State::Certifying, "spira.r9.certify1"),
    ] {
        let mut carry = one_member(state);
        let rc = carry.repos.get_mut("spira").unwrap();
        rc.leg = Some(leg.into());
        rc.members.clear();
        let ran = runtime::units::UnitRan {
            handle: runtime::units::UnitHandle::of(NAME, leg).unwrap(),
            exit: runtime::units::Exit::Code(1),
            duration_ms: 1,
        };
        let (next, acts) = on_ran(&carry, &ran, NOW).expect("the carried leg ended");
        assert_eq!(
            next.repos.get("spira").cloned().unwrap_or_default().state,
            State::Idle,
            "{state:?}"
        );
        assert!(
            !names(&acts).contains(&PAUSE) && !names(&acts).contains(&INBOX_APPEND),
            "{state:?}: {acts:?}"
        );
    }
}
