//! # inbox.notify — contract
//!
//! This contract replaces the unit `spira-notify-prod`, which runs `watchd notify` (oneshot, `SuccessExitStatus=1`, TimeoutStartSec=120, output appended to `run/watch-notify.log`). Pattern P1. It is part of [[scheduler-consolidation-2026-11-03]].
//!
//! Catalogue differs: the catalogue lists the children as "mail-health.sh, mail, systemctl". The code forks more than that:
//!
//! - `sop match -` and `sop show` (`watchd/src/ops.rs:300-309`)
//! - `sop` applied (`:345-356`)
//! - one `bash -c <health>` for each row with an assertion (`watchd/src/health.rs:25-35`)
//! - `pgrep -f` (`ops.rs:231`)
//! - `systemd-run --user --wait --pipe` for a SOP's RUN tool (`ops.rs:316-342`)
//! - `mail` for the concierge note (`ops.rs:358`)
//!
//! ## Tick
//!
//! - A `Timer` fires every 300 s; the first fire is at boot + 300 s.
//! - Single-flight.
//! - Today every log row is polled. A later wake on `inbox.append` / log-append effects is possible but not required.
//!
//! ## Observe
//!
//! `inbox.notify.observe` returns `Snapshot { now, halted, rows: [RowView], units: {unit→(ActiveState, NRestarts)}, health: {row→Ok|Degraded(why)}, mail_health }`. It reads:
//!
//! - **Manifest rows.** These come from `SPIRA_WATCHERS` plus the overlay (`watchd/src/manifest.rs`). Kind `off` is skipped (`watchd/src/notify.rs:92-95`).
//! - **Per row (`RowView`).** The row's log line count, its cursor, the unread chunk, and its `.pending` (`pos`, `epoch`) and `.unhealthy` stamps (`notify.rs:96-122`, `:242-262`). These are files under `run/watchd/`, which is the log+cursor contract, and they stay.
//! - **Units.** Today: `systemctl --user show -p ActiveState -p NRestarts` for each daemon/extern row (`ops.rs:156-162`, `notify.rs:216-233`). Target: `unit.facts` over D-Bus (S6). Hosted tasks get their health from `scheduler status` (S13), not from units. After W2 no `daemon` rows remain, only `extern` rows (mail-deliver, spira-submission).
//! - **Health.** Each row's health command, run under `SPIRA_HEALTH_TIMEOUT` (`health.rs:25`). It is probed only for rows whose unit is active (`notify.rs:244-255`).
//! - **`halted`.** Whether `run/world.halted` exists (`notify.rs:74-76`).
//! - **`mail_health`.** The exit code of `mail-health.sh`: 0, 1 or 3 (`ops.rs:261-273`). That script is its own check over `SPIRA_MAIL_READERS` and mails the operator itself. It should become its own task `mail.health` (unverified that anyone else wants it); until then it is folded in here as an effect.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Vec<Act>)` is the pure split of `cmd_notify` and `notify_health` (`notify.rs:83-184`, `:216-316`):
//!
//! - **Backlog half.**
//!   - Only actionable lines count; the filter is `SPIRA_ACTIONABLE`.
//!   - The age is the oldest `[ts]` prefix, or else the pending clock.
//!   - A row that has not reached `SPIRA_NOTIFY_AGE` is skipped.
//!   - Each stale row is offered to a SOP first (`sop_run::attempt`). A Handled result drops the row from the ask.
//! - **Health half.**
//!   - Degraded for at least `SPIRA_NOTIFY_AGE` → ask.
//!   - A halted world clears the unhealthy clock instead.
//! - **Ask.**
//!   - At most `NOTIFY_MAX = 12` lines are shown per row (`notify.rs:21`).
//!   - The ask is suppressed while its FNV fingerprint equals the stamp (`notify.rs:27-58`).
//!
//! The carry is `inbox.notify.state = { pending: {row→(pos, at)}, unhealthy: {row→at}, stamp_backlog, stamp_health, sop_runs: {key→[at]} }`. Today these live in the files `<name>.pending`, `<name>.unhealthy`, `notify.escalated`, `notify-health.escalated` and `<name>.sop-runs`.
//!
//! The fixtures are the `notify.rs` tests at `:347-520`: age threshold, own-clock stamp, no re-escalation, refused ask not stamped, dead daemon, halted world, two asks from two stamps, and the three SOP cases. The `sop_run.rs` tests are fixtures too.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `inbox.notify.observe` (probe) | host + unit facts | 60 s (`SPIRA_HEALTH_TIMEOUT` 10 s per health command) | none | — |
//! | `inbox.notify.ask` | store (in-process mail, §2) | 30 s | `inbox.notify:<backlog\|health>:<fingerprint>` | a fingerprint equal to the carried stamp. A refused send leaves the stamp unset, so the next pass asks again (`notify.rs:52-55`) |
//! | `inbox.notify.sop_match` | store (today `sop match -`, 30 s) | 30 s | `inbox.notify:match:<row key>` | — |
//! | `unit.start` (shared) | unit: the SOP's `RUN:` tool, today `systemd-run … timeout --kill-after=5 <DEADLINE>` (`ops.rs:329-337`) | SOP `DEADLINE:` | `inbox.notify:sop:<slug>:<row key>:<n>` | a third run of the same SOP for one key within `SPIRA_NOTIFY_AGE`, which escalates instead |
//! | `inbox.notify.sop_applied` | store (`sop` applied, 60 s; `ops.rs:345-356`) | 60 s | `inbox.notify:applied:<slug>:<target>` | — |
//! | `inbox.notify.concierge_note` | store (mail; `ops.rs:358`) | 30 s | `inbox.notify:note:<slug>:<row key>` | — |
//! | `inbox.notify.mail_health` | store (until split into `mail.health`) | 60 s | `inbox.notify:mailhealth:<tick at>` | exit 3 means the task could not check: `notify` fails today, which becomes `effect.failed` |
//!
//! The ask is mailed exactly as today: `mail send operator --from "Watchd <watchd@spira>" --subject <s> --kind question --default <d>`, with the body `"## Question\n{subject}\n\n## Default\n{default}\n\n{why}\n\n{evidence}\n"` (`ops.rs:252-259`). There are two subjects:
//!
//! - `Events a watcher produced have reached no reader`
//! - `A watcher has stopped producing events`
//!
//! Each row's evidence block reads `{name} — {k} actionable event(s) with no reader, the oldest for {age}` or `{name} — DEGRADED for {age}: {why}` (`notify.rs:146`, `:286`).
//!
//! `notify` itself appends nothing to the inbox. `watchd page` (`notify.rs:189-214`) does, as `[watch:{name}] {text}`, but that is a different verb whose callers move with their own tasks.
//!
//! ## Silence
//!
//! Every row is caught up or younger than `SPIRA_NOTIFY_AGE`, and no unit is degraded. The tick then emits the probe and nothing else. Carry is emitted only when a pending or unhealthy clock starts or clears.
//!
//! ## Replaces
//!
//! - **Units.** `spira-notify-prod.service` and `spira-notify-prod.timer`. No watchd row.
//! - **Children.** All those listed at the top. `mail-health.sh` sources `conf.sh` (`spira/mail-health.sh:19`).
//! - **Keys pinned:**
//!   - `SPIRA_ACTIONABLE`, `SPIRA_HEALTH_TIMEOUT`, `SPIRA_NOTIFY_AGE` (`watchd/src/context.rs:127-129`)
//!   - `SPIRA_INSTANCE`, `SPIRA_RUN`, `SPIRA_WATCHERS`, `SPIRA_WATCHERS_OVERLAY`
//!   - `SPIRA_SYSTEMCTL`, `SPIRA_SYSTEMD_RUN` (test seams; dropped)
//!   - `SPIRA_MAIL_READERS`, `SPIRA_MAIL_UNREAD_AGE`, `SPIRA_MAIL`
//!
//! ## Reverse
//!
//! - Rollback restores the unit. The stamp files are absent after the move, so the first restored pass may re-ask a standing condition once.
//! - Observe mode shadows `ask`, `unit.start`, `sop_applied` and `concierge_note`.

