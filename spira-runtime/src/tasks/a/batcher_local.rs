//! # batcher.local — contract
//!
//! Unit `spira-rounds-prod` (`batcher rounds-local`, every 30 s, `TimeoutStartSec=7200`), pattern P2, from [[scheduler-consolidation-2026-11-03]] §1 A. Paths are relative to `spira-releases/current`. The `batcher` binary is the `batcher-cut` crate (`batcher-cut/Cargo.toml` `[[bin]] name = "batcher"`); the pure core is the `batcher` crate. The forge half is [[batcher-forge]].
//!
//! **Code vs catalogue.** The catalogue lists `batcher-cut` as a child; it is not — `rounds` calls `cut` in-process (`batcher-cut/src/main.rs:1320-1331`). The real long children are `round-vm run` under `systemd-run --scope` (`batcher-cut/src/vm.rs:381-394`) and the `queue round` verbs (`io.rs:1531`, `:1843`).
//!
//! ## Tick
//!
//! - Cadence 30 s, single-flight **per queue.local repo**. A repo cuts a further round only while fewer than `SPIRA_ROUND_MAX_OPEN` are in flight (default 1): it is assembled on the newest open round's head in a tree of its own, certified beside it, and lands only after the rounds below it (`State::Certified` holds a green round until then). A round the queue voids with the one below it is forgotten and its members are cut again.
//! - Admission (today `main.rs:1299-1320`, `:1183-1206`): only `land == queue.local` repos; the home repo only while `SPIRA_BATCHER_ENABLE != 0`.
//! - Wakes, besides the tick (proposed names, none exist today): `bead.certified` (a pool grew), `world.unpaused` for the repo's `rounds` lane, and `unit.ran` for any leg it started.
//!
//! ## Observe
//!
//! Probe `batcher.local.observe` (deadline 10 s) returns one snapshot per admitted repo, from:
//!
//! - **LcClient (S7a):** `list --state SUBMITTED,CERTIFIED` — pool and `sifted_tip` (`io.rs:212`, `:928-937`); `list --ids` for blockers (`io.rs:221`); `show <id>` for `stack` and the gate-red mark (`io.rs:244`, `:1614-1626`); `plan items --full --ids` for title, priority and dependencies (`io.rs:372-377`); `plan items --external-contains basefail:<repo>:` (`io.rs:495`).
//! - **Round projection (S7b):** the open round id, members, head, worktree and phase, read in-process from the store's watched projection (`store_view`, `io::open_rounds`), no `queue round status` — and whether a `round certify` holds the round (`certify-<batch>.lock`, `io.rs:2316-2327`). The snapshot carries the "driven" bit from the projection.
//! - **Config (S10), pinned:** the repo registry (`io.rs:135`; `spira_config::repos`), and the keys below.
//! - Git is read at the edge (allowed child): branch tips `for-each-ref refs/heads/spira/*` (`io.rs:394-412`).
//! - Raw files read today become **carried state**, not observe: `run/rounds-pace/<repo>/{state,pause,refusals}` (`pace.rs:166-194`, `:227`), `queue/<repo>/{base-moved,integration-hold,local-verdict}` (`io.rs:641`, `:1356-1373`, `:1434`, `:1815`), conflict streaks (`io.rs:793-817`). The scoped stop (`spira_config::scoped_stop::stop_for`, `main.rs:227`) is read through the world store verb (unverified that one exists).
//!
//! ## Decide
//!
//! Pure, no IO. Reuses: `screen::cut_pool` (`batcher-cut/src/screen.rs:50`), `base_fix_lane`, `select_round`, `refuse_blocked`, `cap_round(MAX_CUT_MEMBERS)`, `should_cut`, `topo_order`, `stack_sequencing`, `combine`, `round_key`, `stale_retry_due`, `terminal_ready` (`batcher/src/core.rs:44-717`); attribution `drive::attribute_round` (`batcher/src/drive.rs:296`) driven by legs instead of a blocking runner; pacing `pace::hold / classify / advance / orphan_step` (`batcher-cut/src/pace.rs:59-131`), which move into the task. Carry: per repo the pace `State` (infra count, paged/orphan ids), refusal window, base-moved (sha, at), conflict streaks, held round keys, the machine state below. Fixtures: `batcher/src/core/tests.rs`, `batcher/src/attrib/tests.rs`, `pace.rs` tests, `e2e.rs`.
//!
//! **States:** `Idle`, `Held`, `Orphan`, `Assembling`, `Opened`, `Corpus`, `Attributing`, `Mending`, `Certifying`, `Landing`, `Promoting`, `Done`.
//!
//! | State | Event | → State |
//! |---|---|---|
//! | Idle | observe: stopped / paused (`pace.rs:77`) | Held (no write) |
//! | Idle | observe: open round, driven | Idle |
//! | Idle | observe: open round, undriven | Orphan (`orphan_step`: Wait / Redrive once / then `abandon` naming why, pace Infra) |
//! | Idle | observe: `should_cut` = None | Idle (silent) |
//! | Idle | observe: trigger | Assembling (`fetch`, `assemble`) |
//! | Assembling | `assemble` ok, merged empty / held key | Idle |
//! | Assembling | `assemble` ok | Opened (`round_open`) |
//! | Opened | `round_open` ok, suites present | Corpus (`corpus` = unit.start; `stage` if hypervisor fits, `io.rs:52`) |
//! | Opened | no suites and repo declares own testenv | Certifying |
//! | Corpus, Attributing, Certifying | the leg's unit fails to start, or is unlisted for two observes with no `unit.ran` | the same state with the leg started once more; the second time Done (`abandon` naming the leg, pace Infra) |
//! | Corpus | `unit.ran` red | Attributing (`eject` + re-run leg) |
//! | Corpus | `unit.ran` green | Mending if menders open, else Certifying |
//! | Attributing | `unit.ran` (re-run) | Corpus-judged: Mending / Certifying / Done(blocked) |
//! | Mending | `mending.settled` or timer 900 s (`main.rs:368`) | Certifying |
//! | Certifying | `unit.ran` green | Landing (`lc_certify`, `land`) |
//! | Certifying | `unit.ran` red naming no suite and no guilty file, first time | Attributing (the round is attributed, not abandoned whole) |
//! | Certifying | `unit.ran` red naming suites in a round of several members, first time | Attributing (only the named suites run, alone on each member) |
//! | Certifying | `unit.ran` red / failed | Done (eject guilty, `abandon`, verdict red) |
//! | Certifying | `unit.ran` failed naming no verdict and no suite, after the attribution | Done (output kept, round left open, pace Infra) |
//! | Attributing | `unit.ran` land | Certifying, certify held until no pass scope of the round is alive |
//! | Certifying | `unit.ran` exit 75, the VM lost under the pass | Certifying (`certify` again: same head, a new VM), at most [`LOST_VM_RECERTIFIES`] times; then Done (pace Infra, the round left open, never abandoned) (sp-e30jhh) |
//! | Certifying | `unit.ran` exit 77, the VM produced no output | Certifying (`certify` again, a fresh VM), at most [`NO_OUTPUT_RECERTIFIES`] times; then Done (pace Infra, the round parked open, never abandoned) |
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
//! `spira-rounds-prod.{service,timer}`; children `systemd-run` (`vm.rs:381`), `timeout round-vm run` (`vm.rs:306`), `queue round open|eject|certify|land|abandon|stage|stage-test|promote|status` (`io.rs:1531-1960`), `queue verdict` for a forge batch left by a mode switch (`main.rs:255`, `:1258`), `timeout spira-lc` (`io.rs:197`), `bash lib.sh bead_reopen` (`io.rs:144-154`), `rebase-stale` (`io.rs:180`), `round-recert` (`io.rs:1773`), `escape-classify.sh` (`io.rs:1212-1245`), `incident.sh` (`io.rs:1307`, `:1339`, `:1793`, `:2077`), `bead.sh` (`io.rs:946`, `:2146`, `:2188`), `bash plan-matrix.sh` (`io.rs:1407`), `tsd-write` (`io.rs:2110`), `date` (`io.rs:2198`). Keys pinned: `SPIRA_RUN`, `SPIRA_DB`, `SPIRA_QUEUE_DIR`, `SPIRA_FORGE`, `SPIRA_HYPERVISOR_MEM_MIB`, `SPIRA_ROUND_VM_MEM_MIB`, `SPIRA_CI_RUNNER_MEM_MIB`, `SPIRA_BATCH_MAXPAR`, `SPIRA_BATCHER_WALL_SECS`, `SPIRA_GIT_NAME/EMAIL`, `SPIRA_LC_TIMEOUT`, `SPIRA_BATCHER_ORPHAN_REDRIVE_SECS` (`main.rs:187-223`), `SPIRA_QUEUE_LOCK_WAIT` (`main.rs:261`), `SPIRA_BATCHER_ENABLE`, `SPIRA_ROUNDS_PACING`, `SPIRA_CONCIERGE_INBOX` (`main.rs:1299`, `:1313`, `:1282`); unregistered env reads to register:
//! `SPIRA_BATCHER_ROUND_SLOTS`, `_POLL_SECS`, `_LAND_LOCK_ATTEMPTS`, `_LAND_LOCK_WAIT`, `_LAND_UNPAUSE`, `_MEND_DEADLINE_SECS`, `SPIRA_VERDICTS`, `ROUND_CPU_QUOTA` (`main.rs:203-222`, `:378`; `vm.rs:382`).
//!
//! ## Reverse
//!
//! Release rollback restores `spira-rounds-prod`. Per task, `Observe` mode (S4) journals every effect above as `effect.shadowed`. The repo-level stop (`world.sh unpause --repo <r> --lanes rounds`) keeps working in both.
//!

use std::collections::BTreeMap;

use batcher::core;
use batcher_cut::pace;
use runtime::event::*;
use batcher_cut::attribute::Attribution;
use runtime::units::{Exit, RanOutput, UnitFacts, UnitHandle, UnitRan, UnitState, UNIT_FACTS, UNIT_RAN};
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
pub const KEEP_CERTIFY_OUTPUT: &str = "batcher.local.keep_certify_output";
pub const INBOX_APPEND: &str = crate::watchers::INBOX_APPEND;

pub const CADENCE_MS: u64 = 30_000;
pub const OBSERVE_DEADLINE_MS: u64 = 10_000;
/// The unit legs (`corpus`, `stage`, `stage_test`, `certify`) are started under this deadline;
/// the leg itself runs under `SPIRA_BATCHER_WALL_SECS` ([`Policy::wall_ms`]) and ends on its
/// `unit.ran`.
pub const UNIT_START_DEADLINE_MS: u64 = 10_000;
/// Past the longest effect this task waits on (`land`, 900 s): an outcome that has not come is lost.
pub const OUTCOME_WAIT_MS: u64 = 1_200_000;

pub const ACTS: &[(&str, u64)] = &[
    (FETCH, 60_000),
    (ASSEMBLE, 120_000),
    (ROUND_OPEN, 30_000),
    (CORPUS, UNIT_START_DEADLINE_MS),
    (STAGE, UNIT_START_DEADLINE_MS),
    (STAGE_TEST, UNIT_START_DEADLINE_MS),
    (EJECT, 30_000),
    (WITHDRAW, 10_000),
    (REBASE_STALE, UNIT_START_DEADLINE_MS),
    (CERTIFY, UNIT_START_DEADLINE_MS),
    (LC_CERTIFY, 10_000),
    (LAND, 900_000),
    (ABANDON, 30_000),
    (PROMOTE, 30_000),
    (REDRIVE, 60_000),
    (FILE, 30_000),
    (PAUSE, 5_000),
    (KEEP_CERTIFY_OUTPUT, 5_000),
    (INBOX_APPEND, 10_000),
];
pub const EMITS: &[EventKindTag] = &[];
/// Hand-offs that wake it at once, besides its tick: a submission sifted, a batch settled.
pub const WAKES: &[EventKindTag] = &[kit::WORK_SUBMITTED, super::queue_step::CUT_WANTED];
pub const EFFECTS: &[&str] = &[runtime::units::UNIT_FACTS, OBSERVE, FETCH, ASSEMBLE, ROUND_OPEN, CORPUS, STAGE, STAGE_TEST, EJECT, WITHDRAW, REBASE_STALE, CERTIFY, LC_CERTIFY, LAND, ABANDON, PROMOTE, REDRIVE, FILE, PAUSE, KEEP_CERTIFY_OUTPUT, INBOX_APPEND];
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
    "SPIRA_BATCHER_PAUSE_LIFT_SECS",
    "SPIRA_QUEUE_LOCK_WAIT",
    "SPIRA_BATCHER_ENABLE",
    "SPIRA_ROUNDS_PACING",
    "SPIRA_CONCIERGE_INBOX",
    "SPIRA_ROUND_MAX_OPEN",
];
/// The unregistered env reads the contract says to register. The seam
/// (`batcher_cut::env_from_cfg`) still reads them as bare environment with its defaults; the
/// task itself reads none of them.
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
    /// Stacked prerequisites as `id:tip`, the tip this member's own work was built on.
    pub stack: Vec<String>,
    pub gate_red: bool,
    /// The fix for an open base-red bead (`basefail:<repo>:`): it lands alone (`base_fix_lane`).
    #[serde(default)]
    pub base_fix: bool,
    #[serde(default)]
    pub express: bool,
    #[serde(default)]
    pub certified_at: u64,
}

impl Member {
    /// The pure core's view of this member; `blockers` are the repo's unlanded beads.
    pub fn core(&self, blockers: &[String]) -> core::Member {
        let stack = self.stack.iter().map(|s| s.split_once(':').map_or((s.clone(), String::new()), |(i, t)| (i.to_string(), t.to_string()))).collect();
        let blocked_by = self.deps.iter().filter(|d| blockers.contains(d)).cloned().collect();
        core::Member { id: self.id.clone(), tip: self.tip.clone(), title: self.title.clone(), priority: Some(self.priority), express: self.express, base_fix: self.base_fix, certified_at: self.certified_at, stack, blocked_by }
    }
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
    /// The queue marked the round red: a round is never stacked on it.
    #[serde(default)]
    pub red: bool,
    /// Seconds since the round opened; the least is the newest, the one the next round stacks on.
    #[serde(default)]
    pub age_secs: u64,
    /// Why `round certify` would refuse this round at once (`round_certifiable`), if it would.
    #[serde(default)]
    pub unfit: Option<String>,
    /// The round's phase as `queue round status` names it; `green` once a pass on its head passed.
    #[serde(default)]
    pub phase: String,
}

