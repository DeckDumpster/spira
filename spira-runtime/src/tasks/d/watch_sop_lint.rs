//! # watch.sop_lint — contract
//!
//! Replaces `spira-sop-lint-prod`. That unit runs `ExecStartPre=world guard`, then `ExecStart=spira/sop-lint-watch.sh`, with stdout and stderr appended to `run/sop-lint.log` and TimeoutStartSec=300. Pattern P1. Part of [[scheduler-consolidation-2026-11-03]].
//!
//! ## Tick
//!
//! - `Timer` every 3600 s, first at boot + 600 s (`OnBootSec=10min`, `OnUnitActiveSec=1h`).
//! - Single-flight.
//! - **World halt.** Today `world guard` exits 1 when the halt stamp is present (`spira-world/src/bin/world.rs:1098-1107`), so the pass is skipped. The task instead reads the halt as a snapshot field and decides nothing while the world is halted.
//!
//! ## Observe
//!
//! `watch.sop_lint.observe` returns a `Snapshot { halted: bool, shelf: Option<BTreeMap<key, text>>, word_cap }`.
//!
//! - **`shelf`.** `None` means the shelf could not be read, which is not the same as an empty shelf (law-a-control-that-cannot-check-must-refuse; `sop/src/logic.rs:470-475`). Today `sop lint` reads it with `bdjson memories` under `bash -c '. lib.sh'` (`sop/src/real.rs:91-103` via the seam at `:20-31`). That read is retried 4 times, with backoff of 1, 2, 4 and 8 s. The target is the store's `content memories` verb through `LcClient` (S7a). `bd` is not called from this task.
//! - **`word_cap`.** It comes from `sop`'s `build_env` (`sop/src/main.rs:428`). The key name is unverified.
//! - **`halted`.** This is the world halt stamp that `spira_world::guard::halt_refusal` reads (`world.rs:1099-1100`).
//!
//! ## Decide
//!
//! `decide(snap) -> Vec<Act>` is pure, and there is no carry. It reuses `sop::logic::lint` (`sop/src/logic.rs:470-503`) over the shelf map, together with `validate` per SOP.
//!
//! The decision follows the script (`spira/sop-lint-watch.sh:10-20`):
//!
//! | Snapshot | Act | Today's exit |
//! |---|---|---|
//! | `halted` | nothing | the pass is skipped by ExecStartPre |
//! | `shelf = None` | file the incident `sop lint could not read the shelf (control cannot check)`, with the lint output as its body | 3 |
//! | lint code ≠ 0 | file the incident `sop lint finding on production shelf`, with the `FAIL  {key}: {violation}` lines plus `sop: {n} of {total} SOP(s) failed — …` as its body (`logic.rs:488-494`) | 1 |
//! | clean | nothing | 0 |
//!
//! **No carry, by parity.** A standing finding re-files every hour, as it does today. The incident machinery deduplicates by reference: the reference is `incident:<slug of title>` (`incident/src/main.rs:429-432`), and an open or unclosed hit is bumped and noted, not re-filed (`incident/src/run.rs:132-142`).
//!
//! **Fixtures:**
//! - `sop-lint-watch.sh --selftest`, with its three cases clean, finding and blind (`:21-33`)
//! - `sop/src/logic/tests.rs:461` (`lint_reports_failures_and_refuses_an_unreadable_shelf`)
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.sop_lint.observe` (probe) | store | 30 s (up to 15 s of read backoff today) | none | — |
//! | `watch.sop_lint.file_incident` | store (library call into `incident`, see §2; today `incident.sh file <title> -`) | 60 s | `watch.sop_lint:incident:<title slug>:<tick at>`. The durable dedup is the incident reference | an activation-skew payload (`run.rs:126-129`). An unreachable store is spooled and retried, not dropped (`run.rs:132-136`; `main.rs:442-453`) |
//!
//! No inbox line is written. `run/sop-lint.log` receives only the incident's own stderr. No reader of that log was found: the only hits in the release are the unit file and forensic run listings in `run/concierge-notes/ci-forensics-*`.
//!
//! ## Silence
//!
//! - A clean shelf or a halted world emits the probe only: no Emit and no effect, because the task carries no state.
//!
//! ## Replaces
//!
//! - **Units:** `spira-sop-lint-prod.service` and `spira-sop-lint-prod.timer`. There is no watchd row.
//! - **Children forked today:**
//!   - `world guard` (ExecStartPre)
//!   - `sop lint` (`sop-lint-watch.sh:12`), which in turn forks `bash -c . lib.sh; bdjson memories` (`sop/src/real.rs:29-31`)
//!   - `incident.sh file` (`sop-lint-watch.sh:9`), which sources `lib.sh` and execs `incident` (`spira/incident.sh:31-36`)
//! - **Config pinned (S10):** `SOP_LINT_SOP` and `SOP_LINT_FILER` are test seams and drop away. Pinned are the sop word cap (key unverified) and the incident filer's `SPIRA_DB` and spool directory.
//!
//! ## Reverse
//!
//! - Release rollback restores the unit.
//! - Observe mode shadows `file_incident`, and the shadowed spec carries the title and body for parity review.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use super::pattern;

pub const NAME: &str = "watch.sop_lint";
pub const STREAM: &str = "watch.sop_lint";
pub const TICK: EventKindTag = "watch.sop_lint.tick";
/// Never emitted: the task carries no state. Named so the shared answer path has a tag.
pub const STATE: EventKindTag = "watch.sop_lint.state";
pub const OBSERVE: &str = "watch.sop_lint.observe";
pub const FILE_INCIDENT: &str = "watch.sop_lint.file_incident";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE_INCIDENT];
/// The incident filer's store. The sop word cap's key is unverified in the contract and not read.
pub const KEYS: &[&str] = &["SPIRA_DB"];
/// `OnUnitActiveSec=1h`.
pub const CADENCE_MS: u64 = 3_600_000;
const OBSERVE_DEADLINE_MS: u64 = 30_000;
pub const BLIND_TITLE: &str = "sop lint could not read the shelf (control cannot check)";
pub const FINDING_TITLE: &str = "sop lint finding on production shelf";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub db: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { db: cfg("SPIRA_DB")? })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    /// `None` is a shelf that could not be read, not an empty one.
    pub shelf: Option<BTreeMap<String, String>>,
    pub word_cap: u64,
}

/// What `file_incident` takes: `incident file <title> -` with `body` on stdin, as a library call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub title: String,
    pub body: String,
}

const FILE_DEADLINE_MS: u64 = 60_000;

/// The shelf as `sop lint` reads it, from the snapshot: `None` is the unreadable shelf.
struct Shelf<'a>(Option<&'a BTreeMap<String, String>>);

impl sop::ports::Bd for Shelf<'_> {
    fn remember(&self, _: &str, _: &str) -> Result<(), String> {
        Err("watch.sop_lint reads the shelf only".into())
    }
    fn recall(&self, key: &str) -> Option<String> {
        self.0?.get(key).cloned()
    }
    fn forget(&self, _: &str) -> bool {
        false
    }
    fn memories_json(&self) -> Option<String> {
        self.0.and_then(|m| serde_json::to_string(m).ok())
    }
    fn note(&self, _: &str, _: &str) -> bool {
        false
    }
}

/// What `sop lint 2>&1` printed and its exit code, from `sop::logic::lint` over `snap`'s shelf.
pub fn lint(snap: &Snapshot) -> (i32, String) {
    let cap = usize::try_from(snap.word_cap).unwrap_or(usize::MAX);
    let r = sop::logic::lint(&Shelf(snap.shelf.as_ref()), cap);
    (r.code, r.out.iter().chain(&r.err).cloned().collect::<Vec<_>>().join("\n"))
}

/// `sop::logic::lint` over the shelf: nothing, file(blind) or file(finding); nothing while halted.
pub fn decide(snap: &Snapshot, _cfg: &Config) -> Vec<EffectSpec> {
    if snap.halted {
        return vec![];
    }
    let (code, out) = lint(snap);
    // `sop-lint-watch.sh:13-18`: the blind refusal is read from the output, before the code.
    let title = if out.contains("could not read the shelf") {
        BLIND_TITLE
    } else if code != 0 {
        FINDING_TITLE
    } else {
        return vec![];
    };
    let input = serde_json::to_string(&Filing { title: title.into(), body: out }).unwrap_or_default();
    vec![EffectSpec::keyed(FILE_INCIDENT, format!("watch.sop_lint:incident:{title}"), input, FILE_DEADLINE_MS)]
}

/// The sop word cap `sop lint` applies when `SOP_WORD_CAP` is unset (`sop/src/main.rs:47`); the
/// key is not registered config, so the default is the one production ran.
pub const WORD_CAP: u64 = 250;

/// The probe: the halt stamp, and the shelf over the store's `content memories --json`. A failed
/// or unparseable read is `shelf: None`, the blind case, never an empty shelf.
pub fn observe(e: &crate::Effects, _key: &str, _spec: &EffectSpec) -> Result<String, String> {
    let halted = e.world.run_dir.join("world.halted").exists();
    let shelf = if halted {
        None
    } else {
        spira_config::lc_call::LcClient::socket(&e.world.lc_socket).answer(&["content", "memories", "--json"]).ok().and_then(|raw| sop::shelf::parse(&raw))
    };
    serde_json::to_string(&Snapshot { halted, shelf, word_cap: WORD_CAP }).map_err(|e| e.to_string())
}

/// The filing, in-process as `incident file <title> -` files it: deduplicated on the incident
/// reference, spooled when the store is down.
pub fn file_incident(_e: &crate::Effects, _key: &str, spec: &EffectSpec) -> Result<String, String> {
    let f: Filing = serde_json::from_str(&spec.input).map_err(|e| format!("{FILE_INCIDENT}: bad input: {e}"))?;
    incident::intake::file_payload(&f.title, f.body.as_bytes())?;
    Ok(format!("filed: {}", f.title))
}

pub struct WatchSopLint {
    pub config: Result<Config, String>,
}

impl WatchSopLint {
    pub fn new(config: Config) -> Self {
        WatchSopLint { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchSopLint { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.sop_lint/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[];

impl Handler for WatchSopLint {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, |_: &(), snap: &Snapshot, _, config: &Config| ((), decide(snap, config)))
    }
}