use std::collections::BTreeMap;

use runtime::event::*;
use runtime::units::UNIT_START;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use crate::tasks::d::pattern;

pub const NAME: &str = "inbox.notify";
pub const STREAM: &str = "inbox.notify";
pub const TICK: EventKindTag = "inbox.notify.tick";
pub const STATE: EventKindTag = "inbox.notify.state";
pub const OBSERVE: &str = "inbox.notify.observe";
pub const ASK: &str = "inbox.notify.ask";
pub const SOP_MATCH: &str = "inbox.notify.sop_match";
pub const SOP_APPLIED: &str = "inbox.notify.sop_applied";
pub const CONCIERGE_NOTE: &str = "inbox.notify.concierge_note";
pub const MAIL_HEALTH: &str = "inbox.notify.mail_health";
pub const EFFECTS: &[&str] = &[OBSERVE, ASK, SOP_MATCH, UNIT_START, SOP_APPLIED, CONCIERGE_NOTE, MAIL_HEALTH];
/// `SPIRA_SYSTEMCTL` and `SPIRA_SYSTEMD_RUN` are test seams and are dropped.
pub const KEYS: &[&str] = &["SPIRA_ACTIONABLE", "SPIRA_HEALTH_TIMEOUT", "SPIRA_NOTIFY_AGE", "SPIRA_INSTANCE", "SPIRA_RUN", "SPIRA_WATCHERS", "SPIRA_WATCHERS_OVERLAY", "SPIRA_MAIL_READERS", "SPIRA_MAIL_UNREAD_AGE", "SPIRA_MAIL"];
/// The timer's 300 s.
pub const CADENCE_MS: u64 = 300_000;
const OBSERVE_DEADLINE_MS: u64 = 60_000;
/// Lines shown per row in one ask (`notify.rs:21`).
pub const NOTIFY_MAX: usize = 12;
pub const BACKLOG_SUBJECT: &str = "Events a watcher produced have reached no reader";
pub const HEALTH_SUBJECT: &str = "A watcher has stopped producing events";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub actionable: String,
    pub health_timeout_s: u64,
    pub notify_age_s: u64,
    pub instance: String,
    pub run: String,
    pub watchers: String,
    pub watchers_overlay: String,
    pub mail_readers: String,
    pub mail_unread_age_s: u64,
    pub mail: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config {
            actionable: cfg("SPIRA_ACTIONABLE")?,
            health_timeout_s: cfg_parse("SPIRA_HEALTH_TIMEOUT")?,
            notify_age_s: cfg_parse("SPIRA_NOTIFY_AGE")?,
            instance: cfg("SPIRA_INSTANCE")?,
            run: cfg("SPIRA_RUN")?,
            watchers: cfg("SPIRA_WATCHERS")?,
            watchers_overlay: cfg("SPIRA_WATCHERS_OVERLAY")?,
            mail_readers: cfg("SPIRA_MAIL_READERS")?,
            mail_unread_age_s: cfg_parse("SPIRA_MAIL_UNREAD_AGE")?,
            mail: cfg("SPIRA_MAIL")?,
        })
    }
}

