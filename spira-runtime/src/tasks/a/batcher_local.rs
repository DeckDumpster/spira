//! # batcher.local — contract
//!
//! Unit `spira-rounds-prod` (`batcher rounds-local`, every 30 s, `TimeoutStartSec=7200`), pattern P2, from [[scheduler-consolidation-2026-11-03]] §1 A. Paths are relative to `spira-releases/current`. The `batcher` binary is the `batcher-cut` crate (`batcher-cut/Cargo.toml` `[[bin]] name = "batcher"`); the pure core is the `batcher` crate. The forge half is [[batcher-forge]].
//!
//! **Code vs catalogue.** The catalogue lists `batcher-cut` as a child; it is not — `rounds` calls `cut` in-process (`batcher-cut/src/main.rs:1320-1331`). The real long children are `round-vm run` under `systemd-run --scope` (`batcher-cut/src/vm.rs:381-394`) and the `queue round` verbs (`io.rs:1531`, `:1843`).
//!
//! ## Tick
//!
//! - Cadence 30 s, single-flight **per queue.local repo**. A repo cuts a further round only while fewer than `SPIRA_ROUND_MAX_OPEN` are in flight (default 1): it is assembled on the newest open round's head in a tree of its own, certified beside it, and lands only after the rounds below it (`State::Certified` holds a green round until then).
//! - Admission (today `main.rs:1299-1320`, `:1183-1206`): only `land == queue.local` repos; the home repo only while `SPIRA_BATCHER_ENABLE != 0`.
//! - Wakes, besides the tick (proposed names, none exist today): `bead.certified` (a pool grew), `world.unpaused` for the repo's `rounds` lane, and `unit.ran` for any leg it started, an image build included.
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
//! | Idle | observe: stopped, cut-held by a hand-landing, or paused with no open round to drive (`pace.rs:77`) | Held (no write) |
//! | Idle | observe: paused, open round not ours | as the open-round rows below (a pause stops only a new cut) |
//! | Idle | observe: open round, driven | Idle |
//! | Idle | observe: open round, undriven, last pass red | Attributing (`adopt_red`: attribute that pass's kept results against the base the round's record names) |
//! | Idle | observe: open round not ours, undriven, phase opened, past `UNDRIVEN_GRACE_S`, fit, every tip, its record's base and the landing ref known, every open round below it this task's, the repo not stopped or cut-held, no `abandon` in flight and no round of this task waiting on `assemble`, `round_open` or `promote`, not adopted before while open (a reached `SPIRA_ROUND_MAX_OPEN` or a pause does not bar it) | Certifying (adopted as one of this task's rounds, on the base its record names, at its place in the queue's order; stacked when a live round is below it, as any of this task's above it then are; nothing is cut on that tick) |
//! | Idle | observe: open round not ours, undriven, above the repo's first open round, fit, not adopted above | Idle (left alone until it is the first) |
//! | Idle | observe: open round, undriven | Orphan (`orphan_step`: Wait / Redrive once / then `abandon` naming why, pace Infra) |
//! | Idle | observe: `should_cut` = None | Idle (silent) |
//! | Idle | observe: trigger | Assembling (`fetch`, `assemble`) |
//! | Assembling | `assemble` ok, merged empty / held key | Idle |
//! | Assembling | `assemble` ok | Opened (`round_open`) |
//! | Opened | `round_open` ok, suites present | Certifying (one pass; green is the certificate) |
//! | Opened | no suites and repo declares own testenv | Certifying |
//! | Corpus, Attributing, Certifying | the leg's unit fails to start, or is unlisted for two observes with no `unit.ran` | the same state with the leg started once more; the second time Done (`abandon` naming the leg, pace Infra) |
//! | Corpus | `unit.ran` red | Attributing (`eject` + re-run leg) |
//! | Corpus | `unit.ran` green | Mending if menders open, else Certifying |
//! | Corpus, Attributing, Certifying | `unit.ran` exit SIGTERM, or the attribution line says abandoned | Done (not judged: no attribution, no abandon, pace Quiet, no pause) |
//! | Attributing | `unit.ran` (re-run) | Corpus-judged: Mending / Certifying / Done(blocked) |
//! | Corpus, Attributing, Certifying | observe: no leg carries the round, its record names red suites (`red=`), first time on this head | Attributing (the pass's kept results are attributed; pace untouched) |
//! | Corpus, Attributing, Certifying | observe: the same, a second time on the same head | Done (`red_naming` the red suites, pace Red) |
//! | Corpus, Attributing, Certifying | observe: no leg, no recorded red, no verdict | Done (`abandon` as no-driver, pace Infra) |
//! | Mending | `mending.settled` or timer 900 s (`main.rs:368`) | Certifying |
//! | Certifying | `unit.ran` green | Landing (`lc_certify`, `land`) |
//! | Certifying | `unit.ran` red with no `red-output=`, first time (VM lost, no output and refused eject excepted) | Attributing, `after_certify` (the certify pass's own results are the prior) |
//! | Certifying | `unit.ran` red after the attribution, one member, no `red-output=`, reds named, first time | Attributing, `after_certify`, as the first (rerun, base, member alone); the second attribution is spent |
//! | Certifying | `unit.ran` red after the attribution, two or more members, no `red-output=`, reds named, first time | Attributing, `after_certify` and `confirmed` (suspects named from the red suites' `# covers:` and the diffs) |
//! | Certifying | `unit.ran` red after both attributions, any member count, no `red-output=`, a red suite no attribution of the round covered | Attributing, `after_certify` (its rerun reaches the flip handling; once per red suite) |
//! | Certifying | `unit.ran` red past the rows above, one member, `red-output=` | the member is ejected on that output, and the emptied round is forgotten |
//! | Certifying | `unit.ran` red / failed otherwise | Done (eject those `red-output=` implicates, `abandon`, verdict red, pace Red) |
//! | Certifying | `unit.ran` failed naming no verdict, no suite and no `red-output=`, after the attribution, any member count | Done (output kept, round left open, pace Infra) |
//! | Corpus, Attributing, Certifying | observe: the round's open round is green on the carried head | Landing (`land`), or Certified when stacked; the carried leg is stopped in the same tick |
//! | Attributing | observe: the round's record no longer holds a member the leg carries (ejected by another actor; the batcher's own eject is the leg's work: the leg stays carried and adopts the record's moved head) | Certifying for the record's survivors at its head, certify held until no pass runs; the attribution leg is stopped (a batcher eject: stays Attributing) |
//! | Attributing | `unit.ran` land | Certifying, certify held until no pass of the round is running (a scope its pass has left is stopped, not waited on) |
//! | Certifying | `unit.ran` exit 75, the VM lost under the pass | Certifying (`certify` again: same head, a new VM), at most [`LOST_VM_RECERTIFIES`] times; then Done (pace Infra, the round left open, never abandoned) (sp-e30jhh) |
//! | Certifying | `unit.ran` exit 77, the VM produced no output | Certifying (`certify` again, a fresh VM), at most [`NO_OUTPUT_RECERTIFIES`] times; then Done (pace Infra, the round parked open, never abandoned) |
//! | Attributing | `unit.ran` runner-difference (the red tier is green on the host it last ran green on) | Certifying (`certify` again, its one next pass pinned to the host with `--place host`; no member charged, nothing ejected) |
//! | Landing | observe: no `land` in flight (a restart) | Landing (`lc_certify`, `land` again; each walks the store's own state) |
//! | Landing | observe: batch settled LANDED in the lifecycle | forgotten, after `supersede_reds` with the round's members (ABANDONED: forgotten, nothing asked) |
//! | Landing | land exit 0 / 3 | Promoting if staged else Done (verdict green) |
//! | Landing | land refused (lock busy ×N, or any other refusal) | Done (output kept, round left open, page with the refusing line, no pause) |
//! | Promoting | promotion named | Certifying for the staged round (`main.rs:1069`) |
//! | Done | — | Idle (`pace.advance` writes carry, pages) |
//!
//! Corpus is entered only by a carry the previous release recorded; its rows stay so that round finishes.
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
//! | `unit.stop` | unit | `UNIT_START_DEADLINE_MS` | `batcher.local:stop:<leg key>` | — (the leg's later `unit.ran` changes nothing) |
//! | `batcher.local.corpus` | unit (`unit.start`) | `SPIRA_BATCHER_WALL_SECS` | batch + run n | 2nd run while one live (`vm.rs:370`) |
//! | `batcher.local.stage` / `.stage_test` | unit | wall secs | staged batch | hypervisor full (`io.rs:1881`, `:1906`) |
//! | `batcher.local.eject` | store | 30 s | batch + bead | not in round (`io.rs:1600`, `:1628`) |
//! | `batcher.local.eject_red` | store | 30 s | batch + members | not in round |
//! | `batcher.local.withdraw` | store | 10 s | bead + version | state ≠ CERTIFIED (`io.rs:851-887`) |
//! | `batcher.local.rebase_stale` | unit | 600 s (unverified) | bead + base sha | not due (`io.rs:178`) |
//! | `batcher.local.image_build` | unit (class `image-build`) | `SPIRA_BATCHER_WALL_SECS` | `<repo>:image:<tag>` | a second build of the same tag |
//! | `batcher.local.image_clean` | git | 60 s | `<repo>:image:<tag>:clean` | the tag's build is running |
//! | `batcher.local.certify` | unit | wall secs | batch | head ≠ worktree (`io.rs:1662`) |
//! | `batcher.local.lc_certify` | store | 10 s | bead + `round:<key>` | state ≠ SUBMITTED (`io.rs:830-849`) |
//! | `batcher.local.land` | host | 900 s (unverified) | batch | not certified; lock busy retried ×`LAND_LOCK_ATTEMPTS` (`io.rs:2002`) |
//! | `batcher.local.abandon` / `.promote` | store | 30 s | batch | already closed (`io.rs:1823`, `:1945`) |
//! | `batcher.local.redrive` | unit | 60 s | batch | once per orphan (`io.rs:1772`) |
//! | `batcher.local.file` (incident/judgement/flip) | store | 30 s | incident ref, e.g. `round-orphan:<repo>:<batch>` | duplicate ref (`io.rs:1780`, `:2066`, `:2129`, `:2178`) |
//! | `batcher.local.supersede_reds` | store | 30 s | repo + landed head + members | none: the close and the registry clearing say their failure and never block the landing |
//! | `queue.step.publish_wanted` (emitted, not an effect) | publish stream | — | repo + `queue.local` | a landing, fresh or recovered, wakes publish at once |
//! | `batcher.local.clear_landed` | store | 30 s | the registry's LANDED beads | none: a failed clear is tried again on the next tick |
//! | `batcher.local.pause` | store | 5 s | repo + lane | — (`pace.rs:248`) |
//! | `batcher.local.pause_ask` | store | 15 s | `<repo>:pause:<since>` | open duplicate ask (`mail`) |
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
pub const NOTE_LEFT_OUT: &str = "batcher.local.note_left_out";
pub const IMAGE_BUILD: &str = "batcher.local.image_build";
pub const IMAGE_CLEAN: &str = "batcher.local.image_clean";
/// The `round open` refusal kind a built testenv image lifts.
const IMAGE_ABSENT: &str = "image-absent";
const IMAGE_KEY: &str = ".image-";
const SOLE_KEY: &str = ":sole";
/// Refusal kinds the observe's pool read (`batcher_cut::io::pool_rows`) already excludes while they hold; remembering them would only outlive the lifting.
const POOL_READ_REFUSALS: [&str; 3] = ["held", "state", "no-row"];
pub const EJECT: &str = "batcher.local.eject";
pub const EJECT_GATE_RED: &str = "batcher.local.eject_red";
pub const WITHDRAW: &str = "batcher.local.withdraw";
pub const REBASE_STALE: &str = "batcher.local.rebase_stale";
pub const CERTIFY: &str = "batcher.local.certify";
pub const LC_CERTIFY: &str = "batcher.local.lc_certify";
pub const LAND: &str = "batcher.local.land";
pub const ABANDON: &str = "batcher.local.abandon";
pub const PROMOTE: &str = "batcher.local.promote";
pub const REDRIVE: &str = "batcher.local.redrive";
pub const FILE: &str = "batcher.local.file";
pub const SUPERSEDE_REDS: &str = "batcher.local.supersede_reds";
pub const CLEAR_LANDED: &str = "batcher.local.clear_landed";
pub const PAUSE: &str = "batcher.local.pause";
pub const PAUSE_ASK: &str = "batcher.local.pause_ask";
pub const KEEP_CERTIFY_OUTPUT: &str = "batcher.local.keep_certify_output";
pub const INBOX_APPEND: &str = crate::watchers::INBOX_APPEND;

pub const CADENCE_MS: u64 = 30_000;
pub const OBSERVE_DEADLINE_MS: u64 = 10_000;
/// The unit legs (`corpus`, `stage`, `stage_test`, `certify`) are started under this deadline;
/// a corpus or attribution leg runs under `SPIRA_BATCHER_WALL_SECS` ([`Policy::wall_ms`]), the
/// certify leg under `queue round certify`'s own caps; each ends on its `unit.ran`.
pub const UNIT_START_DEADLINE_MS: u64 = 10_000;
/// Past the longest effect this task waits on (`land`, 900 s): an outcome that has not come is lost.
/// A just-opened round is given this long to be driven by whoever opened it before the tick starts its pass.
const UNDRIVEN_GRACE_S: u64 = 20;
pub const OUTCOME_WAIT_MS: u64 = 1_200_000;

pub const ACTS: &[(&str, u64)] = &[
    (FETCH, 60_000),
    (ASSEMBLE, 120_000),
    (ROUND_OPEN, 30_000),
    (CORPUS, UNIT_START_DEADLINE_MS),
    (STAGE, UNIT_START_DEADLINE_MS),
    (STAGE_TEST, UNIT_START_DEADLINE_MS),
    (NOTE_LEFT_OUT, 60_000),
    (IMAGE_BUILD, 60_000),
    (IMAGE_CLEAN, 60_000),
    (EJECT, 30_000),
    (EJECT_GATE_RED, 30_000),
    (WITHDRAW, 10_000),
    (REBASE_STALE, UNIT_START_DEADLINE_MS),
    (CERTIFY, UNIT_START_DEADLINE_MS),
    (runtime::units::UNIT_STOP, UNIT_START_DEADLINE_MS),
    (LC_CERTIFY, 10_000),
    (LAND, 900_000),
    (ABANDON, 30_000),
    (PROMOTE, 30_000),
    (REDRIVE, 60_000),
    (FILE, 30_000),
    (SUPERSEDE_REDS, 30_000),
    (CLEAR_LANDED, 30_000),
    (PAUSE, 5_000),
    (PAUSE_ASK, 15_000),
    (KEEP_CERTIFY_OUTPUT, 5_000),
    (INBOX_APPEND, 10_000),
];
pub const EMITS: &[EventKindTag] = &[super::queue_step::PUBLISH_WANTED];
/// Hand-offs that wake it at once, besides its tick: a submission sifted, a batch settled.
pub const WAKES: &[EventKindTag] = &[kit::WORK_SUBMITTED, super::queue_step::CUT_WANTED];
pub const EFFECTS: &[&str] = &[runtime::units::UNIT_FACTS, runtime::units::UNIT_STOP, OBSERVE, FETCH, ASSEMBLE, ROUND_OPEN, CORPUS, STAGE, STAGE_TEST, NOTE_LEFT_OUT, EJECT, EJECT_GATE_RED, WITHDRAW, REBASE_STALE, CERTIFY, LC_CERTIFY, LAND, ABANDON, PROMOTE, REDRIVE, FILE, SUPERSEDE_REDS, CLEAR_LANDED, PAUSE, PAUSE_ASK, KEEP_CERTIFY_OUTPUT, IMAGE_BUILD, IMAGE_CLEAN, INBOX_APPEND];
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
    "SPIRA_BATCHER_STALL_SECS",
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
    /// The fix for an open base-red bead (`basefail:<repo>:`): it lands alone (`base_fix_lane`).
    #[serde(default)]
    pub base_fix: bool,
    #[serde(default)]
    pub express: bool,
    #[serde(default)]
    pub certified_at: u64,
}

impl Member {
    /// A member of the pool as the store read gave it, its version unread: a provisional one
    /// stays SUBMITTED, so the cut ranks it after the certified ones.
    pub fn read(m: core::Member, sifted_tip: Option<String>) -> Member {
        Member {
            state: if m.provisional { "SUBMITTED" } else { "CERTIFIED" }.into(),
            sifted_tip,
            title: m.title,
            priority: m.priority.unwrap_or(u8::MAX),
            deps: m.blocked_by,
            stack: m.stack.iter().map(|(i, t)| format!("{i}:{t}")).collect(),
            base_fix: m.base_fix,
            express: m.express,
            certified_at: m.certified_at,
            id: m.id,
            tip: m.tip,
            ..Member::default()
        }
    }

    /// The pure core's view of this member; `blockers` are the repo's unlanded beads.
    pub fn core(&self, blockers: &[String]) -> core::Member {
        let stack = self.stack.iter().map(|s| s.split_once(':').map_or((s.clone(), String::new()), |(i, t)| (i.to_string(), t.to_string()))).collect();
        let blocked_by = self.deps.iter().filter(|d| blockers.contains(d)).cloned().collect();
        core::Member { id: self.id.clone(), tip: self.tip.clone(), title: self.title.clone(), priority: Some(self.priority), express: self.express, base_fix: self.base_fix, certified_at: self.certified_at, stack, blocked_by, provisional: self.state == "SUBMITTED" }
    }
}

/// What a carried member list lost to the record: at least one member, every one by the batcher's own eject.
fn lost_only_to_batcher(carried: &[(String, String)], open: &Round) -> bool {
    let mut lost = carried.iter().map(|(m, _)| m).filter(|m| !open.members.contains(m)).peekable();
    lost.peek().is_some()
        && lost.all(|m| open.ejected.iter().zip(open.ejected_by.iter()).rfind(|(e, _)| *e == m).is_some_and(|(_, by)| by == batcher_cut::io::BATCHER_ACTOR))
}

impl From<String> for Round {
    /// A round known by its head alone: it records no ejection.
    fn from(head: String) -> Round {
        Round { head, ..Round::default() }
    }
}

