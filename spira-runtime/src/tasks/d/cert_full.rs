//! # cert.full — contract
//!
//! This task replaces `spira-cert-sweep-full-prod`:
//!
//! - **ExecStart:** `cert-sweep pass --mode full --repo ~/spira/harness --tree run/cert-sweep/tree`.
//! - **Output:** stdout and stderr go to `run/cert-sweep.log`.
//! - **Timeout:** TimeoutStartSec=3300.
//! - **Pattern:** P4 tick driving a P2 machine. Part of [[scheduler-consolidation-2026-11-03]].
//!
//! Catalogue differs on three points:
//!
//! - **Children.** The catalogue lists "testenv (podman)". In full mode the code runs `round-vm run` on the round VM (`cert-sweep/src/main.rs:539-579`), not `testenv`. `testenv` is the subset path ([[cert-sample]]).
//! - **Not one long leg.** Each red is re-run 3 times, probed at the last green round and bisected (`main.rs:422-468`). Every one of those runs is another round-VM leg.
//! - **Pattern.** The legs take minutes, so the catalogue's P4 is a P2 machine with a calendar tick. §3 of the plan already counts the cert sweeps in P2.
//!
//! ## Tick
//!
//! - **Calendar:** `*-*-* *:07:00` (S8).
//! - **Single-flight:** a tick while the machine is not `Idle` re-arms only.
//! - **Clash with cert.sample:** [[cert-sample]] fires at the same `:07`. Today the two serialize only on the history flock at record time (`main.rs:159-165`). Here they share a class cap (`container`/`unit`), and the record step takes the same named lock.
//!
//! ## Observe
//!
//! `cert.full.observe` returns `Snapshot { tip, tree, round, suites, history, same_tree: {commit→bool}, leases }`. Sources:
//!
//! - **Git reads (class git; they stay children per the ask):**
//!   - The landing ref: `local/main` if present, else the repo map's land ref (`main.rs:121-128`).
//!   - `rev-parse` of the tip (`:242`).
//!   - The round label from `refs/archive/rounds` (`:243-248`).
//!   - The suites, from `ls-tree spira/` filtered to `test-*.sh` (`:249-254`).
//!   - The tree id, and the tree ids of the commits in the history (`:264-267`).
//! - **History:** the tsd family file `cert-history` (`main.rs:146-148`, `tsd::family_path`; `FAMILY` at `lib.rs:6`). That file is the record of truth and stays.
//! - **Round-VM leases:** `round-vm status --json`, parsed by `live_leases` (`main.rs:480-494`, `lib.rs:280`). The target is a library call into `round-vm` (unverified that one exists) or a lease projection.
//! - **Open beads** (read only on the filing step):
//!   - Today: `bdq list --all --limit 0 --brief --json`, filtered by lifecycle `spira_config::lc_state::list()` (`main.rs:399-418`; `still_open`, `lib.rs:406`).
//!   - Target: `content list` plus `lc list` through `LcClient` (S7a).
//!
//! ## Decide
//!
//! The pure core is `cert_sweep` (`lib.rs`):
//!
//! - `certified` (`:269`)
//! - `record` and `classify` (`:251`, `~:205`)
//! - `judge` (`:334`)
//! - `bisect` (`:368`)
//! - `open_duplicate` (`:411`)
//! - `filing_kind`, `bead_title`, `bead_body`, `culprit_title`, `culprit_body`
//!
//! Carry `cert.full.state = { phase, tip, round, picks, seq, pending: [suite], reruns: {suite→[verdict]}, bisect: {suite→(lo,hi)}, held: [suite], filed }`.
//!
//! | State | Event | Next | Effect |
//! |---|---|---|---|
//! | Idle | tick, no suites at tip or tree already certified | Idle | none |
//! | Idle | tick, not certified | WaitRound | none (leases are re-probed every 15 s by `Timer`, up to 2400 s; `main.rs:480-504`) |
//! | WaitRound | leases empty, or status unreadable (proceeds, `:493`) | Running | `cert.full.checkout`, then `unit.start` |
//! | WaitRound | 2400 s elapsed | Idle | none. Today it prints "deferred to the next tick" (`:272-275`) |
//! | Running | `unit.ran` | Collect | `cert.full.observe` (results dir) |
//! | Running | no output for 900 s | Idle | `unit.stop`, then the line `SWEEP FAULT … VERDICT no-verdict …` (`:567-573`) |
//! | Collect | no results, runner declined | Idle | none (`:282-285`) |
//! | Collect | no results otherwise | Idle | `cert.full.record` (SweepFault event) |
//! | Collect | events with a NewRed or RedUnbounded | Attribute | `unit.start` for each rerun (×3) |
//! | Attribute | reruns judged Flaky | File | none |
//! | Attribute | reruns judged Reproducible | Probe green, then Bisect | `unit.start` for each probe |
//! | Attribute | reruns judged Inconclusive | File | suite held (`:439-441`) |
//! | File | at most 5 filings and no open duplicate | Record | `cert.full.file_bead` |
//! | Record | done | Idle | `cert.full.record` (rows minus held suites; `:352-354`), then a Summary |
//!
//! Fixtures: `cert-sweep/src/tests.rs`, all 23 tests.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `cert.full.observe` (probe) | git | 30 s | none | — |
//! | `cert.full.checkout` | git | 120 s | `cert.full:checkout:<tip>` | an `index.lock` held by a live git pid, or younger than 600 s (`main.rs:523-537`) |
//! | `unit.start` (shared) | unit | `start-deadline` 900 s silent; whole pass ≤ 3300 s | `cert.full:<tip>:<seq>` | a second leg while one is in flight; any leg while a round-VM lease is held |
//! | `unit.stop` (shared) | unit | 30 s | `cert.full:stop:<tip>:<seq>` | a unit this task did not start |
//! | `cert.full.file_bead` | store (`bead.sh file … --for builder --repo spira --priority 1 --json`, `main.rs:179-199`) | 60 s | `cert.full:<suite>:<filing kind>:<tip>` | an open duplicate; the 6th filing (`max-beads 5`); an unreadable open list (suite held) |
//! | `cert.full.record` | host (flocked append to tsd `cert-history`/`cert-event`, `main.rs:150-176`) | 10 s | `cert.full:<tip>:<suite>:<seq>` | a held suite's rows |
//!
//! The leg runs `round-vm run <tree> --maxpar 16 --suites <csv> --results-dir run/cert-sweep/<start>-full-<seq>` with `SPIRA_ROUND_VM_ACQUIRE_DEADLINE=900` (`main.rs:551-558`). It runs inside the transient unit, so that unit's cgroup stop replaces `reap_unit_leftovers` (`main.rs:37-45`).
//!
//! The event lines are the `text` of each cert-event row and today's log lines (`lib.rs:180-201`):
//!
//! - `NEW RED {suite}: last green round {r} ({c9}), first red round {r} ({c9})`
//! - `RED {suite} at round {round} ({c9}); no earlier result`
//! - `FLIP {suite}: green and red on the same commit {c9} (round {round})`
//! - `FAULT {suite} at round {round} ({c9}): verdict lost, not a red`
//! - `SWEEP FAULT mode=full round={round}: {why}`
//! - `FULL round={round} ran={n} red={n} fault={n} wall={s}s`
//!
//! There is no `inbox.append` today. `cert-history` is read by `tsd/src/store.rs`. Nothing reads `run/cert-sweep.log` except a note in `run/concierge-notes/pool-one-vm.md`.
//!
//! ## Silence
//!
//! An already-certified tree emits the probe only. Today that case logs "tree … is already certified; no pass" (`main.rs:268-271`); here it does not.
//!
//! ## Replaces
//!
//! - **Units:** `spira-cert-sweep-full-prod.service` and `spira-cert-sweep-full-prod.timer`.
//! - **Children:**
//!   - `git` (stays)
//!   - `round-vm status` and `round-vm run`
//!   - `spira-admit status` (`:570`)
//!   - `bdq`
//!   - `bead.sh`
//!   - the `lc_state` fork of `spira-lc`
//! - **Keys pinned:**
//!   - `SPIRA_RUN` (`main.rs:111-115`)
//!   - `SPIRA_HOME` with the repo map (`:125-127`)
//!   - `SPIRA_METRICS_ENDPOINT`
//!   - the flags `maxpar 16`, `start-deadline 900`, `round-wait 2400`, `round-poll 15`, `reruns 3`, `max-beads 5`, `priority 1`
//!
//! ## Reverse
//!
//! - Release rollback restores the unit.
//! - Observe mode shadows `unit.start`, `file_bead` and `record`. Shadowed, the machine stops at WaitRound/Running in the sim only.

