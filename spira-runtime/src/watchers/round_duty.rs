//! ROUND DUE, ROUND RESULT, ROUND NO VERDICTS, ROUNDS STOPPED ON PURPOSE and ROUND STATE UNKNOWN
//! for the round owner. Each tick asks the world for one snapshot; the decision over snapshot and
//! carried state is `decide`, which does no IO.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.round-duty";
pub const TICK: EventKindTag = "round-duty.tick";
pub const STATE: EventKindTag = "round-duty.state";
pub const OBSERVE: &str = "round-duty.observe";
pub const NAME: &str = "round-duty";

const DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundDuty {
    pub interval_ms: u64,
    pub min: u64,
    pub max_open: u64,
    pub remind_secs: u64,
    pub idle_cut_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoundState {
    Open,
    None,
    #[default]
    Unknown,
}

/// What the world said this tick. `lines` is the line count of every result file now, `fresh`
/// the lines past the counts the tick was given.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub certified: u64,
    pub by_repo: String,
    pub stopped: String,
    pub state: RoundState,
    pub open: u64,
    pub lines: BTreeMap<String, u64>,
    pub fresh: Vec<(String, String)>,
}

/// Carried between ticks, as the body of the latest `round-duty.state` event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub last_due: u64,
    pub last_unknown: u64,
    pub last_c: Option<u64>,
    pub last_change: u64,
    pub last_stopped: String,
    pub lines: BTreeMap<String, u64>,
}

fn verdictless(line: &str) -> bool {
    let l = line.to_lowercase();
    ["no verdict", "no-verdict", "verdict-less", "verdictless"].iter().any(|k| l.contains(k))
}

/// The messages a snapshot calls for, and the state to carry into the next tick.
pub fn decide(cfg: &RoundDuty, carry: &Carry, snap: &Snapshot, now: u64) -> (Carry, Vec<String>) {
    let mut c = carry.clone();
    let mut say = vec![];
    c.lines = snap.lines.clone();
    for (round, line) in &snap.fresh {
        say.push(format!("ROUND RESULT {round}: {line}"));
        if verdictless(line) {
            say.push(format!("ROUND NO VERDICTS {round}: the round produced no verdicts — a fault of the round machinery (VM, mirror, host address), not of the candidates; fix it before cutting another"));
        }
    }
    let certified = snap.certified;
    if snap.stopped != c.last_stopped {
        let shown = if snap.stopped.is_empty() { "none" } else { snap.stopped.as_str() };
        say.push(format!("ROUNDS STOPPED ON PURPOSE: {shown} (world.sh status names owner and release condition) — not counted as due"));
        c.last_stopped = snap.stopped.clone();
    }
    if c.last_c != Some(certified) {
        c.last_c = Some(certified);
        c.last_change = now;
    }
    let idle = now.saturating_sub(c.last_change);
    if snap.state == RoundState::Unknown {
        if now.saturating_sub(c.last_unknown) >= cfg.remind_secs {
            say.push("ROUND STATE UNKNOWN: 'queue round status' gave no usable answer — cannot tell whether a round is running, so no round is called due".into());
            c.last_unknown = now;
        }
        return (c, say);
    }
    c.last_unknown = 0;
    let may_cut = snap.state == RoundState::None || (snap.state == RoundState::Open && snap.open < cfg.max_open);
    if may_cut && (certified >= cfg.min || (certified >= 1 && idle >= cfg.idle_cut_secs)) {
        if now.saturating_sub(c.last_due) >= cfg.remind_secs {
            say.push(format!("ROUND DUE: {certified} certified waiting [{}] (idle {idle}s), {} of {} rounds open — cut the next round", snap.by_repo, snap.open, cfg.max_open));
            c.last_due = now;
        }
    } else {
        c.last_due = 0;
    }
    (c, say)
}