/// The round projection's open round.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    pub id: String,
    pub members: Vec<String>,
    /// Each member's tip, in `members` order.
    #[serde(default)]
    pub tips: Vec<String>,
    pub head: String,
    /// The commit the queue built the round on (its record's `base`): the landing ref, or the head
    /// of the round below a stacked one.
    #[serde(default)]
    pub base: String,
    pub worktree: String,
    /// A `round certify` or this task's own machine holds the round.
    pub driven: bool,
    /// The queue marked the round red: a round is never stacked on it.
    #[serde(default)]
    pub red: bool,
    /// The suites the queue recorded red for the round's pass.
    #[serde(default)]
    pub red_suites: Vec<String>,
    /// The beads `queue round eject` took out of the round.
    #[serde(default)]
    pub ejected: Vec<String>,
    /// Who ejected each of `ejected`, in its order.
    #[serde(default)]
    pub ejected_by: Vec<String>,
    /// Seconds since the round opened; the least is the newest, the one the next round stacks on.
    #[serde(default)]
    pub age_secs: u64,
    /// Why `round certify` would refuse this round at once (`round_certifiable`), if it would.
    #[serde(default)]
    pub unfit: Option<String>,
    /// The round's phase as `queue round status` names it; `green` once a pass on its head passed.
    #[serde(default)]
    pub phase: String,
    /// Members whose own gate went red while the round holds them, as `(id, tip, the gate's mark)`.
    #[serde(default)]
    pub gate_red: Vec<(String, String, String)>,
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
    /// A hand-landing's cut hold, `owner (reason) until <until>`: no new round is cut while it stands.
    #[serde(default)]
    pub cut_hold: Option<String>,
    /// The pace's pause file (`run/rounds-pace/<repo>/pause`), its text, when it stands: a red
    /// round wrote it. A text that starts with `hold:` is an operator's deliberate hold.
    #[serde(default)]
    pub paused: Option<String>,
    /// When a red round's pause was set, in epoch seconds; `None` for no pause or a `hold:`.
    #[serde(default)]
    pub paused_since: Option<u64>,
    /// The failing test line of the round's kept results, for a red pause's page.
    #[serde(default)]
    pub pause_evidence: Option<String>,
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
    /// The subset of `settled` whose lifecycle state is LANDED.
    #[serde(default)]
    pub landed: Vec<String>,
    /// Beads the pool read kept out of the pool, with why (held, ejected red, tip moved, ...).
    #[serde(default)]
    pub left_out: Vec<batcher_cut::io::LeftOut>,
    /// The carried image wants whose tag the host's container store holds.
    #[serde(default)]
    pub images_present: Vec<String>,
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
    /// The beads test-registry rows name that the lifecycle reports LANDED.
    #[serde(default)]
    pub registry_landed: Vec<String>,
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
    /// A round the queue holds with no driver is green and the lowest open: it lands.
    OpenRoundGreen,
    /// An open round no pass drives whose last pass ended red: attributed as one of this task's own.
    OpenRoundRed,
    /// A round another actor opened and nothing drives: certified as one of this task's own.
    OpenRoundOpened,
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
    /// The attribution reached its probe ceiling: the round is abandoned whole, its red kept for the next round.
    RerunUnconverged,
    MendingSettled,
    MendTimeout,
    CertifyGreen,
    CertifyRed,
    /// A red certify attributed from that pass's results, every member alike.
    CertifyRedFirst,
    /// A red that survived its attribution is attributed once more, by suspect.
    CertifyRedConfirmed,
    LandedStaged,
    Landed,
    LandRefused,
    /// `round land` refused base-moved (its exit, sp-tjrjca): the round is certified again on
    /// the base it was rebuilt on, at most once per base (not in the contract's table).
    LandBaseMoved,
    PromotedToCertifying,
    /// `round promote` named no round (not in the contract's table): the cut goes on from the pool.
    PromoteFailed,
    Advanced,
    /// A leg event whose `round_key` is not the carried round's.
    ForeignLeg,
    /// The certify leg exited [`queue::ops::round::VM_LOST`]: the pass is incomplete, the round
    /// is not judged, and it is certified again on its head (sp-e30jhh; not in the contract's table).
    CertifyLost,
    /// The attribution found the red tier green on the host: no member is charged and the round is certified again there.
    RerunRunnerDifference,
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
    /// The queue recorded the round's pass red and no leg carries it: the attribution runs on the pass's kept results.
    PassRedRecorded,
    /// An eject left the round with no member: the round ended in the store and nothing is left to attribute (not in the contract's table).
    RoundEmptied,
    /// The leg was stopped by its round's abandon (or its attribution said so): the round is not judged.
    LegStopped,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::Stopped, State::Held),
    (State::Held, Trigger::Stopped, State::Held),
    (State::Held, Trigger::Released, State::Idle),
    (State::Idle, Trigger::OpenRoundDriven, State::Idle),
    (State::Idle, Trigger::OpenRoundUndriven, State::Orphan),
    (State::Idle, Trigger::OpenRoundGreen, State::Landing),
    (State::Idle, Trigger::OpenRoundRed, State::Attributing),
    (State::Idle, Trigger::OpenRoundOpened, State::Certifying),
    (State::Orphan, Trigger::OrphanStepped, State::Idle),
    (State::Idle, Trigger::NoTrigger, State::Idle),
    (State::Idle, Trigger::Cut, State::Assembling),
    (State::Assembling, Trigger::AssembledEmpty, State::Idle),
    (State::Assembling, Trigger::Assembled, State::Opened),
    (State::Opened, Trigger::OpenedWithSuites, State::Certifying),
    (State::Opened, Trigger::OpenedOwnTestenv, State::Certifying),
    (State::Opened, Trigger::OpenFailed, State::Done),
    (State::Opened, Trigger::OpenRefusedSome, State::Opened),
    (State::Opened, Trigger::OpenRefusedAll, State::Done),
    (State::Corpus, Trigger::LegRed, State::Attributing),
    (State::Corpus, Trigger::LegGreenMenders, State::Mending),
    (State::Corpus, Trigger::LegGreen, State::Certifying),
    (State::Attributing, Trigger::RerunMending, State::Mending),
    (State::Attributing, Trigger::RerunCertifying, State::Certifying),
    (State::Attributing, Trigger::RerunRunnerDifference, State::Certifying),
    (State::Corpus, Trigger::RerunCertifying, State::Certifying),
    (State::Certifying, Trigger::RerunCertifying, State::Certifying),
    (State::Attributing, Trigger::RerunBlocked, State::Done),
    (State::Attributing, Trigger::RerunUnconverged, State::Done),
    (State::Mending, Trigger::MendingSettled, State::Certifying),
    (State::Mending, Trigger::MendTimeout, State::Certifying),
    (State::Certifying, Trigger::CertifyGreen, State::Landing),
    (State::Certifying, Trigger::CertifyRed, State::Done),
    (State::Certifying, Trigger::CertifyRedFirst, State::Attributing),
    (State::Certifying, Trigger::CertifyRedConfirmed, State::Attributing),
    // CertifyRedFirst again: a red naming a suite no attribution of the round covered.
    (State::Landing, Trigger::LandedStaged, State::Promoting),
    (State::Landing, Trigger::Landed, State::Done),
    (State::Landing, Trigger::LandRefused, State::Done),
    (State::Landing, Trigger::LandBaseMoved, State::Certifying),
    (State::Promoting, Trigger::PromotedToCertifying, State::Certifying),
    (State::Promoting, Trigger::PromoteFailed, State::Done),
    (State::Done, Trigger::Advanced, State::Idle),

    (State::Certifying, Trigger::CertifyLost, State::Certifying),
    (State::Certifying, Trigger::CertifyLostSpent, State::Done),
    (State::Corpus, Trigger::LegRestarted, State::Corpus),
    (State::Attributing, Trigger::LegRestarted, State::Attributing),
    (State::Certifying, Trigger::LegRestarted, State::Certifying),
    (State::Corpus, Trigger::LegLost, State::Done),
    (State::Corpus, Trigger::PassRedRecorded, State::Attributing),
    (State::Attributing, Trigger::PassRedRecorded, State::Attributing),
    (State::Certifying, Trigger::PassRedRecorded, State::Attributing),
    (State::Corpus, Trigger::RoundEmptied, State::Done),
    (State::Attributing, Trigger::RoundEmptied, State::Done),
    (State::Certifying, Trigger::RoundEmptied, State::Done),
    (State::Corpus, Trigger::LegStopped, State::Done),
    (State::Attributing, Trigger::LegStopped, State::Done),
    (State::Certifying, Trigger::LegStopped, State::Done),
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
    /// The batch that last advanced `infra`: one round counts once.
    #[serde(default)]
    pub infra_batch: String,
    #[serde(default)]
    pub open_failed: u32,
    /// The `paused_since` of the pause last paged as unattended.
    #[serde(default)]
    pub pause_paged: u64,
}

/// The reason a failed `round_open` carries into the pace page; a timeout arrives as an empty body.
fn open_failed_detail(ok: bool, body: &str) -> String {
    if ok {
        return "queue round open named no batch".into();
    }
    match body.trim() {
        "" => {
            let secs = ACTS.iter().find(|(e, _)| *e == ROUND_OPEN).map_or(0, |(_, ms)| ms / 1000);
            format!("queue round open did not answer within {secs}s (the batcher.local.round_open deadline); a round it opened late is adopted by the next pass")
        }
        b => b.to_string(),
    }
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
            open_failed: self.open_failed,
        }
    }

    pub fn of(st: &pace::State, prev: &Pace) -> Pace {
        let one = |s: &str| if s.is_empty() { vec![] } else { vec![s.to_string()] };
        Pace {
            infra: st.infra,
            no_vm: st.no_vm,
            paged: one(&st.paged_open),
            orphan: one(&st.orphan),
            refusals: prev.refusals.clone(),
            orphan_since_ms: st.orphan_since * 1000,
            redriven: st.redriven.clone(),
            pause_lifts: st.pause_lifts,
            infra_batch: prev.infra_batch.clone(),
            open_failed: st.open_failed,
            pause_paged: prev.pause_paged,
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
    /// The (id, cause tag, tip) of each bead the last note effect named as left out of the cut.
    #[serde(default)]
    pub left_noted: Vec<(String, String, String)>,
    /// Pool members refused for an unlanded prerequisite, with the first one, as of the last pass.
    #[serde(default)]
    pub blocked: BTreeMap<String, String>,
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
    /// The round's next certify is pinned to the host: a runner difference was found, and that pass is the proof.
    #[serde(default)]
    pub host_pin: bool,
    /// This round's unnamed certify red has already been attributed.
    #[serde(default)]
    pub attributed: bool,
    /// The second attribution is spent: by suspect when there are members to compare.
    #[serde(default)]
    pub confirmed: bool,
    /// Red suites an attribution of this round already covered; a red naming one outside it is attributed afresh.
    #[serde(default)]
    pub attributed_reds: Vec<String>,
    /// The phase and first failing line of the certify pass whose red this round was attributed
    /// for; leads every later reason the round ends with.
    #[serde(default)]
    pub pass_red: String,
    /// The attribution's survivors await certify, which starts once no pass scope of the round is alive.
    #[serde(default)]
    pub certify_held: bool,
    /// An eject the queue held because a pass of the round was running; issued again once none is.
    #[serde(default)]
    pub eject_held: Option<EjectHold>,
    /// The gate-red eject asked for and not yet answered; nothing else drives the round meanwhile.
    #[serde(default)]
    pub eject_gate_red: Option<Leg>,
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
    /// The head whose move already discarded the leg: a move is handled once.
    #[serde(default)]
    pub discarded_head: String,
    /// The head the discarded pass was for and the beads ejected when the head moved: a later abandon names them.
    #[serde(default)]
    pub moved_from: String,
    #[serde(default)]
    pub moved_ejected: Vec<String>,
    /// The head the attribution rebuilt the round tree at, which the certify that follows is for.
    #[serde(default)]
    pub certify_head: String,
    #[serde(default)]
    pub run: u32,
    /// The head whose recorded red was already re-attributed: it is attributed once per head.
    #[serde(default)]
    pub red_attributed_head: String,
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
    /// The members this repo's latest pass would cut; empty when it cut nothing. Round-duty reads it.
    #[serde(default)]
    pub admissible: Vec<String>,
    /// The assemble error of the last cut and the ticks it has now failed the same way running.
    #[serde(default)]
    pub assemble_failed: Option<(String, u32)>,
    /// Members `queue round open` refused, as id → (tip, base, kind): each stays out of the cut
    /// until its tip or the base moves. Kinds in `POOL_READ_REFUSALS` are not kept.
    #[serde(default)]
    pub refused: BTreeMap<String, (String, String, String)>,
    /// The testenv images refused members wait on, by tag: each is built once.
    #[serde(default)]
    pub images: BTreeMap<String, ImageWant>,
    /// The base the round was re-certified on after a base-moved land refusal: at most once per
    /// base change.
    #[serde(default)]
    pub recertified_on: Option<String>,
    /// The open rounds of other actors this task has adopted: each is adopted once while it stays open.
    #[serde(default)]
    pub adopted: Vec<String>,
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
    /// The gate reds recorded against the round's members, `(id, tip, the gate's mark)`: evidence a pass ends on, never a reason to stop one.
    #[serde(default)]
    pub gate_red: Vec<(String, String, String)>,
    /// The batch the last reconcile dropped because the lifecycle had already settled it.
    #[serde(default)]
    pub reconciled: Option<String>,
}

/// One testenv image refused members need.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageWant {
    /// The member tip the image is built from.
    pub tip: String,
    pub members: Vec<String>,
    /// The key of the build's unit while no result of it has come.
    pub unit: Option<String>,
    pub failed: bool,
    #[serde(default)]
    pub at_ms: u64,
    /// Each member's refused tip: a moved base drops its refusal, and the want must outlive that.
    #[serde(default)]
    pub tips: BTreeMap<String, String>,
}

/// The unit key of the build of `tag`; [`image_of_key`] reads it back.
pub fn image_unit_key(repo: &str, tag: &str) -> String {
    format!("{repo}{IMAGE_KEY}{tag}")
}

fn image_of_key(key: &str) -> Option<(&str, &str)> {
    key.split_once(IMAGE_KEY)
}

/// The wanted tags `held` finds in the store; with nothing wanted it is never asked.
pub fn images_held(rc: &RepoCarry, held: &dyn Fn(&str) -> bool) -> Vec<String> {
    rc.images.keys().filter(|tag| held(tag)).cloned().collect()
}

fn image_leg(repo: &str, tag: &str, want: &ImageWant) -> Leg {
    Leg { key: format!("{repo}:image:{tag}"), unit: image_unit_key(repo, tag), image: tag.to_string(), head: want.tip.clone(), ..Leg::on(repo) }
}

fn image_clean(repo: &str, tag: &str) -> Act {
    Leg { key: format!("{repo}:image:{tag}:clean"), unit: image_unit_key(repo, tag), image: tag.to_string(), ..Leg::on(repo) }.act(IMAGE_CLEAN)
}

const IMAGE_SAID_LINES: usize = 5;

fn exit_words(exit: Exit) -> String {
    match exit {
        Exit::Code(c) => format!("exit {c}"),
        Exit::Signal(s) => format!("signal {s}"),
        Exit::Unknown => "no exit status".into(),
    }
}

/// A held eject, and whether its act is out: the round is waiting on that act's outcome.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EjectHold {
    pub eject: batcher_cut::io::HeldEject,
    #[serde(default)]
    pub issued: bool,
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
            host_pin: self.host_pin,
            attributed: self.attributed,
            confirmed: self.confirmed,
            attributed_reds: self.attributed_reds.clone(),
            pass_red: self.pass_red.clone(),
            certify_held: self.certify_held,
            eject_held: self.eject_held.clone(),
            leg: self.leg.clone(),
            leg_at_ms: self.leg_at_ms,
            head: self.head.clone(),
            leg_head: self.leg_head.clone(),
            discarded_head: self.discarded_head.clone(),
            moved_from: self.moved_from.clone(),
            moved_ejected: self.moved_ejected.clone(),
            certify_head: self.certify_head.clone(),
            run: self.run,
            leg_restarts: self.leg_restarts,
            leg_missing: self.leg_missing,
            recertified_on: self.recertified_on.clone(),
            stacked: self.stacked,
            landing_base: self.landing_base.clone(),
            waiting: self.waiting,
            gate_red: self.gate_red.clone(),
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
            host_pin: round.host_pin,
            attributed: round.attributed,
            confirmed: round.confirmed,
            attributed_reds: round.attributed_reds.clone(),
            pass_red: round.pass_red.clone(),
            certify_held: round.certify_held,
            eject_held: round.eject_held.clone(),
            leg: round.leg.clone(),
            leg_at_ms: round.leg_at_ms,
            head: round.head.clone(),
            leg_head: round.leg_head.clone(),
            discarded_head: round.discarded_head.clone(),
            moved_from: round.moved_from.clone(),
            moved_ejected: round.moved_ejected.clone(),
            certify_head: round.certify_head.clone(),
            run: round.run,
            leg_restarts: round.leg_restarts,
            leg_missing: round.leg_missing,
            recertified_on: round.recertified_on.clone(),
            stacked: round.stacked,
            landing_base: round.landing_base.clone(),
            waiting: round.waiting,
            gate_red: round.gate_red.clone(),
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

impl State {
    /// A round is open and the machine is moving it.
    pub fn drives_a_round(self) -> bool {
        !matches!(self, State::Idle | State::Held | State::Orphan | State::Done)
    }
}

/// Whether the carry `body` has any repo driving a round.
pub fn driving(body: &str) -> bool {
    serde_json::from_str::<Carry>(body).is_ok_and(|c| c.repos.values().any(|r| r.state.drives_a_round()))
}

/// Everything one act of this task (and of [`super::batcher_forge`]) carries as its input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leg {
    pub repo: String,
    /// The eject an EJECT act issues again verbatim, when it is a held one.
    #[serde(default)]
    pub held: Option<batcher_cut::io::HeldEject>,
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
    /// The opened batch PR a forge `shots` comments on.
    #[serde(default)]
    pub pr: String,
    /// The testenv image tag an image build or its clean is for.
    #[serde(default)]
    pub image: String,
    /// The head of the open round an assemble builds on, when it stacks; empty for the landing ref.
    #[serde(default)]
    pub onto: String,
    /// How many members were ejected before this abandon, when the leg knows.
    #[serde(default)]
    pub ejected: Option<usize>,
    /// The suites a red certify named, for the abandon to keep their evidence.
    #[serde(default)]
    pub reds: Vec<String>,
    /// The results of the corpus pass an attribution leg follows: it attributes that pass's red
    /// tests from them instead of running the corpus again.
    #[serde(default)]
    pub after_pass: String,
    /// The attribution follows a red certify: it reads the pass `queue round certify` kept for the batch.
    #[serde(default)]
    pub after_certify: bool,
    /// The certify runs on the host only (`--place host`).
    #[serde(default)]
    pub place_host: bool,
    /// The attribution names suspects from the red suites' `# covers:` instead of mending.
    #[serde(default)]
    pub confirmed: bool,
}

