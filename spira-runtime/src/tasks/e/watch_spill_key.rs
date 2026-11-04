//! # watch.spill_key — contract
//!
//! watchd row `round-spill-key` (`spira/watchers:157`), target `@SPIRA_HOME@/round-spill-key.sh watch --interval 3600`; pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! Catalogue differs: the children are not just `lib.sh` — each pass may fork `round-vm status --json` (`spira/round-spill-key.sh:64`), `round-vm ec2-probe --dry-run` (`:85`), `round-vm template-check` (`:103`) and `python3 -I` (`:66`). The task watches three conditions, not one: the tailscale key's expiry, whether an EC2 launch can succeed, and whether the local round template exists.
//!
//! ## Tick
//!
//! - Cadence 3600 s (`spira/watchers:157`; loop `:152-168`). No waking events. Single-flight; the probe leg can run minutes (see deadline), so a tick while one is outstanding is dropped.
//!
//! ## Observe
//!
//! `watch.spill_key.observe` returns:
//!
//! ```text
//! Snapshot {
//!   spill: String,
//!   template: Option<Ok | Missing { tail2 }>,                     // only when spill ∉ {ec2-always, ec2-only}
//!   launch: Option<RecentOk | RecentFail { reason } | Probed { rc, out_tail3, took_s }>,  // only when spill ∈ ec2*
//!   key_file: Option<String>, expires: Option<u64>, now: u64
//! }
//! ```
//!
//! - `template` from round-vm's own check: `round-vm template-check` (`:103`), rc 0|3 = ok (`:105`). Library call in-process.
//! - `launch`: first the round-vm pool record, the component that recorded the launches — `SPIRA_ROUND_VM_STATE_DIR/pool.json` (`:60-62`) or `round-vm status --json` when unset (`:64`) — reduced by the embedded Python (`:39-57`) to `ok | fail <reason> | none` within `SPIRA_ROUND_VM_EC2_LAUNCH_WINDOW`. Only on `none` does it run the dry-run probe `round-vm ec2-probe --dry-run` (`:85`, `round-vm/src/ec2.rs:333` `probe_dry`). The Python reduction moves into Rust over round-vm's pool type.
//! - `expires` from `<SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE>.expires` (`:123`), written by the operator who pastes the key. `took_s` feeds the adaptive deadline (`:83`).
//!
//! ## Decide
//!
//! - Carry `watch.spill_key.state` = `{ said: BTreeSet<String>, probe_latency_s: Option<u64> }` — today `run/watchd/round-spill-key.said` (one key per line, `:33-36`) and `run/watchd/round-spill-key.latency` (`:76`, `:88`).
//! - Keys and lines (all said once per key, `_rsk_once` `:32-37`):
//!   - `template-<cksum>` → "ROUND TEMPLATE: the local round pool cannot lease: …" (`:107`); template ok clears `template-*` (`:111-115`).
//!   - `probe-<cksum>` → "ROUND SPILL LAUNCH: a real EC2 launch failed; EC2 rounds cannot launch: …" (`:80`) or "…round-vm ec2-probe --dry-run failed…" (`:97`); `probe-timeout` → "ROUND SPILL LAUNCH: probe could not decide: …" (`:93`). Recent ok or probe ok clears `probe-*` (`:69-73`).
//!   - `nofile` (`:122`), `unrecorded` (`:125`), `expired-<exp>` (`:131`), `warn-<exp>` (`:133`, `SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS`, default 14).
//! - The probe deadline is `max(SPIRA_ROUND_VM_EC2_PROBE_DEADLINE, 4 × last latency)` (`:83`) — decide computes it from carry and passes it in the next observe's input.
//! - Fixtures to port: `spira/test-round-spill-key.sh` (169 lines; tier T1): warn once, expiry, unrecorded, spill off, recent-launch stand-in, probe timeout, template missing.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.spill_key.observe` | host (aws edge) | adaptive probe deadline + 10 s | (probe) | spill off → template leg only |
//! | `inbox.append` (shared) | host | 10 s | `spill_key/<said-key>` | — |
//!
//! Inbox lines, exactly as today (`spira/round-spill-key.sh:28`): `<YYYY-MM-DDTHH:MM:SSZ> [watch:round-spill-key] <message>` with the message texts above. The script writes each line to stdout → `run/watchd/round-spill-key.log` (`:26`) and straight to `SPIRA_CONCIERGE_INBOX` (`:27-28`). Readers of `run/watchd/round-spill-key.log`: none by name in the release or `run/concierge-notes`; watchd's generic walkers only. `run/watchd/round-spill-key.pending` (2026-10-14) shows `watchd notify` once saw an undrained actionable line; no cursor exists. The log copy is dropped.
//!
//! Health (`round-spill-key.sh health`, `:142-150`) → S13 `scheduler status`.
//!
//! ## Silence
//!
//! No key added or removed and latency unchanged: probe only, no `Emit`. A successful dry-run that only moves `probe_latency_s` by under 10 % should not emit either (new rule; today the latency file is rewritten every probe, `:88`).
//!
//! ## Replaces
//!
//! - `spira-watch-round-spill-key-prod.service`; row `spira/watchers:154-157`.
//! - Children: `bash lib.sh` (`:21`), `round-vm status|ec2-probe|template-check` (`:64`, `:85`, `:103`), `python3` (`:66`), `timeout`, `cksum`, `tr`, `grep`.
//! - Files retired: `run/watchd/round-spill-key.{said,latency,health,log,pending,restarts}`.
//! - Config pinned: `SPIRA_ROUND_VM_SPILL`, `SPIRA_ROUND_VM_STATE_DIR`, `SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE` (`spira/conf.d.index:3445`), `SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS` (`:3359`), `SPIRA_ROUND_VM_EC2_LAUNCH_WINDOW` (`:3367`), `SPIRA_ROUND_VM_EC2_PROBE_DEADLINE` (`:3391`).
//!
//! ## Reverse
//!
//! Release rollback restores unit and row; `Observe` mode shadows `inbox.append`.

