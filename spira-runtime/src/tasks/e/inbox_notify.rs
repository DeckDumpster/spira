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
use crate::watchers::{answered_spec, carried};

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

const ACT_DEADLINE_MS: u64 = 30_000;
const LONG_DEADLINE_MS: u64 = 60_000;

/// Which of the two asks a candidate belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AskKind {
    Backlog,
    Health,
}

/// One row that is stale or degraded past `SPIRA_NOTIFY_AGE`, offered to a SOP before it is
/// asked: `key` names the incident across passes, `payload` is what `sop match` reads, and
/// `evidence` is the row's block of the ask.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub kind: AskKind,
    pub row: String,
    pub key: String,
    pub payload: String,
    pub evidence: String,
}

/// `sop_match`'s input: every candidate of one pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub candidates: Vec<Candidate>,
}

/// `sop_run::attempt`'s verdict for one candidate, in `Offer` order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tried {
    pub handled: bool,
    pub note: Option<String>,
}

/// `ask`'s input: the operator question as `Real::mail_ask` mailed it, and the fingerprint the
/// carry is stamped with once the ask is accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub kind: AskKind,
    pub fingerprint: String,
    pub from: String,
    pub subject: String,
    pub default: String,
    pub body: String,
}

/// The oldest `[ts]` prefix among `lines`, when it is not in the future: a stamp ahead of the
/// clock is no evidence of age, so the row falls back to its pending clock.
fn stamp_age(lines: &[&String], now: i64) -> Option<i64> {
    lines.iter().filter_map(|l| l.strip_prefix('[').and_then(|r| r.split_once(']')).map(|(ts, _)| ts)).filter_map(watchd::iso8601::parse_utc).min().map(|oldest| now - oldest).filter(|a| *a >= 0)
}

/// The unit a row's health is read from, as the snapshot names it: the watch unit of a daemon
/// row, the `spira-<target>` unit of an extern row.
fn unit_of<'a>(snap: &'a Snapshot, row: &str) -> Option<(&'a String, &'a (String, u64))> {
    snap.units.iter().find(|(u, _)| u.starts_with(&format!("spira-watch-{row}-")) || u.starts_with(&format!("spira-{row}-")) || u.as_str() == format!("spira-{row}.service"))
}