/// Where the corpus leg `unit` of the round tree `wt` keeps its results.
pub fn round_results(wt: &str, unit: &str) -> std::path::PathBuf {
    std::path::Path::new(wt).join(format!(".round-results-{unit}"))
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
    /// The testenv tag each image-absent refusal names, by member id.
    #[serde(default)]
    pub images: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landed {
    /// The round `land` left staged behind it, to promote.
    #[serde(default)]
    pub staged: Option<String>,
    /// `round land` refused base-moved (its exit): the `base` the queue wrote on the rebuilt
    /// round's record. The round is certified again, not landed.
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
    pub pause_lift_s: u64,
}

impl Default for Policy {
    /// The registered defaults (`spira/conf.d/SPIRA_BATCHER_ORPHAN_REDRIVE_SECS`, `_WALL_SECS`).
    fn default() -> Self {
        Policy { orphan_wait_s: 600, wall_ms: 3_600_000, max_members: batcher_cut::MAX_CUT_MEMBERS, max_open: 1, pause_lift_s: 900 }
    }
}

impl Policy {
    pub fn pinned(pinned: &BTreeMap<String, String>) -> Policy {
        let d = Policy::default();
        let num = |k: &str| pinned.get(k).and_then(|v| v.trim().parse::<u64>().ok());
        Policy { orphan_wait_s: num("SPIRA_BATCHER_ORPHAN_REDRIVE_SECS").unwrap_or(d.orphan_wait_s), wall_ms: num("SPIRA_BATCHER_WALL_SECS").map_or(d.wall_ms, |s| s * 1000), max_open: num("SPIRA_ROUND_MAX_OPEN").map_or(d.max_open, |n| (n as usize).max(1)), pause_lift_s: num("SPIRA_BATCHER_PAUSE_LIFT_SECS").unwrap_or(d.pause_lift_s), ..d }
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

/// Whether any of `listed`, the units found under one round-recert driver's name, is running.
pub fn recert_live(listed: &[runtime::units::UnitDetail]) -> bool {
    listed.iter().any(|d| matches!(d.state, runtime::units::UnitState::Active | runtime::units::UnitState::Activating))
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
/// Cause word on the `.result` line of a round whose attribution reached its probe ceiling without naming a member.
pub const ATTRIBUTION_NOT_CONVERGING: &str = "attribution-not-converging";
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

/// `queue round certify`'s argv for `leg`: it names the head the leg is for when the attribution rebuilt the tree.
pub fn certify_argv(queue_bin: &str, leg: &Leg) -> Vec<String> {
    let mut argv = vec![
        queue_bin.to_string(),
        "round".into(),
        "certify".into(),
        leg.batch.clone(),
        leg.repo.clone(),
        "--by".into(),
        "batcher".into(),
    ];
    if !leg.head.is_empty() {
        argv.extend(["--rehead".to_string(), leg.head.clone()]);
    }
    if leg.place_host {
        argv.extend(["--place".to_string(), "host".to_string()]);
    }
    argv
}

/// Where a certify leg's output is kept, relative to the run's parent.
pub fn certify_output_file(batch: &str, run: u32) -> String {
    format!("run/rounds/{batch}.certify-{run}.out")
}

/// Where a round's kept pass results are, relative to the run's parent.
pub fn kept_results_file(batch: &str) -> String {
    format!("run/rounds/{batch}.results")
}

/// Where a refused land leg's output is kept, relative to the run's parent.
pub fn land_output_file(batch: &str, run: u32) -> String {
    format!("run/rounds/{batch}.land-{run}.out")
}

/// What the page for a refused landing says: the refusing line, the kept file, the next verbs.
pub fn land_refused_detail(batch: &str, out: &str, kept: &str) -> String {
    let said = out.trim().lines().last().unwrap_or("no output");
    format!("round {batch} is green and left open with its members; the landing was refused: {said}; output kept at {kept}; run `queue round land {batch}` again, unchanged; only if the refusing line names a member's change, eject it with `queue round eject {batch} <member>`, then `queue round certify {batch}` and `queue round land {batch}`")
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

/// What a red certify pass said went red: its last `red-reason=` line, else its red suites, else
/// its verdict line; empty when the output says none.
pub fn pass_red_of(out: &str) -> String {
    if let Some(r) = out.lines().rev().find_map(|l| l.strip_prefix("red-reason=")) {
        return r.trim().to_string();
    }
    match certify_red_evidence(out) {
        (_, reds) if !reds.is_empty() => reds.join(","),
        (Some(v), _) => v,
        (None, _) => String::new(),
    }
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
    select_split(repo, max_members).0
}

pub type Selection = Result<(Vec<core::Member>, String), Vec<String>>;

/// [`select_why`], with every pool member the selection kept out of the cut and why.
pub fn select_split(repo: &Repo, max_members: usize) -> (Selection, Vec<batcher_cut::io::LeftOut>) {
    use batcher_cut::io::{Cause, LeftOut};
    let mut why: Vec<String> = Vec::new();
    let mut left: Vec<LeftOut> = Vec::new();
    let out = |m: &core::Member, cause| LeftOut { id: m.id.clone(), tip: m.tip.clone(), cause };
    let pool: Vec<core::Member> = repo.pool.iter().map(|m| m.core(&repo.blockers)).collect();
    let sifted: BTreeMap<String, String> = repo.pool.iter().filter_map(|m| m.sifted_tip.clone().map(|t| (m.id.clone(), t))).collect();
    let screened = batcher_cut::screen::cut_pool(pool.clone(), &sifted);
    for m in pool.iter().filter(|m| !screened.iter().any(|s| s.id == m.id)) {
        why.push(format!("{}: no sift pass at its tip {}", m.id, m.tip));
        left.push(out(m, Cause::NoSift));
    }
    let blocked_out = |before: &[core::Member], blocked: &[core::BlockedRefusal], left: &mut Vec<LeftOut>| {
        left.extend(blocked.iter().filter_map(|r| before.iter().find(|m| m.id == r.id).map(|m| out(m, Cause::Blocked { dep: r.blocker.clone() }))));
    };
    let (kept, blocked) = core::refuse_blocked(screened.clone());
    why.extend(blocked.iter().map(|r| r.to_string()));
    blocked_out(&screened, &blocked, &mut left);
    let (selected, kind) = core::select_round(core::base_fix_lane(kept.clone()));
    if let core::RoundKind::Feature(root) = &kind {
        left.extend(kept.iter().filter(|m| !selected.iter().any(|s| s.id == m.id)).map(|m| out(m, Cause::OutsideFeature { root: root.clone() })));
    }
    let (capped, _) = core::cap_round(core::topo_order(&selected), max_members);
    left.extend(selected.iter().filter(|m| !capped.iter().any(|c| c.id == m.id)).map(|m| out(m, Cause::RoundFull { cap: max_members })));
    let (pool, blocked) = core::refuse_blocked(capped.clone());
    why.extend(blocked.iter().map(|r| r.to_string()));
    blocked_out(&capped, &blocked, &mut left);
    if core::should_cut(&core::TriggerInputs { pool: &pool, main_red: false, batch_open: false }).is_none() {
        return (Err(why), left);
    }
    let key = core::round_key(&pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect::<Vec<_>>());
    (Ok((pool, key)), left)
}

/// The leg of the note a cut owes the beads it left out, one per bead, only when the set differs
/// from `noted`, the last one named; an emptied set is sent once, so its records are dropped.
pub fn left_out_note(repo: &str, noted: &mut Vec<(String, String, String)>, mut left: Vec<batcher_cut::io::LeftOut>) -> Option<Leg> {
    let mut seen = std::collections::BTreeSet::new();
    left.retain(|l| seen.insert(l.id.clone()));
    let named: Vec<(String, String, String)> = left.iter().map(|l| (l.id.clone(), l.tag(), l.tip.clone())).collect();
    if named == *noted {
        return None;
    }
    let cleared = left.is_empty() && !noted.is_empty();
    *noted = named;
    if left.is_empty() && !cleared {
        return None;
    }
    let why = serde_json::to_string(&left).unwrap_or_default();
    Some(Leg { key: format!("{repo}:left-out:{}", digest(&why)), why, ..Leg::on(repo) })
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
        pace::Hold::CutHeld => format!("cut held by {} — no cut until it lifts or its holder ends", r.cut_hold.as_deref().unwrap_or("an unnamed holder")),
        pace::Hold::Paused => format!(
            "paused by a red round ({}) — the next landing on {repo} lifts it, or batcher pause-lift --repo {repo} --by concierge --reason \"<why>\"",
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
        self.rc.leg_restarts = 0;
        self.rc.leg_missing = 0;
        if effect != CERTIFY {
            self.rc.certify_head.clear();
        }
        let head = if self.rc.certify_head.is_empty() { self.rc.head.clone() } else { self.rc.certify_head.clone() };
        self.rc.leg_head = head.clone();
        let place_host = effect == CERTIFY && std::mem::take(&mut self.rc.host_pin);
        let leg = Leg { key: unit.clone(), unit, place_host, head: if effect == CERTIFY { self.rc.certify_head.clone() } else { String::new() }, ..self.leg() };
        self.acts.push(leg.act(effect));
    }

    fn corpus(&mut self, run: u32) {
        self.rc.run = run;
        self.start(CORPUS, "corpus");
    }

    /// The attribution leg: `batcher attribute` (`drive::attribute_round`) on the red round, a
    /// corpus leg marked [`ATTRIBUTE`] so it runs under the same effect, captured.
    fn attribute(&mut self) {
        self.attribute_after(String::new());
    }

    /// [`Self::attribute`] of the corpus pass whose results are at `after_pass`, when named.
    fn attribute_after(&mut self, after_pass: String) {
        self.rc.run += 1;
        self.start(CORPUS, "attribute");
        if let Some(a) = self.acts.last_mut() {
            let mut leg: Leg = serde_json::from_str(&a.input).unwrap_or_default();
            leg.why = ATTRIBUTE.into();
            leg.after_pass = after_pass;
            a.input = serde_json::to_string(&leg).unwrap_or_default();
        }
    }

    /// The attribution of the certify pass just ended red: it reads that pass's kept results.
    fn attribute_after_certify(&mut self) {
        self.attribute_after(String::new());
        if let Some(a) = self.acts.last_mut() {
            let mut leg: Leg = serde_json::from_str(&a.input).unwrap_or_default();
            leg.after_certify = true;
            a.input = serde_json::to_string(&leg).unwrap_or_default();
        }
    }

    fn record_attributed_reds(&mut self, out: &str) {
        for red in certify_red_evidence(out).1 {
            if !self.rc.attributed_reds.contains(&red) {
                self.rc.attributed_reds.push(red);
            }
        }
    }

    /// A red names a suite no attribution of this round covered. A row with no record (written before it was kept) has none.
    fn unattributed_reds(&self, out: &str) -> bool {
        !self.rc.attributed_reds.is_empty() && certify_red_evidence(out).1.iter().any(|r| !self.rc.attributed_reds.contains(r))
    }

    fn attribute_confirmed(&mut self) {
        self.attribute_after_certify();
        if let Some(a) = self.acts.last_mut() {
            let mut leg: Leg = serde_json::from_str(&a.input).unwrap_or_default();
            leg.confirmed = true;
            a.input = serde_json::to_string(&leg).unwrap_or_default();
        }
    }

    fn certify(&mut self) {
        if self.eject_recorded_gate_reds() {
            return;
        }
        self.start(CERTIFY, "certify");
    }

    /// Certify refuses a round whose last pass is still finishing: it starts once none is running.
    fn hold_certify(&mut self) {
        self.rc.certify_held = true;
        self.rc.leg = None;
        self.rc.leg_at_ms = self.now_ms;
    }

    fn hold_eject(&mut self, eject: batcher_cut::io::HeldEject) {
        self.rc.eject_held = Some(EjectHold { eject, issued: false });
        self.rc.leg = None;
        self.rc.leg_at_ms = self.now_ms;
    }

    fn release_eject(&mut self, policy: &Policy, repo: &Repo) {
        if self.rc.eject_held.as_ref().is_none_or(|h| h.issued) {
            return;
        }
        if self.pass_running(policy, repo) {
            return;
        }
        let Some(hold) = self.rc.eject_held.as_mut() else { return };
        hold.issued = true;
        let eject = hold.eject.clone();
        let tip = self.rc.members.iter().find(|(id, _)| *id == eject.id).map(|(_, t)| t.clone()).unwrap_or_default();
        let key = format!("{}:{}:{}:eject-held:{}", self.repo, self.rc.batch.clone().unwrap_or_default(), eject.id, self.now_ms);
        self.acts.push(Leg { bead: eject.id.clone(), head: tip, key, held: Some(eject), ..self.leg() }.act(EJECT));
    }

    /// The held eject went through: the survivors are certified, as after an ordinary eject.
    fn eject_released(&mut self) {
        let Some(hold) = self.rc.eject_held.take() else { return };
        self.rc.members.retain(|(id, _)| *id != hold.eject.id);
        if self.rc.members.is_empty() {
            self.go(Trigger::RerunBlocked);
            self.emptied("the held eject left the round with no member — nothing is left to land");
            return;
        }
        self.rc.round_key = Some(core::round_key(&self.rc.members));
        self.go(Trigger::RerunCertifying);
        self.hold_certify();
    }

    /// A pass scope of the round is alive and the wait for it is within the wall.
    fn pass_running(&self, policy: &Policy, repo: &Repo) -> bool {
        let alive = self.rc.batch.as_ref().is_some_and(|b| repo.scoped_batches.as_ref().is_some_and(|s| s.contains(b)));
        alive && self.now_ms.saturating_sub(self.rc.leg_at_ms) <= policy.wall_ms
    }

    fn release_certify(&mut self, policy: &Policy, repo: &Repo) {
        if self.pass_running(policy, repo) {
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

    /// `pace::advance`, except that a batch which already advanced `infra` does not advance it again.
    fn advance_once(&self, st: &pace::State, outcome: &pace::Outcome, detail: &str, batch: &str) -> (pace::State, pace::Effects, String) {
        let carried = self.rc.pace.infra_batch.clone();
        if matches!(outcome, pace::Outcome::Infra) {
            if !batch.is_empty() && batch == carried {
                return (st.clone(), pace::Effects::default(), carried);
            }
            let (next, fx) = pace::advance(st, outcome, self.repo, detail);
            return (next, fx, batch.to_string());
        }
        let (next, fx) = pace::advance(st, outcome, self.repo, detail);
        (next, fx, carried)
    }

    /// The hold an advance asks for, then its pages: a page about a stop follows the stop's write.
    fn push_effects(&mut self, fx: &pace::Effects, detail: &str) {
        if fx.pause || fx.stop {
            let leg = Leg { why: detail.to_string(), key: format!("{}:rounds", self.repo), bead: if fx.stop { "stop".into() } else { "pause".into() }, ..Leg::on(self.repo) };
            self.acts.push(leg.act(PAUSE));
        }
        for p in &fx.pages {
            self.acts.push(Act::new(INBOX_APPEND, p.clone()).with_input(page_line(self.now_ms, p)));
        }
    }

    /// `Done`: the pace advances on the round's outcome, its pages and pause are written, and the
    /// machine is Idle with the round forgotten.
    fn finish(&mut self, outcome: pace::Outcome, detail: &str) {
        let led;
        let detail = if self.rc.pass_red.is_empty() || detail.is_empty() {
            detail
        } else {
            led = format!("certify red in {}; {detail}", self.rc.pass_red);
            &led
        };
        let batch = self.rc.batch.clone().unwrap_or_default();
        let (st, fx, infra_batch) = self.advance_once(&self.rc.pace.core(), &outcome, detail, &batch);
        self.rc.pace = Pace { infra_batch, ..Pace::of(&st, &self.rc.pace) };
        if fx.pause {
            let on = if self.rc.landing_base.is_empty() { &self.rc.base } else { &self.rc.landing_base };
            self.rc.paused_on = Some(on.clone()).filter(|b| !b.is_empty());
        }
        self.push_effects(&fx, detail);
        self.go(Trigger::Advanced);
        self.forget_round();
    }

    /// A round whose last member was ejected is closed in the store: it ends Quiet, with nothing to abandon or attribute.
    fn end_emptied_round(&mut self, why: &str) -> bool {
        if !self.rc.members.is_empty() || !matches!(self.rc.state, State::Corpus | State::Attributing | State::Certifying) {
            return false;
        }
        eprintln!("{NAME} {}: round {:?} ended: its last member was ejected ({why})", self.repo, self.rc.batch);
        self.go(Trigger::RoundEmptied);
        self.finish(pace::Outcome::Quiet, "");
        true
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
            images: rc.images,
            adopted: rc.adopted,
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

    /// A landing is publishable now: wake publish rather than leave it to its cadence.
    fn wake_publish(&mut self) {
        let input = serde_json::json!({ "repo": self.repo, "mode": "queue.local" }).to_string();
        let key = format!("{}/landed/{}", self.repo, self.now_ms);
        self.acts.push(Act::new(super::queue_step::PUBLISH_WANTED, key).with_input(input));
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
            if batch.as_ref().is_some_and(|b| repo.landed.contains(b)) {
                let batch = batch.clone().unwrap_or_default();
                self.acts.push(Leg { key: format!("supersede:{batch}"), members: self.rc.members.clone(), ..Leg::on(self.repo) }.act(SUPERSEDE_REDS));
                self.wake_publish();
            }
            self.forget_round();
            self.rc.reconciled = batch;
            return;
        }
        if self.voided(repo) || self.outcome_lost() || self.assemble_unsent(repo) {
            eprintln!("{NAME} {}: forgetting its {:?} round (batch {:?}): voided, outcome never came back, or assemble never sent", self.repo, self.rc.state, self.rc.batch);
            self.forget_round();
            return;
        }
        self.note_waiting();
        if self.attribution_lost_member(repo) || self.superseded_by_green_head(repo) {
            return;
        }
        if self.rc.eject_held.is_some() {
            self.release_eject(policy, repo);
            return;
        }
        if let Some(leg) = self.rc.eject_gate_red.clone() {
            if self.unsent(repo, EJECT_GATE_RED) {
                self.acts.push(leg.act(EJECT_GATE_RED));
            }
            return;
        }
        let passing = matches!(self.rc.state, State::Corpus | State::Attributing | State::Certifying) && !self.rc.certify_held;
        self.record_gate_reds(repo);
        if passing && self.pass_ended_without_verdict(repo) {
            return;
        }
        match self.rc.state {
            State::Certifying if self.rc.certify_held => self.release_certify(policy, repo),
            State::Corpus | State::Attributing | State::Certifying if self.now_ms.saturating_sub(self.rc.leg_at_ms) > leg_deadline_ms(policy, self.rc.state == State::Attributing, self.rc.members.len()) && !self.certify_listed(repo) => self.leg_ended(Exit::Unknown),
            State::Corpus | State::Attributing | State::Certifying => self.watch_leg(repo),
            State::Opened if self.rc.batch.is_none() => {
                self.adopt_opened(repo);
                if self.rc.batch.is_none() && self.unsent(repo, ROUND_OPEN) {
                    let why = if self.rc.suites.unwrap_or(true) { "suites" } else { "" };
                    let key = core::round_key(&self.rc.members);
                    self.acts.push(Leg { key: format!("{}:{}", self.repo, digest(&key)), why: why.into(), ..self.leg() }.act(ROUND_OPEN));
                }
            }
            // A restart keeps no request in flight; the store, not the lost outcome, says how far the land got.
            State::Landing if self.unsent(repo, LAND) => self.land_now(),
            State::Certified if !self.below => {
                self.go(Trigger::LowerEnded);
                self.land_now();
            }
            _ => {}
        }
    }

    /// The gate reds the queue's open round records against this round's members. A running pass
    /// is never stopped for one: they are read where a pass ends, or before the next one starts.
    fn record_gate_reds(&mut self, repo: &Repo) {
        let Some(open) = self.rc.batch.as_deref().and_then(|b| repo.open_rounds().find(|r| r.id == b)) else { return };
        self.rc.gate_red = open.gate_red.iter().filter(|(_, _, mark)| batcher_cut::io::mark_bars_tip(mark)).cloned().collect();
    }

    /// Ejects, ahead of any attribution and before another pass starts, each member whose own gate
    /// went red at the tip the round holds, carrying the gate's lines. True when it asked.
    fn eject_recorded_gate_reds(&mut self) -> bool {
        let Some(batch) = self.rc.batch.clone() else { return false };
        let mut seen = std::collections::BTreeSet::new();
        let red: Vec<&(String, String, String)> = self.rc.gate_red.iter().filter(|(id, tip, _)| self.rc.members.iter().any(|(m, t)| m == id && t == tip) && seen.insert(id.as_str())).collect();
        if red.is_empty() {
            return false;
        }
        let ids: Vec<String> = red.iter().map(|(id, ..)| id.clone()).collect();
        let marks: Vec<String> = red.iter().map(|(.., mark)| mark.clone()).collect();
        let why = red.iter().map(|(id, tip, mark)| format!("{id}: its own gate went red at {tip} ({mark})")).collect::<Vec<_>>().join("; ");
        let key = format!("{}:{batch}:eject-gate-red:{}:{}", self.repo, ids.join(","), self.rc.head);
        let leg = Leg { bead: ids.join(","), why, reds: marks, key, ..self.leg() };
        self.acts.push(leg.act(EJECT_GATE_RED));
        self.rc.eject_gate_red = Some(leg);
        self.rc.leg = None;
        self.rc.gate_red.clear();
        true
    }

    /// A pass the queue started that is no leg of this task and reports to nobody: with
    /// no leg, no certify lock held and no verdict, the round is ended as a fault and its members re-cut.
    fn pass_ended_without_verdict(&mut self, repo: &Repo) -> bool {
        let Some(open) = self.rc.batch.as_deref().and_then(|b| repo.open_rounds().find(|r| r.id == b)) else { return false };
        if self.rc.leg.is_some() || open.driven || open.phase == "green" || self.now_ms.saturating_sub(self.rc.leg_at_ms) < LEG_START_GRACE_MS {
            return false;
        }
        if open.red {
            self.recorded_red(open);
            return true;
        }
        let why = if self.rc.moved_from.is_empty() || self.rc.discarded_head != open.head {
            format!("round head {}: no pass is running and none will report (the pass the queue started ended without a verdict)", open.head)
        } else {
            format!("round head {}: the pass for {} was discarded when the head moved (eject of {}) and no pass for {} reported", open.head, self.rc.moved_from, self.rc.moved_ejected.join(" "), open.head)
        };
        self.go(Trigger::LegLost);
        self.abandon_naming(&why, Vec::new());
        if let Some(a) = self.acts.last_mut() {
            let mut leg: Leg = serde_json::from_str(&a.input).unwrap_or_default();
            leg.bead = NO_DRIVER.into();
            a.input = serde_json::to_string(&leg).unwrap_or_default();
        }
        self.finish(pace::Outcome::Infra, &why);
        true
    }

    /// The queue recorded the pass red: that is its verdict. The red suites' owners are attributed
    /// once per head; a head that is red and leg-less again ends the round red.
    fn recorded_red(&mut self, open: &Round) {
        let suites = if open.red_suites.is_empty() { vec!["a recorded red".to_string()] } else { open.red_suites.clone() };
        if self.rc.red_attributed_head == open.head {
            let why = format!("round head {}: the pass is red ({}) and its attribution left the same head red", open.head, suites.join(", "));
            self.go(Trigger::PassRedRecorded);
            self.red_naming(&why, suites);
            return;
        }
        self.rc.red_attributed_head = open.head.clone();
        self.go(Trigger::PassRedRecorded);
        self.attribute_after(kept_results_file(&open.id));
        if let Some(a) = self.acts.last_mut() {
            let mut leg: Leg = serde_json::from_str(&a.input).unwrap_or_default();
            leg.reds = suites;
            a.input = serde_json::to_string(&leg).unwrap_or_default();
        }
    }

    /// The live leg certifies the tree the attribution rebuilt and the store still names the head
    /// the round had before it (`current`), or the rebuilt one.
    fn certifying_rebuilt_head(&self, before: &str, current: &str) -> bool {
        !self.rc.certify_head.is_empty() && self.rc.leg_head == self.rc.certify_head && current == before
    }

    /// Follows the round's head. A leg started for an earlier head is orphaned: its result is
    /// never applied (`on_ran_with`), and the pass that replaced it decides the round. A green pass
    /// on the current head lands the round. True when the round was moved to land.
    fn superseded_by_green_head(&mut self, repo: &Repo) -> bool {
        let Some(open) = self.rc.batch.as_deref().and_then(|b| repo.open_rounds().find(|r| r.id == b)) else { return false };
        if open.head.is_empty() || !matches!(self.rc.state, State::Corpus | State::Attributing | State::Certifying) {
            return false;
        }
        let seen = std::mem::replace(&mut self.rc.head, open.head.clone());
        if self.rc.leg_head.is_empty() {
            self.rc.leg_head = open.head.clone();
        }
        if self.rc.leg_head == open.head && open.phase == "green" {
            self.stop_carried_leg();
            return self.land_green();
        }
        if self.rc.leg_head == open.head || self.certifying_rebuilt_head(&seen, &open.head) {
            return false;
        }
        if self.rc.state == State::Attributing && self.ejected_only_by_batcher(open) {
            self.rc.leg_head = open.head.clone();
            self.rc.certify_head = open.head.clone();
            return false;
        }
        let mut recertify = false;
        if self.rc.discarded_head != open.head {
            recertify = open.phase != "green" && self.rc.certify_head.is_empty() && matches!(self.rc.state, State::Corpus | State::Certifying);
            self.rc.certify_head.clear();
            eprintln!("{NAME} {}: round head moved {} -> {}: the step started for the old head is discarded", self.repo, self.rc.leg_head, open.head);
            self.rc.moved_from.clear();
            self.rc.moved_ejected.clear();
            if recertify {
                self.stop_carried_leg();
                self.rc.moved_from = self.rc.leg_head.clone();
                self.rc.moved_ejected = open.ejected.clone();
            }
            self.rc.leg = None;
            self.rc.discarded_head = open.head.clone();
            self.rc.leg_at_ms = self.now_ms;
        }
        if recertify {
            self.rc.state = State::Certifying;
            self.rc.run += 1;
            self.certify();
            return false;
        }
        if open.phase != "green" {
            return false;
        }
        self.land_green()
    }

    /// True when a carried member is gone from the record and the batcher's own eject is the last word on every one that is.
    fn ejected_only_by_batcher(&self, open: &Round) -> bool {
        lost_only_to_batcher(&self.rc.members, open)
    }

    /// Another actor (not the batcher's own attribution) ejected a member the attribution leg carries: the leg is stopped and the
    /// record's survivors are certified at the record's head. Once per eject: the state leaves
    /// Attributing.
    fn attribution_lost_member(&mut self, repo: &Repo) -> bool {
        if self.rc.state != State::Attributing || self.rc.certify_held {
            return false;
        }
        let Some(open) = self.rc.batch.as_deref().and_then(|b| repo.open_rounds().find(|r| r.id == b)) else { return false };
        if open.head.is_empty() || open.phase == "green" || open.tips.len() != open.members.len() {
            return false;
        }
        let survivors: Vec<(String, String)> = open.members.iter().enumerate().filter(|(_, id)| self.rc.members.iter().any(|(m, _)| m == *id)).map(|(i, id)| (id.clone(), open.tips[i].clone())).collect();
        if survivors.is_empty() || survivors.len() == self.rc.members.len() {
            return false;
        }
        if self.ejected_only_by_batcher(open) {
            return false;
        }
        self.stop_carried_leg();
        self.rc.members = survivors;
        self.rc.round_key = Some(core::round_key(&self.rc.members));
        self.rc.head = open.head.clone();
        self.rc.certify_head.clear();
        self.go(Trigger::RerunCertifying);
        self.hold_certify();
        true
    }

    fn land_green(&mut self) -> bool {
        self.go(if self.below { Trigger::HeadGreenHeld } else { Trigger::HeadGreen });
        if !self.below {
            self.land_now();
        }
        true
    }

    fn stop_carried_leg(&mut self) {
        let Some(key) = self.rc.leg.clone() else { return };
        self.stop_unit(&key);
    }

    fn stop_unit(&mut self, key: &str) {
        let Ok(handle) = UnitHandle::of(NAME, key) else { return };
        self.acts.push(Act::new(runtime::units::UNIT_STOP, format!("{NAME}:stop:{key}")).with_input(serde_json::to_string(&handle).unwrap_or_default()));
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

    /// `queue round certify` bounds itself, so a listed certify unit is never judged lost on the batcher's leg deadline.
    fn certify_listed(&self, repo: &Repo) -> bool {
        self.rc.state == State::Certifying && matches!((&repo.live_legs, &self.rc.leg), (Some(live), Some(leg)) if live.contains(leg))
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
            self.go(Trigger::LegRestarted);
            match self.rc.state {
                State::Corpus => self.corpus(self.rc.run + 1),
                State::Attributing => self.attribute(),
                _ => {
                    self.rc.run = self.rc.run.max(1) + 1;
                    self.certify();
                }
            }
            self.rc.leg_restarts = 1;
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
        self.rc.admissible.clear();
        if self.adopt_undriven(repo, live) {
            return;
        }
        if live.len() >= policy.max_open {
            return;
        }
        let st = self.rc.pace.core();
        let paused = self.pause_in_force(policy, repo);
        if let Some(h) = pace::hold(paused, repo.stopped, repo.cut_hold.is_some()) {
            self.rc.skip_why = Some(hold_why(&h, self.repo, repo));
            let ours: Vec<&str> = live.iter().filter_map(|r| r.batch.as_deref()).collect();
            let undriven = (h == pace::Hold::Paused && !live.iter().any(|r| matches!(r.state, State::Assembling | State::Opened)))
                .then(|| repo.open_rounds().find(|r| !ours.contains(&r.id.as_str())))
                .flatten();
            match undriven {
                Some(open) => {
                    if self.rc.state == State::Held {
                        self.go(Trigger::Released);
                    }
                    self.foreign(policy, open, repo, &st);
                }
                None => {
                    self.go(Trigger::Stopped);
                }
            }
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
        let (repo, refused, refused_out) = self.without_refused(repo);
        let repo = Repo { pool: repo.pool.into_iter().filter(|m| !held.contains(&m.id.as_str())).collect(), ..repo };
        let repo = &repo;
        let (selected, selection_out) = select_split(repo, policy.max_members);
        let observed = repo.left_out.iter().filter(|l| !held.contains(&l.id.as_str())).cloned();
        let left: Vec<batcher_cut::io::LeftOut> = observed.chain(refused_out).chain(selection_out).collect();
        self.acts.extend(left_out_note(self.repo, &mut self.rc.left_noted, left).map(|leg| leg.act(NOTE_LEFT_OUT)));
        match selected {
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
                self.rc.admissible = pool.iter().map(|m| m.id.clone()).collect();
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

    /// Takes up the first open round of another actor that is ready for its first pass and drives
    /// it as one of this task's own. Not a cut: `max_open` and a pause do not bar it. True when
    /// one was adopted.
    fn adopt_undriven(&mut self, repo: &Repo, live: &[RepoCarry]) -> bool {
        self.rc.adopted.retain(|id| repo.open_rounds().any(|r| &r.id == id));
        // An unnamed or just-promoted open round may be this task's own, and one being abandoned is over.
        let unsettled = live.iter().any(|r| matches!(r.state, State::Assembling | State::Opened | State::Promoting)) || repo.in_flight.as_ref().is_some_and(|f| f.iter().any(|e| e == ABANDON));
        if repo.stopped || repo.cut_hold.is_some() || repo.base_sha.is_empty() || unsettled {
            return false;
        }
        // Only the first round not ours: this task can order a landing only behind rounds it carries.
        let Some(open) = repo.open_rounds().find(|r| !live.iter().any(|l| l.batch.as_ref() == Some(&r.id))) else { return false };
        let ready = !open.driven && open.phase == "opened" && open.age_secs >= UNDRIVEN_GRACE_S && unfit_of(open, repo).is_none() && tips_known(open) && !open.base.is_empty();
        if !ready || self.rc.adopted.contains(&open.id) {
            return false;
        }
        eprintln!("{NAME} {}: adopting open round {}, which has no pass and no driver, and certifying it", self.repo, open.id);
        if self.rc.state == State::Held {
            self.go(Trigger::Released);
        }
        self.rc.skip_why = None;
        self.adopt(open, &self.rc.pace.core(), repo);
        self.rc.adopted.push(open.id.clone());
        self.rc.stacked = place_of(repo, live, &open.id) > 0;
        self.go(Trigger::OpenRoundOpened);
        self.certify();
        true
    }

    /// An open round this task does not drive: while it stands the repo cuts nothing. Undriven
    /// long enough it is re-driven, then a person is paged.
    fn foreign(&mut self, policy: &Policy, open: &Round, repo: &Repo, st: &pace::State) {
        if open.driven {
            self.go(Trigger::OpenRoundDriven);
            if !st.orphan.is_empty() {
                self.rc.pace = Pace::of(&pace::State { orphan: String::new(), orphan_since: 0, ..st.clone() }, &self.rc.pace);
            }
            return;
        }
        if open.phase == "green" {
            if self.adoptable(open, repo) {
                self.adopt_green(open, st, repo);
            }
            return;
        }
        if open.red && open.phase == "attributing" && self.adoptable(open, repo) {
            self.adopt_red(open, st, repo);
            return;
        }
        let unfit = unfit_of(open, repo);
        if unfit.is_none() && !is_first(open, repo) {
            return;
        }
        self.go(Trigger::OpenRoundUndriven);
        let fit = unfit.map_or(Ok(()), Err);
        let now_s = self.now_ms / 1000;
        let wait = if open.phase == "opened" && open.age_secs >= UNDRIVEN_GRACE_S && st.redriven != open.id { 0 } else { policy.orphan_wait_s };
        let (mut next, verdict) = pace::orphan_step(st, &open.id, now_s, wait, fit);
        if matches!(verdict, pace::Orphan::Redrive) && wait == 0 {
            eprintln!("{NAME} {}: open round {} has no pass and no driver — starting its first pass", self.repo, open.id);
        }
        let mut infra_batch = self.rc.pace.infra_batch.clone();
        let leg = Leg { batch: open.id.clone(), wt: open.worktree.clone(), ..Leg::on(self.repo) };
        match verdict {
            pace::Orphan::Wait => {}
            pace::Orphan::Redrive => self.acts.push(Leg { key: format!("{}:{}", self.repo, open.id), ..leg }.act(REDRIVE)),
            pace::Orphan::Incident(why) => {
                let why = format!("round {} had no driver: {why}", open.id);
                self.acts.push(Leg { key: format!("{}:{}", self.repo, open.id), why: why.clone(), bead: NO_DRIVER.into(), ..leg }.act(ABANDON));
                let (after, fx, batch) = self.advance_once(&next, &pace::Outcome::Infra, &why, &open.id);
                next = after;
                infra_batch = batch;
                self.push_effects(&fx, &why);
            }
        }
        self.rc.pace = Pace { infra_batch, ..Pace::of(&next, &self.rc.pace) };
        self.go(Trigger::OrphanStepped);
    }

    /// The round is the repo's first open one, and every member's tip, its base and the landing ref
    /// are known: an unread landing ref would leave a pause keyed to the round's base.
    fn adoptable(&self, open: &Round, repo: &Repo) -> bool {
        tips_known(open) && !open.base.is_empty() && !repo.base_sha.is_empty() && is_first(open, repo)
    }

    /// A round no pass drives whose last pass ended red is attributed from that pass's kept results.
    fn adopt_red(&mut self, open: &Round, st: &pace::State, repo: &Repo) {
        self.adopt(open, st, repo);
        self.go(Trigger::OpenRoundRed);
        self.attribute_after(kept_results_file(&open.id));
    }

    fn adopt(&mut self, open: &Round, st: &pace::State, repo: &Repo) {
        self.rc.base = open.base.clone();
        self.rc.landing_base = repo.base_sha.clone();
        self.rc.pace = Pace::of(&pace::State { orphan: String::new(), orphan_since: 0, redriven: String::new(), ..st.clone() }, &self.rc.pace);
        self.rc.batch = Some(open.id.clone());
        self.rc.members = open.members.iter().enumerate().map(|(i, id)| (id.clone(), open.tips[i].clone())).collect();
        self.rc.round_key = Some(core::round_key(&self.rc.members));
        self.rc.wt = open.worktree.clone();
        self.rc.head = open.head.clone();
    }

    /// A green round no pass drives is landed on the pass that made it green, as one of this task's own.
    fn adopt_green(&mut self, open: &Round, st: &pace::State, repo: &Repo) {
        self.adopt(open, st, repo);
        self.go(Trigger::OpenRoundGreen);
        self.land_now();
    }

    /// The pause a red round wrote, if it still holds the repo. It carries the base its round was
    /// cut on; once a round lands past that base the red it guarded is behind the repo, so the pause is
    /// lifted (removed, and the lift logged). An operator's `hold:` is never lifted here.
    fn pause_in_force(&mut self, policy: &Policy, repo: &Repo) -> bool {
        let Some(text) = repo.paused.as_deref() else {
            if self.rc.paused_on.as_deref().is_some_and(|on| !repo.base_sha.is_empty() && on != repo.base_sha) {
                self.rc.paused_on = None;
            }
            return false;
        };
        if text.starts_with("hold:") {
            return true;
        }
        let held_for = repo.paused_since.map(|since| (self.now_ms / 1000).saturating_sub(since));
        let mut st = self.rc.pace.core();
        if held_for.is_some_and(|held| pace::pause_expired(&st, held, policy.pause_lift_s)) {
            st.pause_lifts += 1;
            self.rc.pace = Pace::of(&st, &self.rc.pace);
            self.rc.paused_on = None;
            let why = format!("{} pause held {}s past its {}s bound — lifted, cutting; a second red stays paused ({})", self.repo, held_for.unwrap_or_default(), policy.pause_lift_s, text.trim());
            self.acts.push(Leg { why, key: format!("{}:rounds:lift:expired:{}", self.repo, self.now_ms / 1000), bead: "lift".into(), ..Leg::on(self.repo) }.act(PAUSE));
            let page = format!("[watch:rounds] PAUSE LIFTED {} after {}s — cutting again; a second red stays paused", self.repo, held_for.unwrap_or_default());
            self.acts.push(Act::new(INBOX_APPEND, page.clone()).with_input(page_line(self.now_ms, &page)));
            return false;
        }
        if let Some(since) = repo.paused_since.filter(|_| held_for.is_some_and(|h| h > policy.pause_lift_s) && !repo.pool.is_empty() && self.rc.pace.pause_paged != repo.paused_since.unwrap_or_default()) {
            self.page_unattended(repo, text, since, held_for.unwrap_or_default());
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

    /// A red pause held past its bound with no lift left: one page and one ask for this pause.
    fn page_unattended(&mut self, repo: &Repo, text: &str, since: u64, held: u64) {
        self.rc.pace.pause_paged = since;
        let reason = match text.trim() {
            t if t.is_empty() || t.starts_with("red:") => "a red round",
            t => t,
        };
        let evidence = repo.pause_evidence.as_deref().map(|e| format!(" — {e}")).unwrap_or_default();
        let page = format!("PAUSE UNATTENDED {}: {reason}, held {held}s, {} certified waiting{evidence} — attribute and eject, then batcher pause-lift --repo {} --by concierge --reason \"<why>\"", self.repo, repo.pool.len(), self.repo);
        self.acts.push(Act::new(INBOX_APPEND, page.clone()).with_input(page_line(self.now_ms, &page)));
        let key = format!("{}:pause:{since}", self.repo);
        let body = format!("## Question\n{page}\n\n## Default\nLift the pause once the red is attributed and its members ejected.\n\n## Class basis\nWhether a red pause stays held is policy (law-a-pause-releases-itself).\n\nMOOT-WHEN: batcher pause-show --repo {} --held-since {since}\n", self.repo);
        let message = tsd::alarm::Message { mailbox: "operator".into(), subject: format!("P0 {key}: red pause held {held}s with {} certified waiting", repo.pool.len()), kind: "question".into(), class: Some("policy".into()), default: "Lift the pause once the red is attributed and its members ejected.".into(), body };
        let send = crate::tasks::b::incident_alarms::Send { reference: key.clone(), kind: "pause".into(), message, rows: vec![], binding: Some(key.clone()) };
        self.acts.push(Act::new(PAUSE_ASK, key).with_input(serde_json::to_string(&send).unwrap_or_default()));
    }

    /// Lifts the image-absent refusals of the wants whose tag is present, then forgets every want
    /// no refusal names and whose members' tips have all moved: forgetting one over a moved base
    /// would build its tag again.
    fn readmit_imaged(&mut self, repo: &Repo) {
        let (name, acts) = (self.repo, &mut self.acts);
        let RepoCarry { refused, images, .. } = &mut self.rc;
        images.retain(|tag, want| {
            let built = repo.images_present.contains(tag);
            if built {
                refused.retain(|id, (_, _, kind)| kind != IMAGE_ABSENT || !want.members.contains(id));
            }
            let waited_on = want.members.iter().any(|id| refused.get(id).is_some_and(|(_, _, kind)| kind == IMAGE_ABSENT) || repo.pool.iter().any(|m| &m.id == id && want.tips.get(id) == Some(&m.tip)));
            let keep = !built && waited_on;
            // Its result may never be read once the want is gone; the clean spares a running build.
            if !keep && want.unit.is_some() {
                acts.push(image_clean(name, tag));
            }
            keep
        });
    }

    /// `id`, refused for want of `tag` at `tip`, joins that tag's want; only a new want builds.
    fn want_image(&mut self, tag: &str, id: &str, tip: &str) {
        for (_, other) in self.rc.images.iter_mut().filter(|(t, _)| *t != tag) {
            other.members.retain(|m| m != id);
            other.tips.remove(id);
        }
        if let Some(want) = self.rc.images.get_mut(tag) {
            want.tips.insert(id.to_string(), tip.to_string());
            if !want.members.iter().any(|m| m == id) {
                want.members.push(id.to_string());
            }
            return;
        }
        let want = ImageWant { tip: tip.to_string(), members: vec![id.to_string()], unit: Some(image_unit_key(self.repo, tag)), failed: false, at_ms: self.now_ms, tips: [(id.to_string(), tip.to_string())].into() };
        self.acts.push(image_leg(self.repo, tag, &want).act(IMAGE_BUILD));
        self.rc.images.insert(tag.to_string(), want);
    }

    /// The build of `tag` gave no image: it is not built again, and one line says how to.
    fn image_failed(&mut self, tag: &str, how: &str, said: &str) {
        let Some(want) = self.rc.images.get_mut(tag) else { return };
        want.unit = None;
        want.failed = true;
        let lines: Vec<&str> = said.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let last = lines[lines.len().saturating_sub(IMAGE_SAID_LINES)..].join(" | ");
        let said = if last.is_empty() { String::new() } else { format!(" — last output: {last}") };
        let page = format!("batcher {}: testenv image build for localhost/spira-testenv:{tag} failed ({how}); {} stay(s) out of the cut — build it by hand from a checkout of {} with `{}`{said}", self.repo, want.members.join(", "), want.tip, spira_config::recipe::HAND_BUILD);
        self.acts.push(Act::new(INBOX_APPEND, page.clone()).with_input(page_line(self.now_ms, &page)));
    }

    /// A build with no result past the wall is stopped and failed, and so is a green one whose
    /// tag the store still does not hold a wall later.
    fn expire_image_builds(&mut self, policy: &Policy, repo: &Repo) {
        let overdue: Vec<(String, Option<String>)> = self.rc.images.iter().filter(|(tag, w)| !w.failed && !repo.images_present.contains(tag) && self.now_ms.saturating_sub(w.at_ms) > policy.wall_ms + LEG_GRACE_MS).map(|(tag, w)| (tag.clone(), w.unit.clone())).collect();
        for (tag, unit) in overdue {
            let Some(unit) = unit else {
                self.image_failed(&tag, "it passed but the host store does not hold the tag", "");
                continue;
            };
            self.stop_unit(&unit);
            self.acts.push(image_clean(self.repo, &tag));
            self.image_failed(&tag, "no result within the wall", "");
        }
    }

    /// `repo` without the members `queue round open` refused at their current tip and base, and
    /// a line naming each; a refusal whose tip or base has moved since is forgotten.
    fn without_refused(&mut self, repo: &Repo) -> (Repo, Vec<String>, Vec<batcher_cut::io::LeftOut>) {
        self.rc.refused.retain(|id, (tip, base, _)| repo.pool.iter().any(|m| &m.id == id && &m.tip == tip) && base == &repo.base_sha);
        self.readmit_imaged(repo);
        let refused = &self.rc.refused;
        let why = repo.pool.iter().filter_map(|m| refused.get(&m.id).map(|(_, _, kind)| format!("{}: queue round open refused it ({kind}) at this tip and base", m.id))).collect();
        let left = repo.pool.iter().filter_map(|m| refused.get(&m.id).map(|(tip, _, kind)| batcher_cut::io::LeftOut { id: m.id.clone(), tip: tip.clone(), cause: batcher_cut::io::Cause::Refused { kind: kind.clone() } })).collect();
        let pool = repo.pool.iter().filter(|m| !refused.contains_key(&m.id)).cloned().collect();
        (Repo { pool, ..repo.clone() }, why, left)
    }

    fn leg_ended(&mut self, exit: Exit) {
        self.leg_ended_with(exit, "");
    }

    /// A leg ended with `exit`, having written `out` (its captured stdout tail, empty when none
    /// came back): the attribution and certify legs are judged by what they said.
    fn leg_ended_with(&mut self, exit: Exit, out: &str) {
        self.rc.leg_restarts = 0;
        self.rc.leg_missing = 0;
        if self.rc.state == State::Certifying {
            self.rc.certify_head.clear();
        }
        let ok = green(exit);
        if !ok && self.rc.members.is_empty() && self.end_emptied_round("its leg ended red with no member left") {
            return;
        }
        if !ok && matches!(exit, Exit::Code(_)) && matches!(self.rc.state, State::Corpus | State::Certifying) && self.eject_recorded_gate_reds() {
            return;
        }
        if matches!(self.rc.state, State::Corpus | State::Attributing | State::Certifying)
            && (exit == Exit::Signal(SIGTERM) || Attribution::parse(out) == Some(Attribution::Abandoned))
        {
            eprintln!("{NAME} {}: batch {:?} {:?} leg: the round was abandoned while it ran — stopped, no verdict written", self.repo, self.rc.batch, self.rc.state);
            self.go(Trigger::LegStopped);
            self.finish(pace::Outcome::Quiet, "");
            return;
        }
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
                let pass = self.rc.leg.as_deref().map(|unit| round_results(&self.rc.wt, unit).display().to_string()).unwrap_or_default();
                self.attribute_after(pass);
            }
            (State::Attributing, _) => match Attribution::parse(out) {
                Some(Attribution::Land { members, mended, head, .. }) if !members.is_empty() => {
                    if !head.is_empty() && head != self.rc.head {
                        self.rc.certify_head = head;
                    }
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
                Some(Attribution::Unconverged(why)) => {
                    self.go(Trigger::RerunUnconverged);
                    if self.rc.batch.is_some() {
                        let leg = Leg { why: why.clone(), bead: ATTRIBUTION_NOT_CONVERGING.into(), key: format!("{}:{}", self.repo, self.rc.batch.clone().unwrap_or_default()), ..self.leg() };
                        self.acts.push(leg.act(ABANDON));
                    }
                    self.finish(pace::Outcome::Quiet, "");
                }
                Some(Attribution::RunnerDifference { red_on, green_on, .. }) => {
                    eprintln!("{NAME} {}: batch {:?}: the red ran on {red_on} and is green on {green_on} — certifying again on the host, no member charged", self.repo, self.rc.batch);
                    self.go(Trigger::RerunRunnerDifference);
                    self.rc.host_pin = true;
                    self.rc.run += 1;
                    self.certify();
                }
                Some(Attribution::Faulted(why)) => {
                    self.go(Trigger::RerunBlocked);
                    self.finish(pace::Outcome::AttributionFaulted, &why);
                }
                Some(Attribution::Blocked(why)) if batcher_cut::io::HeldEject::parse(&why).is_some() => {
                    self.hold_eject(batcher_cut::io::HeldEject::parse(&why).unwrap_or_default());
                }
                Some(Attribution::Abandoned) => {
                    self.go(Trigger::LegStopped);
                    self.finish(pace::Outcome::Quiet, "");
                }
                Some(Attribution::Blocked(why)) => {
                    self.go(Trigger::RerunBlocked);
                    self.red(&format!("local corpus red — held before reaching CI: {why}"));
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
            (State::Certifying, false) if out.lines().any(|l| l.starts_with(queue::ops::round::EJECT_REFUSED)) => {
                let said = out.lines().rev().find_map(|l| l.strip_prefix(queue::ops::round::EJECT_REFUSED)).unwrap_or_default().trim().to_string();
                self.go(Trigger::LegLost);
                self.red_naming(&format!("eject {said}"), Vec::new());
            }
            (State::Certifying, false) if !self.rc.attributed && !out.lines().any(|l| l.starts_with(RED_OUTPUT)) => {
                self.rc.attributed = true;
                self.record_attributed_reds(out);
                if self.rc.pass_red.is_empty() {
                    self.rc.pass_red = pass_red_of(out);
                }
                self.go(Trigger::CertifyRedFirst);
                self.attribute_after_certify();
            }
            (State::Certifying, false) if self.rc.attributed && !self.rc.confirmed && self.rc.members.len() == 1 && !out.lines().any(|l| l.starts_with(RED_OUTPUT)) && !certify_red_evidence(out).1.is_empty() => {
                self.rc.confirmed = true;
                self.record_attributed_reds(out);
                self.go(Trigger::CertifyRedFirst);
                self.attribute_after_certify();
            }
            (State::Certifying, false) if self.rc.attributed && !self.rc.confirmed && self.rc.members.len() >= 2 && !out.lines().any(|l| l.starts_with(RED_OUTPUT)) && !certify_red_evidence(out).1.is_empty() => {
                self.rc.confirmed = true;
                self.record_attributed_reds(out);
                self.go(Trigger::CertifyRedConfirmed);
                self.attribute_confirmed();
            }
            (State::Certifying, false) if self.rc.attributed && self.rc.confirmed && !out.lines().any(|l| l.starts_with(RED_OUTPUT)) && self.unattributed_reds(out) => {
                self.record_attributed_reds(out);
                self.go(Trigger::CertifyRedFirst);
                self.attribute_after_certify();
            }
            (State::Certifying, false) if certify_red_evidence(out).0.is_none() && certify_red_evidence(out).1.is_empty() && !out.lines().any(|l| l.starts_with(RED_OUTPUT)) => self.certify_faulted(exit, out),
            (State::Certifying, false) => {
                let (verdict, reds) = certify_red_evidence(out);
                let why = match (&verdict, reds.is_empty()) {
                    (Some(v), _) => format!("queue round certify judged the round red: {v}"),
                    (None, false) => format!("queue round certify judged the round red: {}", reds.join(",")),
                    (None, true) => "queue round certify judged the round red (its output named no verdict and no suite)".into(),
                };
                let output = out.lines().rev().find_map(|l| l.strip_prefix(RED_OUTPUT)).map(str::trim);
                if let ([(id, tip)], Some(o)) = (self.rc.members.clone().as_slice(), output) {
                    let evidence = format!("{why}; output kept at {o}");
                    let key = format!("{}:{}{SOLE_KEY}", self.repo, self.rc.batch.clone().unwrap_or_default());
                    self.acts.push(Leg { bead: id.clone(), head: tip.clone(), why: evidence, reds, key, ..self.leg() }.act(EJECT));
                    self.rc.leg = None;
                    self.rc.leg_at_ms = self.now_ms;
                    return;
                }
                self.go(Trigger::CertifyRed);
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
            (State::Opened, ROUND_OPEN) if !ok && body == runtime::engine::LOST_AT_RESTART => eprintln!("{NAME} {}: round open's outcome was lost at a restart — the next tick adopts the open round", self.repo),
            // Whether it ran is unknown, so it is no refusal: the ticks read what it did.
            _ if !ok && body == runtime::engine::LOST_AT_RESTART => {}
            (State::Opened, ROUND_OPEN) => {
                let o: Opened = if ok { serde_json::from_str(body).unwrap_or_default() } else { Opened::default() };
                if o.batch.is_empty() && !o.refused.is_empty() {
                    self.refused_some(&o, leg);
                    return;
                }
                if o.batch.is_empty() {
                    self.go(Trigger::OpenFailed);
                    self.finish(pace::Outcome::OpenFailed, &open_failed_detail(ok, body));
                    return;
                }
                self.rc.batch = Some(o.batch);
                self.rc.pace = Pace::of(&pace::opened(&self.rc.pace.core()), &self.rc.pace);
                self.opened(leg.why == "suites");
            }
            (State::Corpus | State::Attributing, CORPUS) | (State::Certifying, CERTIFY) if !ok && self.rc.leg.as_deref() == Some(leg.unit.as_str()) => self.leg_unstarted(&format!("it could not be started ({})", body.trim())),
            (State::Corpus | State::Attributing | State::Certifying, EJECT) if ok && self.rc.eject_held.as_ref().is_some_and(|h| h.issued) => self.eject_released(),
            (State::Certifying, EJECT) if ok && leg.key.ends_with(SOLE_KEY) => {
                self.rc.members.retain(|(id, _)| *id != leg.bead);
                self.end_emptied_round("the red round's only member was ejected");
            }
            (State::Corpus | State::Attributing | State::Certifying, EJECT) if !ok && self.rc.eject_held.as_ref().is_none_or(|h| !h.issued) && batcher_cut::io::HeldEject::parse(body).is_some() => {
                self.hold_eject(batcher_cut::io::HeldEject::parse(body).unwrap_or_default());
            }
            (State::Corpus | State::Attributing | State::Certifying, EJECT) if !ok => {
                self.rc.eject_held = None;
                self.go(Trigger::LegLost);
                let evidence = if leg.key.ends_with(SOLE_KEY) { format!("; {}", leg.why) } else { String::new() };
                self.red_naming(&format!("eject of {} refused: {}{evidence}", leg.bead, body.trim()), leg.reds.clone());
            }
            (State::Corpus | State::Attributing | State::Certifying, EJECT_GATE_RED) if ok => {
                self.rc.eject_gate_red = None;
                self.rc.members.retain(|(id, _)| !leg.bead.split(',').any(|b| b == id));
                if self.rc.members.is_empty() {
                    self.go(Trigger::RerunBlocked);
                    self.emptied("the gate-red eject left the round with no member — nothing is left to land");
                    return;
                }
                self.rc.round_key = Some(core::round_key(&self.rc.members));
                self.go(Trigger::RerunCertifying);
                self.certify();
            }
            (State::Corpus | State::Attributing | State::Certifying, EJECT_GATE_RED) if !ok && self.rc.eject_held.as_ref().is_none_or(|h| !h.issued) && batcher_cut::io::HeldEject::parse(body).is_some() => {
                self.rc.eject_gate_red = None;
                let held = batcher_cut::io::HeldEject::parse(body).unwrap_or_default();
                let gone: Vec<&str> = leg.bead.split(',').take_while(|b| *b != held.id).collect();
                self.rc.members.retain(|(id, _)| !gone.contains(&id.as_str()));
                self.rc.round_key = Some(core::round_key(&self.rc.members));
                self.hold_eject(held);
            }
            (State::Corpus | State::Attributing | State::Certifying, EJECT_GATE_RED) => {
                self.rc.eject_gate_red = None;
                let why = format!("{} went red on its own gate ({}) and its eject was refused ({})", leg.bead, leg.reds.join(" "), body.trim());
                self.go(Trigger::LegLost);
                self.abandon_naming(&why, vec![]);
                self.finish(pace::Outcome::Quiet, "");
            }
            (State::Landing, LAND) if ok => {
                let l: Landed = serde_json::from_str(body).unwrap_or_default();
                if let Some(base) = l.base_moved {
                    self.base_moved(&base);
                    return;
                }
                self.acts.push(Leg { key: format!("supersede:{}", leg.batch), members: leg.members.clone(), ..Leg::on(self.repo) }.act(SUPERSEDE_REDS));
                self.wake_publish();
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
            (State::Landing, LAND) => self.land_refused(body),
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
                self.go(Trigger::PromotedToCertifying);
                self.certify();
            }
            _ => {}
        }
    }

    /// The round is open and named: its first leg starts.
    fn opened(&mut self, suites: bool) {
        if !suites && self.rc.own_testenv {
            self.go(Trigger::OpenedOwnTestenv);
        } else {
            self.go(Trigger::OpenedWithSuites);
        }
        self.certify();
    }

    /// A round the queue holds with exactly our members in our tree is ours: driven now, not after [`OUTCOME_WAIT_MS`].
    fn adopt_opened(&mut self, repo: &Repo) {
        let ours: std::collections::BTreeSet<&str> = self.rc.members.iter().map(|(id, _)| id.as_str()).collect();
        let held = repo.open_rounds().find(|r| !ours.is_empty() && (self.rc.wt.is_empty() || r.worktree == self.rc.wt) && r.members.iter().map(String::as_str).collect::<std::collections::BTreeSet<_>>() == ours);
        let Some(held) = held else { return };
        eprintln!("{NAME} {}: round open's outcome did not arrive; adopting the queue's open round {} and starting its first pass", self.repo, held.id);
        self.rc.batch = Some(held.id.clone());
        self.rc.pace = Pace::of(&pace::opened(&self.rc.pace.core()), &self.rc.pace);
        self.opened(self.rc.suites.unwrap_or(true));
    }

    /// `round open` refused the members it names (typed, sp-822b8l): each is set aside at its tip
    /// and the round's base (but not a `POOL_READ_REFUSALS` kind, which only leaves this leg), and the round is asked for again with the rest; with none left the
    /// cut is over, nothing opened and nothing abandoned. The queue has already routed them (a
    /// base conflict back to rework, a round conflict into an integration that carries it).
    fn refused_some(&mut self, opened: &Opened, leg: &Leg) {
        let refused = &opened.refused;
        for (id, kind, _) in refused.iter().filter(|(_, kind, _)| !POOL_READ_REFUSALS.contains(&kind.as_str())) {
            let tip = self.rc.members.iter().find(|(m, _)| m == id).map(|(_, t)| t.clone()).unwrap_or_default();
            let landing = if self.rc.landing_base.is_empty() { &self.rc.base } else { &self.rc.landing_base };
            self.rc.refused.insert(id.clone(), (tip.clone(), landing.clone(), kind.clone()));
            if let Some(tag) = opened.images.get(id).filter(|_| kind == IMAGE_ABSENT) {
                self.want_image(tag, id, &tip);
            }
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
        self.rc.base = base.to_string();
        self.go(Trigger::LandBaseMoved);
        self.rc.run += 1;
        self.certify();
    }

    /// `round land` refused a green round: the refusal may name a member's change or the host, so the round
    /// stays open with all of them, the output is kept, and a person is paged with the refusing line.
    fn land_refused(&mut self, body: &str) {
        let batch = self.rc.batch.clone().unwrap_or_default();
        let kept = land_output_file(&batch, self.rc.run);
        self.acts.push(Leg { why: body.to_string(), key: kept.clone(), ..self.leg() }.act(KEEP_CERTIFY_OUTPUT));
        self.go(Trigger::LandRefused);
        self.finish(pace::Outcome::LandRefused, &land_refused_detail(&batch, body, &kept));
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
            self.rc.admissible.clear();
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

/// Where `batch` goes among `live` (lowest first): under the first of them the queue lists above it.
fn place_of(repo: &Repo, live: &[RepoCarry], batch: &str) -> usize {
    let over: Vec<&str> = repo.open_rounds().map(|r| r.id.as_str()).skip_while(|id| *id != batch).skip(1).collect();
    live.iter().position(|l| l.batch.as_deref().is_some_and(|b| over.contains(&b))).unwrap_or(live.len())
}

fn is_first(open: &Round, repo: &Repo) -> bool {
    repo.open_round.as_ref().is_some_and(|r| r.id == open.id)
}

fn tips_known(open: &Round) -> bool {
    open.tips.len() == open.members.len() && open.tips.iter().all(|t| !t.is_empty())
}

/// The repo's `unfit` speaks for its first open round; a round above carries its own.
fn unfit_of(open: &Round, repo: &Repo) -> Option<String> {
    if is_first(open, repo) { repo.unfit.clone() } else { open.unfit.clone() }
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
    if !snap.registry_landed.is_empty() {
        let ids = &snap.registry_landed;
        let home = snap.repos.keys().next().map(String::as_str).unwrap_or_default();
        acts.push(Leg { key: format!("clear-landed:{}", ids.join(",")), members: ids.iter().map(|i| (i.clone(), String::new())).collect(), ..Leg::on(home) }.act(CLEAR_LANDED));
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
    m.expire_image_builds(policy, repo);
    if ended {
        m.rc.admissible.clear();
    } else {
        m.cut(policy, repo, &live);
    }
    if m.rc.live() {
        let at = m.rc.batch.as_deref().map_or(live.len(), |batch| place_of(repo, &live, batch));
        live.iter_mut().skip(at).for_each(|above| above.stacked = true);
        live.insert(at, m.rc.round());
    }
    let state = if live.is_empty() { m.rc.state } else { idle };
    let mut rc = RepoCarry::rebuilt(&m.rc, live, state);
    rc.blocked = blocked_of(repo);
    (rc, m.acts)
}

/// Each pool member the cut refuses for an unlanded prerequisite, with the first one it names.
pub fn blocked_of(repo: &Repo) -> BTreeMap<String, String> {
    repo.pool.iter().filter_map(|m| m.core(&repo.blockers).blocked_by.into_iter().next().map(|dep| (m.id.clone(), dep))).collect()
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
    if effect == NOTE_LEFT_OUT {
        if !ok {
            next.repos.entry(leg.repo.clone()).or_default().left_noted.clear();
        }
        return (next, vec![]);
    }
    if effect == IMAGE_CLEAN {
        if !ok {
            eprintln!("{NAME} {}: the checkout of image {} was not removed; the next build of the tag clears it: {body}", leg.repo, leg.image);
        }
        return (next, vec![]);
    }
    if effect == IMAGE_BUILD {
        let mut m = Machine { repo: &leg.repo, rc, acts: vec![], now_ms, below: false };
        let Some(want) = m.rc.images.get(&leg.image).filter(|w| w.unit.as_deref() == Some(leg.unit.as_str())) else { return (next, vec![]) };
        if !ok && body == runtime::engine::LOST_AT_RESTART {
            m.acts.push(image_leg(&leg.repo, &leg.image, want).act(IMAGE_BUILD));
        } else if !ok {
            m.acts.push(image_clean(&leg.repo, &leg.image));
            m.image_failed(&leg.image, &format!("it did not start: {}", body.trim()), "");
        }
        store(&mut next, &leg.repo, m.rc);
        return (next, m.acts);
    }
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
    on_ran_seeing(carry, ran, out, now_ms, &|_, _| None::<Round>)
}

/// [`on_ran_with`] that first reads the round's record as of now (`round_of(repo, batch)`): a step
/// started for another head is replaced whatever the carried head last said, unless the batcher's
/// own eject moved the head, in which case the attribution's outcome is applied at the new head.
pub fn on_ran_seeing<R: Into<Round>>(carry: &Carry, ran: &UnitRan, out: &RanOutput, now_ms: u64, round_of: &dyn Fn(&str, &str) -> Option<R>) -> Option<(Carry, Vec<Act>)> {
    let key = ran.key_for(NAME)?;
    if let Some((repo, tag)) = image_of_key(&key) {
        return Some(image_ran(carry, repo, tag, ran.exit, out, now_ms));
    }
    let (repo, rc, at) = carry.repos.iter().find_map(|(repo, rc)| {
        let at = rc.rounds().iter().position(|r| r.leg.as_deref() == Some(key.as_str()) && matches!(r.state, State::Corpus | State::Attributing | State::Certifying))?;
        Some((repo, rc, at))
    })?;
    let held = &rc.rounds()[at];
    let seen: Option<Round> = held.batch.as_deref().and_then(|b| round_of(repo, b)).map(Into::into).filter(|r| !r.head.is_empty());
    let current = seen.as_ref().map_or_else(|| held.head.clone(), |r| r.head.clone());
    let rebuilt = !held.certify_head.is_empty() && held.leg_head == held.certify_head && current == held.head;
    let moved = !current.is_empty() && !held.leg_head.is_empty() && current != held.leg_head && !rebuilt;
    let own_eject = moved && held.state == State::Attributing && seen.as_ref().is_some_and(|open| lost_only_to_batcher(&held.members, open));
    if moved && !own_eject {
        eprintln!("{NAME} {repo}: step {key} was started for head {} and the round is at {current}: its result is discarded", held.leg_head);
        return None;
    }
    let mut next = carry.clone();
    let mut m = Machine { repo, rc: rc.holding(&rc.rounds()[at]), acts: vec![], now_ms, below: at > 0 };
    if own_eject {
        m.rc.head = current.clone();
        m.rc.leg_head = current.clone();
        m.rc.certify_head = current;
    }
    let said = said_by(out);
    m.leg_ended_with(ran.exit, &said);
    let acts = std::mem::take(&mut m.acts);
    store(&mut next, repo, replace_round(rc, at, &m));
    Some((next, acts))
}

fn said_by(out: &RanOutput) -> String {
    [out.stdout_tail.as_deref(), out.stderr_tail.as_deref()].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
}

/// An image build ended: its checkout goes whether or not a refusal still waits on it.
fn image_ran(carry: &Carry, repo: &str, tag: &str, exit: Exit, out: &RanOutput, now_ms: u64) -> (Carry, Vec<Act>) {
    let mut next = carry.clone();
    let mut m = Machine { repo, rc: carry.repos.get(repo).cloned().unwrap_or_default(), acts: vec![image_clean(repo, tag)], now_ms, below: false };
    if m.rc.images.get(tag).is_some_and(|w| w.unit.is_some()) {
        if green(exit) {
            if let Some(want) = m.rc.images.get_mut(tag) {
                want.unit = None;
                want.at_ms = now_ms;
            }
        } else {
            m.image_failed(tag, &exit_words(exit), &said_by(out));
        }
        store(&mut next, repo, m.rc);
    }
    (next, m.acts)
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
        let ran = |carry: &Carry, r: &UnitRan| on_ran_seeing(carry, r, &out, ev.at, &round_now);
        let legs = |carry: &Carry| {
            let rounds = carry.repos.values().flat_map(|rc| rc.rounds()).filter(|r| matches!(r.state, State::Corpus | State::Attributing | State::Certifying)).filter_map(|r| r.leg);
            rounds.chain(carry.repos.values().flat_map(|rc| rc.images.values().filter_map(|w| w.unit.clone()))).collect()
        };
        handle_p2(&P2 { shape: &SHAPE, cadence: &self.cadence, acts: ACTS, emits: EMITS, effects: EFFECTS, wakes: WAKES }, view, ev, On { observed, outcome, ran, legs })
    }
}

/// The round the store's projection holds for `batch` right now; `None` when this process keeps
/// no projection or the round is not open.
fn round_now(repo: &str, batch: &str) -> Option<Round> {
    let views = store_view::shared()?;
    let view = views.round_view(repo).ok()?;
    let open = batcher_cut::io::open_round_of(view.value.rounds.iter().find(|r| r.record.id == batch)?, 0);
    Some(Round { id: open.id, members: open.members, tips: open.tips, head: open.head, ejected: open.ejected, ejected_by: open.ejected_by, ..Round::default() })
}

/// Each repo of `snap` given the effects of this task the stream has asked for and not had answered.
fn in_flight(view: &Projection, stream: &runtime::event::StreamKey, snap: &mut Snapshot) {
    let mut flying = flying_by_repo(view, stream, |input| serde_json::from_str::<Leg>(input).ok().map(|leg| leg.repo));
    for (name, repo) in &mut snap.repos {
        repo.in_flight = Some(flying.remove(name).unwrap_or_default());
    }
}

/// The names of the effects the stream has requested and not had answered, or that a halt holds
/// for it and has not released, by the repo `repo_of` reads from each one's input.
pub(super) fn flying_by_repo(view: &Projection, stream: &runtime::event::StreamKey, repo_of: impl Fn(&str) -> Option<String>) -> BTreeMap<String, Vec<String>> {
    let events = view.events(stream);
    let answered: std::collections::BTreeSet<&runtime::event::EventId> = events.iter().filter(|e| [EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT].contains(&e.kind.tag.as_str())).filter_map(|e| e.cause.as_ref()).collect();
    let body = |e: &Event| serde_json::from_str::<serde_json::Value>(&e.kind.body).ok();
    let requested = events.iter().filter(|e| e.kind.tag == EFFECT_REQUESTED && !answered.contains(&e.id)).filter_map(body);
    // The resume releases everything held before it, including a release the engine refuses.
    let since_resume = view.events(&runtime::event::StreamKey(HALT_STREAM.into())).rsplit(|e| e.kind.tag == SCHEDULER_RESUMED).next().unwrap_or_default();
    let held = since_resume.iter().filter(|e| e.kind.tag == EFFECT_HELD).filter_map(body).filter(|b| b["stream"] == stream.0.as_str());
    let mut flying: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for b in requested.chain(held) {
        let (Some(name), Some(input)) = (b["spec"]["name"].as_str(), b["spec"]["input"].as_str()) else { continue };
        if let Some(repo) = repo_of(input) {
            flying.entry(repo).or_default().push(name.to_string());
        }
    }
    flying
}

/// The runners: each effect's real work, in this process. The seam is `batcher_cut::io`, the
/// batcher's own IO library (git as a child, the store through `spira-lc`'s socket, the queue's
/// round verbs where the store has no door yet); every long leg is a unit on `Effects::units`.
pub mod edge {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use batcher_cut::io::{self, Env};
    use runtime::units::{ScopeSpec, UnitSpec, UNIT_RUN_SCOPE, UNIT_START};

    use super::{Assembled, Landed, Leg, Member, Opened, Promoted, Repo, Round, Snapshot};

    /// Each gate-red row's mark, followed by where the gate recorded the failure when it did.
    pub(super) fn with_failing(rows: Vec<(String, String, String)>, failing: impl Fn(&str, &str) -> Option<String>) -> Vec<(String, String, String)> {
        rows.into_iter()
            .map(|(id, tip, mark)| match failing(&id, &tip) {
                Some(at) => (id, tip, format!("{mark}; {at}")),
                None => (id, tip, mark),
            })
            .collect()
    }

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
        members.iter().map(|(id, tip)| batcher::core::Member { id: id.clone(), tip: tip.clone(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: BTreeMap::new(), blocked_by: vec![], provisional: false }).collect()
    }

    /// One unit leg under the naming rule: a scope with the round's CPU quota when the backend
    /// runs scopes, else a plain transient unit.
    /// The base environment every leg unit needs, as sentinel-call gives a dwarf: the release, a
    /// PATH onto it, HOME and the one config spec. A transient unit inherits none of the
    /// runtime's environment, so without `SPIRA_TOML` round-vm refuses at once (exit 2) and the
    /// round is abandoned before it runs. A variable the leg already set is kept.
    pub fn leg_env(vars: Vec<(String, String)>, release: &str, home: &str, toml: &str, runtime_path: &str) -> Vec<(String, String)> {
        let tail = if runtime_path.is_empty() { system_paths::SYSTEM_PATH } else { runtime_path };
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

    fn attribute_argv(batcher: String, round_vm: &Path, run: &Path, leg: &Leg) -> Vec<String> {
        let mut argv = vec![batcher, "attribute".into(), leg.repo.clone(), "--members".into(), batcher_cut::attribute::members_arg(&leg.members), "--wt".into(), leg.wt.clone(), "--base".into(), leg.base.clone(), "--round-vm".into(), round_vm.display().to_string()];
        if !leg.batch.is_empty() {
            argv.extend(["--batch".into(), leg.batch.clone()]);
        }
        if !leg.reds.is_empty() {
            argv.extend(["--suites".into(), leg.reds.join(",")]);
        }
        if leg.confirmed {
            argv.push("--confirmed".into());
        }
        if leg.after_certify && !leg.batch.is_empty() {
            argv.extend(["--after-pass".into(), batcher_cut::io::round_results_dir(run, &leg.batch).display().to_string()]);
        } else if !leg.after_pass.is_empty() {
            let pass = Path::new(&leg.after_pass);
            let pass = if pass.is_relative() { run.parent().unwrap_or(run).join(pass) } else { pass.to_path_buf() };
            argv.extend(["--after-pass".into(), pass.display().to_string()]);
        }
        argv
    }

    /// `batcher attribute` on the red round: `drive::attribute_round` as one captured leg.
    pub fn attribute_for(task: &str, e: &Effects, env: &Env, leg: &Leg) -> Result<String, String> {
        let argv = attribute_argv(spira_config::release_env::tool("batcher")?, &env.round_vm, &env.run, leg);
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
        let units = e.units.as_deref();
        for name in reg.names().into_iter().filter(|n| reg.land(n) == "queue.local" && (enabled || n != reg.home_repo())) {
            let repo = batcher_cut::find_repo(&env, &name)?;
            let mut seen = observe_repo(&env, &repo, units)?;
            seen.live_legs = live_legs(e);
            seen.scoped_batches = scoped_batches(e, &name, &carried);
            seen.images_present = carried.repos.get(&name).map(|rc| super::images_held(rc, &host_holds_image)).unwrap_or_default();
            (seen.settled, seen.landed) = carried.repos.get(&name).map(|rc| settled_batches(&env, rc, &seen)).unwrap_or_default();
            snap.repos.insert(name.clone(), seen);
        }
        snap.registry_landed = registry_landed(&env);
        json(&snap)
    }

    /// The check round-vm makes of the host store before it places a round there.
    pub fn host_holds_image(tag: &str) -> bool {
        spira_config::bounded::bounded("podman").args(round_vm::host::image_exists_args(tag)).output().is_ok_and(|o| o.status.success())
    }

    /// An unreadable unit is an error, never "not running": its checkout may be in use.
    fn unit_running(e: &Effects, task: &str, key: &str) -> Result<bool, String> {
        use runtime::units::{UnitFacts, UnitHandle, UnitState};
        let units = e.units.as_deref().ok_or("no unit manager configured")?;
        let handle = UnitHandle::of(task, key).map_err(|e| e.0)?;
        let facts = runtime::units::effect(units, runtime::units::UNIT_FACTS, &json(&handle)?)?;
        let facts: UnitFacts = serde_json::from_str(&facts).map_err(|e| format!("unit facts of {key}: {e}"))?;
        Ok(matches!(facts.state, UnitState::Active | UnitState::Activating))
    }

    pub fn image_tree(run: &Path, repo: &str, tag: &str) -> PathBuf {
        run.join("worktree").join(format!(".image-{repo}-{tag}"))
    }

    /// Cuts the build's checkout and starts the build as its own unit, a scope so that what it
    /// wrote comes back on its `unit.ran`. A build of the tag already running is left alone.
    pub fn start_image_build(e: &Effects, repo: &batcher_cut::io::Repo, run: &Path, testenv: String, vars: Vec<(String, String)>, leg: &Leg) -> Result<String, String> {
        if unit_running(e, super::NAME, &leg.unit)? {
            return Ok("{}".into());
        }
        let tree = image_tree(run, &leg.repo, &leg.image);
        io::worktree_remove(repo, &tree)?;
        io::worktree_reset(repo, &tree, &leg.head)?;
        let argv = vec![testenv, "container".into(), "image".into(), "--tree".into(), tree.display().to_string()];
        start_scope(e, super::NAME, &leg.unit, argv, vars, None)
    }

    pub fn image_build(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        start_image_build(e, &repo_of(&env, &leg)?, &env.run, spira_config::release_env::tool("testenv")?, home_vars(&env), &leg)
    }

    pub fn remove_image_tree(e: &Effects, repo: &batcher_cut::io::Repo, run: &Path, leg: &Leg) -> Result<String, String> {
        if !unit_running(e, super::NAME, &leg.unit)? {
            io::worktree_remove(repo, &image_tree(run, &leg.repo, &leg.image))?;
        }
        Ok("{}".into())
    }

    pub fn image_clean(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        remove_image_tree(e, &repo_of(&env, &leg)?, &env.run, &leg)
    }

    /// The batches of `rc`'s leg-driven rounds that the queue no longer holds open and the
    /// lifecycle reports LANDED or ABANDONED; a batch the store cannot say is not settled.
    fn settled_batches(env: &Env, rc: &super::RepoCarry, seen: &Repo) -> (Vec<String>, Vec<String>) {
        let settled: Vec<(String, bool)> = rc.rounds()
            .into_iter()
            .filter(|r| super::lifecycle_driven(r.state))
            .filter_map(|r| r.batch)
            .filter(|b| !seen.open_rounds().any(|o| &o.id == b))
            .filter_map(|b| {
                let shown = io::lcq_held(env, &["show-batch", &b]).ok().and_then(|out| serde_json::from_str::<serde_json::Value>(&out).ok());
                match shown.as_ref().and_then(|v| v.get("state")).and_then(|s| s.as_str()) {
                    Some("LANDED") => Some((b, true)),
                    Some("ABANDONED") => Some((b, false)),
                    _ => None,
                }
            })
            .collect();
        let landed = settled.iter().filter(|(_, l)| *l).map(|(b, _)| b.clone()).collect();
        (settled.into_iter().map(|(b, _)| b).collect(), landed)
    }

    /// The beads the registry's rows name that the lifecycle reports LANDED; empty, said on stderr, when either read fails.
    fn registry_landed(env: &Env) -> Vec<String> {
        if env.test_registry_bin.is_none() {
            return vec![];
        }
        let ids = match batcher_cut::io::registry_row_beads(env) {
            Ok(ids) => ids,
            Err(why) => {
                eprintln!("{}: cannot read the registry's rows: {why}", super::NAME);
                return vec![];
            }
        };
        match batcher_cut::io::lc_rows_of(env, &ids) {
            Ok(rows) => rows.iter().filter(|r| r.get("state").and_then(|s| s.as_str()) == Some("LANDED")).filter_map(|r| r.get("id").and_then(|i| i.as_str())).map(String::from).collect(),
            Err(why) => {
                eprintln!("{}: cannot read the lifecycle state of the registry's beads: {why}", super::NAME);
                vec![]
            }
        }
    }

    /// The keys of this task's leg units the manager lists as running; `None` when it cannot say.
    fn live_legs(e: &Effects) -> Option<Vec<String>> {
        let units = e.units.as_deref()?;
        let listed = runtime::units::effect(units, runtime::units::UNIT_LIST, &json(&runtime::units::task_glob(super::NAME)).ok()?).ok()?;
        let all: Vec<runtime::units::UnitDetail> = serde_json::from_str(&listed).ok()?;
        Some(super::running_legs(&all))
    }

    /// Of the carried batches, those with a pass running; `None` when the manager cannot say.
    /// A scope whose pass has exited is stopped here, said on stderr, so a daemon left in it
    /// cannot hold the round's certify.
    fn scoped_batches(e: &Effects, repo: &str, carried: &super::Carry) -> Option<Vec<String>> {
        scoped_batches_in(e.units.as_deref()?, repo, carried)
    }

    pub fn scoped_batches_in(units: &dyn runtime::units::Units, repo: &str, carried: &super::Carry) -> Option<Vec<String>> {
        let mut live = vec![];
        for batch in carried.repos.get(repo)?.rounds().into_iter().filter_map(|r| r.batch) {
            match runtime::units::stop_ended_pass_scopes(units, repo, &batch) {
                Ok(lines) => lines.iter().for_each(|l| eprintln!("batcher.local: {repo} {batch}: {l}")),
                Err(why) => eprintln!("batcher.local: {repo} {batch}: cannot stop an ended pass scope: {why}"),
            }
            if runtime::units::round_scope_live(units, repo, &batch).ok()? {
                live.push(batch);
            }
        }
        Some(live)
    }

    /// Whether the manager runs round-recert's driver unit for `batch`, bare or under a lease token.
    /// A manager that errs reads as driven: no land is started on a guess; with none, undriven.
    pub fn recert_driven(units: Option<&dyn runtime::units::Units>, batch: &str) -> bool {
        let Some(units) = units else { return false };
        let bare = release::round_recert::unit_for(batch);
        [format!("{bare}.service"), format!("{bare}-*.service")].iter().any(|glob| units.list(glob).map_or(true, |l| super::recert_live(&l)))
    }

    /// The queue's open round as the task sees it, with what only the host can say about it.
    pub fn round_of(r: io::OpenRound, driven: bool, unfit: Option<String>, gate_red: Vec<(String, String, String)>) -> Round {
        Round { id: r.id, members: r.members, tips: r.tips, head: r.head, base: r.base, worktree: r.worktree, driven, red: r.red, red_suites: r.red_suites, ejected: r.ejected, ejected_by: r.ejected_by, age_secs: r.age_secs, unfit, phase: r.phase, gate_red }
    }

    fn observe_repo(env: &Env, repo: &batcher_cut::io::Repo, units: Option<&dyn runtime::units::Units>) -> Result<Repo, String> {
        let sifted = io::sifted_tips(env)?;
        let (certified, left_out) = io::certified_pool_left_out(env, repo)?;
        let pool: Vec<Member> = certified.into_iter().map(|m| { let sifted = sifted.get(&m.id).cloned(); Member::read(m, sifted) }).collect();
        let blockers = pool.iter().flat_map(|m| m.deps.clone()).collect();
        let branch_tips = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
        let mut open = io::open_rounds(env, repo)?.into_iter().map(|r| {
            let driven = io::certify_driven(env, &repo.name, &r.id) || recert_driven(units, &r.id);
            let unfit = io::round_certifiable(env, repo, &r.id).err();
            let queue = landing_pass::gateq::GateQueue::new(&env.run);
            let gate_red = with_failing(io::gate_red_rows(env, &r.members), |id, tip| queue.failing_at(&repo.name, &format!("spira/{id}"), tip));
            round_of(r, driven, unfit, gate_red)
        });
        let open_round = open.next();
        let open_above: Vec<Round> = open.collect();
        let (stopped, stop_why, paused, paused_since) = holds(env, &repo.name);
        let pause_evidence = paused.as_deref().filter(|p| !p.starts_with("hold:")).and_then(|_| io::red_test_line(&env.run));
        let cut_hold = cut_hold_in_force(&env.queue_dir, &repo.name);
        let unfit = open_round.as_ref().and_then(|r| r.unfit.clone());
        let base_sha = if open_round.is_some() { io::resolve_base_sha(repo).unwrap_or_default() } else { io::resolve_base_sha(repo)? };
        Ok(Repo { pool, blockers, branch_tips, open_round, open_above, base_sha, stopped, stop_why, cut_hold, paused, paused_since, pause_evidence, unfit, left_out, ..Repo::default() })
    }

    pub fn refuse_if_cut_held(queue_dir: &std::path::Path, repo: &str) -> Result<(), String> {
        match cut_hold_in_force(queue_dir, repo) {
            Some(h) => Err(format!("{repo}: cut held by {h} — no cut until it lifts or its holder ends")),
            None => Ok(()),
        }
    }

    pub fn cut_hold_in_force(queue_dir: &std::path::Path, repo: &str) -> Option<String> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        spira_config::cut_hold::held(queue_dir, repo, now).map(|h| format!("{} ({}) until {}", h.owner, h.reason, h.until))
    }

    /// The repo's holds: the scoped stop of its `rounds` lane (and its reason), and the pace's
    /// pause file's text — each told apart, so the hold the pass names is the one that stands.
    fn holds(env: &Env, repo: &str) -> (bool, Option<String>, Option<String>, Option<u64>) {
        let files = batcher_cut::pace::Files::new(&env.run, repo);
        let stop = spira_config::scoped_stop::stop_for(&env.run, repo, "rounds").map(|s| format!("{}: {} (until: {})", s.owner, s.reason, s.until));
        let paused = files.pause_file().exists().then(|| std::fs::read_to_string(files.pause_file()).unwrap_or_default());
        (stop.is_some(), stop, paused, files.pause_since())
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
        refuse_if_cut_held(&env.queue_dir, &repo.name)?;
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
        let suites = !io::suites_in(&wt, "HEAD")?.is_empty();
        json(&Assembled { merged: combined.merged.iter().map(|m| (m.id.clone(), m.tip.clone())).collect(), own_testenv: batcher_cut::declares_own_testenv(&wt), wt: wt.display().to_string(), base, suites, conflicts, base_conflicts, cleared })
    }

    pub fn round_open(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let repo = repo_of(&env, &leg)?;
        let Some(_lock) = io::try_lock(&env, &repo.name)? else { return Err(format!("{}: another operation holds the lock", repo.name)) };
        io::reap_attribution_branches(super::NAME, &repo);
        match io::round_open_or_refused(&env, &repo, std::path::Path::new(&leg.wt), &core_members(&leg.members))? {
            io::OpenOutcome::Opened(batch) => json(&Opened { batch, ..Opened::default() }),
            io::OpenOutcome::Refused(refused, line) => {
                for r in &refused {
                    eprintln!("{} {}: queue round open refused {} ({}): {} — cutting without it", super::NAME, repo.name, r.id, r.kind, r.why);
                }
                eprintln!("{} {}: {line}", super::NAME, repo.name);
                json(&opened_refusing(refused))
            }
        }
    }

    /// What the machine is told of a refused open.
    pub fn opened_refusing(refused: Vec<io::Refused>) -> Opened {
        let images = refused.iter().filter_map(|r| Some((r.id.clone(), r.image.clone()?))).collect();
        Opened { refused: refused.into_iter().map(|r| (r.id, r.kind, r.why)).collect(), images, ..Opened::default() }
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
            io::try_round_pass_start(&env, &repo, &leg.batch)?;
        }
        let wt = PathBuf::from(&leg.wt);
        let history = std::fs::read_to_string(tsd::family_path(&env.run, "suite-timing")).unwrap_or_default();
        let suites = batcher_cut::order::longest_first(&io::suites_in(&wt, "HEAD")?, &history).list;
        let results = super::round_results(&leg.wt, &leg.unit);
        let argv = ["timeout", "-k", "10", &env.wall_secs.to_string()].iter().map(|s| s.to_string()).chain([env.round_vm.display().to_string(), "run".into(), wt.display().to_string(), "--suites".into(), suites.join(","), "--maxpar".into(), env.maxpar.to_string(), "--results-dir".into(), results.display().to_string(), "--base".into(), repo.base.clone()]).collect();
        start_unit(e, task, &leg.unit, argv, home_vars(&env), None)
    }

    /// A round staged behind the open one: `queue round stage`, as a unit.
    pub fn stage(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let file = env.run.join(format!("round-stage-{}.members", leg.unit));
        std::fs::write(
            &file,
            leg.members
                .iter()
                .map(|(i, t)| format!("{i}:{t}\n"))
                .collect::<String>(),
        )
        .map_err(|e| format!("{}: {e}", file.display()))?;
        let argv = vec![
            env.queue_bin.display().to_string(),
            "round".into(),
            "stage".into(),
            leg.repo.clone(),
            "--members-file".into(),
            file.display().to_string(),
            "--worktree".into(),
            leg.wt.clone(),
            "--by".into(),
            "batcher".into(),
        ];
        start_unit(e, super::NAME, &leg.unit, argv, home_vars(&env), None)
    }

    /// The staged round's own pass: `queue round stage-test`, as a unit.
    pub fn stage_test(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let argv = vec![
            env.queue_bin.display().to_string(),
            "round".into(),
            "stage-test".into(),
            leg.repo.clone(),
            "--by".into(),
            "batcher".into(),
        ];
        start_unit(e, super::NAME, &leg.unit, argv, home_vars(&env), None)
    }

    /// Records the red tip, then ejects: a refused eject still keeps the tip out of the next cut.
    pub fn record_then_eject(run: &Path, repo: &str, id: &str, tip: &str, eject: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        batcher_cut::redtip::record(run, repo, id, tip);
        eject()
    }

    pub fn eject(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        if let Some(held) = &leg.held {
            io::round_eject_held(&env, &repo_of(&env, &leg)?, &leg.batch, held)?;
            return Ok("{}".into());
        }
        if leg.bead.is_empty() {
            // The guilty of a red certify: the members whose files its output names.
            let repo = repo_of(&env, &leg)?;
            let text = std::fs::read_to_string(&leg.why).map_err(|e| format!("{}: {e}", leg.why))?;
            let changed: Vec<(String, Vec<String>)> = leg.members.iter().map(|(id, tip)| (id.clone(), io::changed_paths(&repo, &leg.base, tip))).collect();
            let guilty = io::implicated(&text, &changed);
            for id in &guilty {
                let tip = leg.members.iter().find(|(m, _)| m == id).map_or("", |(_, t)| t.as_str());
                record_then_eject(&env.run, &leg.repo, id, tip, || io::round_eject(&env, &repo, &leg.batch, id, &io::ci_red_suites(std::path::Path::new(&leg.wt)), &[], true))?;
            }
            return Ok(serde_json::json!({ "ejected": guilty }).to_string());
        }
        let suites = if leg.reds.is_empty() { io::ci_red_suites(std::path::Path::new(&leg.wt)) } else { leg.reds.clone() };
        record_then_eject(&env.run, &leg.repo, &leg.bead, &leg.head, || io::round_eject_sole_red(&env, &repo_of(&env, &leg)?, &leg.batch, &leg.bead, &suites, &leg.why))?;
        Ok("{}".into())
    }

    pub fn eject_gate_red(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let repo = repo_of(&env, &leg)?;
        for (i, id) in leg.bead.split(',').filter(|s| !s.is_empty()).enumerate() {
            io::round_eject_gate_red(&env, &repo, &leg.batch, id, leg.reds.get(i).map_or("", String::as_str))?;
        }
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
        let argv = super::certify_argv(&env.queue_bin.display().to_string(), &leg);
        start_scope(e, super::NAME, &leg.unit, argv, home_vars(&env), None)
    }

    pub fn lc_certify(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        io::lc_certify_round(&env, &leg.members, &leg.round_key);
        Ok("{}".into())
    }

    /// The base `batch`'s record names, which a base-moved refusal rebuilt the round on.
    pub fn rebuilt_base(rounds: &[io::OpenRound], batch: &str) -> Result<String, String> {
        rounds.iter().find(|r| r.id == batch).map(|r| r.base.clone()).filter(|b| !b.is_empty()).ok_or_else(|| no_rebuilt_base(batch))
    }

    fn no_rebuilt_base(batch: &str) -> String {
        format!("round {batch} was refused base-moved and its record carries no base")
    }

    /// [`rebuilt_base`] of the records read now. `views` is read afresh because a watched
    /// projection may not yet hold what `round land` has just written, and would not refuse.
    pub fn rebuilt_record_base(views: &store_view::Projection, repo: &str, batch: &str) -> Result<String, String> {
        let view = views.refresh_batches().and_then(|()| views.round_view(repo)).map_err(|e| format!("{}: {e}", no_rebuilt_base(batch)))?;
        rebuilt_base(&view.value.rounds.iter().map(|r| io::open_round_of(r, 0)).collect::<Vec<_>>(), batch)
    }

    /// `queue round land`, retried while another operation holds the lock; a refusal is an `Err`.
    pub fn land(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let repo = repo_of(&env, &leg)?;
        let run = io::round_land(&env, &repo, &leg.batch)?;
        if io::land_base_moved(run.code) {
            eprintln!("{} {}: queue round land refused base-moved for {} ({}) — certifying it again", super::NAME, repo.name, leg.batch, run.refusal);
            // A private projection: refreshing the shared watched one would drop its held bead rows.
            let views = spira_store::lc_call::LcClient::configured().and_then(store_view::Projection::configured).map_err(|e| format!("{}: {e}", no_rebuilt_base(&leg.batch)))?;
            return json(&Landed { base_moved: Some(rebuilt_record_base(&views, &repo.name, &leg.batch)?), ..Landed::default() });
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
        if leg.bead == super::ATTRIBUTION_NOT_CONVERGING {
            eprintln!("batcher.local {}: abandoning round {}: attribution did not converge: {}", leg.repo, leg.batch, leg.why);
            io::round_abandon_considered(&env, &repo_of(&env, &leg)?, &leg.batch, &leg.why, super::ATTRIBUTION_NOT_CONVERGING, &leg.why);
            return Ok("{}".into());
        }
        let why = io::keep_red_evidence(&env.run, &leg.batch, &leg.why, &leg.reds, spira_config::process::cfg_parse::<u32>("SPIRA_SENDBACK_KEEP_DAYS"));
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

    /// A round landed: the repo's open local-red incidents are over, and the landed head names the close.
    pub fn supersede_reds(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let repo = repo_of(&env, &leg)?;
        let head = io::resolve_base_sha(&repo)?;
        batcher_cut::io::close_superseded_reds(&repo, &head);
        Ok("{}".into())
    }

    /// Clears the registry rows of beads the lifecycle reports LANDED; a failure leaves the rows for the next tick.
    pub fn clear_landed(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let ids: Vec<String> = leg.members.iter().map(|(id, _)| id.clone()).collect();
        let cleared = batcher_cut::io::clear_landed_rows(&env, &ids)?;
        if !cleared.is_empty() {
            println!("{}: re-drove the registry clearing for {}: cleared {}", super::NAME, ids.join(" "), cleared.join(" "));
        }
        Ok("{}".into())
    }

    /// Notes each bead the cut left out, once per (cause, tip), as the legacy cut does.
    pub fn note_left_out(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        let left: Vec<batcher_cut::io::LeftOut> = serde_json::from_str(&leg.why).map_err(|e| format!("{}: bad list: {e}", spec.name))?;
        match batcher_cut::io::announce_left_out(&env, &repo_of(&env, &leg)?, &left) {
            0 => Ok("{}".into()),
            n => Err(format!("{n} left-out note(s) failed; they are tried again")),
        }
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

    /// Files the unattended-pause ask through the alarms delivery path, as the floor ask is filed.
    pub fn pause_ask(e: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let d = crate::tasks::b::incident_alarms::Delivery::configured(&crate::tasks::b::lc(e), &e.world.run_dir)?;
        crate::tasks::b::incident_alarms::send(&d, &spec.input)
    }

    /// The pace's hold: the pause file on a red, the scoped stop on repeated infra faults.
    pub fn pause(_: &Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
        let (env, leg) = (env()?, leg(spec)?);
        apply_pause(&env.run, &leg, spira_config::vtime::now_epoch())?;
        Ok("{}".into())
    }

    fn apply_pause(run: &Path, leg: &Leg, now: u64) -> Result<(), String> {
        let files = batcher_cut::pace::Files::new(run, &leg.repo);
        if leg.bead == "stop" {
            files.stop_rounds(&leg.why, now)?;
        } else if leg.bead == "lift" {
            // A landing past the base the red round's pause was seen on: the pause is the loop's
            // own and ends here; a person's `hold:` is left as it stands.
            match std::fs::read_to_string(files.pause_file()) {
                Ok(text) if !text.starts_with("hold:") => {
                    files.lift_pause("batcher", &leg.why, now)?;
                    eprintln!("{} {}: {}", super::NAME, leg.repo, leg.why);
                }
                _ => {}
            }
        } else {
            files.set_pause(&leg.why, "batcher", &leg.why, now)?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod cut_hold_tests {
        use super::*;

        #[test_id(thxfzr0)]
        #[test]
        fn a_cut_held_by_a_live_holder_is_refused_naming_it_and_a_lifted_one_is_not() {
            let q = testkit::TempDir::new("cut-hold");
            assert_eq!(refuse_if_cut_held(q.path(), "spira"), Ok(()));
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            let hold = spira_config::cut_hold::CutHold { owner: "concierge".into(), reason: "hand-landing".into(), pid: std::fs::read_link(format!("{}/self", system_paths::PROC)).unwrap().to_string_lossy().parse().unwrap(), since: now, until: now + 600 };
            spira_config::cut_hold::place(q.path(), "spira", &hold).unwrap();
            let err = refuse_if_cut_held(q.path(), "spira").unwrap_err();
            assert!(err.contains("concierge") && err.contains("hand-landing"), "{err}");
            assert_eq!(refuse_if_cut_held(q.path(), "other"), Ok(()), "another repo is not held");
        }
    }

    #[cfg(test)]
    mod pause_tests {
        use super::*;

        fn landing(run: &Path) -> String {
            std::fs::read_to_string(run.join("landing.log")).unwrap_or_default()
        }

        fn lift_leg(why: &str) -> Leg {
            Leg { why: why.into(), bead: "lift".into(), ..Leg::on("spira") }
        }

        #[test_id(tlty2j3)]
        #[test]
        fn the_batchers_own_set_and_lift_each_record_the_batcher_as_actor() {
            let d = testkit::TempDir::new("pause-effect");
            let set = Leg { why: "local corpus red".into(), ..Leg::on("spira") };
            apply_pause(d.path(), &set, 1000).unwrap();
            let p = d.path().join("rounds-pace/spira/pause");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "local corpus red");
            assert!(landing(d.path()).contains("QUEUE ROUND-PAUSE 1000 repo=spira by=batcher kind=red reason=local corpus red"));
            apply_pause(d.path(), &lift_leg("landed past base"), 1090).unwrap();
            assert!(!p.exists());
            assert!(landing(d.path()).contains("QUEUE ROUND-PAUSE-LIFT 1090 repo=spira by=batcher held_s="), "{}", landing(d.path()));
        }

        #[test_id(t7w8ce6)]
        #[test]
        fn a_lift_leaves_a_hold_in_place_and_records_nothing() {
            let d = testkit::TempDir::new("pause-effect-hold");
            let p = d.path().join("rounds-pace/spira/pause");
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "hold:operator working").unwrap();
            apply_pause(d.path(), &lift_leg("landed past base"), 1090).unwrap();
            assert!(p.exists());
            assert_eq!(landing(d.path()), "");
        }
    }

    #[cfg(test)]
    mod attribute_argv_tests {
        use super::*;

        fn after_pass(leg: &Leg) -> Option<String> {
            let argv = attribute_argv("batcher".into(), Path::new("rv"), Path::new("var/spira-run"), leg);
            argv.iter().position(|a| a == "--after-pass").map(|i| argv[i + 1].clone())
        }

        #[test_id(t6tjdz8)]
        #[test]
        fn an_attribution_after_a_certify_reads_the_batchs_kept_results_under_the_run_dir() {
            let leg = Leg { batch: "r9".into(), after_certify: true, ..Leg::on("spira") };
            assert_eq!(after_pass(&leg).as_deref(), Some("var/spira-run/rounds/r9.results"));
        }

        #[test_id(tp589mp)]
        #[test]
        fn an_attribution_after_a_relative_pass_reads_it_under_the_runs_parent() {
            let leg = Leg { batch: "r9".into(), after_pass: super::super::super::kept_results_file("r9"), ..Leg::on("spira") };
            assert_eq!(after_pass(&leg).as_deref(), Some("var/run/rounds/r9.results"));
        }

        #[test_id(tt82x57)]
        #[test]
        fn a_confirmed_attribution_passes_confirmed_and_an_ordinary_one_does_not() {
            let argv = |confirmed| attribute_argv("batcher".into(), Path::new("rv"), Path::new("r"), &Leg { confirmed, ..Leg::on("spira") });
            assert!(argv(true).contains(&"--confirmed".to_string()));
            assert!(!argv(false).contains(&"--confirmed".to_string()));
        }

        #[test_id(t8dc3e4)]
        #[test]
        fn an_attribution_after_a_certify_with_no_batch_names_no_prior() {
            let leg = Leg { after_certify: true, ..Leg::on("spira") };
            assert_eq!(after_pass(&leg), None);
        }

        #[test_id(ti9cksg)]
        #[test]
        fn an_attribution_after_a_corpus_pass_reads_that_passs_dir_and_a_fresh_one_reads_none() {
            let root = testkit::TempDir::new("attribute-argv-corpus-pass");
            let pass = format!("{}/w/.round-results-u", root.display());
            let leg = Leg { batch: "r9".into(), after_pass: pass.clone(), ..Leg::on("spira") };
            assert_eq!(after_pass(&leg).as_deref(), Some(pass.as_str()));
            assert_eq!(after_pass(&Leg::on("spira")), None);
        }
    }

    #[cfg(test)]
    mod saved_round_tests {
        use super::*;
        use crate::tasks::a::batcher_local::{decide, on_ran_with, Carry, RepoCarry, State, ABANDON, CERTIFY, LEG_START_GRACE_MS};
        use batcher_cut::attribute::Attribution;
        use batcher_cut::replay_run::{bare_members, Fixture, ReplayEdge};
        use runtime::units::{Exit, RanOutput, UnitHandle, UnitRan};

        const NOW: u64 = 1_793_000_000_000;
        const LEG: &str = "spira.a2";

        fn fixture(id: &str, scratch: &testkit::TempDir) -> Fixture {
            Fixture::load(&testkit::manifest_dir().join("fixtures/rounds").join(id), scratch.path()).unwrap()
        }

        /// The attribution leg the queue would start on the saved round, as its argv and its outcome line.
        fn attribute(fx: &Fixture) -> (Leg, String, ReplayEdge<'_>) {
            let leg = Leg {
                repo: "spira".into(),
                key: LEG.into(),
                members: fx.members(),
                batch: fx.round.batch.clone(),
                base: fx.round.base.clone(),
                wt: "w".into(),
                reds: fx.round.red_suites.clone(),
                after_pass: fx.results().display().to_string(),
                ..Leg::default()
            };
            let argv = attribute_argv("batcher".into(), Path::new("rv"), Path::new("run"), &leg);
            let o = batcher_cut::opts::parse(&argv).unwrap();
            let given = batcher_cut::attribute::parse_members(o.members.as_deref().unwrap()).unwrap();
            let mut edge = ReplayEdge::new(fx, o.confirmed);
            let budget = batcher::attrib::Budget::with_default(2, None);
            let line = batcher_cut::judge::attribute(&o, &mut edge, bare_members(&given), Ok(fx.round.tree_suites.clone()), "", budget).unwrap();
            (leg, line, edge)
        }

        fn ids(pairs: &[(String, String)]) -> Vec<String> {
            pairs.iter().map(|(i, _)| i.clone()).collect()
        }

        /// The queue's record of the round once the batcher's own ejects are in it.
        fn record(fx: &Fixture, ejected: &[String], head: &str) -> Round {
            let kept: Vec<(String, String)> = fx.members().into_iter().filter(|(i, _)| !ejected.contains(i)).collect();
            Round {
                id: fx.round.batch.clone(),
                members: ids(&kept),
                tips: kept.iter().map(|(_, t)| t.clone()).collect(),
                head: head.into(),
                worktree: "w".into(),
                phase: "attributing".into(),
                ejected: ejected.to_vec(),
                ejected_by: ejected.iter().map(|_| batcher_cut::io::BATCHER_ACTOR.to_string()).collect(),
                ..Round::default()
            }
        }

        fn replay(id: &str) {
            let scratch = testkit::TempDir::new("saved-round");
            let fx = fixture(id, &scratch);
            let (_, line, edge) = attribute(&fx);
            let done = edge.done.borrow();
            assert_eq!(done.ejected, fx.expect.ejected, "{line}");
            let Some(Attribution::Land { members, head, .. }) = Attribution::parse(&line) else { panic!("not a land: {line}") };
            assert_eq!(ids(&members), fx.expect.survivors, "{line}");
            assert_eq!(head, done.head);

            let rc = RepoCarry { state: State::Attributing, round_key: Some("k".into()), batch: Some(fx.round.batch.clone()), members: fx.members(), wt: "w".into(), base: fx.round.base.clone(), leg: Some(LEG.into()), leg_at_ms: NOW, leg_head: fx.round.cut_head.clone(), head: fx.round.cut_head.clone(), run: 1, ..RepoCarry::default() };
            let carry = Carry { repos: [("spira".to_string(), rc)].into() };
            let at = |now_ms| Snapshot {
                repos: [("spira".to_string(), Repo { base_sha: fx.round.base.clone(), open_round: Some(record(&fx, &done.ejected, &done.head)), live_legs: Some(vec![LEG.into()]), ..Repo::default() })].into(),
                now_ms,
                ..Snapshot::default()
            };
            let (next, acts) = decide(&carry, &at(NOW));
            let (later, more) = decide(&next, &at(NOW + LEG_START_GRACE_MS + 15_000));
            for a in acts.iter().chain(more.iter()) {
                assert!(a.effect != ABANDON && a.effect != runtime::units::UNIT_STOP, "{a:?}");
            }
            assert_eq!((later.repos["spira"].state, later.repos["spira"].leg.as_deref()), (State::Attributing, Some(LEG)));

            let ran = UnitRan { handle: UnitHandle::of("batcher.local", LEG).unwrap(), exit: Exit::Code(0), duration_ms: 1 };
            let out = RanOutput::captured(format!("{line}\n").as_bytes(), b"");
            let (ended, mut certify) = on_ran_with(&later, &ran, &out, NOW + LEG_START_GRACE_MS + 30_000).unwrap();
            certify.extend(decide(&ended, &at(NOW + LEG_START_GRACE_MS + 31_000)).1);
            let certify = certify.iter().find(|a| a.effect == CERTIFY).unwrap_or_else(|| panic!("no certify: {certify:?}"));
            let asked: Leg = serde_json::from_str(&certify.input).unwrap();
            assert_eq!((ids(&asked.members), asked.head), (fx.expect.survivors.clone(), done.head.clone()));
        }

        #[test_id(teo297c)]
        #[test]
        fn a_leg_started_with_four_members_of_a_record_of_three_ejects_only_the_one_a_mender_returned() {
            let scratch = testkit::TempDir::new("saved-round");
            let fx = fixture("spira-20261124T101422Z", &scratch);
            assert_eq!(fx.members().len(), 4);
            let (_, line, edge) = attribute(&fx);
            let Some(Attribution::Land { members, .. }) = Attribution::parse(&line) else { panic!("not a land: {line}") };
            assert_eq!(edge.done.borrow().ejected, ["sp-zz4u61"], "{line}");
            assert_eq!(ids(&members), ["sp-8amflc", "sp-3t2mtn"], "{line}");
        }

        #[test_id(tb9ljet)]
        #[test]
        fn the_same_round_with_no_mending_rows_probes_only_the_members_the_record_holds() {
            let scratch = testkit::TempDir::new("saved-round");
            let fx = fixture("spira-20261124T101422Z-no-mending", &scratch);
            let (_, line, edge) = attribute(&fx);
            assert_eq!(edge.done.borrow().ejected, fx.expect.ejected, "{line}");
            assert!(!edge.done.borrow().ejected.contains(&"sp-k3il1u.1".to_string()), "the member the record no longer holds is not charged: {line}");
        }

        #[test_id(tyalr6i)]
        #[test]
        fn a_saved_round_whose_unit_tests_went_red_on_one_member_ejects_it_and_certifies_the_rest() {
            replay("spira-20261121T062659Z");
        }

        #[test_id(tv6lz8m)]
        #[test]
        fn a_saved_round_with_one_member_ejected_among_three_keeps_its_leg_and_certifies_two_at_the_rebuilt_head() {
            replay("spira-20261122T015849Z");
        }

        #[test_id(t0cdpux)]
        #[test]
        fn a_saved_round_whose_shell_reds_were_overwritten_still_ejects_the_one_member_and_certifies_the_rest() {
            replay("spira-20261122T024932Z");
        }

        #[test_id(tghak6b)]
        #[test]
        fn a_saved_round_whose_lint_named_members_ejects_every_one_it_named() {
            replay("spira-20261122T065604Z");
        }
    }

    #[cfg(test)]
    mod recert_tests {
        use crate::tasks::a::batcher_local::{edge::recert_driven, recert_live};
        use runtime::units::{ScriptedUnits, UnitDetail, UnitFacts, UnitState};

        fn manager_with(unit: &str, state: UnitState) -> ScriptedUnits {
            let u = ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0)));
            u.install(unit, UnitFacts { state, main_pid: Some(7), exit: None, since_ms: Some(1) });
            u
        }

        #[test_id(th0cinh)]
        #[test]
        fn a_bare_round_recert_unit_named_with_its_service_suffix_reads_driven() {
            let u = manager_with("round-recert-b1.service", UnitState::Active);
            assert!(recert_driven(Some(&u), "b1"));
        }

        #[test_id(td3q7bl)]
        #[test]
        fn a_token_named_round_recert_unit_reads_driven_and_another_batchs_does_not() {
            let u = manager_with("round-recert-b1-abc.service", UnitState::Active);
            assert!(recert_driven(Some(&u), "b1"));
            assert!(!recert_driven(Some(&u), "b2"));
        }

        fn unit(state: UnitState) -> UnitDetail {
            UnitDetail { name: "round-recert-b1.service".into(), state, ..UnitDetail::default() }
        }

        #[test_id(txfb9rl)]
        #[test]
        fn a_round_whose_recert_driver_unit_is_running_reads_driven() {
            assert!(recert_live(&[unit(UnitState::Active)]));
            assert!(recert_live(&[unit(UnitState::Activating)]));
        }

        #[test_id(tkgyffk)]
        #[test]
        fn a_round_whose_recert_driver_unit_is_absent_or_ended_reads_undriven() {
            assert!(!recert_live(&[]));
            for s in [UnitState::Inactive, UnitState::Failed, UnitState::Deactivating, UnitState::Absent] {
                assert!(!recert_live(&[unit(s)]), "{s:?}");
            }
        }
    }

    #[cfg(test)]
    mod green_round_tests {
        use crate::tasks::a::batcher_local::{decide, Carry, Repo, Round, Snapshot, LAND};

        fn lands(driven: bool) -> bool {
            let open = Round { id: "b1".into(), members: vec!["m1".into()], tips: vec!["t1".into()], head: "h1".into(), base: "base".into(), phase: "green".into(), driven, ..Round::default() };
            let repo = Repo { open_round: Some(open), base_sha: "base".into(), ..Repo::default() };
            let snap = Snapshot { repos: [("r".to_string(), repo)].into(), now_ms: 1_000_000, ..Default::default() };
            decide(&Carry::default(), &snap).1.iter().any(|a| a.effect == LAND)
        }

        #[test_id(tifs9gx)]
        #[test]
        fn a_green_round_round_recert_still_drives_is_not_landed_by_the_tick() {
            assert!(!lands(true));
        }

        #[test_id(tl5sxa3)]
        #[test]
        fn a_green_round_with_no_driver_is_adopted_and_landed() {
            assert!(lands(false));
        }
    }

    #[cfg(test)]
    mod failing_tests {
        use super::with_failing;

        #[test_id(t2ykd9w)]
        #[test]
        fn a_gate_red_mark_gains_the_failing_step_and_line_only_where_the_gate_recorded_one() {
            let rows = vec![("a".to_string(), "t1".to_string(), "suites-failed".to_string()), ("b".to_string(), "t2".to_string(), "no-rebase".to_string())];
            let got = with_failing(rows, |id, _| (id == "a").then(|| "failing step: s; failing line: error: x".to_string()));
            assert_eq!(got[0].2, "suites-failed; failing step: s; failing line: error: x");
            assert_eq!(got[1].2, "no-rebase");
        }
    }
}

#[cfg(test)]
mod saved_state_tests {
    use super::{Member, RepoCarry};
    use serde::de::DeserializeOwned;

    fn with_old_field<T: DeserializeOwned + serde::Serialize + Default>(field: &str, value: serde_json::Value) -> T {
        let mut saved = serde_json::to_value(T::default()).unwrap();
        assert!(saved.get(field).is_none(), "{field} is no longer written");
        saved.as_object_mut().unwrap().insert(field.into(), value);
        serde_json::from_value(saved).unwrap_or_else(|e| panic!("a saved record carrying {field} must still load: {e}"))
    }

    #[test_id(tbza0vf)]
    #[test]
    fn a_saved_repo_carry_with_the_retired_preempted_field_still_loads() {
        let _: RepoCarry = with_old_field("preempted", serde_json::json!([["sp-a", "t1"]]));
    }

    #[test_id(tknyajg)]
    #[test]
    fn a_saved_pool_member_with_the_retired_gate_red_flag_still_loads() {
        let _: Member = with_old_field("gate_red", serde_json::json!(true));
    }
}