use std::collections::BTreeMap;

use runtime::event::*;
use runtime::units::{UnitRan, UNIT_RAN, UNIT_START, UNIT_STOP};
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use super::pattern;
use crate::watchers::carried;

pub const NAME: &str = "cert.full";
pub const STREAM: &str = "cert.full";
pub const TICK: EventKindTag = "cert.full.tick";
pub const STATE: EventKindTag = "cert.full.state";
/// A move the transition table refuses, its body naming the state it was refused in.
pub const REFUSED: EventKindTag = "cert.full.refused";
pub const OBSERVE: &str = "cert.full.observe";
pub const CHECKOUT: &str = "cert.full.checkout";
pub const FILE_BEAD: &str = "cert.full.file_bead";
pub const RECORD: &str = "cert.full.record";
pub const EFFECTS: &[&str] = &[OBSERVE, CHECKOUT, UNIT_START, UNIT_STOP, FILE_BEAD, RECORD];
pub const KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_METRICS_ENDPOINT"];
/// `OnCalendar=*-*-* *:07:00`.
pub const CALENDAR: &str = "*-*-* *:07:00";
const OBSERVE_DEADLINE_MS: u64 = 30_000;
/// The unit's flags (`cert-sweep pass --mode full`); no config key carries them today.
pub const MAXPAR: u32 = 16;
pub const START_DEADLINE_S: u64 = 900;
pub const ROUND_WAIT_S: u64 = 2400;
pub const ROUND_POLL_S: u64 = 15;
pub const RERUNS: u32 = 3;
pub const MAX_BEADS: usize = 5;
pub const PRIORITY: u32 = 1;

