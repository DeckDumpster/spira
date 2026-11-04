//! # cert.sample — contract
//!
//! This replaces the `spira-cert-sweep-sample-prod` unit. That unit runs `cert-sweep pass --mode subset --repo ~/spira/harness --tree run/cert-sweep/tree-sample --branch cert-sweep/tip --repo-name spira --deadline 1200`, appends its output to `run/cert-sweep.log`, and has TimeoutStartSec=1500. The task is a P4 tick that drives a P2 machine. It is part of [[scheduler-consolidation-2026-11-03]], and it shares its decision core with [[cert-full]].
//!
//! Catalogue differs: the catalogue calls this P4. The run is a `testenv` leg on the host, bounded at 1200 s, and every red adds 3 more of those legs as reruns (`cert-sweep/src/main.rs:430-438`). That makes it a P2 machine. Unlike full mode, subset mode never waits on the round VM and never checks out a tree; `--tree` is required but unused in subset (`main.rs:388-390`). Bisect probes also run as host `testenv` legs.
//!
//! ## Tick
//!
//! - Calendar `*-*-* *:07,37:00` (S8).
//! - Single-flight.
//! - At `:07` it fires together with [[cert-full]]. Each records under the same named history lock (`main.rs:159-165`).
//!
//! ## Observe
//!
//! `cert.sample.observe` returns `Snapshot { tip, round, suites, history }`. The sources are the same git reads and the same tsd `cert-history` read as [[cert-full]] (`main.rs:242-254`, `:146-148`). There is no tree-certified short-circuit and no lease read: the subset branch skips `main.rs:263-276`. Open beads are read only at the filing step, through the same `LcClient` target as in [[cert-full]].
//!
//! ## Decide
//!
//! The decide step reuses these `cert_sweep` functions:
//!
//! - `pick_subset(all, div=4, seed)` (`lib.rs:292`)
//! - `record` and `classify`
//! - `judge`
//! - `bisect`
//! - `open_duplicate`
//!
//! Today the subset seed is `now ^ pid << 32` (`main.rs:262`), so the pick cannot be reproduced. The task draws the seed from `Ctx::rand_u64()`, which is pinned per event, so T6 holds.
//!
//! The carry is `cert.sample.state = { phase, tip, round, picks, pending, reruns, bisect, held, filed }`.
//!
//! | State | Event | Next | Effect |
//! |---|---|---|---|
//! | Idle | tick, no suites | Idle | none |
//! | Idle | tick | Running | `cert.sample.branch`, then `unit.start` (testenv) |
//! | Running | `unit.ran` with results | Collect | `cert.sample.observe` (parsed testenv stdout, `parse_testenv_stdout`) |
//! | Running | `unit.ran`, no results, runner declined | Idle | none (`main.rs:282-285`) |
//! | Running | `unit.ran`, no results | Idle | `cert.sample.record` (SweepFault) |
//! | Collect | red events | Attribute | `unit.start` ×3 (reruns on the host) |
//! | Attribute | judged | File / Bisect | `unit.start` per probe |
//! | File | ≤5 filings, no duplicate | Record | `cert.sample.file_bead` |
//! | Record | done | Idle | `cert.sample.record`, then Summary |
//!
//! The fixtures are those in `cert-sweep/src/tests.rs`, including the `pick_subset` cases.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `cert.sample.observe` (probe) | git | 30 s | none | — |
//! | `cert.sample.branch` | git (`branch -f cert-sweep/tip <tip>`, `main.rs:582-583`) | 30 s | `cert.sample:branch:<tip>` | a branch name other than the configured one |
//! | `unit.start` (shared) | container (testenv runs podman; cap ≤2 per S3) | 1200 s (`--deadline`) | `cert.sample:<tip>:<seq>` | a second leg in flight; a full container class, which is deferred, not dropped |
//! | `cert.sample.file_bead` | store (`bead.sh file … --for builder --repo spira --priority 1 --json`, `main.rs:179-199`) | 60 s | `cert.sample:<suite>:<filing kind>:<tip>` | an open duplicate; a 6th filing |
//! | `cert.sample.record` | host (flocked tsd append, `main.rs:150-176`) | 10 s | `cert.sample:<tip>:<suite>:<seq>` | rows for a held suite |
//!
//! The leg is `testenv --deadline 1200 --suites - cert-sweep/tip spira`, with the picks on stdin (`main.rs:585-596`). It runs in a transient unit, and the unit's stop replaces `reap_unit_leftovers` (`:601`). The lines written are the `Event::render` texts listed in [[cert-full]], except that the summary reads `SAMPLE round={round} ran={n} red={n} fault={n} wall={s}s` (`lib.rs:196-198`). Nothing is written to the inbox.
//!
//! ## Silence
//!
//! A tick whose run is all green against a green history still records its rows; that is the sample's purpose. It journals no event lines beyond the Summary. A tick with no suites emits only the probe.
//!
//! ## Replaces
//!
//! - **Units:** `spira-cert-sweep-sample-prod.service` and `spira-cert-sweep-sample-prod.timer`.
//! - **Children:** `git`, which stays; `testenv` (`main.rs:585`); `bdq`; `bead.sh`; and the `spira-lc` call behind `lc_state`.
//! - **Pinned config:** the keys `SPIRA_RUN` and `SPIRA_REPO`, and the settings `subset-div 4`, `deadline 1200`, `branch cert-sweep/tip`, `repo-name spira`, `reruns 3`, `max-beads 5`.
//!
//! ## Reverse
//!
//! - A release rollback restores the unit.
//! - In Observe mode, `unit.start`, `file_bead` and `record` are shadowed.