/// One admitted `queue.local` repository.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub pool: Vec<Member>,
    pub blockers: Vec<String>,
    /// `basefail:<repo>:` plan items open against the repo.
    pub basefail: Vec<String>,
    pub open_round: Option<Round>,
    /// The rounds the queue holds open above `open_round`, oldest first.
    #[serde(default)]
    pub open_above: Vec<Round>,
    pub branch_tips: BTreeMap<String, String>,
    pub base_sha: String,
    /// The scoped stop for the repo's `rounds` lane.
    pub stopped: bool,
    /// The scoped stop's own reason (owner and why), when the lane is stopped.
    #[serde(default)]
    pub stop_why: Option<String>,
    /// The pace's pause file (`run/rounds-pace/<repo>/pause`), its text, when it stands: a red
    /// round wrote it. A text that starts with `hold:` is an operator's deliberate hold.
    #[serde(default)]
    pub paused: Option<String>,
    pub declares_own_testenv: bool,
    /// Why `round certify` would refuse the open round at once (`round_certifiable`), if it would.
    #[serde(default)]
    pub unfit: Option<String>,
    /// The keys of the leg units the unit manager lists as running; `None` when it could not say.
    #[serde(default)]
    pub live_legs: Option<Vec<String>>,
    /// The carried batches that have a pass scope (`systemd-run --scope`) running; `None` when the unit manager could not say.
    #[serde(default)]
    pub scoped_batches: Option<Vec<String>>,
    /// The effects of this task asked for this repo and not yet answered, as the scheduler holds
    /// them; `None` when the handler could not say.
    #[serde(default)]
    pub in_flight: Option<Vec<String>>,
    /// The carried batches the lifecycle reports LANDED or ABANDONED.
    #[serde(default)]
    pub settled: Vec<String>,
}

impl Repo {
    /// Every round the queue holds open, oldest first.
    pub fn open_rounds(&self) -> impl Iterator<Item = &Round> {
        self.open_round.iter().chain(self.open_above.iter())
    }
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
    /// Certify green with a round still open below: it lands only after that one has.
    Certified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// observe: stopped or paused.
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
    /// `round_open` refused, or the merged tree has no suites and declares no testenv (not in the
    /// contract's table, which gives `Opened` no failure exit).
    OpenFailed,
    /// `round open` refused some named members, by name (sp-822b8l): it is asked again for the
    /// rest (not in the contract's table).
    OpenRefusedSome,
    /// `round open` refused every named member: nothing to open, nothing to abandon.
    OpenRefusedAll,
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
    /// A red certify named no suite and no guilty file: the round is attributed, once, instead of
    /// abandoned whole.
    CertifyRedUnnamed,
    /// A red certify named suites: the round is attributed on those suites, once, so the member
    /// that reds one is ejected before the round is abandoned.
    CertifyRedNamed,
    LandedStaged,
    Landed,
    LandRefused,
    /// `round land` refused base-moved (its exit, sp-tjrjca): the round is certified again on
    /// the base it was rebuilt on, at most once per base (not in the contract's table).
    LandBaseMoved,
    PromotedToCorpus,
    PromotedToCertifying,
    /// `round promote` named no round (not in the contract's table): the cut goes on from the pool.
    PromoteFailed,
    Advanced,
    /// A leg event whose `round_key` is not the carried round's.
    ForeignLeg,
    /// The certify leg exited [`queue::ops::round::VM_LOST`]: the pass is incomplete, the round
    /// is not judged, and it is certified again on its head (sp-e30jhh; not in the contract's table).
    CertifyLost,
    /// The VM was lost under more passes than [`LOST_VM_RECERTIFIES`] allows.
    CertifyLostSpent,
    /// A pass on the round's current head passed while the carried leg was started for an earlier
    /// head: the round lands on that pass.
    HeadGreen,
    /// [`Self::HeadGreen`] with a round still open below.
    HeadGreenHeld,
    /// A leg never started or is gone with no `unit.ran`: started once more (not in the contract's table).
    LegRestarted,
    /// The restarted leg is lost too: the round is abandoned (not in the contract's table).
    LegLost,
    /// Certify green while a round below is still open: landing waits for it.
    CertifyHeld,
    /// The rounds below have ended: the held round lands.
    LowerEnded,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::Stopped, State::Held),
    (State::Held, Trigger::Stopped, State::Held),
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
    (State::Opened, Trigger::OpenFailed, State::Done),
    (State::Opened, Trigger::OpenRefusedSome, State::Opened),
    (State::Opened, Trigger::OpenRefusedAll, State::Done),
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
    (State::Certifying, Trigger::CertifyRedUnnamed, State::Attributing),
    (State::Certifying, Trigger::CertifyRedNamed, State::Attributing),
    (State::Landing, Trigger::LandedStaged, State::Promoting),
    (State::Landing, Trigger::Landed, State::Done),
    (State::Landing, Trigger::LandRefused, State::Done),
    (State::Landing, Trigger::LandBaseMoved, State::Certifying),
    (State::Promoting, Trigger::PromotedToCorpus, State::Corpus),
    (State::Promoting, Trigger::PromotedToCertifying, State::Certifying),
    (State::Promoting, Trigger::PromoteFailed, State::Done),
    (State::Done, Trigger::Advanced, State::Idle),

    (State::Certifying, Trigger::CertifyLost, State::Certifying),
    (State::Certifying, Trigger::CertifyLostSpent, State::Done),
    (State::Corpus, Trigger::LegRestarted, State::Corpus),
    (State::Attributing, Trigger::LegRestarted, State::Attributing),
    (State::Certifying, Trigger::LegRestarted, State::Certifying),
    (State::Corpus, Trigger::LegLost, State::Done),
    (State::Attributing, Trigger::LegLost, State::Done),
    (State::Certifying, Trigger::LegLost, State::Done),
    (State::Certifying, Trigger::CertifyHeld, State::Certified),
    (State::Certified, Trigger::LowerEnded, State::Landing),
    (State::Corpus, Trigger::HeadGreen, State::Landing),
    (State::Attributing, Trigger::HeadGreen, State::Landing),
    (State::Certifying, Trigger::HeadGreen, State::Landing),
    (State::Corpus, Trigger::HeadGreenHeld, State::Certified),
    (State::Attributing, Trigger::HeadGreenHeld, State::Certified),
    (State::Certifying, Trigger::HeadGreenHeld, State::Certified),
];

/// The pace state of `batcher-cut/src/pace.rs`, carried rather than kept in `run/rounds-pace`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pace {
    pub infra: u32,
    #[serde(default)]
    pub no_vm: u32,
    /// The open round already paged as stranded (`paged_open`), at most one.
    pub paged: Vec<String>,
    /// The open round seen undriven (`orphan`), at most one.
    pub orphan: Vec<String>,
    pub refusals: Vec<u64>,
    #[serde(default)]
    pub orphan_since_ms: u64,
    #[serde(default)]
    pub redriven: String,
    #[serde(default)]
    pub pause_lifts: u32,
}

impl Pace {
    pub fn core(&self) -> pace::State {
        pace::State {
            infra: self.infra,
            no_vm: self.no_vm,
            paged_open: self.paged.first().cloned().unwrap_or_default(),
            orphan: self.orphan.first().cloned().unwrap_or_default(),
            orphan_since: self.orphan_since_ms / 1000,
            redriven: self.redriven.clone(),
            pause_lifts: self.pause_lifts,
        }
    }

