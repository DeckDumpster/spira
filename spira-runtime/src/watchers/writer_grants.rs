//! WRITER GRANTS for the Concierge inbox: lc-serve's content writer lacks a write grant the code
//! needs, or the grants cannot be read. Each tick asks lc-serve for the verdict its startup check
//! kept; the decision over that and the keys already said is `decide`, which does no IO.

use std::collections::BTreeSet;

use runtime::event::*;
use runtime::rows::Part;
use serde::{Deserialize, Serialize};

use super::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.writer-grants";
pub const TICK: EventKindTag = "writer-grants.tick";
pub const STATE: EventKindTag = "writer-grants.state";
pub const OBSERVE: &str = "writer-grants.observe";
pub const NAME: &str = "writer-grants";

const ROW_PARTS: &[Part] = &[Part::Field("said")];

const DEADLINE_MS: u64 = 10_000;
const CANNOT: &str = "cannot";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterGrants {
    pub interval_ms: u64,
}

/// The `writer-grants` verb's answer. `checked_at` is null before the server's startup check ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub checked_at: Option<u64>,
    pub writer: bool,
    pub missing: Vec<String>,
    pub error: Option<String>,
}

/// Carried between ticks: the failure modes now said and still true.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub said: BTreeSet<String>,
}

pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<String>) {
    if snap.checked_at.is_none() {
        return (carry.clone(), vec![]);
    }
    let mut lines: Vec<(String, String)> = snap.missing.iter().map(|t| (format!("missing:{t}"), format!("WRITER GRANTS: the content writer lacks write grants on {t}: run spira-lc admin-provision-writer"))).collect();
    if let Some(e) = &snap.error {
        lines.push((CANNOT.into(), format!("WRITER GRANTS: lc-serve cannot check the content writer's grants: {e}")));
    }
    let said = lines.iter().map(|(k, _)| k.clone()).collect();
    let say = lines.into_iter().filter(|(k, _)| !carry.said.contains(k)).map(|(_, m)| m).collect();
    (Carry { said }, say)
}

impl WriterGrants {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("writer-grants/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for WriterGrants {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return vec![
                Command::Probe(EffectSpec::probe(OBSERVE, String::new(), DEADLINE_MS)),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body).map_err(drop).and_then(|s| if ev.kind.tag == EFFECT_SUCCEEDED { Ok(s) } else { Err(()) }) else {
            ctx.event().set("writer_grants.skipped", ev.kind.tag.as_str());
            return vec![];
        };
        let now = ev.at / 1000;
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let (next, say) = decide(&carry, &snap);
        ctx.event().set("writer_grants.said", say.len() as u64);
        let mut cmds: Vec<Command> = say.into_iter().map(|m| Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:inbox:{}:{:016x}", now, runtime::event::fnv1a(&m)), format!("{} [watch:{NAME}] {m}", iso8601(now)), DEADLINE_MS))).collect();
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        cmds
    }
}
