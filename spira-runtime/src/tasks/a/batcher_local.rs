//! # batcher.local — contract
//!
//! Unit `spira-rounds-prod` (`batcher rounds-local`, every 30 s, `TimeoutStartSec=7200`), pattern P2, from [[scheduler-consolidation-2026-11-03]] §1 A. Paths are relative to `spira-releases/current`. The `batcher` binary is the `batcher-cut` crate (`batcher-cut/Cargo.toml` `[[bin]] name = "batcher"`); the pure core is the `batcher` crate. The forge half is [[batcher-forge]].
//!
//! **Code vs catalogue.** The catalogue lists `batcher-cut` as a child; it is not — `rounds` calls `cut` in-process (`batcher-cut/src/main.rs:1320-1331`). The real long children are `round-vm run` under `systemd-run --scope` (`batcher-cut/src/vm.rs:381-394`) and the `queue round` verbs (`io.rs:1531`, `:1843`).
//!
//! ## Tick
//!
//! - Cadence 30 s, single-flight **per queue.local repo**. A repo with a machine not in `Idle` ignores the tick.
//! - Admission (today `main.rs:1299-1320`, `:1183-1206`): only `land == queue.local` repos; the home repo only while `SPIRA_BATCHER_ENABLE != 0`.
//! - Wakes, besides the tick (proposed names, none exist today): `bead.certified` (a pool grew), `world.unpaused` for the repo's `rounds` lane, and `unit.ran` for any leg it started.
//!
//! ## Observe
//!
//! Probe `batcher.local.observe` (deadline 10 s) returns one snapshot per admitted repo, from:
//!
//! - **LcClient (S7a):** `list --state SUBMITTED,CERTIFIED --snapshot` — pool and `sifted_tip` (`io.rs:212`, `:928-937`); `list --ids` for blockers (`io.rs:221`); `show <id>` for `stack` and the gate-red mark (`io.rs:244`, `:1614-1626`); `plan items --full --ids` for title, priority and dependencies (`io.rs:372-377`); `plan items --external-contains basefail:<repo>:` (`io.rs:495`).
//! - **Round projection (S7b):** today `queue round status <repo>` — open round id, members, head, worktree (`io.rs:1705`, `:1728`, `:1742`) — and whether a `round certify` holds the round (`certify-<batch>.lock`, `io.rs:2316-2327`). The snapshot carries the "driven" bit from the projection.
//! - **Config (S10), pinned:** the repo registry (`io.rs:135`; `spira_config::repos`), and the keys below.
//! - Git is read at the edge (allowed child): branch tips `for-each-ref refs/heads/spira/*` (`io.rs:394-412`).
//! - Raw files read today become **carried state**, not observe: `run/rounds-pace/<repo>/{state,pause,refusals}` (`pace.rs:166-194`, `:227`), `queue/<repo>/{base-moved,integration-hold,local-verdict}` (`io.rs:641`, `:1356-1373`, `:1434`, `:1815`), conflict streaks (`io.rs:793-817`). The scoped stop (`spira_config::scoped_stop::stop_for`, `main.rs:227`) is read through the world store verb (unverified that one exists).
//!
//! ## Decide
//!
//! Pure, no IO. Reuses: `screen::cut_pool` (`batcher-cut/src/screen.rs:50`), `base_fix_lane`, `select_round`, `refuse_blocked`, `cap_round(MAX_CUT_MEMBERS)`, `should_cut`, `topo_order`, `stack_sequencing`, `combine`, `round_key`, `stale_retry_due`, `terminal_ready` (`batcher/src/core.rs:44-717`); attribution `drive::attribute_round` (`batcher/src/drive.rs:296`) driven by legs instead of a blocking runner; pacing `pace::hold / classify / advance / orphan_step` (`batcher-cut/src/pace.rs:59-131`), which move into the task. Carry: per repo the pace `State` (infra count, backoff, paged/orphan ids), refusal window, base-moved (sha, at), conflict streaks, held round keys, the machine state below. Fixtures: `batcher/src/core/tests.rs`, `batcher/src/attrib/tests.rs`, `pace.rs` tests, `e2e.rs`.
//!
//! **States:** `Idle`, `Held`, `Orphan`, `Assembling`, `Opened`, `Corpus`, `Attributing`, `Mending`, `Certifying`, `Landing`, `Promoting`, `Done`.
//!
//! | State | Event | → State |
//! |---|---|---|
//! | Idle | observe: stopped / paused / backoff (`pace.rs:77`) | Held (no write) |
//! | Idle | observe: open round, driven | Idle |
//! | Idle | observe: open round, undriven | Orphan (`orphan_step`: Wait / Redrive / Incident) |
//! | Idle | observe: `should_cut` = None | Idle (silent) |
//! | Idle | observe: trigger | Assembling (`fetch`, `assemble`) |
//! | Assembling | `assemble` ok, merged empty / held key | Idle |
//! | Assembling | `assemble` ok | Opened (`round_open`) |
//! | Opened | `round_open` ok, suites present | Corpus (`corpus` = unit.start; `stage` if hypervisor fits, `io.rs:52`) |
//! | Opened | no suites and repo declares own testenv | Certifying |
//! | Corpus | `unit.ran` red | Attributing (`eject` + re-run leg) |
//! | Corpus | `unit.ran` green | Mending if menders open, else Certifying |
//! | Attributing | `unit.ran` (re-run) | Corpus-judged: Mending / Certifying / Done(blocked) |
//! | Mending | `mending.settled` or timer 900 s (`main.rs:368`) | Certifying |
//! | Certifying | `unit.ran` green | Landing (`lc_certify`, `land`) |
//! | Certifying | `unit.ran` red / failed | Done (eject guilty, `abandon`, verdict red) |
//! | Landing | land exit 0 / 3 | Promoting if staged else Done (verdict green) |
//! | Landing | land refused (lock busy ×N) | Done (`abandon`, verdict red) |
//! | Promoting | promotion named | Corpus / Certifying for the staged round (`main.rs:1069`) |
//! | Done | — | Idle (`pace.advance` writes carry, pages) |
//!
//! Illegal: any leg event whose `round_key` is not the carried round's is refused naming the state.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `batcher.local.observe` | store | 10 s | tick | — (Probe) |
//! | `batcher.local.fetch` | git | 60 s | repo + base sha | — (`io.rs:672`) |
//! | `batcher.local.assemble` | git | 120 s | repo + round_key | dirty lock (`io.rs:698-753`) |
//! | `batcher.local.round_open` | store | 30 s | repo + round_key | members ≠ named (`io.rs:1574-1597`) |
//! | `batcher.local.corpus` | unit (`unit.start`) | `SPIRA_BATCHER_WALL_SECS` | batch + run n | 2nd run while one live (`vm.rs:370`) |
//! | `batcher.local.stage` / `.stage_test` | unit | wall secs | staged batch | hypervisor full (`io.rs:1881`, `:1906`) |
//! | `batcher.local.eject` | store | 30 s | batch + bead | not in round (`io.rs:1600`, `:1628`) |
//! | `batcher.local.withdraw` | store | 10 s | bead + version | state ≠ CERTIFIED (`io.rs:851-887`) |
//! | `batcher.local.rebase_stale` | unit | 600 s (unverified) | bead + base sha | not due (`io.rs:178`) |
//! | `batcher.local.certify` | unit | wall secs | batch | head ≠ worktree (`io.rs:1662`) |
//! | `batcher.local.lc_certify` | store | 10 s | bead + `round:<key>` | state ≠ SUBMITTED (`io.rs:830-849`) |
//! | `batcher.local.land` | host | 900 s (unverified) | batch | not certified; lock busy retried ×`LAND_LOCK_ATTEMPTS` (`io.rs:2002`) |
//! | `batcher.local.abandon` / `.promote` | store | 30 s | batch | already closed (`io.rs:1823`, `:1945`) |
//! | `batcher.local.redrive` | unit | 60 s | batch | once per orphan (`io.rs:1772`) |
//! | `batcher.local.file` (incident/judgement/flip) | store | 30 s | incident ref, e.g. `round-orphan:<repo>:<batch>` | duplicate ref (`io.rs:1780`, `:2066`, `:2129`, `:2178`) |
//! | `batcher.local.pause` | store | 5 s | repo + lane | — (`pace.rs:248`) |
//! | `inbox.append` | store | 10 s | line | — (`pace.rs:269`, `main.rs:930`) |
//!
//! Metrics (`tsd-write --family batch-round`, `io.rs:2104-2118`) become in-process `spira-otel` pushes, not an effect.
//!
//! ## Silence
//!
//! An idle tick (no trigger, held, or driven round) emits only its probe: no Emit, no Effect, no inbox line. Today the held path prints only (`pace.rs:286`).
//!
//! ## Replaces
//!
//! `spira-rounds-prod.{service,timer}`; children `systemd-run` (`vm.rs:381`), `timeout round-vm run` (`vm.rs:306`), `round-vm refresh` (`io.rs:695`), `queue round open|eject|certify|land|abandon|stage|stage-test|promote|status` (`io.rs:1531-1960`), `queue verdict` for a forge batch left by a mode switch (`main.rs:255`, `:1258`), `timeout spira-lc` (`io.rs:197`), `bash lib.sh bead_reopen` (`io.rs:144-154`), `rebase-stale` (`io.rs:180`), `round-recert` (`io.rs:1773`), `escape-classify.sh` (`io.rs:1212-1245`), `incident.sh` (`io.rs:1307`, `:1339`, `:1793`, `:2077`), `bead.sh` (`io.rs:946`, `:2146`, `:2188`), `bash plan-matrix.sh` (`io.rs:1407`), `tsd-write` (`io.rs:2110`), `date` (`io.rs:2198`). Keys pinned: `SPIRA_RUN`, `SPIRA_DB`, `SPIRA_QUEUE_DIR`, `SPIRA_FORGE`, `SPIRA_HYPERVISOR_MEM_MIB`, `SPIRA_ROUND_VM_MEM_MIB`, `SPIRA_CI_RUNNER_MEM_MIB`, `SPIRA_BATCH_MAXPAR`, `SPIRA_BATCHER_WALL_SECS`, `SPIRA_GIT_NAME/EMAIL`, `SPIRA_LC_TIMEOUT`, `SPIRA_BATCHER_ORPHAN_REDRIVE_SECS` (`main.rs:187-223`), `SPIRA_QUEUE_LOCK_WAIT` (`main.rs:261`), `SPIRA_BATCHER_ENABLE`, `SPIRA_ROUNDS_PACING`, `SPIRA_CONCIERGE_INBOX` (`main.rs:1299`, `:1313`, `:1282`); unregistered env reads to register:
//! `SPIRA_BATCHER_ROUND_SLOTS`, `_POLL_SECS`, `_LAND_LOCK_ATTEMPTS`, `_LAND_LOCK_WAIT`, `_LAND_UNPAUSE`, `_MEND_DEADLINE_SECS`, `SPIRA_VERDICTS`, `ROUND_CPU_QUOTA` (`main.rs:203-222`, `:378`; `vm.rs:382`).
//!
//! ## Reverse
//!
//! Release rollback restores `spira-rounds-prod`. Per task, `Observe` mode (S4) journals every effect above as `effect.shadowed`. The repo-level stop (`world.sh unpause --repo <r> --lanes rounds`) keeps working in both.
//!

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "batcher.local";
pub const STREAM: &str = "task.batcher.local";
pub const TICK: EventKindTag = "batcher.local.tick";
pub const STATE: EventKindTag = "batcher.local.state";
pub const OBSERVE: &str = "batcher.local.observe";
pub const FETCH: &str = "batcher.local.fetch";
pub const ASSEMBLE: &str = "batcher.local.assemble";
pub const ROUND_OPEN: &str = "batcher.local.round_open";
pub const CORPUS: &str = "batcher.local.corpus";
pub const STAGE: &str = "batcher.local.stage";
pub const STAGE_TEST: &str = "batcher.local.stage_test";
pub const EJECT: &str = "batcher.local.eject";
pub const WITHDRAW: &str = "batcher.local.withdraw";
pub const REBASE_STALE: &str = "batcher.local.rebase_stale";
pub const CERTIFY: &str = "batcher.local.certify";
pub const LC_CERTIFY: &str = "batcher.local.lc_certify";
pub const LAND: &str = "batcher.local.land";
pub const ABANDON: &str = "batcher.local.abandon";
pub const PROMOTE: &str = "batcher.local.promote";
pub const REDRIVE: &str = "batcher.local.redrive";
pub const FILE: &str = "batcher.local.file";
pub const PAUSE: &str = "batcher.local.pause";
pub const INBOX_APPEND: &str = crate::watchers::INBOX_APPEND;