impl RoundDuty {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("round-duty/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for RoundDuty {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let known = carried::<Carry>(view, &ev.stream, STATE).lines;
            let input = serde_json::to_string(&known).unwrap_or_default();
            return vec![
                Command::Probe(EffectSpec { name: OBSERVE.into(), input, deadline_ms: DEADLINE_MS }),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        if ev.kind.tag == EFFECT_TIMED_OUT {
            ctx.event().set("round_duty.skipped", "observe timed out");
            return vec![];
        }
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let snap = match (ev.kind.tag.as_str(), serde_json::from_str::<Snapshot>(&ev.kind.body)) {
            (EFFECT_SUCCEEDED, Ok(s)) => s,
            _ => Snapshot { certified: carry.last_c.unwrap_or(0), lines: carry.lines.clone(), stopped: carry.last_stopped.clone(), ..Snapshot::default() },
        };
        let now = ev.at / 1000;
        let (next, say) = decide(self, &carry, &snap, now);
        ctx.event().set("round_duty.said", say.len() as u64);
        let mut cmds: Vec<Command> = say.into_iter().map(|m| Command::Effect(EffectSpec { name: INBOX_APPEND.into(), input: format!("{} [watch:{NAME}] {m}", iso8601(now)), deadline_ms: DEADLINE_MS })).collect();
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RoundDuty {
        RoundDuty { interval_ms: 15_000, min: 1, max_open: 1, remind_secs: 600, idle_cut_secs: 900 }
    }

    fn snap(state: RoundState, certified: u64) -> Snapshot {
        Snapshot { certified, by_repo: "spira=1".into(), state, ..Snapshot::default() }
    }

    fn outcome(tag: EventKindTag) -> Vec<String> {
        use runtime::ports::VirtualClock;
        use runtime::{Engine, Executor, MemJournal, Outcome};
        let clock = VirtualClock::new(1_800_000_000_000);
        let mut e = Engine::open(MemJournal::new(), std::sync::Arc::new(clock.clone()), vec![cfg().registration()], 0).unwrap();
        let (id, stream, kind) = RoundDuty::boot();
        e.submit(id, stream, kind).unwrap();
        e.run_until_idle().unwrap();
        let observe = e.pending_effects().remove(0);
        let out = if tag == EFFECT_TIMED_OUT { Outcome::TimedOut } else { Outcome::Failed("lifecycle did not answer for spira".into()) };
        e.complete(&observe.key, out).unwrap();
        e.run_until_idle().unwrap();
        e.pending_effects().into_iter().filter(|p| p.spec.name == INBOX_APPEND).map(|p| p.spec.input).collect()
    }

    #[test]
    fn a_timed_out_observe_says_nothing_and_a_failed_one_is_unknown() {
        assert_eq!(outcome(EFFECT_TIMED_OUT), Vec::<String>::new(), "a stale observe left over from a restart read nothing of the world");
        let said = outcome(EFFECT_FAILED);
        assert!(said.len() == 1 && said[0].contains("ROUND STATE UNKNOWN"), "{said:?}");
    }

    #[test]
    fn a_free_slot_with_certified_work_is_due_once_per_reminder_window() {
        let (c, say) = decide(&cfg(), &Carry::default(), &snap(RoundState::None, 1), 1000);
        assert_eq!(say, ["ROUND DUE: 1 certified waiting [spira=1] (idle 0s), 0 of 1 rounds open — cut the next round"]);
        assert!(decide(&cfg(), &c, &snap(RoundState::None, 1), 1100).1.is_empty());
        assert_eq!(decide(&cfg(), &c, &snap(RoundState::None, 1), 1600).1.len(), 1);
    }

    #[test]
    fn an_unanswered_state_is_never_a_round_less_queue() {
        let (_, say) = decide(&cfg(), &Carry::default(), &snap(RoundState::Unknown, 5), 1000);
        assert_eq!(say.len(), 1);
        assert!(say[0].starts_with("ROUND STATE UNKNOWN"));
    }

    #[test]
    fn a_full_cap_suppresses_and_a_free_slot_counts_open_rounds() {
        let open = |n| Snapshot { open: n, ..snap(RoundState::Open, 1) };
        assert!(decide(&cfg(), &Carry::default(), &open(1), 1000).1.is_empty());
        let wide = RoundDuty { max_open: 2, ..cfg() };
        assert!(decide(&wide, &Carry::default(), &open(1), 1000).1[0].contains("1 of 2 rounds open"));
    }

    #[test]
    fn a_verdictless_result_raises_its_own_line_and_a_stop_is_announced_on_change_only() {
        let s = Snapshot { fresh: vec![("300".into(), "round 300: no verdict from any VM".into())], stopped: "spira".into(), ..snap(RoundState::None, 0) };
        let (c, say) = decide(&cfg(), &Carry::default(), &s, 1000);
        assert_eq!(say.iter().filter(|m| m.starts_with("ROUND NO VERDICTS 300")).count(), 1);
        assert!(say.iter().any(|m| m.starts_with("ROUNDS STOPPED ON PURPOSE: spira ")));
        assert!(decide(&cfg(), &c, &Snapshot { stopped: "spira".into(), ..snap(RoundState::None, 0) }, 1100).1.is_empty());
    }
}