    pub fn of(st: &pace::State, refusals: &[u64]) -> Pace {
        let one = |s: &str| if s.is_empty() { vec![] } else { vec![s.to_string()] };
        Pace {
            infra: st.infra,
            no_vm: st.no_vm,
            paged: one(&st.paged_open),
            orphan: one(&st.orphan),
            refusals: refusals.to_vec(),
            orphan_since_ms: st.orphan_since * 1000,
            redriven: st.redriven.clone(),
            pause_lifts: st.pause_lifts,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCarry {
    pub state: State,
    pub round_key: Option<String>,
    #[serde(default)]
    pub pace: Pace,
    #[serde(default)]
    pub base_moved: Option<(String, u64)>,
    /// Per member: the tip that last conflicted and how many cuts in a row it did.
    #[serde(default)]
    pub conflict_streaks: BTreeMap<String, (String, u32)>,
    #[serde(default)]
    pub held_keys: Vec<String>,
    /// The round the machine is driving: its batch, members, tree and base.
    #[serde(default)]
    pub batch: Option<String>,
    #[serde(default)]
    pub members: Vec<(String, String)>,
    #[serde(default)]
    pub wt: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub own_testenv: bool,
    /// The assembled tree carries `test-*.sh` suites; `None` for a row written before this was kept.
    #[serde(default)]
    pub suites: Option<bool>,
    /// Certify passes of this round whose VM was lost under them (sp-e30jhh).
    #[serde(default)]
    pub lost_vms: u32,
    /// This round's unnamed certify red has already been attributed.
    #[serde(default)]
    pub attributed: bool,
    /// The attribution's survivors await certify, which starts once no pass scope of the round is alive.
    #[serde(default)]
    pub certify_held: bool,
    /// Certify passes of this round whose VM answered ssh and never started the round.
    #[serde(default)]
    pub no_output_vms: u32,
    /// The unit key of the live leg, when it started, and which run of the corpus it is.
    #[serde(default)]
    pub leg: Option<String>,
    #[serde(default)]
    pub leg_at_ms: u64,
    /// The head the round was last seen at, and the head the live leg was started for.
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub leg_head: String,
    #[serde(default)]
    pub run: u32,
    /// Times the carried leg has been started again because it never ran or vanished.
    #[serde(default)]
    pub leg_restarts: u32,
    /// Consecutive observes that did not list the carried leg's unit.
    #[serde(default)]
    pub leg_missing: u32,
    /// The base the red round was cut on, recorded when its pause is written: a landing past it lifts the pause.
    #[serde(default)]
    pub paused_on: Option<String>,
    /// Why this repo's last pass cut nothing while members waited, named
    /// (law-a-failure-carries-its-own-explanation); logged once each time it changes.
    #[serde(default)]
    pub skip_why: Option<String>,
    /// The assemble error of the last cut and the ticks it has now failed the same way running.
    #[serde(default)]
    pub assemble_failed: Option<(String, u32)>,
    /// Members `queue round open` refused, as id → (tip, base, kind): each stays out of the cut
    /// until its tip or the base moves.
    #[serde(default)]
    pub refused: BTreeMap<String, (String, String, String)>,
    /// The base the round was re-certified on after a base-moved land refusal: at most once per
    /// base change.
    #[serde(default)]
    pub recertified_on: Option<String>,
    /// The rounds stacked above this one, oldest first: each carries only its own round's fields,
    /// the rest of the repo's carry being this one's.
    #[serde(default)]
    pub above: Vec<RepoCarry>,
    /// The round was cut on the head of a round still open; it is gone when that one is voided.
    #[serde(default)]
    pub stacked: bool,
    /// The landing ref the round was cut against, which `base` is not for a stacked round.
    #[serde(default)]
    pub landing_base: String,
    /// Since when the round has waited, unmoved, on the outcome of an effect it issued.
    #[serde(default)]
    pub waiting: Option<Waiting>,
    /// The batch the last reconcile dropped because the lifecycle had already settled it.
    #[serde(default)]
    pub reconciled: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    pub state: State,
    pub since_ms: u64,
}

impl RepoCarry {
    fn live(&self) -> bool {
        !matches!(self.state, State::Idle | State::Held)
    }

    /// This carry's own round, without what the repo shares across its rounds.
    fn round(&self) -> RepoCarry {
        RepoCarry {
            state: self.state,
            round_key: self.round_key.clone(),
            batch: self.batch.clone(),
            members: self.members.clone(),
            wt: self.wt.clone(),
            base: self.base.clone(),
            own_testenv: self.own_testenv,
            suites: self.suites,
            lost_vms: self.lost_vms,
            attributed: self.attributed,
            certify_held: self.certify_held,
            leg: self.leg.clone(),
            leg_at_ms: self.leg_at_ms,
            head: self.head.clone(),
            leg_head: self.leg_head.clone(),
            run: self.run,
            leg_restarts: self.leg_restarts,
            leg_missing: self.leg_missing,
            recertified_on: self.recertified_on.clone(),
            stacked: self.stacked,
            landing_base: self.landing_base.clone(),
            waiting: self.waiting,
            ..RepoCarry::default()
        }
    }

    /// What the repo shares, with `round` as the one round the machine is moving.
    fn holding(&self, round: &RepoCarry) -> RepoCarry {
        RepoCarry { above: vec![], ..self.clone() }.with_round(round)
    }

    fn with_round(self, round: &RepoCarry) -> RepoCarry {
        RepoCarry {
            state: round.state,
            round_key: round.round_key.clone(),
            batch: round.batch.clone(),
            members: round.members.clone(),
            wt: round.wt.clone(),
            base: round.base.clone(),
            own_testenv: round.own_testenv,
            suites: round.suites,
            lost_vms: round.lost_vms,
            attributed: round.attributed,
            certify_held: round.certify_held,
            leg: round.leg.clone(),
            leg_at_ms: round.leg_at_ms,
            head: round.head.clone(),
            leg_head: round.leg_head.clone(),
            run: round.run,
            leg_restarts: round.leg_restarts,
            leg_missing: round.leg_missing,
            recertified_on: round.recertified_on.clone(),
            stacked: round.stacked,
            landing_base: round.landing_base.clone(),
            waiting: round.waiting,
            ..self
        }
    }

    /// The live rounds, lowest first.
    pub(crate) fn rounds(&self) -> Vec<RepoCarry> {
        std::iter::once(self.round()).chain(self.above.iter().cloned()).filter(RepoCarry::live).collect()
    }

    /// `shared` with `rounds` (lowest first) as its rounds, or at `idle` when there are none.
    fn rebuilt(shared: &RepoCarry, mut rounds: Vec<RepoCarry>, idle: State) -> RepoCarry {
        let first = if rounds.is_empty() { RepoCarry { state: idle, ..RepoCarry::default() } } else { rounds.remove(0) };
        RepoCarry { above: rounds, ..shared.holding(&first) }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub repos: BTreeMap<String, RepoCarry>,
}

/// Everything one act of this task (and of [`super::batcher_forge`]) carries as its input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leg {
    pub repo: String,
    /// The act's idempotency key, as the contract's Act table gives it.
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub members: Vec<(String, String)>,
    #[serde(default)]
    pub versions: BTreeMap<String, u64>,
    #[serde(default)]
    pub round_key: String,
    #[serde(default)]
    pub batch: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub wt: String,
    #[serde(default)]
    pub bead: String,
    #[serde(default)]
    pub why: String,
    /// The unit a leg runs under (`unit_key`), for the legs that start one.
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub head: String,
    /// The head of the open round an assemble builds on, when it stacks; empty for the landing ref.
    #[serde(default)]
    pub onto: String,
    /// How many members were ejected before this abandon, when the leg knows.
    #[serde(default)]
    pub ejected: Option<usize>,
    /// The suites a red certify named, for the abandon to keep their evidence.
    #[serde(default)]
    pub reds: Vec<String>,
}

impl Leg {
    pub fn on(repo: &str) -> Leg {
        Leg { repo: repo.to_string(), ..Leg::default() }
    }

    pub fn act(&self, effect: &str) -> Act {
        Act::new(effect, self.key.clone()).with_input(serde_json::to_string(self).unwrap_or_default())
    }
}

/// What `assemble` answers: the members that merged cleanly on `base`, in `wt`, and the facts the
/// cut's set-asides need.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assembled {
    #[serde(default)]
    pub merged: Vec<(String, String)>,
    #[serde(default)]
    pub wt: String,
    #[serde(default)]
    pub base: String,
    /// The merged tree has `test-*.sh` suites.
    #[serde(default)]
    pub suites: bool,
    #[serde(default)]
    pub own_testenv: bool,
    /// Members whose merge conflicted with the round's own accumulation, at their tips.
    #[serde(default)]
    pub conflicts: Vec<(String, String)>,
    /// Members whose merge conflicted with the base itself: the rebase path's.
    #[serde(default)]
    pub base_conflicts: Vec<String>,
    /// What the assemble cleared from the round tree's path before building there.
    #[serde(default)]
    pub cleared: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    #[serde(default)]
    pub batch: String,
    /// The members a refused open named as not admitted, as (id, kind, why); nothing opened.
    #[serde(default)]
    pub refused: Vec<(String, String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landed {
    /// The round `land` left staged behind it, to promote.
    #[serde(default)]
    pub staged: Option<String>,
    /// `round land` refused base-moved (its exit): the base it now stands on. The round is
    /// certified again, not landed.
    #[serde(default)]
    pub base_moved: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Promoted {
    #[serde(default)]
    pub batch: String,
    #[serde(default)]
    pub members: Vec<(String, String)>,
    #[serde(default)]
    pub attested: bool,
}

/// The pinned values `decide` reads, from the task's keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub orphan_wait_s: u64,
    pub wall_ms: u64,
    pub max_members: usize,
    pub max_open: usize,
}

impl Default for Policy {
    /// The registered defaults (`spira/conf.d/SPIRA_BATCHER_ORPHAN_REDRIVE_SECS`, `_WALL_SECS`).
    fn default() -> Self {
        Policy { orphan_wait_s: 600, wall_ms: 3_600_000, max_members: batcher_cut::MAX_CUT_MEMBERS, max_open: 1 }
    }
}

impl Policy {
    pub fn pinned(pinned: &BTreeMap<String, String>) -> Policy {
        let d = Policy::default();
        let num = |k: &str| pinned.get(k).and_then(|v| v.trim().parse::<u64>().ok());
        Policy { orphan_wait_s: num("SPIRA_BATCHER_ORPHAN_REDRIVE_SECS").unwrap_or(d.orphan_wait_s), wall_ms: num("SPIRA_BATCHER_WALL_SECS").map_or(d.wall_ms, |s| s * 1000), max_open: num("SPIRA_ROUND_MAX_OPEN").map_or(d.max_open, |n| (n as usize).max(1)), ..d }
    }
}

/// How a stopped unit ends: the abandon of a round stops its legs with this.
const SIGTERM: i32 = 15;

/// The `why` of a [`CORPUS`] leg that is the attribution (`batcher attribute`), not the corpus.
pub const ATTRIBUTE: &str = "attribute";
/// The line `queue round certify` names its red output file by (`io::round_certify`).
pub const RED_OUTPUT: &str = "red-output=";

/// The keys of this task's leg units among `listed` that are running.
pub fn running_legs(listed: &[runtime::units::UnitDetail]) -> Vec<String> {
    use runtime::units::{UnitHandle, UnitState};
    listed
        .iter()
        .filter(|d| matches!(d.state, UnitState::Active | UnitState::Activating))
        .filter_map(|d| UnitHandle::named(&d.name).owner().filter(|(t, _)| t == NAME).map(|(_, k)| k))
        .collect()
}

/// How long past its wall a leg with no `unit.ran` is given before it is judged lost.
pub const LEG_GRACE_MS: u64 = 120_000;
/// How long past its start a leg with no `unit.ran` is judged lost: a wall and grace, and for
/// the attribution one wall for each of the corpus runs it may make (`members + 2`).
pub fn leg_deadline_ms(policy: &Policy, attributing: bool, members: usize) -> u64 {
    let runs = if attributing { members as u64 + 2 } else { 1 };
    policy.wall_ms * runs + LEG_GRACE_MS
}

/// How long a started leg may go unlisted before it counts as missing.
pub const LEG_START_GRACE_MS: u64 = 30_000;
/// Consecutive observes that must miss the leg before it is started again or the round abandoned.
pub const LEG_MISSING_OBSERVES: u32 = 2;
/// Cause word on the `.result` line of a round abandoned for want of a driver.
pub const NO_DRIVER: &str = "no-driver";
/// How many times a round whose VM was lost under its certify is certified again before the
/// loss is the host's to look at (pace Infra): a lost VM says nothing about the members.
pub const LOST_VM_RECERTIFIES: u32 = 2;
/// How many times a round whose VM produced no output is certified again, on a fresh VM, before
/// it is parked: a second such VM says the image or the provider is at fault, not the members.
pub const NO_OUTPUT_RECERTIFIES: u32 = 1;

pub fn step(state: State, event: Trigger) -> Result<State, String> {
    table_step(NAME, TABLE, state, event)
}

/// The row of `table` for `(state, event)`, or a refusal naming the state.
pub fn table_step<S: Copy + PartialEq + std::fmt::Debug, T: Copy + PartialEq + std::fmt::Debug>(task: &str, table: &[(S, T, S)], state: S, event: T) -> Result<S, String> {
    table.iter().find(|(s, t, _)| *s == state && *t == event).map(|(_, _, to)| *to).ok_or_else(|| format!("{task}: {event:?} is not legal in state {state:?}"))
}

/// A short stable digest of `s` (FNV-1a), so a unit name stays under systemd's bound.
pub fn digest(s: &str) -> String {
    runtime::units::digest(s)
}

/// The unit key of a leg: repo, round, what, run. Parsed back from `unit.ran` by `key_for`.
pub fn unit_key(repo: &str, round: &str, what: &str, run: u32) -> String {
    format!("{}.{what}{run}", runtime::units::round_stem(repo, round))
}

/// Where a certify leg's output is kept, relative to the run's parent.
pub fn certify_output_file(batch: &str, run: u32) -> String {
    format!("run/rounds/{batch}.certify-{run}.out")
}

/// Why a certify leg that judged nothing leaves its round open: its last line and the kept file.
pub fn certify_fault_reason(exit: Exit, out: &str, kept: &str) -> String {
    let said = out.trim().lines().last().map(|l| format!(": {l}")).unwrap_or_default();
    format!("queue round certify exited ({exit:?}) naming no verdict and no suite{said}; its output is kept at {kept}; the round is left open")
}

/// The verdict line and the red suites a `queue round certify` leg printed.
pub fn certify_red_evidence(out: &str) -> (Option<String>, Vec<String>) {
    let verdict = out.lines().rev().find(|l| l.contains(" is RED at ")).map(|l| l.trim().to_string());
    let reds = out.lines().rev().find_map(|l| l.strip_prefix("red=")).map(|r| r.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    (verdict, reds)
}

pub fn green(exit: Exit) -> bool {
    exit == Exit::Code(0)
}

/// The inbox line a page is, as `pace.rs`'s `page` wrote it.
pub fn page_line(now_ms: u64, text: &str) -> String {
    format!("{} [watch:rounds] {text}", batcher_cut::io::utc_stamp(now_ms / 1000))
}

/// The cut `screen::cut_pool` → `refuse_blocked` → `base_fix_lane` → `select_round` →
/// `cap_round` → `should_cut` selects from `repo`, with its round key; `None` when nothing triggers.
pub fn select(repo: &Repo, max_members: usize) -> Option<(Vec<core::Member>, String)> {
    select_why(repo, max_members).ok()
}

/// [`select`], or why nothing was cut: one line per waiting member set aside (empty when the
/// pool is). The blocked are refused before the round is selected, so a feature group whose
/// every member waits on an unlanded prerequisite never starves the catch-all.
pub fn select_why(repo: &Repo, max_members: usize) -> Result<(Vec<core::Member>, String), Vec<String>> {
    let mut why: Vec<String> = repo.pool.iter().filter(|m| m.gate_red).map(|m| format!("{}: its gate is red at {}", m.id, m.tip)).collect();
    let pool: Vec<core::Member> = repo.pool.iter().filter(|m| !m.gate_red).map(|m| m.core(&repo.blockers)).collect();
    let sifted: BTreeMap<String, String> = repo.pool.iter().filter_map(|m| m.sifted_tip.clone().map(|t| (m.id.clone(), t))).collect();
    let screened = batcher_cut::screen::cut_pool(pool.clone(), &sifted);
    why.extend(pool.iter().filter(|m| !screened.iter().any(|s| s.id == m.id)).map(|m| format!("{}: no sift pass at its tip {}", m.id, m.tip)));
    let (pool, blocked) = core::refuse_blocked(screened);
    why.extend(blocked);
    let (pool, _) = core::select_round(core::base_fix_lane(pool));
    let (pool, _) = core::cap_round(pool, max_members);
    if core::should_cut(&core::TriggerInputs { pool: &pool, main_red: false, batch_open: false }).is_none() {
        return Err(why);
    }
    let key = core::round_key(&pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect::<Vec<_>>());
    Ok((pool, key))
}

/// Ticks the same assemble error must repeat before a person is paged.
const ASSEMBLE_PAGE_AFTER: u32 = 3;

fn assemble_failed_why(err: &str, n: u32) -> String {
    format!("assemble failed {n} time(s) running, so no round is cut: {err}")
}

/// Why the round `key` is not cut again: its last assemble merged nothing new.
fn held_key_why(key: &str) -> String {
    format!("the round it would cut ({key}) is held: its last assemble merged nothing new")
}

/// Why a held repo cuts nothing, with how the hold lifts.
fn hold_why(h: &pace::Hold, repo: &str, r: &Repo) -> String {
    match h {
        pace::Hold::Stopped => format!("the rounds lane is stopped ({}) — a landing does not lift it; lift with: queue round start {repo} --by <owner> --reason <why>", r.stop_why.as_deref().unwrap_or("no reason recorded")),
        pace::Hold::Paused => format!(
            "paused by a red round ({}) — the next landing on {repo} lifts it, or rm run/rounds-pace/{repo}/pause",
            r.paused.as_deref().unwrap_or("").trim()
        ),
    }
}

/// The log lines a pass owes when it moved from `old` to `new`: each repo whose named skip
/// reason changed says so once, and once more when it clears.
pub fn notes(old: &Carry, new: &Carry) -> Vec<String> {
    let names: std::collections::BTreeSet<&String> = old.repos.keys().chain(new.repos.keys()).collect();
    names
        .into_iter()
        .filter_map(|name| {
            let dropped = new.repos.get(name).and_then(|rc| rc.reconciled.as_ref());
            if dropped.is_some() && dropped != old.repos.get(name).and_then(|rc| rc.reconciled.as_ref()) {
                return dropped.map(|b| format!("{NAME} {name}: batch {b} is LANDED or ABANDONED in the lifecycle — its carried round was dropped"));
            }
            let was = old.repos.get(name).and_then(|rc| rc.skip_why.as_ref());
            let now = new.repos.get(name).and_then(|rc| rc.skip_why.as_ref());
            match (was, now) {
                (a, Some(b)) if a != Some(b) => Some(format!("{NAME} {name}: no cut — {b}")),
                (Some(_), None) => Some(format!("{NAME} {name}: the hold is gone — cutting again")),
                _ => None,
            }
        })
        .collect()
}

/// How many recent held round keys the carry keeps.
pub const HELD_KEYS: usize = 8;

fn hold_key(rc: &mut RepoCarry, key: &str) {
    if !rc.held_keys.iter().any(|k| k == key) {
        rc.held_keys.push(key.to_string());
    }
    let over = rc.held_keys.len().saturating_sub(HELD_KEYS);
    rc.held_keys.drain(..over);
}

/// One repo's machine, advanced by one event.
struct Machine<'a> {
    repo: &'a str,
    rc: RepoCarry,
    acts: Vec<Act>,
    now_ms: u64,
    /// A round of the repo below this one is still open.
    below: bool,
}

/// The states whose next move comes from the queue's lifecycle or a leg, so a restart can lose
/// it: a batch the lifecycle has settled leaves nothing to wait for. Promoting is not among them,
/// because a staged landing settles its batch on the way in.
fn lifecycle_driven(state: State) -> bool {
    matches!(state, State::Corpus | State::Attributing | State::Mending | State::Certifying | State::Certified | State::Landing)
}

/// The states in which a round may be stacked on: it is still being certified, or certified and
/// waiting to land (`queue round open` refuses any other).
fn stackable(state: State) -> bool {
    matches!(state, State::Corpus | State::Attributing | State::Mending | State::Certifying | State::Certified)
}

/// How long past its last leg's start a stacked round may be missing from the queue's open list
/// before it is judged voided: a snapshot may predate its open.
const VOID_GRACE_MS: u64 = 2 * CADENCE_MS;

impl Machine<'_> {
    fn go(&mut self, on: Trigger) -> bool {
        match step(self.rc.state, on) {
            Ok(to) => {
                self.rc.state = to;
                true
            }
            Err(_) => false,
        }
    }

    fn leg(&self) -> Leg {
        Leg { batch: self.rc.batch.clone().unwrap_or_default(), members: self.rc.members.clone(), wt: self.rc.wt.clone(), base: self.rc.base.clone(), round_key: self.rc.round_key.clone().unwrap_or_default(), ..Leg::on(self.repo) }
    }

    /// Start a unit leg: `what` names it, and its key is the carried leg from here on.
    fn start(&mut self, effect: &str, what: &str) {
        let round = self.rc.batch.clone().or_else(|| self.rc.round_key.clone()).unwrap_or_default();
        let unit = unit_key(self.repo, &round, what, self.rc.run.max(1));
        self.rc.leg = Some(unit.clone());
        self.rc.leg_at_ms = self.now_ms;
        self.rc.leg_head = self.rc.head.clone();
        let leg = Leg { key: unit.clone(), unit, ..self.leg() };
        self.acts.push(leg.act(effect));
    }

    fn corpus(&mut self, run: u32) {
        self.rc.run = run;
        self.start(CORPUS, "corpus");
    }

    /// The attribution leg: `batcher attribute` (`drive::attribute_round`) on the red round, a
    /// corpus leg marked [`ATTRIBUTE`] so it runs under the same effect, captured.
    fn attribute(&mut self) {
        self.attribute_suites(Vec::new());
    }

    /// [`Self::attribute`], restricted to `suites` when any are named.
    fn attribute_suites(&mut self, suites: Vec<String>) {
        self.rc.run += 1;
        self.start(CORPUS, "attribute");
        if let Some(a) = self.acts.last_mut() {
            let mut leg: Leg = serde_json::from_str(&a.input).unwrap_or_default();
            leg.why = ATTRIBUTE.into();
            leg.reds = suites;
            a.input = serde_json::to_string(&leg).unwrap_or_default();
        }
    }

    fn certify(&mut self) {
        self.start(CERTIFY, "certify");
    }

    /// Certify refuses a round whose last pass scope is still finishing: it starts once none is alive.
    fn hold_certify(&mut self) {
        self.rc.certify_held = true;
        self.rc.leg = None;
        self.rc.leg_at_ms = self.now_ms;
    }

    fn release_certify(&mut self, policy: &Policy, repo: &Repo) {
        let alive = self.rc.batch.as_ref().is_some_and(|b| repo.scoped_batches.as_ref().is_some_and(|s| s.contains(b)));
        if alive && self.now_ms.saturating_sub(self.rc.leg_at_ms) <= policy.wall_ms {
            return;
        }
        self.rc.certify_held = false;
        self.certify();
    }

    fn abandon_naming(&mut self, why: &str, reds: Vec<String>) {
        self.abandon_ejecting(why, reds, None);
    }

    fn abandon_ejecting(&mut self, why: &str, reds: Vec<String>, ejected: Option<usize>) {
        if self.rc.batch.is_some() {
            let leg = Leg { why: why.to_string(), reds, ejected, key: format!("{}:{}", self.repo, self.rc.batch.clone().unwrap_or_default()), ..self.leg() };
            self.acts.push(leg.act(ABANDON));
        }
    }

    /// `Done`: the pace advances on the round's outcome, its pages and pause are written, and the
    /// machine is Idle with the round forgotten.
    fn finish(&mut self, outcome: pace::Outcome, detail: &str) {
        let (st, fx) = pace::advance(&self.rc.pace.core(), &outcome, self.repo, detail);
        self.rc.pace = Pace::of(&st, &self.rc.pace.refusals);
        if fx.pause {
            let on = if self.rc.landing_base.is_empty() { &self.rc.base } else { &self.rc.landing_base };
            self.rc.paused_on = Some(on.clone()).filter(|b| !b.is_empty());
        }
        if fx.pause || fx.stop {
            let leg = Leg { why: detail.to_string(), key: format!("{}:rounds", self.repo), bead: if fx.stop { "stop".into() } else { "pause".into() }, ..Leg::on(self.repo) };
            self.acts.push(leg.act(PAUSE));
        }
        for p in &fx.pages {
            self.acts.push(Act::new(INBOX_APPEND, p.clone()).with_input(page_line(self.now_ms, p)));
        }
        self.go(Trigger::Advanced);
        self.forget_round();
    }

    /// The round is over: what the repo shares stays, the round and the skip reason go.
    fn forget_round(&mut self) {
        let rc = std::mem::take(&mut self.rc);
        self.rc = RepoCarry {
            pace: rc.pace,
            base_moved: rc.base_moved,
            conflict_streaks: rc.conflict_streaks,
            held_keys: rc.held_keys,
            paused_on: rc.paused_on,
            refused: rc.refused,
            ..RepoCarry::default()
        };
    }

    /// A round whose every member was ejected holds no red: it is abandoned and the pace is not told.
    fn emptied(&mut self, why: &str) {
        self.abandon_naming(why, Vec::new());
        self.finish(pace::Outcome::Quiet, "");
    }

    fn red(&mut self, why: &str) {
        self.red_naming(why, Vec::new());
    }

    fn red_naming(&mut self, why: &str, reds: Vec<String>) {
        self.abandon_naming(why, reds);
        self.finish(pace::Outcome::Red, why);
    }

    /// A tick's observe, for the states that read the world. A leg's end arrives as its own
    /// `unit.ran` (`on_ran`); a leg past its wall and grace with none is judged lost here.
    fn observe(&mut self, policy: &Policy, repo: &Repo) {
        if self.settled(repo) {
            let batch = self.rc.batch.clone();
            self.forget_round();
            self.rc.reconciled = batch;
            return;
        }
        if self.voided(repo) || self.outcome_lost() || self.assemble_unsent(repo) {
            self.forget_round();
            return;
        }
        self.note_waiting();
        if self.superseded_by_green_head(repo) {
            return;
        }
        match self.rc.state {
            State::Certifying if self.rc.certify_held => self.release_certify(policy, repo),
            State::Corpus | State::Attributing | State::Certifying if self.now_ms.saturating_sub(self.rc.leg_at_ms) > leg_deadline_ms(policy, self.rc.state == State::Attributing, self.rc.members.len()) => self.leg_ended(Exit::Unknown),
            State::Corpus | State::Attributing | State::Certifying => self.watch_leg(repo),
            State::Opened if self.rc.batch.is_none() => {
                self.adopt_opened(repo);
                if self.rc.batch.is_none() && self.unsent(repo, ROUND_OPEN) {
                    let why = if self.rc.suites.unwrap_or(true) { "suites" } else { "" };
                    let key = core::round_key(&self.rc.members);
                    self.acts.push(Leg { key: format!("{}:{}", self.repo, digest(&key)), why: why.into(), ..self.leg() }.act(ROUND_OPEN));
                }
            }
            State::Certified if !self.below => {
                self.go(Trigger::LowerEnded);
                self.land_now();
            }
            _ => {}
        }
    }

    /// Follows the round's head. A leg started for an earlier head is orphaned: its result is
    /// never applied (`on_ran_with`), and the pass that replaced it decides the round. A green pass
    /// on the current head lands the round. True when the round was moved to land.
    fn superseded_by_green_head(&mut self, repo: &Repo) -> bool {
        let Some(open) = self.rc.batch.as_deref().and_then(|b| repo.open_rounds().find(|r| r.id == b)) else { return false };
        if open.head.is_empty() || !matches!(self.rc.state, State::Corpus | State::Attributing | State::Certifying) {
            return false;
        }
        self.rc.head = open.head.clone();
        if self.rc.leg_head.is_empty() {
            self.rc.leg_head = open.head.clone();
        }
        if self.rc.leg_head == open.head {
            return false;
        }
        eprintln!("{NAME} {}: round head moved {} -> {}: the step started for the old head is discarded", self.repo, self.rc.leg_head, open.head);
        self.rc.leg = None;
        if open.phase != "green" {
            return false;
        }
        self.go(if self.below { Trigger::HeadGreenHeld } else { Trigger::HeadGreen });
        if !self.below {
            self.land_now();
        }
        true
    }

    /// The state waits on `effect`'s outcome and the scheduler holds none in flight for the repo:
    /// the request died with a restart, and no outcome will come.
    fn unsent(&self, repo: &Repo, effect: &str) -> bool {
        repo.in_flight.as_ref().is_some_and(|f| !f.iter().any(|e| e == effect))
    }

    fn assemble_unsent(&self, repo: &Repo) -> bool {
        self.rc.state == State::Assembling && self.unsent(repo, ASSEMBLE)
    }

    fn awaits_outcome(&self) -> bool {
        matches!(self.rc.state, State::Assembling | State::Opened | State::Landing | State::Promoting)
    }

    /// The round has waited past [`OUTCOME_WAIT_MS`] in a state that only an effect's outcome
    /// moves: the outcome was lost, and the round would otherwise hold the repo for good.
    fn outcome_lost(&self) -> bool {
        self.awaits_outcome() && self.rc.waiting.is_some_and(|w| w.state == self.rc.state && self.now_ms.saturating_sub(w.since_ms) > OUTCOME_WAIT_MS)
    }

    fn note_waiting(&mut self) {
        let state = self.rc.state;
        self.rc.waiting = match self.rc.waiting {
            _ if !self.awaits_outcome() => None,
            Some(w) if w.state == state => Some(w),
            _ => Some(Waiting { state, since_ms: self.now_ms }),
        };
    }

    /// A leg the unit manager does not list for [`LEG_MISSING_OBSERVES`] observes in a row, past
    /// [`LEG_START_GRACE_MS`], never started or died with no `unit.ran`.
    fn watch_leg(&mut self, repo: &Repo) {
        let (Some(live), Some(leg)) = (&repo.live_legs, self.rc.leg.clone()) else { return };
        if self.now_ms.saturating_sub(self.rc.leg_at_ms) < LEG_START_GRACE_MS || live.contains(&leg) {
            self.rc.leg_missing = 0;
            return;
        }
        self.rc.leg_missing += 1;
        if self.rc.leg_missing >= LEG_MISSING_OBSERVES {
            self.leg_unstarted(&format!("its unit {leg} is not running and sent no result"));
        }
    }

    /// The carried leg did not run: it is started once more, and when that fails too the round is
    /// abandoned with the reason and the pace counts an infrastructure fault.
    fn leg_unstarted(&mut self, why: &str) {
        let what = match self.rc.state {
            State::Corpus => "corpus",
            State::Attributing => "attribution",
            _ => "certify",
        };
        self.rc.leg_missing = 0;
        if self.rc.leg_restarts < 1 {
            self.rc.leg_restarts += 1;
            self.go(Trigger::LegRestarted);
            match self.rc.state {
                State::Corpus => self.corpus(self.rc.run + 1),
                State::Attributing => self.attribute(),
                _ => {
                    self.rc.run += 1;
                    self.certify();
                }
            }
            return;
        }
        let why = format!("the {what} leg did not run: {why}, and it was started again once");
        self.go(Trigger::LegLost);
        if self.rc.batch.is_some() {
            let leg = Leg { why: why.clone(), bead: NO_DRIVER.into(), key: format!("{}:{}", self.repo, self.rc.batch.clone().unwrap_or_default()), ..self.leg() };
            self.acts.push(leg.act(ABANDON));
        }
        self.finish(pace::Outcome::Infra, &why);
    }

    /// A stacked round the queue no longer holds open: the round below it closed without
    /// landing and the queue voided it, members and all. Nothing is abandoned; it is forgotten
    /// and its members are cut again from the pool.
    fn voided(&self, repo: &Repo) -> bool {
        let Some(batch) = self.rc.batch.as_deref() else { return false };
        self.rc.stacked && stackable(self.rc.state) && !repo.open_rounds().any(|r| r.id == batch) && self.now_ms.saturating_sub(self.rc.leg_at_ms) > VOID_GRACE_MS
    }

    /// A round whose batch the lifecycle has already settled, with no open round of it and no
    /// unit running its leg: nothing is left to drive it. A Landing round is given a grace first,
    /// for the landing's own outcome, which may still stage a promotion.
    fn settled(&self, repo: &Repo) -> bool {
        let Some(batch) = self.rc.batch.as_deref() else { return false };
        let Some(live) = repo.live_legs.as_ref() else { return false };
        let running = self.rc.leg.as_ref().is_some_and(|leg| live.contains(leg));
        let landing_quiet = self.rc.state != State::Landing || self.rc.waiting.is_some_and(|w| w.state == State::Landing && self.now_ms.saturating_sub(w.since_ms) > VOID_GRACE_MS);
        lifecycle_driven(self.rc.state) && landing_quiet && repo.settled.iter().any(|b| b == batch) && !repo.open_rounds().any(|r| r.id == batch) && !running
    }

    fn land_now(&mut self) {
        let key = self.rc.round_key.clone().unwrap_or_default();
        self.acts.push(Leg { key: format!("round:{key}"), ..self.leg() }.act(LC_CERTIFY));
        self.acts.push(Leg { key: self.rc.batch.clone().unwrap_or_default(), ..self.leg() }.act(LAND));
    }

    /// The cut: whether this repo may open a further round now, and if so which. `live` are the
    /// rounds of the repo this task already drives, lowest first.
    fn cut(&mut self, policy: &Policy, repo: &Repo, live: &[RepoCarry]) {
        if live.len() >= policy.max_open {
            return;
        }
        let st = self.rc.pace.core();
        let paused = self.pause_in_force(repo);
        if let Some(h) = pace::hold(paused, repo.stopped) {
            self.rc.skip_why = Some(hold_why(&h, self.repo, repo));
            self.go(Trigger::Stopped);
            return;
        }
        self.rc.skip_why = self.rc.assemble_failed.as_ref().map(|(err, n)| assemble_failed_why(err, *n));
        if self.rc.state == State::Held {
            self.go(Trigger::Released);
        }
        if live.iter().any(|r| matches!(r.state, State::Assembling | State::Opened)) {
            return;
        }
        let ours: Vec<&str> = live.iter().filter_map(|r| r.batch.as_deref()).collect();
        if let Some(open) = repo.open_rounds().find(|r| !ours.contains(&r.id.as_str())) {
            self.foreign(policy, open, repo, &st);
            return;
        }
        if !live.iter().all(|r| stackable(r.state)) || repo.open_rounds().any(|r| r.red) {
            return;
        }
        if !live.is_empty() && repo.base_sha.is_empty() {
            return;
        }
        let onto = match repo.open_rounds().min_by_key(|r| r.age_secs) {
            Some(newest) if newest.head.is_empty() => return,
            Some(newest) => newest.head.clone(),
            None => String::new(),
        };
        let held: Vec<&str> = repo.open_rounds().flat_map(|r| r.members.iter().map(String::as_str)).chain(live.iter().flat_map(|r| r.members.iter().map(|(id, _)| id.as_str()))).collect();
        let (repo, refused) = self.without_refused(repo);
        let repo = Repo { pool: repo.pool.into_iter().filter(|m| !held.contains(&m.id.as_str())).collect(), ..repo };
        let repo = &repo;
        match select_why(repo, policy.max_members) {
            Ok((_, key)) if self.rc.held_keys.contains(&key) => {
                self.rc.skip_why = Some(held_key_why(&key));
                self.go(Trigger::NoTrigger);
            }
            Err(mut why) => {
                why.splice(0..0, refused);
                self.rc.skip_why = (!why.is_empty()).then(|| format!("{} member(s) wait, none admissible: {}", why.len(), why.join("; ")));
                self.go(Trigger::NoTrigger);
            }
            Ok((pool, key)) => {
                self.go(Trigger::Cut);
                self.rc.round_key = Some(key.clone());
                self.rc.stacked = !live.is_empty();
                // In merge order (`topo_order`: express, priority, prerequisites first): the
                // assemble effect rebuilds its members from `(id, tip)` alone, so the leg's
                // order is the order they merge in and the order a conflict is decided by.
                let members: Vec<(String, String)> = core::topo_order(&pool).iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
                let versions = repo.pool.iter().filter(|m| members.iter().any(|(id, _)| id == &m.id)).map(|m| (m.id.clone(), m.version)).collect();
                self.acts.push(Leg { key: format!("{}:{}", self.repo, repo.base_sha), base: repo.base_sha.clone(), ..Leg::on(self.repo) }.act(FETCH));
                self.acts.push(Leg { key: format!("{}:{}", self.repo, digest(&key)), members, versions, round_key: key, base: repo.base_sha.clone(), onto, ..Leg::on(self.repo) }.act(ASSEMBLE));
            }
        }
    }

    /// An open round this task does not drive: while it stands the repo cuts nothing. Undriven
    /// long enough it is re-driven, then a person is paged.
    fn foreign(&mut self, policy: &Policy, open: &Round, repo: &Repo, st: &pace::State) {
        if open.driven {
            self.go(Trigger::OpenRoundDriven);
            if !st.orphan.is_empty() {
                self.rc.pace = Pace::of(&pace::State { orphan: String::new(), orphan_since: 0, ..st.clone() }, &self.rc.pace.refusals);
            }
            return;
        }
        self.go(Trigger::OpenRoundUndriven);
        let unfit = if repo.open_round.as_ref().is_some_and(|r| r.id == open.id) { repo.unfit.clone() } else { open.unfit.clone() };
        let fit = unfit.map_or(Ok(()), Err);
        let now_s = self.now_ms / 1000;
        let (mut next, verdict) = pace::orphan_step(st, &open.id, now_s, policy.orphan_wait_s, fit);
        let leg = Leg { batch: open.id.clone(), wt: open.worktree.clone(), ..Leg::on(self.repo) };
        match verdict {
            pace::Orphan::Wait => {}
            pace::Orphan::Redrive => self.acts.push(Leg { key: format!("{}:{}", self.repo, open.id), ..leg }.act(REDRIVE)),
            pace::Orphan::Incident(why) => {
                let why = format!("round {} had no driver: {why}", open.id);
                self.acts.push(Leg { key: format!("{}:{}", self.repo, open.id), why: why.clone(), bead: NO_DRIVER.into(), ..leg }.act(ABANDON));
                let (after, fx) = pace::advance(&next, &pace::Outcome::Infra, self.repo, &why);
                next = after;
                for p in &fx.pages {
                    self.acts.push(Act::new(INBOX_APPEND, p.clone()).with_input(page_line(self.now_ms, p)));
                }
            }
        }
        self.rc.pace = Pace::of(&next, &self.rc.pace.refusals);
        self.go(Trigger::OrphanStepped);
    }

    /// The pause a red round wrote, if it still holds the repo. It carries the base its round was
    /// cut on; once a round lands past that base the red it guarded is behind the repo, so the pause is
    /// lifted (removed, and the lift logged). An operator's `hold:` is never lifted here.
    fn pause_in_force(&mut self, repo: &Repo) -> bool {
        let Some(text) = repo.paused.as_deref() else {
            if self.rc.paused_on.as_deref().is_some_and(|on| !repo.base_sha.is_empty() && on != repo.base_sha) {
                self.rc.paused_on = None;
            }
            return false;
        };
        if text.starts_with("hold:") {
            return true;
        }
        match self.rc.paused_on.clone() {
            None => {
                self.rc.paused_on = Some(repo.base_sha.clone());
                true
            }
            Some(on) if !repo.base_sha.is_empty() && on != repo.base_sha => {
                self.rc.paused_on = None;
                let why = format!("{} landed {} past {on}, the base the red round's pause was seen on — pause lifted ({})", self.repo, repo.base_sha, text.trim());
                self.acts.push(Leg { why, key: format!("{}:rounds:lift:{}", self.repo, repo.base_sha), bead: "lift".into(), ..Leg::on(self.repo) }.act(PAUSE));
                false
            }
            Some(_) => true,
        }
    }

    /// `repo` without the members `queue round open` refused at their current tip and base, and
    /// a line naming each; a refusal whose tip or base has moved since is forgotten.
    fn without_refused(&mut self, repo: &Repo) -> (Repo, Vec<String>) {
        self.rc.refused.retain(|id, (tip, base, _)| repo.pool.iter().any(|m| &m.id == id && &m.tip == tip) && base == &repo.base_sha);
        let refused = &self.rc.refused;
        let why = repo.pool.iter().filter_map(|m| refused.get(&m.id).map(|(_, _, kind)| format!("{}: queue round open refused it ({kind}) at this tip and base", m.id))).collect();
        let pool = repo.pool.iter().filter(|m| !refused.contains_key(&m.id)).cloned().collect();
        (Repo { pool, ..repo.clone() }, why)
    }

    fn leg_ended(&mut self, exit: Exit) {
        self.leg_ended_with(exit, "");
    }

    /// A leg ended with `exit`, having written `out` (its captured stdout tail, empty when none
    /// came back): the attribution and certify legs are judged by what they said.
    fn leg_ended_with(&mut self, exit: Exit, out: &str) {
        self.rc.leg_restarts = 0;
        self.rc.leg_missing = 0;
        let ok = green(exit);
        match (self.rc.state, ok) {
            (State::Corpus, true) => {
                self.go(Trigger::LegGreen);
                self.certify();
            }
            (State::Corpus, false) => {
                // Contract: Corpus × red → Attributing. The attribution leg ejects each red's
                // owners from the round (or hands them to their menders) and re-runs only their
                // suites on the survivors.
                self.go(Trigger::LegRed);
                self.attribute();
            }
            (State::Attributing, _) => match Attribution::parse(out) {
                Some(Attribution::Land { members, mended, .. }) if !members.is_empty() => {
                    self.rc.members = members;
                    self.rc.round_key = Some(core::round_key(&self.rc.members));
                    if mended {
                        // The menders' pass was settled inside the leg (`settle_mending`): Mending
                        // is entered and left on the same outcome.
                        self.go(Trigger::RerunMending);
                        self.go(Trigger::MendingSettled);
                    } else {
                        self.go(Trigger::RerunCertifying);
                    }
                    self.hold_certify();
                }
                Some(Attribution::Land { .. }) => {
                    self.go(Trigger::RerunBlocked);
                    self.emptied("attribution ejected every member — nothing is left to land");
                }
                Some(Attribution::Emptied(why)) => {
                    self.go(Trigger::RerunBlocked);
                    self.emptied(&why);
                }
                Some(Attribution::Fault(why)) => {
                    self.go(Trigger::RerunBlocked);
                    self.abandon_naming(&why, Vec::new());
                    self.finish(pace::Outcome::NoVm, &why);
                }
                Some(Attribution::Faulted(why)) => {
                    self.go(Trigger::RerunBlocked);
                    self.finish(pace::Outcome::AttributionFaulted, &why);
                }
                Some(Attribution::Blocked(why)) => {
                    self.go(Trigger::RerunBlocked);
                    self.red(&format!("local corpus red — held before reaching CI: {why}"));
                }
                None if exit == Exit::Signal(SIGTERM) => {
                    self.go(Trigger::RerunBlocked);
                    self.finish(pace::Outcome::Quiet, "");
                }
                None => {
                    self.go(Trigger::RerunBlocked);
                    let said = out.trim().lines().last().map(|l| format!(": {l}")).unwrap_or_default();
                    // No outcome means nothing was judged: the round stays OPEN, and the pause carries the leg's text.
                    let why = format!("local corpus red — the attribution leg ended ({exit:?}) with no outcome{said}");
                    self.finish(pace::Outcome::Red, &why);
                }
            },
            (State::Certifying, true) if self.below => {
                self.go(Trigger::CertifyHeld);
            }
            (State::Certifying, true) => {
                self.go(Trigger::CertifyGreen);
                self.land_now();
            }
            (State::Certifying, false) if exit == Exit::Code(queue::ops::round::VM_LOST) => self.vm_lost(),
            (State::Certifying, false) if exit == Exit::Code(queue::ops::round::VM_NO_OUTPUT) => self.vm_no_output(),
            (State::Certifying, false) if !self.rc.attributed && certify_red_evidence(out).1.is_empty() && !out.lines().any(|l| l.starts_with(RED_OUTPUT)) => {
                self.rc.attributed = true;
                self.go(Trigger::CertifyRedUnnamed);
                self.attribute();
            }
            (State::Certifying, false) if !self.rc.attributed && self.rc.members.len() > 1 && !certify_red_evidence(out).1.is_empty() => {
                self.rc.attributed = true;
                self.go(Trigger::CertifyRedNamed);
                self.attribute_suites(certify_red_evidence(out).1);
            }
            (State::Certifying, false) if self.rc.members.len() != 1 && certify_red_evidence(out).0.is_none() && certify_red_evidence(out).1.is_empty() && !out.lines().any(|l| l.starts_with(RED_OUTPUT)) => self.certify_faulted(exit, out),
            (State::Certifying, false) => {
                self.go(Trigger::CertifyRed);
                let (verdict, reds) = certify_red_evidence(out);
                let why = match (&verdict, reds.is_empty()) {
                    (Some(v), _) => format!("queue round certify judged the round red: {v}"),
                    (None, false) => format!("queue round certify judged the round red: {}", reds.join(",")),
                    (None, true) => "queue round certify judged the round red (its output named no verdict and no suite)".into(),
                };
                let output = out.lines().rev().find_map(|l| l.strip_prefix(RED_OUTPUT)).map(str::trim);
                if let [(id, tip)] = self.rc.members.clone().as_slice() {
                    let evidence = output.map_or_else(|| why.clone(), |o| format!("{why}; output kept at {o}"));
                    let key = format!("{}:{}:sole", self.repo, self.rc.batch.clone().unwrap_or_default());
                    self.acts.push(Leg { bead: id.clone(), head: tip.clone(), why: evidence, reds: reds.clone(), key, ..self.leg() }.act(EJECT));
                    self.abandon_ejecting(&why, reds, Some(1));
                    self.finish(pace::Outcome::Quiet, "");
                    return;
                }
                // The guilty leave the round charged: the members whose files the repository's
                // red output names (`io::implicated`, as `finish_local_round` ejects them).
                if let Some(output) = output {
                    let leg = Leg { why: output.to_string(), key: format!("{}:{}:guilty", self.repo, self.rc.batch.clone().unwrap_or_default()), ..self.leg() };
                    self.acts.push(leg.act(EJECT));
                }
                self.red_naming(&why, reds);
            }
            _ => {}
        }
    }

    /// `queue round certify` exited non-zero naming no verdict, no suite and no red output, after
    /// the round was attributed: it judged nothing, so the round is not red. Its output is kept,
    /// the round is left open for the re-drive, and the pace counts an Infra fault, not a pause.
    fn certify_faulted(&mut self, exit: Exit, out: &str) {
        let batch = self.rc.batch.clone().unwrap_or_default();
        let kept = certify_output_file(&batch, self.rc.run);
        let why = certify_fault_reason(exit, out, &kept);
        let leg = Leg { why: out.to_string(), key: kept, ..self.leg() };
        self.acts.push(leg.act(KEEP_CERTIFY_OUTPUT));
        self.go(Trigger::CertifyLostSpent);
        self.finish(pace::Outcome::Infra, &why);
    }

    /// `queue round certify` exited VM_LOST: the pass was recorded incomplete and the round is
    /// not judged. Suites are keyed by tree, so certifying the same head on a new VM resumes
    /// from the suites the lost pass finished (sp-e30jhh). Past [`LOST_VM_RECERTIFIES`] the round
    /// is left open for the orphan re-drive and the pace counts an Infra fault; it is never
    /// abandoned, and no member is charged.
    fn vm_lost(&mut self) {
        let batch = self.rc.batch.clone().unwrap_or_default();
        if self.rc.lost_vms < LOST_VM_RECERTIFIES {
            self.rc.lost_vms += 1;
            self.go(Trigger::CertifyLost);
            self.rc.run += 1;
            self.certify();
            return;
        }
        self.go(Trigger::CertifyLostSpent);
        self.finish(pace::Outcome::Infra, &format!("certify exited {}: the VM was lost under {} passes of {batch} running; the round is left open", queue::ops::round::VM_LOST, self.rc.lost_vms + 1));
    }

    /// `queue round certify` exited VM_NO_OUTPUT: the VM took the ssh connection and never started
    /// the round. Certified once more on a fresh VM; a second parks the round open as an Infra
    /// fault, with no member charged.
    fn vm_no_output(&mut self) {
        let batch = self.rc.batch.clone().unwrap_or_default();
        if self.rc.no_output_vms < NO_OUTPUT_RECERTIFIES {
            self.rc.no_output_vms += 1;
            self.go(Trigger::CertifyLost);
            self.rc.run += 1;
            self.certify();
            return;
        }
        self.go(Trigger::CertifyLostSpent);
        self.finish(pace::Outcome::Infra, &format!("certify exited {}: the VM produced no output under {} passes of {batch} running; the round is parked open", queue::ops::round::VM_NO_OUTPUT, self.rc.no_output_vms + 1));
    }

    /// The outcome of one of this task's own effects.
    fn outcome(&mut self, effect: &str, ok: bool, body: &str, leg: &Leg) {
        match (self.rc.state, effect) {
            (State::Assembling, ASSEMBLE) if !ok && body == runtime::engine::LOST_AT_RESTART => self.forget_round(),
            (State::Assembling, ASSEMBLE) if !ok => {
                self.assemble_failed(body.trim());
                self.go(Trigger::AssembledEmpty);
                self.rc.round_key = None;
            }
            (State::Assembling, ASSEMBLE) => {
                let a: Assembled = serde_json::from_str(body).unwrap_or_default();
                self.assembled(a, leg);
            }
            (State::Opened, ROUND_OPEN) if !ok && body == runtime::engine::LOST_AT_RESTART => {}
            (State::Opened, ROUND_OPEN) => {
                let o: Opened = if ok { serde_json::from_str(body).unwrap_or_default() } else { Opened::default() };
                if o.batch.is_empty() && !o.refused.is_empty() {
                    self.refused_some(&o.refused, leg);
                    return;
                }
                if o.batch.is_empty() {
                    self.go(Trigger::OpenFailed);
                    self.finish(pace::Outcome::OpenFailed, if ok { "queue round open named no batch" } else { body });
                    return;
                }
                self.rc.batch = Some(o.batch);
                self.opened(leg.why == "suites");
            }
            (State::Corpus | State::Attributing, CORPUS) | (State::Certifying, CERTIFY) if !ok && self.rc.leg.as_deref() == Some(leg.unit.as_str()) => self.leg_unstarted(&format!("it could not be started ({})", body.trim())),
            (State::Landing, LAND) if ok => {
                let l: Landed = serde_json::from_str(body).unwrap_or_default();
                if let Some(base) = l.base_moved {
                    self.base_moved(&base);
                    return;
                }
                match l.staged {
                    Some(staged) => {
                        self.go(Trigger::LandedStaged);
                        self.acts.push(Leg { batch: staged.clone(), key: staged, round_key: self.rc.round_key.clone().unwrap_or_default(), ..Leg::on(self.repo) }.act(PROMOTE));
                    }
                    None => {
                        self.go(Trigger::Landed);
                        self.finish(pace::Outcome::Quiet, "");
                    }
                }
            }
            (State::Landing, LAND) => {
                self.go(Trigger::LandRefused);
                self.red(&format!("NOT LANDED: {}", body.trim()));
            }
            (State::Promoting, PROMOTE) => {
                let p: Promoted = if ok { serde_json::from_str(body).unwrap_or_default() } else { Promoted::default() };
                if p.batch.is_empty() {
                    self.go(Trigger::PromoteFailed);
                    self.finish(pace::Outcome::Quiet, "");
                    return;
                }
                self.rc.batch = Some(p.batch);
                self.rc.members = p.members;
                self.rc.round_key = Some(core::round_key(&self.rc.members));
                if p.attested {
                    self.go(Trigger::PromotedToCertifying);
                    self.certify();
                } else {
                    self.go(Trigger::PromotedToCorpus);
                    self.corpus(1);
                }
            }
            _ => {}
        }
    }

    /// The round is open and named: its first leg starts.
    fn opened(&mut self, suites: bool) {
        if suites {
            self.go(Trigger::OpenedWithSuites);
            self.corpus(1);
        } else if self.rc.own_testenv {
            self.go(Trigger::OpenedOwnTestenv);
            self.certify();
        } else {
            self.go(Trigger::OpenFailed);
            self.red("round blocked — the merged tree has no test-*.sh suites; an empty corpus certifies nothing");
        }
    }

    /// An opened round whose `round open` outcome has not arrived, and which the queue already
    /// holds with exactly our members in our tree, is driven now rather than after [`OUTCOME_WAIT_MS`].
    fn adopt_opened(&mut self, repo: &Repo) {
        let ours: std::collections::BTreeSet<&str> = self.rc.members.iter().map(|(id, _)| id.as_str()).collect();
        let held = repo.open_rounds().find(|r| !ours.is_empty() && (self.rc.wt.is_empty() || r.worktree == self.rc.wt) && r.members.iter().map(String::as_str).collect::<std::collections::BTreeSet<_>>() == ours);
        let Some(held) = held else { return };
        self.rc.batch = Some(held.id.clone());
        self.opened(self.rc.suites.unwrap_or(true));
    }

    /// `round open` refused the members it names (typed, sp-822b8l): each is set aside at its tip
    /// and the round's base, and the round is asked for again with the rest; with none left the
    /// cut is over, nothing opened and nothing abandoned. The queue has already routed them (a
    /// base conflict back to rework, a round conflict into an integration that carries it).
    fn refused_some(&mut self, refused: &[(String, String, String)], leg: &Leg) {
        for (id, kind, _) in refused {
            let tip = self.rc.members.iter().find(|(m, _)| m == id).map(|(_, t)| t.clone()).unwrap_or_default();
            let landing = if self.rc.landing_base.is_empty() { &self.rc.base } else { &self.rc.landing_base };
            self.rc.refused.insert(id.clone(), (tip, landing.clone(), kind.clone()));
        }
        self.rc.members.retain(|(id, _)| !refused.iter().any(|(r, _, _)| r == id));
        if self.rc.members.is_empty() {
            self.go(Trigger::OpenRefusedAll);
            self.finish(pace::Outcome::Quiet, "");
            return;
        }
        self.go(Trigger::OpenRefusedSome);
        let key = core::round_key(&self.rc.members);
        self.rc.round_key = Some(key.clone());
        self.acts.push(Leg { key: format!("{}:{}", self.repo, digest(&key)), why: leg.why.clone(), ..self.leg() }.act(ROUND_OPEN));
    }

    /// `round land` refused base-moved (sp-tjrjca): the queue rebuilt the round on `base` and
    /// asks for `round certify`. The green round is certified again — once per base — and lands
    /// on its own green; base-moved again on the same base is a red.
    fn base_moved(&mut self, base: &str) {
        if self.rc.recertified_on.as_deref() == Some(base) {
            self.go(Trigger::LandRefused);
            self.red(&format!("NOT LANDED: base-moved again on {base} after the round was certified on it once"));
            return;
        }
        self.rc.recertified_on = Some(base.to_string());
        self.go(Trigger::LandBaseMoved);
        self.rc.run += 1;
        self.certify();
    }

    fn assemble_failed(&mut self, err: &str) {
        let n = match &self.rc.assemble_failed {
            Some((was, n)) if was == err => n + 1,
            _ => 1,
        };
        self.rc.skip_why = Some(assemble_failed_why(err, n));
        self.rc.assemble_failed = Some((err.to_string(), n));
        if n == ASSEMBLE_PAGE_AFTER {
            let page = format!("batcher {}: {}", self.repo, assemble_failed_why(err, n));
            self.acts.push(Act::new(INBOX_APPEND, page.clone()).with_input(page_line(self.now_ms, &page)));
        }
    }

    fn assembled(&mut self, a: Assembled, leg: &Leg) {
        if self.rc.assemble_failed.take().is_some() {
            self.rc.skip_why = None;
        }
        if !a.base.is_empty() && self.rc.base_moved.as_ref().map(|b| &b.0) != Some(&a.base) {
            self.rc.base_moved = Some((a.base.clone(), self.now_ms));
        }
        for (id, tip) in &a.conflicts {
            let prev = self.rc.conflict_streaks.get(id).map(|(t, n)| (t.as_str(), *n));
            let streak = core::conflict_streak(prev, tip);
            if streak >= core::CONFLICT_EJECT_ROUNDS {
                self.rc.conflict_streaks.remove(id);
                let version = leg.versions.get(id).copied().unwrap_or_default();
                self.acts.push(Leg { bead: id.clone(), head: tip.clone(), key: format!("{id}:{version}"), why: format!("conflicted with the round {streak} cuts running"), versions: [(id.clone(), version)].into(), ..Leg::on(self.repo) }.act(WITHDRAW));
            } else {
                self.rc.conflict_streaks.insert(id.clone(), (tip.clone(), streak));
            }
        }
        for id in a.merged.iter().map(|(id, _)| id) {
            self.rc.conflict_streaks.remove(id);
        }
        for bead in &a.base_conflicts {
            let unit = unit_key(self.repo, &format!("{bead}:{}", a.base), "rebase", 1);
            self.acts.push(Leg { bead: bead.clone(), base: a.base.clone(), key: unit.clone(), unit, ..Leg::on(self.repo) }.act(REBASE_STALE));
        }
        let key = core::round_key(&a.merged);
        if a.merged.is_empty() || self.rc.held_keys.contains(&key) {
            hold_key(&mut self.rc, &leg.round_key);
            self.rc.skip_why = Some(held_key_why(&leg.round_key));
            self.go(Trigger::AssembledEmpty);
            self.rc.round_key = None;
            return;
        }
        self.go(Trigger::Assembled);
        self.rc.round_key = Some(key.clone());
        self.rc.members = a.merged;
        self.rc.wt = a.wt;
        self.rc.base = if leg.onto.is_empty() { a.base } else { leg.onto.clone() };
        self.rc.landing_base = leg.base.clone();
        self.rc.own_testenv = a.own_testenv;
        self.rc.suites = Some(a.suites);
        let why = if a.suites { "suites" } else { "" };
        self.acts.push(Leg { key: format!("{}:{}", self.repo, digest(&key)), why: why.into(), ..self.leg() }.act(ROUND_OPEN));
    }
}

/// `rc` stored under `repo`, or removed when it is the default: an idle repo carries nothing.
fn store(carry: &mut Carry, repo: &str, rc: RepoCarry) {
    if rc == RepoCarry::default() {
        carry.repos.remove(repo);
    } else {
        carry.repos.insert(repo.to_string(), rc);
    }
}

pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    decide_with(&Policy::default(), carry, snap)
}

/// `decide` with the pinned policy.
pub fn decide_with(policy: &Policy, carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let (mut next, mut acts) = (carry.clone(), vec![]);
    for (name, repo) in &snap.repos {
        let rc = carry.repos.get(name).cloned().unwrap_or_default();
        let (rc, more) = decide_repo(policy, name, &rc, repo, snap.now_ms);
        store(&mut next, name, rc);
        acts.extend(more);
    }
    (next, acts)
}

/// One repo's tick: each round this task drives is observed lowest first, then the repo may cut
/// a further one. A tick on which one of its rounds finished cuts nothing: the next tick judges
/// the cut on the outcome the finish wrote, as a single round always has. A voided round writes
/// no outcome, so its members are cut again at once.
fn decide_repo(policy: &Policy, name: &str, rc: &RepoCarry, repo: &Repo, now_ms: u64) -> (RepoCarry, Vec<Act>) {
    let idle = if rc.live() { State::Idle } else { rc.state };
    let (mut shared, mut live, mut acts, mut ended) = (rc.clone(), Vec::<RepoCarry>::new(), vec![], false);
    for round in rc.rounds() {
        let mut m = Machine { repo: name, rc: shared.holding(&round), acts: vec![], now_ms, below: !live.is_empty() };
        let voided = m.voided(repo) || m.outcome_lost() || m.settled(repo) || m.assemble_unsent(repo);
        m.observe(policy, repo);
        acts.extend(m.acts);
        if m.rc.live() {
            live.push(m.rc.round());
        } else {
            ended |= !voided;
        }
        shared = m.rc;
    }
    let mut m = Machine { repo: name, rc: shared.holding(&RepoCarry { state: idle, ..RepoCarry::default() }), acts, now_ms, below: !live.is_empty() };
    if !ended {
        m.cut(policy, repo, &live);
    }
    if m.rc.live() {
        live.push(m.rc.round());
    }
    let state = if live.is_empty() { m.rc.state } else { idle };
    (RepoCarry::rebuilt(&m.rc, live, state), m.acts)
}

/// Where `leg`'s round stands in `rc`: the index among its live rounds of the one the leg
/// belongs to, by batch, round key or unit; a leg naming none belongs to the lowest.
fn round_of(rc: &RepoCarry, leg: &Leg) -> Option<usize> {
    let named = !(leg.batch.is_empty() && leg.round_key.is_empty() && leg.unit.is_empty());
    let rounds = rc.rounds();
    if !named {
        return (!rounds.is_empty()).then_some(0);
    }
    rounds.iter().position(|r| {
        (!leg.batch.is_empty() && r.batch.as_deref() == Some(leg.batch.as_str()))
            || (!leg.round_key.is_empty() && r.round_key.as_deref() == Some(leg.round_key.as_str()))
            || (!leg.unit.is_empty() && r.leg.as_deref() == Some(leg.unit.as_str()))
    })
}

/// `rc` with round `at` of its live rounds replaced by what `m` made of it.
fn replace_round(rc: &RepoCarry, at: usize, m: &Machine) -> RepoCarry {
    let mut rounds = rc.rounds();
    if m.rc.live() {
        rounds[at] = m.rc.round();
    } else {
        rounds.remove(at);
    }
    let idle = if rounds.is_empty() { State::Idle } else { rc.state };
    RepoCarry::rebuilt(&m.rc, rounds, idle)
}

/// The machine's answer to the outcome of one of its own effects, whose input was `leg`.
pub fn on_outcome(carry: &Carry, effect: &str, ok: bool, body: &str, leg: &Leg, now_ms: u64) -> (Carry, Vec<Act>) {
    let mut next = carry.clone();
    let rc = carry.repos.get(&leg.repo).cloned().unwrap_or_default();
    let Some(at) = round_of(&rc, leg) else { return (next, vec![]) };
    let mut m = Machine { repo: &leg.repo, rc: rc.holding(&rc.rounds()[at]), acts: vec![], now_ms, below: at > 0 };
    m.outcome(effect, ok, body, leg);
    let acts = std::mem::take(&mut m.acts);
    store(&mut next, &leg.repo, replace_round(&rc, at, &m));
    (next, acts)
}

/// The machine's answer to the end of one of its own legs, delivered on this task's stream: the
/// repo whose carried leg it is moves at once. A run that is no carried leg changes nothing.
pub fn on_ran(carry: &Carry, ran: &UnitRan, now_ms: u64) -> Option<(Carry, Vec<Act>)> {
    on_ran_with(carry, ran, &RanOutput::default(), now_ms)
}

/// [`on_ran`] with what the leg wrote (its `unit.ran`'s captured stdout tail): the attribution
/// and certify legs are judged by their own lines.
pub fn on_ran_with(carry: &Carry, ran: &UnitRan, out: &RanOutput, now_ms: u64) -> Option<(Carry, Vec<Act>)> {
    let key = ran.key_for(NAME)?;
    let (repo, rc, at) = carry.repos.iter().find_map(|(repo, rc)| {
        let at = rc.rounds().iter().position(|r| r.leg.as_deref() == Some(key.as_str()) && matches!(r.state, State::Corpus | State::Attributing | State::Certifying))?;
        Some((repo, rc, at))
    })?;
    let held = &rc.rounds()[at];
    if !held.head.is_empty() && !held.leg_head.is_empty() && held.head != held.leg_head {
        eprintln!("{NAME} {repo}: step {key} was started for head {} and the round is at {}: its result is discarded", held.leg_head, held.head);
        return None;
    }
    let mut next = carry.clone();
    let mut m = Machine { repo, rc: rc.holding(&rc.rounds()[at]), acts: vec![], now_ms, below: at > 0 };
    let said = [out.stdout_tail.as_deref(), out.stderr_tail.as_deref()].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
    m.leg_ended_with(ran.exit, &said);
    let acts = std::mem::take(&mut m.acts);
    store(&mut next, repo, replace_round(rc, at, &m));
    Some((next, acts))
}

/// The fixed names a family-A P2 task's handler is driven by.
pub struct P2<'a> {
    pub shape: &'a Shape,
    pub cadence: &'a Cadence,
    pub acts: &'a [(&'a str, u64)],
    pub emits: &'a [EventKindTag],
    pub effects: &'a [&'a str],
    /// Hand-offs from other tasks that ask for an observe now, without re-arming the tick.
    pub wakes: &'a [EventKindTag],
}