pub const CADENCE_MS: u64 = 30_000;
pub const OBSERVE_DEADLINE_MS: u64 = 10_000;
/// The unit legs (`corpus`, `stage`, `stage_test`, `certify`) run under `SPIRA_BATCHER_WALL_SECS`;
/// until it is pinned into the handler the table carries the unit start's own deadline.
pub const UNIT_START_DEADLINE_MS: u64 = 10_000;

pub const ACTS: &[(&str, u64)] = &[
    (FETCH, 60_000),
    (ASSEMBLE, 120_000),
    (ROUND_OPEN, 30_000),
    (CORPUS, UNIT_START_DEADLINE_MS),
    (STAGE, UNIT_START_DEADLINE_MS),
    (STAGE_TEST, UNIT_START_DEADLINE_MS),
    (EJECT, 30_000),
    (WITHDRAW, 10_000),
    (REBASE_STALE, 600_000),
    (CERTIFY, UNIT_START_DEADLINE_MS),
    (LC_CERTIFY, 10_000),
    (LAND, 900_000),
    (ABANDON, 30_000),
    (PROMOTE, 30_000),
    (REDRIVE, 60_000),
    (FILE, 30_000),
    (PAUSE, 5_000),
    (INBOX_APPEND, 10_000),
];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, FETCH, ASSEMBLE, ROUND_OPEN, CORPUS, STAGE, STAGE_TEST, EJECT, WITHDRAW, REBASE_STALE, CERTIFY, LC_CERTIFY, LAND, ABANDON, PROMOTE, REDRIVE, FILE, PAUSE, INBOX_APPEND];
pub const KEYS: &[&str] = &[
    "SPIRA_RUN",
    "SPIRA_DB",
    "SPIRA_QUEUE_DIR",
    "SPIRA_FORGE",
    "SPIRA_HYPERVISOR_MEM_MIB",
    "SPIRA_ROUND_VM_MEM_MIB",
    "SPIRA_CI_RUNNER_MEM_MIB",
    "SPIRA_BATCH_MAXPAR",
    "SPIRA_BATCHER_WALL_SECS",
    "SPIRA_GIT_NAME",
    "SPIRA_GIT_EMAIL",
    "SPIRA_LC_TIMEOUT",
    "SPIRA_BATCHER_ORPHAN_REDRIVE_SECS",
    "SPIRA_QUEUE_LOCK_WAIT",
    "SPIRA_BATCHER_ENABLE",
    "SPIRA_ROUNDS_PACING",
    "SPIRA_CONCIERGE_INBOX",
];
/// The unregistered env reads the contract says to register.
pub const UNREGISTERED: &[&str] = &["SPIRA_BATCHER_ROUND_SLOTS", "SPIRA_BATCHER_POLL_SECS", "SPIRA_BATCHER_LAND_LOCK_ATTEMPTS", "SPIRA_BATCHER_LAND_LOCK_WAIT", "SPIRA_BATCHER_LAND_UNPAUSE", "SPIRA_BATCHER_MEND_DEADLINE_SECS", "SPIRA_VERDICTS", "ROUND_CPU_QUOTA"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// One pool bead: its lifecycle row joined to its plan item.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub state: String,
    pub version: u64,
    pub tip: String,
    pub sifted_tip: Option<String>,
    pub title: String,
    pub priority: u8,
    pub deps: Vec<String>,
    pub stack: Vec<String>,
    pub gate_red: bool,
}

