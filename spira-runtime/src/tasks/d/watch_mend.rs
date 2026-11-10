//! # watch.mend — contract
//!
//! Replaces `spira-mend-watch-prod`: `spira-lc mend-watch --seen run/mend-watch.seen`, then `ExecStartPost=spira-lc pause-sweep`, both appending stdout to `run/mend-watch.log`. Pattern P1. Part of [[scheduler-consolidation-2026-11-03]].
//!
//! Catalogue differs: the row lists one job with no children. The unit actually runs two jobs: the mender-bounds alarm, and then `pause-sweep` (`spira-lc/src/main.rs:362-363`), which lifts scoped stops. Both are `spira-lc` processes, and both write the same log. This contract carries both, as two effects.
//!
//! ## Tick
//!
//! - `Timer` every 120 s, first at boot + 120 s (the timer has `OnBootSec=2min` and `OnUnitActiveSec=2min`).
//! - Single-flight: a tick while the observe is in flight re-arms only.
//! - No other event wakes it. A later option is a wake on `mending` lifecycle events.
//!
//! ## Observe
//!
//! `watch.mend.observe` returns a `Snapshot { now, overdue: [(batch, pass, suite, deadline)], mends: [(bead, n)], stops: [(repo, bead, state, since, reached: Ok(bool)|Err(why))] }`. It is built from these sources:
//!
//! - **Overdue mends.** The store's `mending` rows that are `MENDING`, have `awaiting_ask IS NULL` and have `deadline <= now`. Today this is SQL over the store connection (`spira-lc/src/mend_watch.rs:65`). The target is the store's `mending list` verb (it is in the usage at `spira-lc/src/main.rs:438`) through `LcClient` (S7a).
//! - **Mends in the last 24 h, per bead.** `mend` rows with `at >= now-86400`, grouped by `bead_id` (`mend_watch.rs:66`). No store verb returns this count yet, so S7a has to add one read method (unverified whether `mending list` can carry it).
//! - **Read retries.** Each read is tried 4 times, 3 s apart (`mend_watch.rs:46-61`). Under the scheduler, the effect deadline and the retry policy on the observe replace that loop.
//! - **Scoped stops.** The `repo-*.json` records under `<SPIRA_RUN>/scoped-stops/` (`spira-config/src/scoped_stop.rs:32`, `:58-69`; the run dir comes from `spira-lc/src/pause_sweep.rs:10`). For each stop whose condition is `bead:ID=STATE`, the observe checks whether the bead's applied lifecycle events show it entering STATE since the stop was set (`pause_sweep.rs:14-21`). Today that is the `event` table. The target is the store's `history <key> --machine bead` verb.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Vec<Act>)` is pure. It reuses this existing core:
//!
//! - `overdue_breach`, `mends_breach` and `fresh` (`spira-lc/src/mend_watch.rs:20-36`; limit `MAX_MENDS_PER_DAY = 2`, `:11`)
//! - `bead_condition` and `entered_state` (`spira-config/src/scoped_stop.rs:42-54`)
//!
//! **Carry** is `watch.mend.state` = `{ seeded, seen: BTreeSet<breach key> }`. Today it is the `--seen` file. That file is rewritten every pass to hold the current breach keys (`mend_watch.rs:84-89`), so a breach that clears and later returns is announced again. The carry keeps the same semantics: `seen := keys of this snapshot`.
//!
//! **Fixtures:**
//! - the four `mend_watch.rs` tests (`:97-124`): retry, over-two-mends, no repeat until cleared, overdue says how late
//! - the `scoped_stop` tests in `spira-config` (unverified count)
//! - `spira/test-mend-watch-units.sh`, which pins the unit and becomes T1
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.mend.observe` (probe) | store | 15 s (12 s of retries today, under TimeoutStartSec=60) | none (probe) | — |
//! | `watch.mend.log_append` | host | 5 s | `watch.mend:<breach key>:<tick at>` | an empty line; a key already in `carry.seen` |
//! | `watch.mend.lift_stop` | host (rewrites `run/scoped-stops/repo-<repo>.json` via `scoped_stop::unpause_repo`) | 5 s | `watch.mend:lift:<repo>:<stop since>` | a free-text or unparsable condition (`scoped_stop.rs:42-45`); a stop whose condition read failed. That stop is kept and logged, which is the same result as `CANNOT_TELL` at `pause_sweep.rs:25` |
//!
//! **Lines written to `run/mend-watch.log`**, which is the `mend-watch|log` read contract at `spira/watchers:166`. The task appends each one exactly as today, with no timestamp prefix:
//!
//! - `MEND-WATCH OVERDUE {batch}#{pass}#{suite} is MENDING {now-deadline}s past its deadline; the sweep did not return it` (`mend_watch.rs:22`)
//! - `MEND-WATCH MENDS {bead} was mended {n} times in 24h (limit 2)` (`mend_watch.rs:29`)
//! - `pause-sweep: lifted the stop on {repo}: its bead condition was met` and `pause-sweep: kept {repo}: {why}` (`pause_sweep.rs:23-24`). These lines reach the same log today because ExecStartPost shares the unit's StandardOutput.
//!
//! No `inbox.append` exists today. Escalation is `inbox.notify`'s job (see [[inbox-notify]]): it reads this log through its cursor.
//!
//! ## Silence
//!
//! An idle tick emits the observe probe and nothing else:
//!
//! - no breach and no stop met → no Emit, no effect
//! - breaches that are all already in `seen`, with an unchanged set → no Emit
//!
//! ## Replaces
//!
//! - **Units:** `spira-mend-watch-prod.service` and `spira-mend-watch-prod.timer`.
//! - **The `mend-watch|log` row stays.** It is a read contract (`spira/watchers:166`); its writer becomes this task.
//! - **Children forked:** `spira-lc mend-watch` and `spira-lc pause-sweep`, as process starts by systemd. Neither forks further.
//! - **Config pinned (S10):** `SPIRA_RUN`, for the scoped-stops directory (`spira-lc/src/lib.rs`'s `lifecycle_run_dir`, unverified key path). The log path and the `--seen` path become task settings.
//!
//! ## Reverse
//!
//! - Release rollback restores both the unit and the timer. The `.seen` file is absent after the move, so the first restored pass re-announces any standing breach once.
//! - Observe mode (S4) shadows `log_append` and `lift_stop`, and keeps the probe running.