/// One manifest log row as `notify` sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowView {
    pub name: String,
    pub kind: String,
    pub lines: u64,
    pub cursor: u64,
    /// The unread chunk past the cursor.
    pub unread: Vec<String>,
    pub pending: Option<(u64, u64)>,
    pub unhealthy: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Health {
    Ok,
    Degraded(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub now: u64,
    pub halted: bool,
    pub rows: Vec<RowView>,
    /// unit → (ActiveState, NRestarts).
    pub units: BTreeMap<String, (String, u64)>,
    pub health: BTreeMap<String, Health>,
    /// `mail-health.sh`'s exit: 0, 1 or 3.
    pub mail_health: Option<i32>,
}

/// `{ pending: {row→(pos, at)}, unhealthy: {row→at}, stamp_backlog, stamp_health, sop_runs }`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub pending: BTreeMap<String, (u64, u64)>,
    pub unhealthy: BTreeMap<String, u64>,
    pub stamp_backlog: Option<String>,
    pub stamp_health: Option<String>,
    pub sop_runs: BTreeMap<String, Vec<u64>>,
}

/// `cmd_notify` and `notify_health` split pure: the backlog half, the health half, the ask.
pub fn decide(_carry: &Carry, _snap: &Snapshot, _now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("inbox.notify: not ported yet")
}

pub struct InboxNotify {
    pub config: Result<Config, String>,
}

impl InboxNotify {
    pub fn new(config: Config) -> Self {
        InboxNotify { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        InboxNotify { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("inbox.notify/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for InboxNotify {
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
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