/// What a family-A P2 task's handler does with each kind of event it hears.
pub struct On<O, U, R, L> {
    /// The observe answer.
    pub observed: O,
    /// The outcome of one of its effects.
    pub outcome: U,
    /// A `unit.ran` delivered on its own stream (the source routes each run to the task its unit
    /// name parses to): the leg's end advances the machine at once, not at the next observe.
    pub ran: R,
    /// The unit keys of the legs the carry has in flight: each is read through `unit.facts` on a
    /// tick, so a leg whose `unit.ran` was lost still ends.
    pub legs: L,
}

/// How long a `unit.facts` read may take.
const FACTS_DEADLINE_MS: u64 = 10_000;

fn facts_probe(task: &str, key: &str) -> Option<Command> {
    let handle = UnitHandle::of(task, key).ok()?;
    Some(Command::Probe(EffectSpec::keyed(UNIT_FACTS, format!("{UNIT_FACTS}/{}", handle.name), serde_json::to_string(&handle).ok()?, FACTS_DEADLINE_MS)))
}

/// The run `facts` reports for the unit a probe read, when that unit has ended.
fn ran_from_facts(input: &str, body: &str) -> Option<UnitRan> {
    let handle: UnitHandle = serde_json::from_str(input).ok()?;
    let facts: UnitFacts = serde_json::from_str(body).ok()?;
    let ended = matches!(facts.state, UnitState::Inactive | UnitState::Failed);
    Some(UnitRan { handle, exit: facts.exit.filter(|_| ended)?, duration_ms: 0 })
}

