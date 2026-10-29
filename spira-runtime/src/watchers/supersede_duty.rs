//! SUPERSEDE REQUEST lines for the Concierge inbox, and the adjudication of each request: the
//! successor LANDED supersedes the bead, any other successor state lifts the hold. Each tick asks
//! the world what every manual hold reads as; the decision over that is `decide`, which does no
//! IO. The writes it calls for leave as `supersede-duty.act` effects, whose outcomes come back
//! here to be reported.

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.supersede-duty";
pub const TICK: EventKindTag = "supersede-duty.tick";
pub const STATE: EventKindTag = "supersede-duty.state";
pub const FAILED: EventKindTag = "supersede-duty.failed";
pub const OBSERVE: &str = "supersede-duty.observe";
pub const ACT: &str = "supersede-duty.act";
pub const NAME: &str = "supersede-duty";
pub const ACTOR: &str = "supersede-duty";
/// What an act effect answers when the bead became terminal under it: nothing to report.
pub const SETTLED: &str = "terminal";
/// What an act effect answers when the write took but the evidence note did not.
pub const NOTE_FAILED: &str = "note-failed";

const OBSERVE_DEADLINE_MS: u64 = 30_000;
const ACT_DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupersedeDuty {
    pub interval_ms: u64,
}

/// What a bead's latest manual hold reads as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reading {
    /// Not read: the bead's version is one already known to hold something else.
    Skipped,
    NotARequest,
    Unreadable,
    Request {
        succ: String,
        own: String,
        succ_state: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub id: String,
    pub version: String,
    pub reading: Reading,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub held: Vec<Held>,
}

/// Carried between ticks: the version at which a bead was read as holding something other than a
/// request (the body of the latest `supersede-duty.state`), and the beads whose failure was already
/// reported (one `supersede-duty.failed` event each, so writes settling in one batch lose none).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub not_a_request: BTreeMap<String, String>,
    pub failed: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verb {
    Unhold,
    Supersede,
}

/// One write: `note` is the evidence left on the bead, `done` the line said once it took, `fail`
/// the reason said (once per bead) when it did not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Act {
    pub id: String,
    pub succ: String,
    pub verb: Verb,
    pub note: String,
    pub done: String,
    pub fail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    Say(String),
    Act(Act),
}

/// The leading capitals of a state read: `LANDED`, and not the words after it.
pub fn terminal(state: &str) -> bool {
    let head: String = state.chars().take_while(char::is_ascii_uppercase).collect();
    matches!(head.as_str(), "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE")
}

/// The successor named by the bead's standing supersede-request, read from its history rows;
/// `Ok(None)` when its latest operator hold is anything else, `Err` when the rows are unreadable.
pub fn request_in(history: &str) -> Result<Option<String>, String> {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(history).map_err(|e| format!("history unparsed: {e}"))?;
    let applied = |r: &serde_json::Value| {
        matches!(&r["applied"], serde_json::Value::String(s) if s == "1" || s == "true")
            || r["applied"] == 1
    };
    let mut by = None;
    for r in rows.iter().filter(|r| applied(r)) {
        let Some(hold) = r["evidence"]
            .as_str()
            .and_then(|e| serde_json::from_str::<serde_json::Value>(e).ok())
            .map(|e| e["Hold"].clone())
        else {
            continue;
        };
        if hold.as_object().is_some_and(|h| !h.is_empty())
            && matches!(
                hold["kind"].as_str().map(str::to_lowercase).as_deref(),
                Some("manual" | "operator")
            )
        {
            by = (hold["cause"] == "supersede-request")
                .then(|| hold["detail"].as_str().unwrap_or("").to_string())
                .filter(|d| !d.is_empty());
        }
    }
    Ok(by)
}

