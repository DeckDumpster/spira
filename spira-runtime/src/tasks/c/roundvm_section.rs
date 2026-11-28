//! # roundvm.section
//!
//! Publishes the `VmPool/pool` section from inside the scheduler, in process: the `cert-pass`
//! rows, the pool state file and the store over the scheduler's own `LcClient`, through the
//! `round_vm::section` library. It starts no program.
//!
//! The first tick is [`BOOT_DELAY_MS`] after start, then every [`CADENCE_MS`]. A tick measures
//! the reading (the observe probe, memory only); the publish is asked for only when the reading
//! differs from the last one published or that publish is older than half the section's
//! `MAX_AGE`, so an idle pool writes the store four times an hour, not twelve. The share and
//! slot judgement is `round_vm::section::measure`'s.

use runtime::event::{Command, Ctx, Event, EventKind, EventKindTag, Handler, Projection, Registration};
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{self, Act, Cadence, Cfg, Decision, Input, Spec, Stub};
use round_vm::section::{Reading, MAX_AGE_SECS};

pub const NAME: &str = "roundvm.section";
pub const STREAM: &str = "task.roundvm.section";
pub const TICK: EventKindTag = "roundvm.section.tick";
pub const STATE: EventKindTag = "roundvm.section.state";

pub const OBSERVE: &str = "roundvm.section.observe";
pub const PUBLISH: &str = "roundvm.section.publish";
pub const EFFECTS: &[&str] = &[OBSERVE, PUBLISH];
pub const KEYS: &[&str] = &[];

pub const BOOT_DELAY_MS: u64 = 60_000;
pub const CADENCE_MS: u64 = 300_000;
pub const REPUBLISH_AFTER_MS: u64 = MAX_AGE_SECS * 1000 / 2;

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: "", calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None, jitter_key: None };

pub type Snapshot = Reading;

/// The reading last published and when (the scheduler's clock, ms).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub published: Option<Reading>,
    pub published_at: Option<u64>,
}

pub struct RoundvmSection {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

impl RoundvmSection {
    pub fn new() -> Self {
        RoundvmSection { cadence: Cadence::Every(CADENCE_MS), cfg: Cfg::new() }
    }
}

impl Default for RoundvmSection {
    fn default() -> Self {
        Self::new()
    }
}

/// Asks for the publish when `reading` is not the one the store holds from a publish younger
/// than [`REPUBLISH_AFTER_MS`]. The carry moves only on a publish that succeeded, so a refused
/// write (a section that moved under `--expect-rev`) is asked again on the next tick.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(reading) => {
            let current = carry.published.as_ref() == Some(reading) && carry.published_at.is_some_and(|at| now_ms.saturating_sub(at) < REPUBLISH_AFTER_MS);
            let acts = if current { vec![] } else { vec![Act { name: PUBLISH.into(), key: "VmPool/pool".into(), input: String::new(), deadline_ms: 30_000, probe: false }] };
            Ok((carry.clone(), acts))
        }
        Input::Outcome { effect, ok, body } if effect == PUBLISH => {
            if !*ok {
                return Ok((carry.clone(), vec![]));
            }
            let reading: Reading = serde_json::from_str(body).map_err(|e| format!("{NAME}: the publish answered {body:?}: {e}"))?;
            Ok((Carry { published: Some(reading), published_at: Some(now_ms) }, vec![]))
        }
        Input::Outcome { effect, .. } => Err(format!("{NAME}: an outcome of {effect} is not expected")),
        Input::Lost(lost) => lost.as_failure(decide, cfg, carry, now_ms),
        Input::Ran(r) => Err(format!("{NAME}: no unit is expected, got {:?}", r.handle)),
    }
}

/// The runner of [`OBSERVE`]: the hour's reading, as JSON.
pub fn observe(now_secs: u64) -> Result<String, String> {
    let reading = round_vm::section::read(&round_vm::config::Config::load()?, now_secs)?;
    serde_json::to_string(&reading).map_err(|e| e.to_string())
}

/// The runner of [`PUBLISH`]: `round_vm::section::publish` over the scheduler's store client;
/// answers with the reading it published.
pub fn publish(lc: spira_store::lc_call::LcClient, now_secs: u64) -> Result<String, String> {
    let reading = round_vm::section::publish(&round_vm::config::Config::load()?, &round_vm::section::SpiraLc(lc), now_secs)?;
    serde_json::to_string(&reading).map_err(|e| e.to_string())
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("published"), runtime::rows::Part::Field("published_at")];