/// The shared handler body of the family-A P2 tasks: the tick's probe, the observe answer, each
/// outcome of one of `effects`, and the end of each of its own unit legs.
pub fn handle_p2<C, O, U, R, L>(p: &P2, view: &Projection, ev: &Event, on: On<O, U, R, L>) -> Vec<Command>
where
    C: Serialize + serde::de::DeserializeOwned + Default + PartialEq,
    O: FnOnce(&C, &str) -> Option<(C, Vec<Act>)>,
    U: FnOnce(&C, &str, bool, &str, &str) -> (C, Vec<Act>),
    R: FnOnce(&C, &UnitRan) -> Option<(C, Vec<Act>)>,
    L: FnOnce(&C) -> Vec<String>,
{
    let (shape, cadence, acts, emits, effects) = (p.shape, p.cadence, p.acts, p.emits, p.effects);
    let On { observed, outcome, ran, legs } = on;
    if ev.stream.0 != shape.stream {
        return vec![];
    }
    let carry: C = kit::carry(shape, view);
    if ev.kind.tag == shape.tick {
        let mut out = kit::on_tick(shape, serde_json::to_string(&carry).unwrap_or_default(), cadence.next_after(ev.at));
        out.extend(legs(&carry).iter().filter_map(|key| facts_probe(shape.name, key)));
        return out;
    }
    if p.wakes.contains(&ev.kind.tag.as_str()) {
        return vec![Command::Probe(EffectSpec::probe(shape.observe, serde_json::to_string(&carry).unwrap_or_default(), shape.observe_deadline_ms))];
    }
    if let Some(r) = UnitRan::from_event_kind(&ev.kind) {
        let Some((next, out)) = ran(&carry, &r) else { return vec![] };
        return kit::commands(shape, acts, emits, &carry, &next, out);
    }
    let Some(spec) = crate::watchers::answered_spec(view, ev) else { return vec![] };
    if spec.name == UNIT_FACTS {
        if ev.kind.tag != EFFECT_SUCCEEDED {
            return vec![];
        }
        let Some((next, out)) = ran_from_facts(&spec.input, &ev.kind.body).and_then(|r| ran(&carry, &r)) else { return vec![] };
        return kit::commands(shape, acts, emits, &carry, &next, out);
    }
    let (next, out) = if spec.name == shape.observe {
        if ev.kind.tag != EFFECT_SUCCEEDED {
            eprintln!("{}: observe did not succeed ({}): {}", shape.name, ev.kind.tag, ev.kind.body);
            return vec![];
        }
        match observed(&carry, &ev.kind.body) {
            Some(d) => d,
            None => {
                eprintln!("{}: observe answered something that is not a snapshot", shape.name);
                return vec![];
            }
        }
    } else if effects.contains(&spec.name.as_str()) {
        outcome(&carry, &spec.name, ev.kind.tag == EFFECT_SUCCEEDED, &ev.kind.body, &spec.input)
    } else {
        return vec![];
    };
    kit::commands(shape, acts, emits, &carry, &next, out)
}