fn adjudicate(carry: &Carry, id: &str, succ: &str, own: &str, st: &str) -> Vec<Out> {
    let mut out = vec![];
    if !carry.failed.contains(id) {
        out.push(Out::Say(format!(
            "SUPERSEDE REQUEST {id}: successor {succ}"
        )));
    }
    let say = |m: String| format!("SUPERSEDE REQUEST {id}: {m}");
    let act = |verb, fail: &str, note: String, done: String| {
        Out::Act(Act {
            id: id.into(),
            succ: succ.into(),
            verb,
            note,
            done: say(done),
            fail: fail.into(),
        })
    };
    out.push(if succ == id {
        act(Verb::Unhold, "unhold failed", format!("supersede-request names the bead itself ({own}): a bead cannot supersede itself. Hold lifted; a merged pull request is recorded LANDED by the landing pass, not by this request."), format!("refused, the successor is the bead itself ({own}); unheld"))
    } else if st.is_empty() {
        act(Verb::Unhold, "unhold failed", format!("supersede-request by {succ} refused: no lifecycle row for {succ}. Hold lifted; the bead proceeds."), format!("refused, successor {succ} has no usable row; unheld"))
    } else if st == "LANDED" {
        act(Verb::Supersede, "supersede failed", format!("supersede-request confirmed: {succ} is LANDED. Superseded by {succ}."), format!("confirmed, superseded by LANDED {succ}"))
    } else {
        act(Verb::Unhold, "unhold failed", format!("supersede-request by {succ} refused: {succ} is {st}, not LANDED. Hold lifted; the bead proceeds."), format!("refused, {succ} is {st}; unheld"))
    });
    out
}

/// The lines and writes a snapshot calls for, and the state to carry into the next tick.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Out>) {
    let mut c = carry.clone();
    let mut out = vec![];
    for h in &snap.held {
        if c.not_a_request.get(&h.id) == Some(&h.version) {
            continue;
        }
        match &h.reading {
            Reading::NotARequest => {
                c.not_a_request.insert(h.id.clone(), h.version.clone());
            }
            Reading::Request {
                succ,
                own,
                succ_state,
            } if !own.is_empty() && !terminal(own) => {
                out.extend(adjudicate(&c, &h.id, succ, own, succ_state))
            }
            _ => {}
        }
    }
    (c, out)
}

/// The lines an act's outcome calls for, and whether it is the bead's first reported failure.
pub fn settle(carry: &Carry, act: &Act, outcome: Result<&str, ()>) -> (bool, Vec<String>) {
    match outcome {
        Ok(SETTLED) => (false, vec![]),
        Ok(detail) => {
            let mut say = vec![];
            if detail == NOTE_FAILED {
                say.push(format!("NOTE FAILED {}: {}", act.id, act.note));
            }
            say.push(act.done.clone());
            (false, say)
        }
        Err(()) if carry.failed.contains(&act.id) => (false, vec![]),
        Err(()) => (
            true,
            vec![format!(
                "SUPERSEDE REQUEST {}: {} — will retry",
                act.id, act.fail
            )],
        ),
    }
}

impl SupersedeDuty {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("supersede-duty/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn carry(view: &Projection, stream: &StreamKey) -> Carry {
        let events = view.events(stream);
        let not_a_request = events
            .iter()
            .rev()
            .find(|e| e.kind.tag == STATE)
            .and_then(|e| serde_json::from_str(&e.kind.body).ok())
            .unwrap_or_default();
        Carry {
            not_a_request,
            failed: events
                .iter()
                .filter(|e| e.kind.tag == FAILED)
                .map(|e| e.kind.body.clone())
                .collect(),
        }
    }

    /// The effect request an outcome event answers: its spec's name and input.
    fn request_of(view: &Projection, ev: &Event) -> Option<(String, String)> {
        let cause = ev.cause.as_ref()?;
        let req = view
            .events(&ev.stream)
            .iter()
            .rev()
            .find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED)?;
        let body = serde_json::from_str::<serde_json::Value>(&req.kind.body).ok()?;
        Some((
            body["spec"]["name"].as_str()?.to_string(),
            body["spec"]["input"].as_str()?.to_string(),
        ))
    }

    fn inbox(now: u64, m: &str) -> Command {
        Command::Effect(EffectSpec {
            name: INBOX_APPEND.into(),
            input: format!("{} [watch:{NAME}] {m}", iso8601(now)),
            deadline_ms: ACT_DEADLINE_MS,
        })
    }
}