/// `cmd_notify` and `notify_health` split pure: the backlog half, the health half, the ask.
///
/// The clock is the scheduler's (`now_ms`), not the snapshot's: one clock decides every age.
/// The stale and degraded rows are not asked here; they go to `sop_match` as one offer, and
/// the asks are composed from its verdicts ([`asks`]). A pass with nothing stale clears that
/// half's stamp, as removing `notify.escalated` did.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let now = (now_ms / 1000) as i64;
    let age_limit = cfg.notify_age_s as i64;
    let mut next = carry.clone();
    let mut candidates = vec![];
    let filter = watchd::filter::Filter::compile(&cfg.actionable).ok();

    next.pending.retain(|name, _| snap.rows.iter().any(|r| &r.name == name));
    for r in snap.rows.iter().filter(|r| r.kind != "off") {
        let shown: Vec<&String> = r.unread.iter().filter(|l| filter.as_ref().is_some_and(|f| f.matches(l))).collect();
        let first = r.unread.iter().position(|l| filter.as_ref().is_some_and(|f| f.matches(l)));
        let (true, Some(first_idx)) = (r.lines > r.cursor, first) else {
            next.pending.remove(&r.name);
            continue;
        };
        let apos = r.cursor + first_idx as u64 + 1;
        let stamp = stamp_age(&shown, now);
        let prev = carry.pending.get(&r.name).copied();
        if prev.map(|(p, _)| p) != Some(apos) {
            next.pending.insert(r.name.clone(), (apos, now.max(0) as u64));
            if stamp.is_none() {
                continue;
            }
        }
        let age = stamp.unwrap_or_else(|| now - prev.map_or(now, |(_, at)| at as i64)).max(0);
        if age < age_limit {
            continue;
        }
        let k = shown.len();
        let mut evidence = format!("\n{} — {k} actionable event(s) with no reader, the oldest for {}\n", r.name, watchd::cursor::format_age(age));
        for l in shown.iter().take(NOTIFY_MAX) {
            evidence.push_str(&format!("    {l}\n"));
        }
        if k > NOTIFY_MAX {
            evidence.push_str(&format!("    ... and {} more — all of them: watchd drain {}\n", k - NOTIFY_MAX, r.name));
        }
        let key = format!("{}|{}|{}\n", r.name, apos, r.unread[first_idx]);
        let payload = shown.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n");
        candidates.push(Candidate { kind: AskKind::Backlog, row: r.name.clone(), key, payload, evidence });
    }

    if snap.halted {
        next.unhealthy.clear();
    } else {
        for (row, h) in &snap.health {
            let Health::Degraded(why) = h else {
                next.unhealthy.remove(row);
                continue;
            };
            let Some(prev_at) = carry.unhealthy.get(row).copied() else {
                next.unhealthy.insert(row.clone(), now.max(0) as u64);
                continue;
            };
            let age = (now - prev_at as i64).max(0);
            if age < age_limit {
                continue;
            }
            let mut evidence = format!("\n{row} — DEGRADED for {}: {why}\n", watchd::cursor::format_age(age));
            let unit = unit_of(snap, row);
            if let Some((u, (_, restarts))) = unit {
                evidence.push_str(&format!("  {u}, restarted {restarts} time(s) by systemd\n"));
            }
            let state = unit.map_or("unknown", |(_, (s, _))| s.as_str());
            let payload = format!("{row} — DEGRADED: {why}\nunit {} is {state}", unit.map_or("?", |(u, _)| u.as_str()));
            candidates.push(Candidate { kind: AskKind::Health, row: row.clone(), key: format!("{row}|{why}\n"), payload, evidence });
        }
        next.unhealthy.retain(|row, _| snap.health.contains_key(row));
    }

    if !candidates.iter().any(|c| c.kind == AskKind::Backlog) {
        next.stamp_backlog = None;
    }
    if !candidates.iter().any(|c| c.kind == AskKind::Health) {
        next.stamp_health = None;
    }
    let mut acts = vec![];
    if !candidates.is_empty() {
        acts.push(EffectSpec::keyed(SOP_MATCH, format!("inbox.notify:match:{now}"), serde_json::to_string(&Offer { candidates }).unwrap_or_default(), ACT_DEADLINE_MS));
    }
    if snap.mail_health.is_none() {
        acts.push(EffectSpec::keyed(MAIL_HEALTH, format!("inbox.notify:mailhealth:{now}"), format!("inbox.notify:mailhealth:{now}"), LONG_DEADLINE_MS));
    }
    (next, acts)
}

/// The asks of one offer once `sop_match` answered: each half's unhandled candidates as one
/// question, unless its fingerprint equals the carried stamp. An answer that is not a verdict
/// list (no SOP could be asked) escalates every candidate, as `Verdict::Escalate(None)`.
pub fn asks(carry: &Carry, offer: &Offer, outcome: &str) -> Vec<EffectSpec> {
    let tried: Vec<Tried> = serde_json::from_str(outcome).ok().filter(|t: &Vec<Tried>| t.len() == offer.candidates.len()).unwrap_or_else(|| offer.candidates.iter().map(|_| Tried { handled: false, note: None }).collect());
    let mut out = vec![];
    for (kind, stamp, subject, default, why) in [
        (AskKind::Backlog, &carry.stamp_backlog, watchd::notify::BACKLOG_SUBJECT, watchd::notify::BACKLOG_DEFAULT, watchd::notify::BACKLOG_WHY),
        (AskKind::Health, &carry.stamp_health, watchd::notify::HEALTH_SUBJECT, watchd::notify::HEALTH_DEFAULT, watchd::notify::HEALTH_WHY),
    ] {
        let (mut key, mut report) = (String::new(), String::new());
        for (c, t) in offer.candidates.iter().zip(&tried).filter(|(c, t)| c.kind == kind && !t.handled) {
            key.push_str(&c.key);
            report.push_str(&c.evidence);
            if let Some(n) = &t.note {
                report.push_str(&format!("  {n}\n"));
            }
        }
        if key.is_empty() {
            continue;
        }
        let fingerprint = watchd::notify::fingerprint(&key);
        if stamp.as_deref() == Some(fingerprint.as_str()) {
            continue;
        }
        let body = format!("## Question\n{subject}\n\n## Default\n{default}\n\n{why}\n\n{report}\n");
        let akey = format!("inbox.notify:{}:{fingerprint}", format!("{kind:?}").to_lowercase());
        let ask = Ask { kind, fingerprint, from: watchd::send::FROM.into(), subject: subject.into(), default: default.into(), body };
        out.push(EffectSpec::keyed(ASK, akey, serde_json::to_string(&ask).unwrap_or_default(), ACT_DEADLINE_MS));
    }
    out
}