/// The tags every family-A P2 task listens for: its tick, its effects' outcomes, its own
/// units' ends, and the hand-offs that wake it.
pub fn interests(tick: EventKindTag, wakes: &[EventKindTag]) -> Vec<EventKindTag> {
    [tick, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT, UNIT_RAN].into_iter().chain(wakes.iter().copied()).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatcherLocal {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
    pub policy: Policy,
    interests: Vec<EventKindTag>,
}

impl BatcherLocal {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        BatcherLocal { cadence: Cadence::Every(CADENCE_MS), policy: Policy::pinned(&pinned), pinned, interests: interests(TICK, WAKES) }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("batcher.local/boot", STREAM, EventKind::new(TICK, ""))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "repos", drop: &[] }];

impl Handler for BatcherLocal {
    fn interests(&self) -> &[EventKindTag] {
        &self.interests
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let observed = |carry: &Carry, body: &str| {
            serde_json::from_str::<Snapshot>(body).ok().map(|mut snap| {
                in_flight(view, &ev.stream, &mut snap);
                let (next, acts) = decide_with(&self.policy, carry, &snap);
                // law-a-failure-carries-its-own-explanation: a pass that cuts nothing while
                // members wait says why, once each time the reason changes.
                for line in notes(carry, &next) {
                    eprintln!("{line}");
                }
                (next, acts)
            })
        };
        let outcome = |carry: &Carry, effect: &str, ok: bool, body: &str, input: &str| match serde_json::from_str::<Leg>(input) {
            Ok(leg) => on_outcome(carry, effect, ok, body, &leg, ev.at),
            Err(_) => (Carry { repos: carry.repos.clone() }, vec![]),
        };
        let out = RanOutput::from_event_kind(&ev.kind).unwrap_or_default();
        let ran = |carry: &Carry, r: &UnitRan| on_ran_with(carry, r, &out, ev.at);
        let legs = |carry: &Carry| carry.repos.values().flat_map(|rc| rc.rounds()).filter(|r| matches!(r.state, State::Corpus | State::Attributing | State::Certifying)).filter_map(|r| r.leg).collect();
        handle_p2(&P2 { shape: &SHAPE, cadence: &self.cadence, acts: ACTS, emits: EMITS, effects: EFFECTS, wakes: WAKES }, view, ev, On { observed, outcome, ran, legs })
    }
}

