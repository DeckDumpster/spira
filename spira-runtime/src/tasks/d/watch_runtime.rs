//! # watch.runtime — contract
//!
//! Replaces `spira-runtime-watch-prod` (`runtime-watch pass --state run/runtime-watch.state --max-files 10`), pattern P1. It files a bump bead when a repo pins a Rust, Node or Python line that upstream no longer supports.
//!
//! ## Tick
//!
//! - Every 6 h (`OnUnitActiveSec=6h`, `OnBootSec=15min`); single-flight.
//!
//! ## Observe
//!
//! `watch.runtime.observe` returns:
//!
//! ```text
//! { today, repos: [{name, path, ref, files: {path → text|absent},
//!   naming: {needle → [file]}}], rust: stable|err, node: eol|err,
//!   python: eol|err, open_bumps: {(repo, runtime) → bead_id} }
//! ```
//!
//! Each part is read from the component that owns it:
//!
//! - **Repos:** the registry (`spira_config::repos`), in-process (`runtime-watch/src/real.rs:169-179`).
//! - **Pin files:** `git ls-tree` / `git show <ref>:<file>` and `git grep -l -F` at the base ref (`real.rs:117-121,181-196`). The git class stays a child.
//! - **Upstream support:** `curl` of four URLs (`real.rs:88-91,110-115,198-208`), parsed by `parse_rust_stable` / `parse_node` / `parse_python` (`lib.rs:104-160`). These are external and stay children (W1 allow-lists curl).
//! - **Open bumps:** store verbs `show <id>` for the ledgered id, else `list --live --repo <name>` filtered by `names_bump` (`real.rs:144-158,210-226`, `lib.rs:77`). Both run over the in-process `LcClient` (S7a). The lifecycle state (`TERMINAL`, `real.rs:92`) is the store's answer.
//!
//! The probe needs each pin's needle before it can grep, so it runs in two phases: pins (`rust_pins` / `node_pins` / `python_pins`, `lib.rs:177-250`), then `naming` for the stale ones. The observe stays one probe; the probe body calls the pin parsers (pure) between the reads.
//!
//! ## Decide
//!
//! - `decide(carry, snap, max_files) -> (Carry, Vec<Bump>)` is today's `pass` (`lib.rs:290-356`) with the `World` reads served from the snapshot. That covers `rust_stale` / `eol_stale`, already-open skip, the `max_files` bound, and the title `bump <runtime> <old pin> to <target>` (`lib.rs:348`).
//! - The body is `body()` (`lib.rs:275-288`).
//! - Carry is today's `--state` ledger, `"<repo>\t<runtime>" → bead id` (`real.rs:123-142`), emitted as `watch.runtime.state` on each successful filing.
//! - Fixtures: `runtime-watch/src/tests.rs`:
//!   - two-behind files once and a second pass files nothing (`:69`)
//!   - stable or grace files nothing (`:86`)
//!   - unreachable is cannot-check (`:97`)
//!   - node EOL (`:105`)
//!   - python floor (`:120`)
//!   - batch bounded (`:127`)
//!   - unlisted line is cannot-check (`:136`)
//!   - parsers (`:144`)
//!   - bump filed outside the ledger is amended, not duplicated (`:168`)
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.runtime.observe` (probe) | host (curl + git) | 120 s (4 × curl 30 s, `real.rs:112`) | — | — |
//! | `watch.runtime.file_bump` | store | 120 s (`real.rs:162`) | `watch.runtime:<repo>:<runtime>:<target>` | a (repo, runtime) whose carried or listed bump is live |
//! | `watch.runtime.metrics` | host | 5 s | `watch.runtime:metrics:<tick>` | — |
//!
//! - **`watch.runtime.file_bump`** files the bead with `--for builder --repo <repo>`, then adds label `LABEL` (`real.rs:228-237`).
//!   - Today this forks `bead.sh file … --body-file <tmp>` and `bead.sh label add` (`real.rs:160-165,231,235`).
//!   - In the task it is the store's file and label verbs. §2 keeps `bead.sh` as an operator tool, so the task does not fork it.
//! - **`watch.runtime.metrics`** pushes `spira_runtime_watch_cannot_check` (gauge) and `spira_runtime_bumps_filed` (count) (`main.rs:59-61`).
//!
//! No inbox line. The summary `runtime-watch: <f> filed, <o> already open, <c> supported, <x> failed, <n> cannot check` and the `cannot check — …` lines (`main.rs:62-75`) go to `run/runtime-watch.log` (unit stdout). That is the log row `spira/watchers:178`, which `watchd notify` reads.
//!
//! The task keeps the `cannot check` lines only, as a `watch.runtime.log` host append to that file with key `watch.runtime:cc:<tag>:<day>`. "Cannot check" must not go silent, and the per-pass summary is dropped.
//!
//! ## Silence
//!
//! An idle tick (every pin supported or already open) emits the probe and one metrics push. There is no state Emit and no log line.
//!
//! ## Replaces
//!
//! - `spira-runtime-watch-prod.service` and `spira-runtime-watch-prod.timer` (`CPUQuota=25%`, `TimeoutStartSec=600`).
//! - Log row `runtime-watch|log|@SPIRA_RUN@/runtime-watch.log` (`spira/watchers:178`) stays as the read contract.
//! - Children:
//!   - `curl` (`real.rs:112`)
//!   - `git` (`real.rs:118`)
//!   - `spira-lc show` / `list` (`real.rs:146,214`)
//!   - `bead.sh file` / `label add` (`real.rs:162`)
//! - Keys pinned: `SPIRA_METRICS_ENDPOINT` (`main.rs:56`), `--persona` (default `builder`, `main.rs:21`), `--max-files` (10).
//!
//! The catalogue row matches the code: curl, git, spira-lc, bead.sh.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Observe mode shadows `file_bump`; the ledger is unchanged, so the bump files after the flip.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use super::pattern;
use crate::watchers::{answered_spec, carried};

pub const NAME: &str = "watch.runtime";
pub const STREAM: &str = "watch.runtime";
pub const TICK: EventKindTag = "watch.runtime.tick";
pub const STATE: EventKindTag = "watch.runtime.state";
pub const OBSERVE: &str = "watch.runtime.observe";
pub const FILE_BUMP: &str = "watch.runtime.file_bump";
pub const METRICS: &str = "watch.runtime.metrics";
pub const LOG: &str = "watch.runtime.log";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE_BUMP, METRICS, LOG];
pub const KEYS: &[&str] = &["SPIRA_METRICS_ENDPOINT", "SPIRA_RUN"];
/// The unit argv (`main.rs:21`); no config key carries them today.
pub const PERSONA: &str = "builder";
pub const MAX_FILES: usize = 10;
/// `OnUnitActiveSec=6h`.
pub const CADENCE_MS: u64 = 6 * 3_600_000;
const OBSERVE_DEADLINE_MS: u64 = 120_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub metrics_endpoint: String,
    pub run: String,
    pub persona: String,
    pub max_files: usize,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { metrics_endpoint: cfg("SPIRA_METRICS_ENDPOINT")?, run: cfg("SPIRA_RUN")?, persona: PERSONA.into(), max_files: MAX_FILES })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    /// path → text, `None` when the file is absent at the ref.
    pub files: BTreeMap<String, Option<String>>,
    /// needle → the files naming it.
    pub naming: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub today: String,
    pub repos: Vec<Repo>,
    /// The upstream answers, each the fetched text or why it could not be read.
    pub rust: Result<String, String>,
    pub node: Result<String, String>,
    pub python: Result<String, String>,
    /// `"<repo>\t<runtime>"` → the live bump bead.
    pub open_bumps: BTreeMap<String, String>,
}

/// The `--state` ledger: `"<repo>\t<runtime>"` → bead id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub ledger: BTreeMap<String, String>,
}

/// Today's `pass` over the snapshot: the bumps to file, the metrics push, the `cannot check` lines.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("watch.runtime: not ported yet")
}

/// The ledger once the filing `spec` succeeded with bead id `outcome`.
pub fn filed(_carry: &Carry, _spec: &EffectSpec, _outcome: &str) -> Carry {
    unimplemented!("watch.runtime: not ported yet")
}

pub struct WatchRuntime {
    pub config: Result<Config, String>,
}

impl WatchRuntime {
    pub fn new(config: Config) -> Self {
        WatchRuntime { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchRuntime { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.runtime/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchRuntime {
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
        if let Some(spec) = answered_spec(view, ev).filter(|s| s.name == FILE_BUMP && ev.kind.tag == EFFECT_SUCCEEDED) {
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            return pattern::commands(STATE, &carry, &filed(&carry, &spec, &ev.kind.body), vec![]);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