use std::collections::BTreeSet;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use crate::tasks::d::pattern;
use crate::watchers::{carried, INBOX_APPEND};

pub const NAME: &str = "watch.spill_key";
pub const STREAM: &str = "watch.spill_key";
pub const TICK: EventKindTag = "watch.spill_key.tick";
pub const STATE: EventKindTag = "watch.spill_key.state";
pub const OBSERVE: &str = "watch.spill_key.observe";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_ROUND_VM_SPILL", "SPIRA_ROUND_VM_STATE_DIR", "SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE", "SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS", "SPIRA_ROUND_VM_EC2_LAUNCH_WINDOW", "SPIRA_ROUND_VM_EC2_PROBE_DEADLINE", "SPIRA_CONCIERGE_INBOX"];
/// The row's tag (`spira/round-spill-key.sh:28`).
pub const INBOX_TAG: &str = "round-spill-key";
/// `--interval 3600`.
pub const CADENCE_MS: u64 = 3_600_000;
/// Added to the adaptive probe deadline for the observe as a whole.
const OBSERVE_SLACK_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub spill: String,
    pub state_dir: String,
    pub key_file: String,
    pub warn_days: u64,
    pub launch_window_s: u64,
    pub probe_deadline_s: u64,
    pub inbox: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config {
            spill: cfg("SPIRA_ROUND_VM_SPILL")?,
            state_dir: cfg("SPIRA_ROUND_VM_STATE_DIR")?,
            key_file: cfg("SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE")?,
            warn_days: cfg_parse("SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS")?,
            launch_window_s: cfg_parse("SPIRA_ROUND_VM_EC2_LAUNCH_WINDOW")?,
            probe_deadline_s: cfg_parse("SPIRA_ROUND_VM_EC2_PROBE_DEADLINE")?,
            inbox: cfg("SPIRA_CONCIERGE_INBOX")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Template {
    Ok,
    Missing { tail2: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Launch {
    RecentOk,
    RecentFail { reason: String },
    Probed { rc: i32, out_tail3: String, took_s: u64 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub spill: String,
    /// Only when spill is not `ec2-always` / `ec2-only`.
    pub template: Option<Template>,
    /// Only when spill is an `ec2*` mode.
    pub launch: Option<Launch>,
    pub key_file: Option<String>,
    pub expires: Option<u64>,
    pub now: u64,
}

/// `{ said, probe_latency_s }`: today's `.said` keys and `.latency`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub said: BTreeSet<String>,
    pub probe_latency_s: Option<u64>,
}

/// The probe deadline the next observe gets: `max(PROBE_DEADLINE, 4 × last latency)`, in seconds.
pub fn probe_deadline_s(carry: &Carry, cfg: &Config) -> u64 {
    cfg.probe_deadline_s.max(4 * carry.probe_latency_s.unwrap_or(0))
}

/// Each key family said once; `template-*` and `probe-*` cleared when the condition clears.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("watch.spill_key: not ported yet")
}

pub struct WatchSpillKey {
    pub config: Result<Config, String>,
}

impl WatchSpillKey {
    pub fn new(config: Config) -> Self {
        WatchSpillKey { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchSpillKey { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.spill_key/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchSpillKey {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let next = Command::Timer { at: ev.at + CADENCE_MS, kind: EventKind::new(TICK, "") };
            let config = match &self.config {
                Ok(c) => c,
                Err(e) => {
                    ctx.event().set("config.refused", e.as_str());
                    return vec![next];
                }
            };
            let deadline_s = probe_deadline_s(&carried::<Carry>(view, &ev.stream, STATE), config);
            return pattern::tick(ev, OBSERVE, deadline_s.to_string(), deadline_s * 1000 + OBSERVE_SLACK_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