/// When the tick after one at `at_ms` is due.
pub fn next_tick(at_ms: u64) -> u64 {
    pattern::calendar(CALENDAR).next_after(at_ms)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub run: String,
    pub metrics_endpoint: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { run: cfg("SPIRA_RUN")?, metrics_endpoint: cfg("SPIRA_METRICS_ENDPOINT")? })
    }
}

/// One `cert-history` row: a suite's verdict at a round.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRow {
    pub round: String,
    pub commit: String,
    pub tree: String,
    pub suite: String,
    pub verdict: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub tip: String,
    pub tree: String,
    pub round: String,
    pub suites: Vec<String>,
    pub history: Vec<HistoryRow>,
    /// commit → whether its tree is the tip's.
    pub same_tree: BTreeMap<String, bool>,
    /// Live round-VM leases; `None` when `status` was unreadable (the pass proceeds).
    pub leases: Option<Vec<String>>,
    /// The leg's results, read on the Collect step; `None` when the leg left no results dir.
    pub results: Option<Vec<HistoryRow>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Idle,
    WaitRound,
    Running,
    Collect,
    Attribute,
    File,
    Record,
}

/// `{ phase, tip, round, picks, seq, pending, reruns, bisect, held, filed }`.
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
    /// When `WaitRound` began, for the 2400 s round wait.
    pub waiting_since: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Observed(Snapshot),
    Ran(UnitRan),
}

/// The contract's state table, as data. `on` names the event and the condition it is taken under.
pub static TRANSITIONS: pattern::Table<Phase> = &[
    (Phase::Idle, "tick certified", Phase::Idle),
    (Phase::Idle, "tick not-certified", Phase::WaitRound),
    (Phase::WaitRound, "leases empty", Phase::Running),
    (Phase::WaitRound, "round wait elapsed", Phase::Idle),
    (Phase::Running, "unit.ran", Phase::Collect),
    (Phase::Running, "silent 900 s", Phase::Idle),
    (Phase::Collect, "no results declined", Phase::Idle),
    (Phase::Collect, "no results", Phase::Idle),
    (Phase::Collect, "red", Phase::Attribute),
    (Phase::Attribute, "flaky", Phase::File),
    (Phase::Attribute, "reproducible", Phase::Attribute),
    (Phase::Attribute, "inconclusive", Phase::File),
    (Phase::File, "filed", Phase::Record),
    (Phase::Record, "done", Phase::Idle),
];

/// One move of the machine: the next carry and its effects, or a refusal naming the state.
pub fn decide(_carry: &Carry, _input: &Input, _now_ms: u64, _cfg: &Config) -> Result<(Carry, Vec<EffectSpec>), String> {
    unimplemented!("cert.full: not ported yet")
}

pub struct CertFull {
    pub config: Result<Config, String>,
}

impl CertFull {
    pub fn new(config: Config) -> Self {
        CertFull { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        CertFull { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("cert.full/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for CertFull {
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
        match decide(&carry, &input, ev.at, config) {
            Ok((next, acts)) => pattern::commands(STATE, &carry, &next, acts),
            Err(why) => vec![Command::Emit(EventKind::new(REFUSED, &why))],
        }
    }
}