/// The carry once an ask was accepted: that half stamped with its fingerprint. A refused ask
/// never reaches here, so it is asked again next pass.
pub fn stamped(carry: &Carry, ask: &Ask) -> Carry {
    let mut next = carry.clone();
    match ask.kind {
        AskKind::Backlog => next.stamp_backlog = Some(ask.fingerprint.clone()),
        AskKind::Health => next.stamp_health = Some(ask.fingerprint.clone()),
    }
    next
}

fn context() -> Result<watchd::context::Context, String> {
    watchd::context::home_dir().and_then(|h| watchd::context::load(&h))
}

/// The manifest's rows read through watchd's log+cursor contract, the unit facts of every
/// daemon and extern row from the unit manager, each active row's health assertion, and the
/// halt marker. `mail_health` is left to its own effect.
pub fn observe(units: Option<&dyn runtime::units::Units>) -> Result<String, String> {
    use watchd::manifest::Kind;
    let ctx = context()?;
    let rows = watchd::commands::load_rows_or_report(&ctx).map_err(|code| format!("{OBSERVE}: the manifest could not be read (exit {code})"))?;
    let mut snap = Snapshot { now: ctx.now.max(0) as u64, halted: std::path::Path::new(&ctx.run).join("world.halted").exists(), ..Snapshot::default() };
    for r in &rows {
        let kind = match r.kind {
            Kind::Daemon => "daemon",
            Kind::Log => "log",
            Kind::Extern => "extern",
            Kind::Off => continue,
        };
        let mut view = RowView { name: r.name.clone(), kind: kind.into(), ..RowView::default() };
        if let Some(lf) = watchd::paths::logfile(&ctx.run, &r.name, r.kind, &r.target) {
            view.lines = watchd::fs_ops::total_lines(&lf);
            view.cursor = watchd::fs_ops::read_pos(&watchd::paths::cursorfile(&ctx.run, &r.name), view.lines);
            if view.lines > view.cursor {
                view.unread = watchd::fs_ops::read_range(&lf, view.cursor, view.lines);
            }
        }
        snap.rows.push(view);
        let unit = match r.kind {
            Kind::Daemon => watchd::paths::watch_unit_name(&r.name, &ctx.instance),
            Kind::Extern if ctx.instance.is_empty() => format!("spira-{}.service", r.target),
            Kind::Extern => format!("spira-{}-{}.service", r.target, ctx.instance),
            _ => continue,
        };
        let Some(units) = units else { continue };
        let facts = units.facts(&runtime::units::UnitHandle::named(&unit)).map_err(|e| e.0)?;
        if facts.state == runtime::units::UnitState::Absent {
            continue;
        }
        let state = format!("{:?}", facts.state).to_lowercase();
        let health = if state != "active" {
            Health::Degraded(format!("unit is {state} — no writer"))
        } else {
            match watchd::health::probe(&r.health, ctx.health_timeout()) {
                watchd::health::Health::Degraded(why) => Health::Degraded(why),
                _ => Health::Ok,
            }
        };
        snap.units.insert(unit, (state, 0));
        snap.health.insert(r.name.clone(), health);
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The operator question, in-process through watchd's mail library (`Real::mail_ask`).
pub fn ask(input: &str) -> Result<String, String> {
    let a: Ask = serde_json::from_str(input).map_err(|e| format!("{ASK}: bad input: {e}"))?;
    watchd::send::send_configured(&watchd::send::Letter { mailbox: "operator", subject: &a.subject, kind: "question", default: &a.default, body: &a.body })?;
    Ok(format!("asked {}", a.fingerprint))
}

/// Each candidate offered to a SOP through `sop_run::attempt`: the verdicts in offer order.
pub fn sop_match(input: &str) -> Result<String, String> {
    let offer: Offer = serde_json::from_str(input).map_err(|e| format!("{SOP_MATCH}: bad input: {e}"))?;
    let ctx = context()?;
    let ops = watchd::ops::Real::new(ctx.systemctl.clone(), ctx.systemctl_override.as_deref());
    let tried: Vec<Tried> = offer
        .candidates
        .iter()
        .map(|c| match watchd::sop_run::attempt(&ops, &ctx, &c.row, &c.key, &c.payload) {
            watchd::sop_run::Verdict::Handled => Tried { handled: true, note: None },
            watchd::sop_run::Verdict::Escalate(note) => Tried { handled: false, note },
        })
        .collect();
    serde_json::to_string(&tried).map_err(|e| e.to_string())
}

/// `sop applied <slug> --bead|--pass <id> --check <c> --why -`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub slug: String,
    pub bead: Option<String>,
    pub pass: Option<String>,
    pub check: String,
    pub why: String,
}

pub fn sop_applied(input: &str) -> Result<String, String> {
    use watchd::ops::Ops;
    let a: Applied = serde_json::from_str(input).map_err(|e| format!("{SOP_APPLIED}: bad input: {e}"))?;
    let target = match (a.bead, a.pass) {
        (Some(b), _) => watchd::ops::Target::Bead(b),
        (None, Some(p)) => watchd::ops::Target::Pass(p),
        (None, None) => return Err(format!("{SOP_APPLIED}: refused: no bead or pass named")),
    };
    watchd::ops::Real::new(context()?.systemctl, None).sop_applied(&a.slug, &target, &a.check, &a.why)?;
    Ok(format!("{} recorded {}", a.slug, a.check))
}

/// A note to the Concierge, as `Real::mail_concierge` sent it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub kind: String,
    pub subject: String,
    pub body: String,
}