use std::collections::BTreeMap;

use runtime::event::*;
use runtime::units::{UnitRan, UNIT_RAN, UNIT_START};
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use super::cert_full::HistoryRow;
use super::pattern;
use crate::watchers::carried;

pub const NAME: &str = "cert.sample";
pub const STREAM: &str = "cert.sample";
pub const TICK: EventKindTag = "cert.sample.tick";
pub const STATE: EventKindTag = "cert.sample.state";
/// A move the transition table refuses, its body naming the state it was refused in.
pub const REFUSED: EventKindTag = "cert.sample.refused";
pub const OBSERVE: &str = "cert.sample.observe";
pub const BRANCH: &str = "cert.sample.branch";
pub const FILE_BEAD: &str = "cert.sample.file_bead";
pub const RECORD: &str = "cert.sample.record";
pub const EFFECTS: &[&str] = &[OBSERVE, BRANCH, UNIT_START, FILE_BEAD, RECORD];
/// `SPIRA_REPO` is not a registered key; the repo name is the unit's `--repo-name`.
pub const KEYS: &[&str] = &["SPIRA_RUN"];
/// `OnCalendar=*-*-* *:07,37:00`.
pub const CALENDAR: &str = "*-*-* *:07,37:00";
const OBSERVE_DEADLINE_MS: u64 = 30_000;
/// The unit's flags (`cert-sweep pass --mode subset`); no config key carries them today.
pub const SUBSET_DIV: usize = 4;
pub const DEADLINE_S: u64 = 1200;
pub const BRANCH_NAME: &str = "cert-sweep/tip";
pub const REPO_NAME: &str = "spira";
pub const RERUNS: u32 = 3;
pub const MAX_BEADS: usize = 5;

/// When the tick after one at `at_ms` is due.
pub fn next_tick(at_ms: u64) -> u64 {
    pattern::calendar(CALENDAR).next_after(at_ms)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub run: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { run: cfg("SPIRA_RUN")? })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub tip: String,
    pub round: String,
    pub suites: Vec<String>,
    pub history: Vec<HistoryRow>,
    /// The leg's parsed testenv stdout, read on the Collect step; `None` when it produced none.
    pub results: Option<Vec<HistoryRow>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Idle,
    Running,
    Collect,
    Attribute,
    File,
    Record,
}

/// `{ phase, tip, round, picks, pending, reruns, bisect, held, filed }`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub phase: Phase,
    pub tip: String,
    pub round: String,
    pub picks: Vec<String>,
    pub seq: u32,
    pub pending: Vec<String>,
    pub reruns: BTreeMap<String, Vec<String>>,
    pub bisect: BTreeMap<String, (String, String)>,
    pub held: Vec<String>,
    pub filed: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Observed(Snapshot),
    Ran(UnitRan),
}

/// The contract's state table, as data.
pub static TRANSITIONS: pattern::Table<Phase> = &[
    (Phase::Idle, "tick no-suites", Phase::Idle),
    (Phase::Idle, "tick", Phase::Running),
    (Phase::Running, "unit.ran results", Phase::Collect),
    (Phase::Running, "unit.ran no-results declined", Phase::Idle),
    (Phase::Running, "unit.ran no-results", Phase::Idle),
    (Phase::Collect, "red", Phase::Attribute),
    (Phase::Attribute, "judged file", Phase::File),
    (Phase::Attribute, "judged bisect", Phase::Attribute),
    (Phase::File, "filed", Phase::Record),
    (Phase::Record, "done", Phase::Idle),
];

/// One move of the machine. `seed` is the subset pick's seed, drawn from `Ctx::rand_u64`.
pub fn decide(_carry: &Carry, _input: &Input, _now_ms: u64, _seed: u64, _cfg: &Config) -> Result<(Carry, Vec<EffectSpec>), String> {
    unimplemented!("cert.sample: not ported yet")
}

pub struct CertSample {
    pub config: Result<Config, String>,
}

impl CertSample {
    pub fn new(config: Config) -> Self {
        CertSample { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        CertSample { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("cert.sample/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for CertSample {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, UNIT_RAN]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, next_tick(ev.at));
        }
        let input = match (pattern::answer::<Snapshot>(view, ev, OBSERVE), pattern::unit_ran(ev, NAME)) {
            (Some(snap), _) => Input::Observed(snap),
            (None, Some((_, ran))) => Input::Ran(ran),
            _ => return vec![],
        };
        let config = match &self.config {
            Ok(c) => c,
            Err(e) => {
                ctx.event().set("config.refused", e.as_str());
                return vec![];
            }
        };
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        match decide(&carry, &input, ev.at, ctx.rand_u64(), config) {
            Ok((next, acts)) => pattern::commands(STATE, &carry, &next, acts),
            Err(why) => vec![Command::Emit(EventKind::new(REFUSED, &why))],
        }
    }
}