/// Each repo of `snap` given the effects of this task the stream has asked for and not had answered.
fn in_flight(view: &Projection, stream: &runtime::event::StreamKey, snap: &mut Snapshot) {
    let events = view.events(stream);
    let answered: std::collections::BTreeSet<&runtime::event::EventId> = events.iter().filter(|e| [EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT].contains(&e.kind.tag.as_str())).filter_map(|e| e.cause.as_ref()).collect();
    let mut flying: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for req in events.iter().filter(|e| e.kind.tag == EFFECT_REQUESTED && !answered.contains(&e.id)) {
        let Ok(body) = serde_json::from_str::<serde_json::Value>(&req.kind.body) else { continue };
        let (Some(name), Some(input)) = (body["spec"]["name"].as_str(), body["spec"]["input"].as_str()) else { continue };
        if let Ok(leg) = serde_json::from_str::<Leg>(input) {
            flying.entry(leg.repo).or_default().push(name.to_string());
        }
    }
    for (name, repo) in &mut snap.repos {
        repo.in_flight = Some(flying.remove(name).unwrap_or_default());
    }
}

/// The runners: each effect's real work, in this process. The seam is `batcher_cut::io`, the
/// batcher's own IO library (git as a child, the store through `spira-lc`'s socket, the queue's
/// round verbs where the store has no door yet); every long leg is a unit on `Effects::units`.
pub mod edge {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use batcher_cut::io::{self, Env};
    use runtime::units::{ScopeSpec, UnitSpec, UNIT_RUN_SCOPE, UNIT_START};

    use super::{Assembled, Landed, Leg, Member, Opened, Promoted, Repo, Round, Snapshot};
    use crate::{Effects, EffectSpec};

    /// The seam's environment from the pinned config, as the `batcher` binary builds it.
    pub fn env() -> Result<Env, String> {
        let home = spira_config::resolve::locate_home_for_process()?;
        let run = PathBuf::from(spira_config::process::cfg("SPIRA_RUN")?);
        batcher_cut::env_from_cfg(home, run, None)
    }

    pub fn leg(spec: &EffectSpec) -> Result<Leg, String> {
        serde_json::from_str(&spec.input).map_err(|e| format!("{}: bad input: {e}", spec.name))
    }

    fn json<T: serde::Serialize>(v: &T) -> Result<String, String> {
        serde_json::to_string(v).map_err(|e| e.to_string())
    }