impl Stub for RoundvmSection {
    type Snapshot = Snapshot;
    type Carry = Carry;
    const SPEC: Spec = SPEC;
    const ROWS: &'static [runtime::rows::Part] = ROW_PARTS;

    fn from_cfg(_: &dyn Fn(&str) -> Result<String, String>) -> Result<Self, String> {
        Ok(Self::new())
    }

    fn cadence(&self) -> &Cadence {
        &self.cadence
    }

    fn cfg(&self) -> &Cfg {
        &self.cfg
    }

    fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
        decide(cfg, carry, input, now_ms)
    }

    fn catch_up(_: &Carry, _: u64, _: &Cadence) -> Option<String> {
        None
    }
}

impl RoundvmSection {
    pub fn boot() -> (&'static str, &'static str, EventKind) {
        (NAME, STREAM, EventKind::new(TICK, kit::BOOT))
    }

    pub fn registration(self) -> Registration {
        <Self as Stub>::registration(self)
    }
}

impl Handler for RoundvmSection {
    fn interests(&self) -> &[EventKindTag] {
        static INTERESTS: [EventKindTag; 5] = kit::interests(TICK);
        &INTERESTS
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 == STREAM && ev.kind.tag == TICK && ev.kind.body == kit::BOOT {
            return vec![Command::Timer { at: ev.at + BOOT_DELAY_MS, kind: EventKind::new(TICK, kit::FIRE) }];
        }
        kit::handle(self, ctx, view, ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_793_664_000_000;

    fn reading(passes: u64) -> Reading {
        Reading { passes, infra_incomplete_share: 0.0, over_cap_share: 0.0, slots_in_use: vec![("proxmox".into(), 0), ("ec2".into(), 0)] }
    }

    fn published(r: Reading, ago_ms: u64) -> Carry {
        Carry { published: Some(r), published_at: Some(NOW - ago_ms) }
    }

    fn asks(carry: &Carry, r: Reading) -> bool {
        let (next, acts) = decide(&Cfg::new(), carry, &Input::Observed(r), NOW).unwrap();
        assert_eq!(&next, carry, "an observation never moves the carry");
        acts.iter().all(|a| a.name == PUBLISH) && !acts.is_empty()
    }

    #[test_id(tqkh379)]
    #[test]
    fn an_unchanged_reading_inside_fifteen_minutes_writes_nothing() {
        assert!(!asks(&published(reading(3), 14 * 60_000), reading(3)));
    }

    #[test_id(twufvbj)]
    #[test]
    fn a_changed_reading_writes() {
        assert!(asks(&published(reading(3), 60_000), reading(4)));
        assert!(asks(&Carry::default(), reading(0)));
    }

    #[test_id(tttlwhc)]
    #[test]
    fn an_unchanged_reading_past_fifteen_minutes_writes() {
        assert!(asks(&published(reading(3), 15 * 60_000), reading(3)));
    }

    #[test_id(tk6vahr)]
    #[test]
    fn a_refused_expect_rev_write_is_retried_on_the_next_tick() {
        let before = published(reading(3), 20 * 60_000);
        let (after, acts) = decide(&Cfg::new(), &before, &Input::Outcome { effect: PUBLISH.into(), ok: false, body: "exit 1: stale revision".into() }, NOW).unwrap();
        assert!(acts.is_empty());
        assert_eq!(after, before, "a refused write is not a publish");
        assert!(asks(&after, reading(3)));
        let (done, _) = decide(&Cfg::new(), &after, &Input::Outcome { effect: PUBLISH.into(), ok: true, body: serde_json::to_string(&reading(3)).unwrap() }, NOW).unwrap();
        assert_eq!(done, Carry { published: Some(reading(3)), published_at: Some(NOW) });
    }

    #[test_id(t7ss814)]
    #[test]
    fn the_first_tick_is_a_minute_after_boot_then_every_five_minutes() {
        use runtime::taskrig::TaskRig;
        let mut rig = TaskRig::new(1, RoundvmSection::new().registration()).unwrap();
        let (_, stream, kind) = RoundvmSection::boot();
        rig.tick(stream, kind).unwrap();
        assert_eq!(rig.engine().next_timer_at(), Some(BOOT_DELAY_MS));
        assert!(rig.requested().is_empty(), "nothing is asked before the first tick");
        rig.advance(BOOT_DELAY_MS).unwrap();
        assert_eq!(rig.engine().next_timer_at(), Some(BOOT_DELAY_MS + CADENCE_MS));
    }
}