pub fn concierge_note(input: &str) -> Result<String, String> {
    let n: Note = serde_json::from_str(input).map_err(|e| format!("{CONCIERGE_NOTE}: bad input: {e}"))?;
    watchd::send::send_configured(&watchd::send::Letter { mailbox: "concierge", subject: &n.subject, kind: &n.kind, default: "", body: &n.body })?;
    Ok(format!("noted {}", n.subject))
}

/// `mail-health.sh` (watchd's FORK-EDGE): 0 and 1 are answers, 3 is "could not check".
pub fn mail_health() -> Result<String, String> {
    use watchd::ops::Ops;
    match watchd::ops::Real::new(context()?.systemctl, None).mail_health() {
        3 => Err("watchd: mail-health.sh could not check".into()),
        code => Ok(code.to_string()),
    }
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
        if ev.kind.tag == EFFECT_SUCCEEDED {
            match answered_spec(view, ev) {
                Some(s) if s.name == SOP_MATCH => {
                    let Ok(offer) = serde_json::from_str::<Offer>(&s.input) else { return vec![] };
                    return asks(&carried::<Carry>(view, &ev.stream, STATE), &offer, &ev.kind.body).into_iter().map(Command::Effect).collect();
                }
                Some(s) if s.name == ASK => {
                    let Ok(a) = serde_json::from_str::<Ask>(&s.input) else { return vec![] };
                    let carry = carried::<Carry>(view, &ev.stream, STATE);
                    return pattern::commands(STATE, &carry, &stamped(&carry, &a), vec![]);
                }
                _ => {}
            }
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