impl Handler for SupersedeDuty {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry = Self::carry(view, &ev.stream);
        if ev.kind.tag == TICK {
            let input = serde_json::to_string(&carry.not_a_request).unwrap_or_default();
            return vec![
                Command::Effect(EffectSpec {
                    name: OBSERVE.into(),
                    input,
                    deadline_ms: OBSERVE_DEADLINE_MS,
                }),
                Command::Schedule {
                    at: ev.at + self.interval_ms,
                    kind: EventKind::new(TICK, ""),
                },
            ];
        }
        let Some((name, input)) = Self::request_of(view, ev) else {
            return vec![];
        };
        let now = ev.at / 1000;
        match name.as_str() {
            OBSERVE if ev.kind.tag == EFFECT_SUCCEEDED => {
                let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else {
                    return vec![];
                };
                let (next, out) = decide(&carry, &snap);
                ctx.event()
                    .set("supersede_duty.held", snap.held.len() as u64);
                let mut cmds = vec![Command::Emit(EventKind::new(
                    STATE,
                    &serde_json::to_string(&next.not_a_request).unwrap_or_default(),
                ))];
                for o in out {
                    cmds.push(match o {
                        Out::Say(m) => Self::inbox(now, &m),
                        Out::Act(a) => Command::Effect(EffectSpec {
                            name: ACT.into(),
                            input: serde_json::to_string(&a).unwrap_or_default(),
                            deadline_ms: ACT_DEADLINE_MS,
                        }),
                    });
                }
                cmds
            }
            ACT => {
                let Ok(act) = serde_json::from_str::<Act>(&input) else {
                    return vec![];
                };
                let outcome = if ev.kind.tag == EFFECT_SUCCEEDED {
                    Ok(ev.kind.body.as_str())
                } else {
                    Err(())
                };
                let (first_failure, say) = settle(&carry, &act, outcome);
                let mut cmds = vec![];
                if first_failure {
                    cmds.push(Command::Emit(EventKind::new(FAILED, &act.id)));
                }
                cmds.extend(say.iter().map(|m| Self::inbox(now, m)));
                cmds
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, succ: &str, own: &str, st: &str) -> Held {
        Held {
            id: id.into(),
            version: "3".into(),
            reading: Reading::Request {
                succ: succ.into(),
                own: own.into(),
                succ_state: st.into(),
            },
        }
    }

    fn only_act(out: &[Out]) -> &Act {
        match out.last() {
            Some(Out::Act(a)) => a,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_landed_successor_supersedes_and_any_other_state_lifts_the_hold() {
        let go = |succ: &str, st: &str| {
            decide(
                &Carry::default(),
                &Snapshot {
                    held: vec![request("sp-a", succ, "WORKING", st)],
                },
            )
            .1
        };
        let out = go("sp-b", "LANDED");
        assert_eq!(
            out[0],
            Out::Say("SUPERSEDE REQUEST sp-a: successor sp-b".into())
        );
        assert_eq!(
            (only_act(&out).verb, only_act(&out).done.as_str()),
            (
                Verb::Supersede,
                "SUPERSEDE REQUEST sp-a: confirmed, superseded by LANDED sp-b"
            )
        );
        assert_eq!(
            only_act(&go("sp-b", "WORKING")).done,
            "SUPERSEDE REQUEST sp-a: refused, sp-b is WORKING; unheld"
        );
        assert_eq!(
            only_act(&go("sp-b", "")).done,
            "SUPERSEDE REQUEST sp-a: refused, successor sp-b has no usable row; unheld"
        );
        assert_eq!(only_act(&go("sp-a", "WORKING")).verb, Verb::Unhold);
    }

    #[test]
    fn the_latest_operator_hold_decides_whether_a_request_stands() {
        let hold = |kind: &str, cause: &str, detail: &str, applied: &str| {
            format!(
                r#"{{"applied":"{applied}","evidence":"{{\"Hold\":{{\"kind\":\"{kind}\",\"cause\":\"{cause}\",\"detail\":\"{detail}\"}}}}"}}"#
            )
        };
        let rows = |hs: &[String]| format!("[{}]", hs.join(","));
        assert_eq!(
            request_in(&rows(&[hold("manual", "supersede-request", "sp-b", "1")])),
            Ok(Some("sp-b".into()))
        );
        assert_eq!(
            request_in(&rows(&[
                hold("manual", "supersede-request", "sp-b", "1"),
                hold("manual", "ask", "q", "1")
            ])),
            Ok(None)
        );
        assert_eq!(
            request_in(&rows(&[
                hold("manual", "supersede-request", "sp-b", "1"),
                hold("wait", "x", "y", "1")
            ])),
            Ok(Some("sp-b".into())),
            "other kinds of hold do not displace it"
        );
        assert_eq!(
            request_in(&rows(&[hold(
                "Operator",
                "supersede-request",
                "sp-b",
                "true"
            )])),
            Ok(Some("sp-b".into()))
        );
        assert_eq!(
            request_in(&rows(&[hold("manual", "supersede-request", "sp-b", "0")])),
            Ok(None),
            "an unapplied event is not a hold"
        );
        assert!(request_in("not json").is_err());
    }

    #[test]
    fn a_bead_already_terminal_or_without_a_row_is_left_alone() {
        for own in ["", "LANDED", "SUPERSEDED x", "DROPPED"] {
            assert!(
                decide(
                    &Carry::default(),
                    &Snapshot {
                        held: vec![request("sp-a", "sp-b", own, "LANDED")]
                    }
                )
                .1
                .is_empty(),
                "{own:?}"
            );
        }
        assert!(
            terminal("LANDED (tip abc)") && !terminal("LANDEDX1") && !terminal("REWORK"),
            "only the leading capitals count"
        );
    }

    #[test]
    fn a_hold_read_as_something_else_is_not_read_again_until_its_version_moves() {
        let other = |v: &str| Snapshot {
            held: vec![Held {
                id: "sp-a".into(),
                version: v.into(),
                reading: Reading::NotARequest,
            }],
        };
        let (c, _) = decide(&Carry::default(), &other("1"));
        let again = Snapshot {
            held: vec![request("sp-a", "sp-b", "WORKING", "LANDED")],
        };
        assert!(decide(
            &c,
            &Snapshot {
                held: vec![Held {
                    version: "1".into(),
                    ..again.held[0].clone()
                }]
            }
        )
        .1
        .is_empty());
        assert!(
            !decide(&c, &again).1.is_empty(),
            "a new version is read afresh"
        );
    }

    #[test]
    fn an_unreadable_hold_says_nothing_and_is_read_again_next_tick() {
        let held = |id: &str, reading| Held { id: id.into(), version: "1".into(), reading };
        let snap = Snapshot { held: vec![held("sp-a", Reading::Unreadable), held("sp-b", Reading::Skipped)] };
        let (c, out) = decide(&Carry::default(), &snap);
        assert!(out.is_empty() && c == Carry::default(), "{c:?} {out:?}");
    }

    #[test]
    fn a_failed_write_is_reported_once_and_silences_the_successor_line_after() {
        let snap = Snapshot {
            held: vec![request("sp-a", "sp-b", "WORKING", "LANDED")],
        };
        let (_, out) = decide(&Carry::default(), &snap);
        let (first, say) = settle(&Carry::default(), only_act(&out), Err(()));
        assert!(first);
        assert_eq!(
            say,
            ["SUPERSEDE REQUEST sp-a: supersede failed — will retry"]
        );
        let c = Carry {
            failed: ["sp-a".to_string()].into(),
            ..Carry::default()
        };
        assert!(settle(&c, only_act(&out), Err(())).1.is_empty());
        let (_, retry) = decide(&c, &snap);
        assert!(matches!(retry.as_slice(), [Out::Act(_)]), "{retry:?}");
    }

    #[test]
    fn a_bead_that_went_terminal_under_a_failing_write_says_nothing() {
        let (_, out) = decide(
            &Carry::default(),
            &Snapshot {
                held: vec![request("sp-a", "sp-b", "WORKING", "LANDED")],
            },
        );
        assert!(settle(&Carry::default(), only_act(&out), Ok(SETTLED))
            .1
            .is_empty());
        assert_eq!(
            settle(&Carry::default(), only_act(&out), Ok(NOTE_FAILED))
                .1
                .len(),
            2
        );
    }
}