/// The round projection's open round.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    pub id: String,
    pub members: Vec<String>,
    pub head: String,
    pub worktree: String,
    /// A `round certify` or this task's own machine holds the round.
    pub driven: bool,
}

/// One admitted `queue.local` repository.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub pool: Vec<Member>,
    pub blockers: Vec<String>,
    /// `basefail:<repo>:` plan items open against the repo.
    pub basefail: Vec<String>,
    pub open_round: Option<Round>,
    pub branch_tips: BTreeMap<String, String>,
    pub base_sha: String,
    /// The scoped stop for the repo's `rounds` lane.
    pub stopped: bool,
    pub declares_own_testenv: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repos: BTreeMap<String, Repo>,
    pub now_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    Held,
    Orphan,
    Assembling,
    Opened,
    Corpus,
    Attributing,
    Mending,
    Certifying,
    Landing,
    Promoting,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// observe: stopped, paused or in backoff.
    Stopped,
    /// observe: hold lifted (not in the contract's table, which gives `Held` no exit).
    Released,
    OpenRoundDriven,
    OpenRoundUndriven,
    /// `orphan_step` answered Wait, Redrive or Incident (not in the contract's table, which gives
    /// `Orphan` no exit).
    OrphanStepped,
    NoTrigger,
    Cut,
    AssembledEmpty,
    Assembled,
    OpenedWithSuites,
    OpenedOwnTestenv,
    LegRed,
    LegGreenMenders,
    LegGreen,
    RerunMending,
    RerunCertifying,
    RerunBlocked,
    MendingSettled,
    MendTimeout,
    CertifyGreen,
    CertifyRed,
    LandedStaged,
    Landed,
    LandRefused,
    PromotedToCorpus,
    PromotedToCertifying,
    Advanced,
    /// A leg event whose `round_key` is not the carried round's.
    ForeignLeg,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::Stopped, State::Held),
    (State::Held, Trigger::Released, State::Idle),
    (State::Idle, Trigger::OpenRoundDriven, State::Idle),
    (State::Idle, Trigger::OpenRoundUndriven, State::Orphan),
    (State::Orphan, Trigger::OrphanStepped, State::Idle),
    (State::Idle, Trigger::NoTrigger, State::Idle),
    (State::Idle, Trigger::Cut, State::Assembling),
    (State::Assembling, Trigger::AssembledEmpty, State::Idle),
    (State::Assembling, Trigger::Assembled, State::Opened),
    (State::Opened, Trigger::OpenedWithSuites, State::Corpus),
    (State::Opened, Trigger::OpenedOwnTestenv, State::Certifying),
    (State::Corpus, Trigger::LegRed, State::Attributing),
    (State::Corpus, Trigger::LegGreenMenders, State::Mending),
    (State::Corpus, Trigger::LegGreen, State::Certifying),
    (State::Attributing, Trigger::RerunMending, State::Mending),
    (State::Attributing, Trigger::RerunCertifying, State::Certifying),
    (State::Attributing, Trigger::RerunBlocked, State::Done),
    (State::Mending, Trigger::MendingSettled, State::Certifying),
    (State::Mending, Trigger::MendTimeout, State::Certifying),
    (State::Certifying, Trigger::CertifyGreen, State::Landing),
    (State::Certifying, Trigger::CertifyRed, State::Done),
    (State::Landing, Trigger::LandedStaged, State::Promoting),
    (State::Landing, Trigger::Landed, State::Done),
    (State::Landing, Trigger::LandRefused, State::Done),
    (State::Promoting, Trigger::PromotedToCorpus, State::Corpus),
    (State::Promoting, Trigger::PromotedToCertifying, State::Certifying),
    (State::Done, Trigger::Advanced, State::Idle),
];

/// The pace state of `batcher-cut/src/pace.rs`, carried rather than kept in `run/rounds-pace`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pace {
    pub infra: u32,
    pub backoff_until_ms: Option<u64>,
    pub paged: Vec<String>,
    pub orphan: Vec<String>,
    pub refusals: Vec<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCarry {
    pub state: State,
    pub round_key: Option<String>,
    pub pace: Pace,
    pub base_moved: Option<(String, u64)>,
    pub conflict_streaks: BTreeMap<String, u32>,
    pub held_keys: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub repos: BTreeMap<String, RepoCarry>,
}

pub fn step(_state: State, _event: Trigger) -> Result<State, String> {
    crate::not_ported!(NAME)
}

pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatcherLocal {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl BatcherLocal {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        BatcherLocal { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("batcher.local/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for BatcherLocal {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return kit::on_tick(&SHAPE, serde_json::to_string(&carry).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