    fn core_members(members: &[(String, String)]) -> Vec<batcher::core::Member> {
        members.iter().map(|(id, tip)| batcher::core::Member { id: id.clone(), tip: tip.clone(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: BTreeMap::new(), blocked_by: vec![] }).collect()
    }

    /// One unit leg under the naming rule: a scope with the round's CPU quota when the backend
    /// runs scopes, else a plain transient unit.
    /// The base environment every leg unit needs, as sentinel-call gives a dwarf: the release, a
    /// PATH onto it, HOME and the one config spec. A transient unit inherits none of the
    /// runtime's environment, so without `SPIRA_TOML` round-vm refuses at once (exit 2) and the
    /// round is abandoned before it runs. A variable the leg already set is kept.
    pub fn leg_env(vars: Vec<(String, String)>, release: &str, home: &str, toml: &str, runtime_path: &str) -> Vec<(String, String)> {
        let tail = if runtime_path.is_empty() { "/usr/local/bin:/usr/bin:/bin" } else { runtime_path };
        let base = [
            ("SPIRA_RELEASE", release.to_string()),
            ("PATH", format!("{release}/bin:{release}/spira:{tail}")),
            ("HOME", home.to_string()),
            ("SPIRA_TOML", toml.to_string()),
        ];
        let base: Vec<(String, String)> = base.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        with_defaults(vars, &base)
    }

    pub fn with_defaults(mut vars: Vec<(String, String)>, base: &[(String, String)]) -> Vec<(String, String)> {
        for (k, v) in base {
            if !vars.iter().any(|(have, _)| have == k) {
                vars.push((k.clone(), v.clone()));
            }
        }
        vars
    }

    pub fn base_env(vars: Vec<(String, String)>, toml: &str) -> Vec<(String, String)> {
        let release = std::env::var(spira_config::RELEASE_ENV).unwrap_or_default();
        let home = std::env::var("HOME").unwrap_or_default();
        let path = std::env::var("PATH").unwrap_or_default();
        leg_env(vars, &release, &home, toml, &path)
    }

    fn with_base_env(vars: Vec<(String, String)>) -> Result<Vec<(String, String)>, String> {
        Ok(base_env(vars, &spira_config::process::spec()?))
    }

    fn logged(task: &str, key: &str, started: Result<String, String>) -> Result<String, String> {
        match &started {
            Ok(_) => eprintln!("{task}: leg {key} started"),
            Err(why) => eprintln!("{task}: leg {key} did not start: {why}"),
        }
        started
    }

    pub fn start_unit(e: &Effects, task: &str, key: &str, argv: Vec<String>, vars: Vec<(String, String)>, cwd: Option<PathBuf>) -> Result<String, String> {
        logged(task, key, start_unit_quiet(e, task, key, argv, vars, cwd))
    }

    fn start_unit_quiet(e: &Effects, task: &str, key: &str, argv: Vec<String>, vars: Vec<(String, String)>, cwd: Option<PathBuf>) -> Result<String, String> {
        let units = e.units.as_deref().ok_or("no unit manager configured")?;
        let vars = with_base_env(vars)?;
        let unit = UnitSpec { task: task.into(), key: key.into(), argv, env: vars, cwd };
        let quota = std::env::var("ROUND_CPU_QUOTA").ok().and_then(|q| q.trim().trim_end_matches('%').parse::<u32>().ok());
        if quota.is_some() {
            let scope = ScopeSpec { unit: unit.clone(), cpu_quota_pct: quota, memory_max_bytes: None };
            if let Ok(h) = runtime::units::effect(units, UNIT_RUN_SCOPE, &json(&scope)?) {
                return Ok(h);
            }
        }
        runtime::units::effect(units, UNIT_START, &json(&unit)?)
    }

    /// A leg whose stdout comes back on its `unit.ran` (`UNIT_RUN_SCOPE`): the attribution and
    /// certify legs, judged by their own lines. Under the round's CPU quota when one is set.
    pub fn start_scope(e: &Effects, task: &str, key: &str, argv: Vec<String>, vars: Vec<(String, String)>, cwd: Option<PathBuf>) -> Result<String, String> {
        logged(task, key, start_scope_quiet(e, task, key, argv, vars, cwd))
    }

    fn start_scope_quiet(e: &Effects, task: &str, key: &str, argv: Vec<String>, vars: Vec<(String, String)>, cwd: Option<PathBuf>) -> Result<String, String> {
        let units = e.units.as_deref().ok_or("no unit manager configured")?;
        let vars = with_base_env(vars)?;
        let quota = std::env::var("ROUND_CPU_QUOTA").ok().and_then(|q| q.trim().trim_end_matches('%').parse::<u32>().ok());
        let scope = ScopeSpec { unit: UnitSpec { task: task.into(), key: key.into(), argv, env: vars, cwd }, cpu_quota_pct: quota, memory_max_bytes: None };
        runtime::units::effect(units, UNIT_RUN_SCOPE, &json(&scope)?)
    }

    /// `batcher attribute` on the red round: `drive::attribute_round` as one captured leg.
    pub fn attribute_for(task: &str, e: &Effects, env: &Env, leg: &Leg) -> Result<String, String> {
        let batcher = spira_config::release_env::tool("batcher")?;
        let mut argv = vec![batcher, "attribute".into(), leg.repo.clone(), "--members".into(), batcher_cut::attribute::members_arg(&leg.members), "--wt".into(), leg.wt.clone(), "--base".into(), leg.base.clone(), "--round-vm".into(), env.round_vm.display().to_string()];
        if !leg.batch.is_empty() {
            argv.extend(["--batch".into(), leg.batch.clone()]);
        }
        if !leg.reds.is_empty() {
            argv.extend(["--suites".into(), leg.reds.join(",")]);
        }
        start_scope(e, task, &leg.unit, argv, home_vars(env), None)
    }

    fn home_vars(env: &Env) -> Vec<(String, String)> {
        vec![("SPIRA_HOME".into(), env.home.display().to_string()), ("SPIRA_RUN".into(), env.run.display().to_string())]
    }

    /// The probe: every admitted `queue.local` repo's pool, open round, base and hold.
    pub fn observe(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let carried: super::Carry = serde_json::from_str(&spec.input).unwrap_or_default();
        let env = env()?;
        io::lc_forget(&env);
        let enabled = spira_config::process::cfg("SPIRA_BATCHER_ENABLE")?.trim() != "0";
        let reg = io::registry(&env);
        let mut snap = Snapshot { now_ms: spira_config::vtime::now_epoch() * 1000, ..Snapshot::default() };
        for name in reg.names().into_iter().filter(|n| reg.land(n) == "queue.local" && (enabled || n != reg.home_repo())) {
            let repo = batcher_cut::find_repo(&env, &name)?;
            let mut seen = observe_repo(&env, &repo)?;
            seen.live_legs = live_legs(e);
            seen.scoped_batches = scoped_batches(e, &name, &carried);
            seen.settled = carried.repos.get(&name).map(|rc| settled_batches(&env, rc, &seen)).unwrap_or_default();
            snap.repos.insert(name.clone(), seen);
        }
        json(&snap)
    }

    /// The batches of `rc`'s leg-driven rounds that the queue no longer holds open and the
    /// lifecycle reports LANDED or ABANDONED; a batch the store cannot say is not settled.
    fn settled_batches(env: &Env, rc: &super::RepoCarry, seen: &Repo) -> Vec<String> {
        rc.rounds()
            .into_iter()
            .filter(|r| super::lifecycle_driven(r.state))
            .filter_map(|r| r.batch)
            .filter(|b| !seen.open_rounds().any(|o| &o.id == b))
            .filter(|b| {
                let shown = io::lcq_held(env, &["show-batch", b]).ok().and_then(|out| serde_json::from_str::<serde_json::Value>(&out).ok());
                matches!(shown.as_ref().and_then(|v| v.get("state")).and_then(|s| s.as_str()), Some("LANDED" | "ABANDONED"))
            })
            .collect()
    }

    /// The keys of this task's leg units the manager lists as running; `None` when it cannot say.
    fn live_legs(e: &Effects) -> Option<Vec<String>> {
        let units = e.units.as_deref()?;
        let listed = runtime::units::effect(units, runtime::units::UNIT_LIST, &json(&runtime::units::task_glob(super::NAME)).ok()?).ok()?;
        let all: Vec<runtime::units::UnitDetail> = serde_json::from_str(&listed).ok()?;
        Some(super::running_legs(&all))
    }

    /// Of the carried batches, those with a pass scope running; `None` when the manager cannot say.
    fn scoped_batches(e: &Effects, repo: &str, carried: &super::Carry) -> Option<Vec<String>> {
        let units = e.units.as_deref()?;
        let mut live = vec![];
        for batch in carried.repos.get(repo)?.rounds().into_iter().filter_map(|r| r.batch) {
            if runtime::units::round_scope_live(units, repo, &batch).ok()? {
                live.push(batch);
            }
        }
        Some(live)
    }

    fn observe_repo(env: &Env, repo: &batcher_cut::io::Repo) -> Result<Repo, String> {
        let sifted = io::sifted_tips(env)?;
        let pool: Vec<Member> = io::certified_pool(env, repo)?
            .into_iter()
            .map(|m| Member {
                state: "CERTIFIED".into(),
                sifted_tip: sifted.get(&m.id).cloned(),
                title: m.title.clone(),
                priority: m.priority.unwrap_or(u8::MAX),
                deps: m.blocked_by.clone(),
                stack: m.stack.iter().map(|(i, t)| format!("{i}:{t}")).collect(),
                base_fix: m.base_fix,
                express: m.express,
                certified_at: m.certified_at,
                id: m.id,
                tip: m.tip,
                ..Member::default()
            })
            .collect();
        let blockers = pool.iter().flat_map(|m| m.deps.clone()).collect();
        let branch_tips = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
        let mut open = io::open_rounds(env, repo)?.into_iter().map(|r| {
            let driven = io::certify_driven(env, &repo.name, &r.id);
            let unfit = io::round_certifiable(env, repo, &r.id).err();
            Round { id: r.id, members: r.members, head: r.head, worktree: r.worktree, driven, red: r.red, age_secs: r.age_secs, unfit, phase: r.phase }
        });
        let open_round = open.next();
        let open_above: Vec<Round> = open.collect();
        let (stopped, stop_why, paused) = holds(env, &repo.name);
        let unfit = open_round.as_ref().and_then(|r| r.unfit.clone());
        let base_sha = if open_round.is_some() { io::resolve_base_sha(repo).unwrap_or_default() } else { io::resolve_base_sha(repo)? };
        Ok(Repo { pool, blockers, branch_tips, open_round, open_above, base_sha, stopped, stop_why, paused, unfit, ..Repo::default() })
    }

    /// The repo's holds: the scoped stop of its `rounds` lane (and its reason), and the pace's
    /// pause file's text — each told apart, so the hold the pass names is the one that stands.
    fn holds(env: &Env, repo: &str) -> (bool, Option<String>, Option<String>) {
        let files = batcher_cut::pace::Files::new(&env.run, repo);
        let stop = spira_config::scoped_stop::stop_for(&env.run, repo, "rounds").map(|s| format!("{}: {} (until: {})", s.owner, s.reason, s.until));
        let paused = files.pause_file().exists().then(|| std::fs::read_to_string(files.pause_file()).unwrap_or_default());
        (stop.is_some(), stop, paused)
    }

    fn repo_of(env: &Env, leg: &Leg) -> Result<batcher_cut::io::Repo, String> {
        batcher_cut::find_repo(env, &leg.repo)
    }

    pub fn fetch(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        io::fetch_base(&repo_of(&env, &leg)?)?;
        Ok("{}".into())
    }

    /// The cut's git half (`cut_new_round_inner` up to the open): the round tree reset to the
    /// base, each member merged in topological order, the core's `combine` over the merges.
    pub fn assemble(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let repo = repo_of(&env, &leg)?;
        let Some(_lock) = io::try_lock(&env, &repo.name)? else { return Err(format!("{}: another operation holds the lock", repo.name)) };
        let base = io::resolve_base_sha(&repo)?;
        let sorted = batcher::core::topo_order(&core_members(&leg.members));
        let sequenced = batcher::core::stack_sequencing(&sorted);
        let (wt, at) = if leg.onto.is_empty() {
            (env.run.join("worktree").join(format!(".batcher-{}", repo.name)), base.clone())
        } else {
            (env.run.join("worktree").join(format!(".batcher-{}-{}", repo.name, &super::digest(&leg.round_key)[..12])), leg.onto.clone())
        };
        let cleared = io::worktree_reset(&repo, &wt, &at)?;
        let merges: BTreeMap<String, batcher::core::MergeResult> = sorted.iter().map(|m| (m.id.clone(), io::merge_member(&env, &wt, &m.id, &m.tip))).collect();
        let combined = batcher::core::combine(&batcher::core::CombineInput { pool: &sorted, merges: &merges, deleted_suites: &BTreeMap::new(), sequenced: &sequenced, siblings: &BTreeMap::new() });
        let conflicts = sorted.iter().filter(|m| merges.get(&m.id) == Some(&batcher::core::MergeResult::Conflict)).map(|m| (m.id.clone(), m.tip.clone())).collect::<Vec<_>>();
        let base_conflicts = conflicts.iter().filter(|(_, tip)| io::base_conflict(&repo, &base, tip).is_some()).map(|(id, _)| id.clone()).collect();
        let suites = !io::suites_in(&wt, "HEAD").is_empty();
        json(&Assembled { merged: combined.merged.iter().map(|m| (m.id.clone(), m.tip.clone())).collect(), own_testenv: batcher_cut::declares_own_testenv(&wt), wt: wt.display().to_string(), base, suites, conflicts, base_conflicts, cleared })
    }

    pub fn round_open(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let repo = repo_of(&env, &leg)?;
        let Some(_lock) = io::try_lock(&env, &repo.name)? else { return Err(format!("{}: another operation holds the lock", repo.name)) };
        match io::round_open_or_refused(&env, &repo, std::path::Path::new(&leg.wt), &core_members(&leg.members))? {
            io::OpenOutcome::Opened(batch) => json(&Opened { batch, ..Opened::default() }),
            io::OpenOutcome::Refused(refused, line) => {
                for r in &refused {
                    eprintln!("{} {}: queue round open refused {} ({}): {} — cutting without it", super::NAME, repo.name, r.id, r.kind, r.why);
                }
                eprintln!("{} {}: {line}", super::NAME, repo.name);
                json(&Opened { refused: refused.into_iter().map(|r| (r.id, r.kind, r.why)).collect(), ..Opened::default() })
            }
        }
    }

    /// The corpus leg: `round-vm run` on the round tree, as a unit; it ends on its `unit.ran`.
    pub fn corpus(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        corpus_for(super::NAME, e, spec)
    }

    /// The corpus leg of `task`'s round.
    pub fn corpus_for(task: &str, e: &Effects, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        if leg.why == super::ATTRIBUTE {
            return attribute_for(task, e, &env, &leg);
        }
        let repo = repo_of(&env, &leg)?;
        if !leg.batch.is_empty() {
            io::round_pass_start(&env, &repo, &leg.batch);
        }
        let wt = PathBuf::from(&leg.wt);
        let history = std::fs::read_to_string(tsd::family_path(&env.run, "suite-timing")).unwrap_or_default();
        let suites = batcher_cut::order::longest_first(&io::suites_in(&wt, "HEAD"), &history).list;
        let results = wt.join(format!(".round-results-{}", leg.unit));
        let argv = ["timeout", "-k", "10", &env.wall_secs.to_string()].iter().map(|s| s.to_string()).chain([env.round_vm.display().to_string(), "run".into(), wt.display().to_string(), "--suites".into(), suites.join(","), "--maxpar".into(), env.maxpar.to_string(), "--results-dir".into(), results.display().to_string(), "--base".into(), repo.base.clone()]).collect();
        start_unit(e, task, &leg.unit, argv, home_vars(&env), None)
    }

    /// A round staged behind the open one: `queue round stage`, as a unit.
    pub fn stage(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let file = env.run.join(format!("round-stage-{}.members", leg.unit));
        std::fs::write(&file, leg.members.iter().map(|(i, t)| format!("{i}:{t}\n")).collect::<String>()).map_err(|e| format!("{}: {e}", file.display()))?;
        let argv = vec![env.queue_bin.display().to_string(), "round".into(), "stage".into(), leg.repo.clone(), "--members-file".into(), file.display().to_string(), "--worktree".into(), leg.wt.clone()];
        start_unit(e, super::NAME, &leg.unit, argv, home_vars(&env), None)
    }

    /// The staged round's own pass: `queue round stage-test`, as a unit.
    pub fn stage_test(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let argv = vec![env.queue_bin.display().to_string(), "round".into(), "stage-test".into(), leg.repo.clone()];
        start_unit(e, super::NAME, &leg.unit, argv, home_vars(&env), None)
    }

    pub fn eject(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        if leg.bead.is_empty() {
            // The guilty of a red certify: the members whose files its output names.
            let repo = repo_of(&env, &leg)?;
            let text = std::fs::read_to_string(&leg.why).map_err(|e| format!("{}: {e}", leg.why))?;
            let changed: Vec<(String, Vec<String>)> = leg.members.iter().map(|(id, tip)| (id.clone(), io::changed_paths(&repo, &leg.base, tip))).collect();
            let guilty = io::implicated(&text, &changed);
            for id in &guilty {
                io::round_eject(&env, &repo, &leg.batch, id, &["ci".to_string()], &[], true)?;
                if let Some((_, tip)) = leg.members.iter().find(|(m, _)| m == id) {
                    batcher_cut::redtip::record(&env.run, &leg.repo, id, tip);
                }
            }
            return Ok(serde_json::json!({ "ejected": guilty }).to_string());
        }
        let suites = if leg.reds.is_empty() { vec!["ci".to_string()] } else { leg.reds.clone() };
        io::round_eject_sole_red(&env, &repo_of(&env, &leg)?, &leg.batch, &leg.bead, &suites, &leg.why)?;
        batcher_cut::redtip::record(&env.run, &leg.repo, &leg.bead, &leg.head);
        Ok("{}".into())
    }

    pub fn withdraw(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        io::lc_withdraw(&env, &leg.bead);
        Ok("{}".into())
    }

    /// `rebase-stale <bead> <repo>`, as a unit.
    pub fn rebase_stale(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let argv = vec![env.rebase_stale_bin.display().to_string(), leg.bead.clone(), leg.repo.clone()];
        start_unit(e, super::NAME, &leg.unit, argv, home_vars(&env), None)
    }

    /// `queue round certify <batch> <repo>`, as a unit: the repo's own testenv or the gate.
    pub fn certify(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let argv = vec![env.queue_bin.display().to_string(), "round".into(), "certify".into(), leg.batch.clone(), leg.repo.clone()];
        start_scope(e, super::NAME, &leg.unit, argv, home_vars(&env), None)
    }

    pub fn lc_certify(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        io::lc_certify_round(&env, &leg.members, &leg.round_key);
        Ok("{}".into())
    }

    /// `queue round land`, retried while another operation holds the lock; a refusal is an `Err`.
    pub fn land(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let repo = repo_of(&env, &leg)?;
        let run = io::round_land(&env, &repo, &leg.batch)?;
        if io::land_base_moved(run.code) {
            eprintln!("{} {}: queue round land refused base-moved for {} ({}) — certifying it again", super::NAME, repo.name, leg.batch, run.refusal);
            return json(&Landed { base_moved: Some(io::resolve_base_sha(&repo)?), ..Landed::default() });
        }
        match run.outcome {
            io::LandOutcome::Refused => Err(run.refusal),
            io::LandOutcome::Landed | io::LandOutcome::DeployFault => json(&Landed::default()),
        }
    }

    pub fn abandon(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        if leg.bead == super::NO_DRIVER {
            eprintln!("batcher.local {}: abandoning round {}: {}", leg.repo, leg.batch, leg.why);
            io::round_abandon(&env, &repo_of(&env, &leg)?, &leg.batch, &leg.why, super::NO_DRIVER);
            return Ok("{}".into());
        }
        let why = io::keep_red_evidence(&env.run, &leg.batch, &leg.why, &leg.reds);
        io::round_abandon_ejecting(&env, &repo_of(&env, &leg)?, &leg.batch, &why, "red", leg.ejected);
        io::write_local_verdict(&env, &leg.repo, "red", &why);
        Ok("{}".into())
    }

    pub fn promote(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let p = io::round_promote(&env, &repo_of(&env, &leg)?).ok_or("queue round promote named no round")?;
        json(&Promoted { batch: p.batch, members: p.members, attested: p.attested })
    }

    /// `round-recert`: it starts the round's driver in its own unit and returns.
    pub fn redrive(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        io::redrive_round(&repo_of(&env, &leg)?, &leg.batch)?;
        Ok("{}".into())
    }

    /// The incident for a round left open with no driver, keyed by the round.
    pub fn file(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        io::file_orphan_round_incident(&env, &repo_of(&env, &leg)?, &leg.batch, &leg.why)
    }

    /// Keeps a certify leg's full output where `leg.key` (relative to the run dir's parent) names it.
    pub fn keep_certify_output(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let name = std::path::Path::new(&leg.key).file_name().ok_or("no file name to keep the output under")?;
        let dir = env.run.join("rounds");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(name);
        std::fs::write(&path, &leg.why).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok("{}".into())
    }

    /// The pace's hold: the pause file on a red, the scoped stop on repeated infra faults.
    pub fn pause(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let files = batcher_cut::pace::Files::new(&env.run, &leg.repo);
        if leg.bead == "stop" {
            files.stop_rounds(&leg.why, spira_config::vtime::now_epoch())?;
        } else if leg.bead == "lift" {
            // A landing past the base the red round's pause was seen on: the pause is the loop's
            // own and ends here; an operator's `hold:` is left as it stands.
            let p = files.pause_file();
            match std::fs::read_to_string(&p) {
                Ok(text) if !text.starts_with("hold:") => {
                    std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                    eprintln!("{} {}: {}", super::NAME, leg.repo, leg.why);
                }
                _ => {}
            }
        } else {
            let p = files.pause_file();
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
            }
            std::fs::write(&p, &leg.why).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        Ok("{}".into())
    }
}