use std::collections::BTreeSet;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::lc_call::LcClient;
use spira_config::process::cfg;
use spira_config::scoped_stop;

use super::pattern;

pub const NAME: &str = "watch.mend";
pub const STREAM: &str = "watch.mend";
pub const TICK: EventKindTag = "watch.mend.tick";
pub const STATE: EventKindTag = "watch.mend.state";
pub const OBSERVE: &str = "watch.mend.observe";
pub const LOG_APPEND: &str = "watch.mend.log_append";
pub const LIFT_STOP: &str = "watch.mend.lift_stop";
pub const EFFECTS: &[&str] = &[OBSERVE, LOG_APPEND, LIFT_STOP];
pub const KEYS: &[&str] = &["SPIRA_RUN"];
/// `OnUnitActiveSec=2min`.
pub const CADENCE_MS: u64 = 120_000;
const OBSERVE_DEADLINE_MS: u64 = 15_000;
/// `MAX_MENDS_PER_DAY` (`spira-lc/src/mend_watch.rs:11`).
pub const MAX_MENDS_PER_DAY: u64 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub run: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { run: cfg("SPIRA_RUN")? })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overdue {
    pub batch: String,
    pub pass: u64,
    pub suite: String,
    pub deadline: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    pub repo: String,
    pub bead: String,
    pub state: String,
    pub since: u64,
    /// Whether the bead entered `state` since the stop was set, or why that could not be read.
    pub reached: Result<bool, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub now: u64,
    pub overdue: Vec<Overdue>,
    /// (bead, mends in the last 24 h).
    pub mends: Vec<(String, u64)>,
    pub stops: Vec<Stop>,
}

/// `seen := keys of this snapshot`, so a breach that clears and returns is announced again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub seeded: bool,
    pub seen: BTreeSet<String>,
}

/// One breach of the mender bounds: its `seen` key and its log line (`spira-lc/src/mend_watch.rs:14-31`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breach {
    pub key: String,
    pub line: String,
}

/// `overdue_breach` (`mend_watch.rs:20-24`).
pub fn overdue_breach(o: &Overdue, now: u64) -> Breach {
    let (batch, pass, suite) = (&o.batch, o.pass, &o.suite);
    Breach { key: format!("overdue:{batch}#{pass}#{suite}"), line: format!("MEND-WATCH OVERDUE {batch}#{pass}#{suite} is MENDING {}s past its deadline; the sweep did not return it", now.saturating_sub(o.deadline)) }
}

/// `mends_breach` (`mend_watch.rs:26-31`): more than [`MAX_MENDS_PER_DAY`] in a day is a loop.
pub fn mends_breach(bead: &str, n: u64) -> Option<Breach> {
    (n > MAX_MENDS_PER_DAY).then(|| Breach { key: format!("mends:{bead}"), line: format!("MEND-WATCH MENDS {bead} was mended {n} times in 24h (limit {MAX_MENDS_PER_DAY})") })
}

/// What the lift effect is handed: the stop's repo and when it was set, its idempotency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lift {
    pub repo: String,
    pub since: u64,
}

/// Every breach and stop finding of `snap`, in the order today's two jobs printed them, each
/// as `(seen key, line, lift)`.
fn findings(snap: &Snapshot) -> Vec<(String, String, Option<Lift>)> {
    let mut out: Vec<(String, String, Option<Lift>)> = snap.overdue.iter().map(|o| overdue_breach(o, snap.now)).chain(snap.mends.iter().filter_map(|(b, n)| mends_breach(b, *n))).map(|b| (b.key, b.line, None)).collect();
    for s in &snap.stops {
        match &s.reached {
            Ok(true) => out.push((format!("lift:{}:{}", s.repo, s.since), format!("pause-sweep: lifted the stop on {}: its bead condition was met", s.repo), Some(Lift { repo: s.repo.clone(), since: s.since }))),
            Ok(false) => {}
            Err(why) => out.push((format!("kept:{}:{}", s.repo, s.since), format!("pause-sweep: kept {}: {why}", s.repo), None)),
        }
    }
    out
}

/// `overdue_breach`, `mends_breach`, `fresh`, then `bead_condition` / `entered_state` per stop:
/// a line for each finding not in `seen`, a lift for every stop reached, and `seen := ` this
/// snapshot's keys. A lift repeats each tick until the stop is gone; its line does not.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, _cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let found = findings(snap);
    let mut acts = vec![];
    for (key, line, lift) in &found {
        if !carry.seen.contains(key) {
            acts.push(EffectSpec::keyed(LOG_APPEND, format!("watch.mend:{key}:{now_ms}"), line.clone(), ACT_DEADLINE_MS));
        }
        if let Some(l) = lift {
            acts.push(EffectSpec::keyed(LIFT_STOP, format!("watch.mend:lift:{}:{}", l.repo, l.since), serde_json::to_string(l).unwrap_or_default(), ACT_DEADLINE_MS));
        }
    }
    (Carry { seeded: true, seen: found.into_iter().map(|(k, _, _)| k).collect() }, acts)
}

const ACT_DEADLINE_MS: u64 = 5_000;

/// The `MEND-WATCH` lines the store's `mend-watch` verb printed, read back into the snapshot's
/// rows: the verb owns the SQL and the bounds, this only reverses its two formats.
pub fn parse_breaches(out: &str, now: u64) -> (Vec<Overdue>, Vec<(String, u64)>) {
    let (mut overdue, mut mends) = (vec![], vec![]);
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("MEND-WATCH OVERDUE ") {
            let Some((id, tail)) = rest.split_once(" is MENDING ") else { continue };
            let late: u64 = tail.split('s').next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let mut parts = id.splitn(3, '#');
            if let (Some(b), Some(p), Some(su)) = (parts.next(), parts.next(), parts.next()) {
                overdue.push(Overdue { batch: b.into(), pass: p.parse().unwrap_or(0), suite: su.into(), deadline: now.saturating_sub(late) });
            }
        } else if let Some(rest) = line.strip_prefix("MEND-WATCH MENDS ") {
            let Some((bead, tail)) = rest.split_once(" was mended ") else { continue };
            mends.push((bead.into(), tail.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0)));
        }
    }
    (overdue, mends)
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The probe: the store's `mend-watch` verb (no `--seen`; the carry is the seen set) and, per
/// scoped stop with a `bead:ID=STATE` condition, the bead's `history` over [`LcClient`].
pub fn observe(cfg: &Config) -> Result<Snapshot, String> {
    let lc = LcClient::configured()?;
    let now = epoch_now();
    let out = lc.answer(&["mend-watch"]).map_err(|e| format!("cannot tell: {e}"))?;
    let (overdue, mends) = parse_breaches(&out, now);
    let run = std::path::Path::new(&cfg.run);
    let mut stops = vec![];
    for s in scoped_stop::repo_stops(run) {
        let Some(cond) = scoped_stop::bead_condition(&s.until) else { continue };
        let (bead, state, reached) = match cond {
            Ok((bead, state)) => {
                let reached = lc
                    .answer(&["history", bead.as_str(), "--machine", "bead"])
                    .map_err(|e| format!("cannot tell: {e}"))
                    .and_then(|o| serde_json::from_str::<Vec<serde_json::Value>>(&o).map_err(|e| format!("cannot tell: history {bead}: {e}")))
                    .map(|rows| scoped_stop::entered_state(&rows, &state, s.since));
                (bead, state, reached)
            }
            Err(e) => (String::new(), String::new(), Err(e)),
        };
        stops.push(Stop { repo: s.repo, bead, state, since: s.since, reached });
    }
    Ok(Snapshot { now, overdue, mends, stops })
}

/// `run/mend-watch.log`, the `mend-watch|log` read contract: the line as is, no prefix.
pub fn log_append(cfg: &Config, line: &str) -> Result<String, String> {
    use std::io::Write;
    if line.is_empty() {
        return Err("watch.mend: an empty log line is refused".into());
    }
    let path = std::path::Path::new(&cfg.run).join("mend-watch.log");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

/// `scoped_stop::unpause_repo` for the stop the decide found reached; a stop already gone or
/// re-set since (a different `since`) is left alone.
pub fn lift_stop(cfg: &Config, input: &str) -> Result<String, String> {
    let lift: Lift = serde_json::from_str(input).map_err(|e| format!("watch.mend.lift_stop: {e}"))?;
    let run = std::path::Path::new(&cfg.run);
    if !scoped_stop::repo_stops(run).iter().any(|s| s.repo == lift.repo && s.since == lift.since) {
        return Ok(format!("no stop on {} since {}", lift.repo, lift.since));
    }
    scoped_stop::unpause_repo(run, &lift.repo, &[]).map(|_| format!("lifted {}", lift.repo))
}

/// The row's runners: each reads the pinned config through the door.
pub fn run_observe() -> Result<String, String> {
    serde_json::to_string(&observe(&Config::pinned()?)?).map_err(|e| e.to_string())
}

pub struct WatchMend {
    pub config: Result<Config, String>,
}

impl WatchMend {
    pub fn new(config: Config) -> Self {
        WatchMend { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchMend { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.mend/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

/// The rows a restart keeps; what a Carry field is left out of is rebuilt by the first tick.
const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("seeded"), runtime::rows::Part::Field("seen")];

impl Handler for WatchMend {
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
