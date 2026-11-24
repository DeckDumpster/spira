//! # watch.off_box — contract
//!
//! Pages the Concierge when the control store's newest off-box revision falls behind. The measure
//! is taken from what was uploaded, so a stalled stager, a stalled uploader and a refused key
//! all page alike.
//!
//! - **Tick:** every 15 s. Single-flight.
//! - **Observe:** `watch.off_box.observe` answers `NoDestination` with `SPIRA_LC_BACKUP_URL`
//!   empty, else `Seen`: the store's head (lc-serve's `ship-status`), the `shipped` revision and
//!   its mtime in the ship directory, the error a *revlog* drain recorded in `last-drain.json`
//!   (a snapshot drain's error is not a run upload's), and the mtime of the oldest staged
//!   snapshot. A `shipped` that is absent or unparsable is the probe's error, never revision 0.
//! - **Carry:** `waiting` is `(rev, since)`: the head first seen ahead of `shipped`, and when. It
//!   clears when `shipped` reaches `rev` and is set again at once while the head is still ahead.
//!   `blind_since_ms` is when the observe began failing; `red`, `snapshot_red` and `blind_red` are
//!   the crossings already paged. All are rows, so a restart neither repeats a page nor resets a clock.
//! - **Decide:** red when `now - since` exceeds `SPIRA_LC_SHIP_MAX_AGE_SECS` and the last revlog
//!   drain was refused, or exceeds `cycle_s` with no refusal: the shipper's worst healthy cycle
//!   (one upload deadline, one staging period, two shipper ticks and one tick of this task). A stall
//!   that ends inside it is silent. Separately, red when the oldest staged snapshot is older than
//!   `SPIRA_LC_SHIP_SNAPSHOT_SECS`. An idle store, whose `shipped` equals its head however old, is
//!   never red; neither is no destination. A failed or timed-out observe makes and clears no RED:
//!   a standing RED stays; blind for longer than `SPIRA_LC_SHIP_MAX_AGE_SECS` it says `CANNOT TELL:`
//!   once, and the next answer says `CLEAR:` for the blindness and decides the rest afresh.
//! - **Act:** each crossing is one `inbox.append`, `RED: ...`, `CANNOT TELL: ...` or `CLEAR: ...`,
//!   keyed by the crossing's time. The RED line carries the wait, the newest uploaded revision,
//!   when it left the box, and the last upload's own error.

use std::path::Path;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};
use state_store_sqlite::durability::revlog::{REVLOG, SNAPSHOT};
use state_store_sqlite::durability::ship;

use crate::tasks::c::lc_ship::{LastDrain, LAST_DRAIN, PUT_DEADLINE};
use crate::tasks::d::pattern;
use crate::watchers::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const NAME: &str = "watch.off_box";
pub const STREAM: &str = "watch.off_box";
pub const TICK: EventKindTag = "watch.off_box.tick";
pub const STATE: EventKindTag = "watch.off_box.state";
pub const OBSERVE: &str = "watch.off_box.observe";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND];
pub const URL_KEY: &str = "SPIRA_LC_BACKUP_URL";
pub const DIR_KEY: &str = "SPIRA_LC_SHIP_DIR";
pub const KEYS: &[&str] = &[URL_KEY, DIR_KEY, "SPIRA_LC_SHIP_MAX_AGE_SECS", "SPIRA_LC_SHIP_SNAPSHOT_SECS", "SPIRA_LC_SHIP_SECS", "SPIRA_CONCIERGE_INBOX", "SPIRA_RUN"];
pub const INBOX_TAG: &str = "off-box";
pub const CADENCE_MS: u64 = 15_000;
const OBSERVE_DEADLINE_MS: u64 = 10_000;
const INBOX_DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub max_age_s: u64,
    pub snapshot_s: u64,
    /// The longest a revision may wait through a healthy cycle: one upload, one staging period, two
    /// shipper ticks and one watcher tick.
    pub cycle_s: u64,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        let max_age_s: u64 = cfg_parse("SPIRA_LC_SHIP_MAX_AGE_SECS")?;
        let ship_s: u64 = cfg_parse("SPIRA_LC_SHIP_SECS")?;
        Ok(Config { max_age_s, snapshot_s: cfg_parse("SPIRA_LC_SHIP_SNAPSHOT_SECS")?, cycle_s: cycle_s(max_age_s, ship_s) })
    }
}

/// The shipper's worst healthy cycle, in seconds, for the configured freshness bound and ship tick.
pub fn cycle_s(max_age_s: u64, ship_s: u64) -> u64 {
    let stage_s = ship::stage_every_ms(ship_s, max_age_s) / 1_000;
    PUT_DEADLINE.as_secs().saturating_add(stage_s).saturating_add(ship_s.saturating_mul(2)).saturating_add(CADENCE_MS / 1_000)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Snapshot {
    NoDestination,
    Seen {
        head: u64,
        shipped: u64,
        shipped_at_ms: u64,
        last_error: Option<String>,
        /// The mtime of the oldest object in `queue/snapshot`; `None` with none staged.
        oldest_snapshot_ms: Option<u64>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub waiting: Option<(u64, u64)>,
    pub blind_since_ms: Option<u64>,
    pub red: bool,
    pub snapshot_red: bool,
    pub blind_red: bool,
}

fn mtime_ms(path: &Path) -> Result<u64, String> {
    let t = std::fs::metadata(path).and_then(|m| m.modified()).map_err(|e| format!("{}: {e}", path.display()))?;
    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).map_err(|e| format!("{}: {e}", path.display()))
}

/// What `dir` and the store's `head` say; `Err` when either cannot be read.
pub fn observe_dir(url: &str, dir: &Path, head: impl FnOnce() -> Result<u64, String>) -> Result<Snapshot, String> {
    if url.is_empty() {
        return Ok(Snapshot::NoDestination);
    }
    let marker = dir.join("shipped");
    let text = std::fs::read_to_string(&marker).map_err(|e| format!("{}: {e}", marker.display()))?;
    let shipped = text.trim().parse::<u64>().map_err(|_| format!("{}: not a revision: {:?}", marker.display(), text.trim()))?;
    let shipped_at_ms = mtime_ms(&marker)?;
    let last = dir.join(LAST_DRAIN);
    let last_error = match std::fs::read_to_string(&last) {
        Ok(t) => {
            let drain = serde_json::from_str::<LastDrain>(&t).map_err(|e| format!("{}: {e}", last.display()))?;
            drain.error.filter(|_| drain.kind == REVLOG)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", last.display())),
    };
    let staged = dir.join("queue").join(SNAPSHOT);
    let oldest_snapshot_ms = ship::names(&runtime::ports::RealFs, &staged)?.iter().filter_map(|n| mtime_ms(&staged.join(n)).ok()).min();
    Ok(Snapshot::Seen { head: head()?, shipped, shipped_at_ms, last_error, oldest_snapshot_ms })
}

/// The runner of [`OBSERVE`].
pub fn observe(run: &Path, head: impl FnOnce() -> Result<u64, String>) -> Result<String, String> {
    let dir = ship::dir_at(Some(cfg(DIR_KEY)?), run);
    let seen = observe_dir(&cfg(URL_KEY)?, &dir, head)?;
    serde_json::to_string(&seen).map_err(|e| e.to_string())
}

fn secs(ms: u64) -> u64 {
    ms / 1_000
}

fn short(why: &str) -> String {
    why.chars().take(200).collect()
}

/// The crossings `seen` makes against `carry` at `now_ms`, as the carry that records them and one
/// inbox line each.
pub fn decide(carry: &Carry, seen: &Result<Snapshot, String>, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let mut next = carry.clone();
    let mut acts = vec![];
    let say = |acts: &mut Vec<EffectSpec>, key: &str, line: String| acts.push(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}/{key}{now_ms}"), pattern::inbox_line(now_ms, INBOX_TAG, &line), INBOX_DEADLINE_MS));
    let snapshot_clear = "CLEAR: no staged control-store snapshot is past its bound";
    // `None`: the observe could not answer, so the standing RED is neither made nor cleared.
    let verdict: Option<(Option<String>, String)> = match seen {
        Ok(Snapshot::NoDestination) => {
            if carry.snapshot_red {
                say(&mut acts, "snapshot/", snapshot_clear.into());
            }
            next = Carry::default();
            Some((None, "no destination is configured".to_string()))
        }
        Ok(Snapshot::Seen { head, shipped, shipped_at_ms, last_error, oldest_snapshot_ms }) => {
            next.blind_since_ms = None;
            if next.waiting.is_some_and(|(rev, _)| shipped >= &rev) {
                next.waiting = None;
            }
            if next.waiting.is_none() && head > shipped {
                next.waiting = Some((*head, now_ms));
            }
            let snapshot_bound = cfg.snapshot_s.saturating_mul(1_000);
            let stale = oldest_snapshot_ms.filter(|at| now_ms.saturating_sub(*at) > snapshot_bound);
            match (stale, carry.snapshot_red) {
                (Some(at), false) => say(&mut acts, "snapshot/", format!("RED: the oldest staged control-store snapshot has waited {}s without upload, past {}s", secs(now_ms.saturating_sub(at)), cfg.snapshot_s)),
                (None, true) => say(&mut acts, "snapshot/", snapshot_clear.into()),
                _ => {}
            }
            next.snapshot_red = stale.is_some();
            let limit_s = if last_error.is_some() { cfg.max_age_s } else { cfg.cycle_s };
            let red = next.waiting.filter(|(_, since)| now_ms.saturating_sub(*since) > limit_s.saturating_mul(1_000)).map(|(_, since)| {
                let wait = secs(now_ms.saturating_sub(since));
                let left = iso8601(secs(*shipped_at_ms));
                let why = last_error.as_deref().map_or("none".to_string(), short);
                format!("RED: control-store revisions up to {head} have waited {wait}s without upload, past {limit_s}s; the newest uploaded revision is {shipped}, which left the box at {left}; the last upload's error: {why}")
            });
            Some((red, format!("the newest uploaded revision is {shipped}")))
        }
        Err(e) => {
            let since = *next.blind_since_ms.get_or_insert(now_ms);
            let blind = now_ms.saturating_sub(since);
            if blind > cfg.max_age_s.saturating_mul(1_000) && !carry.blind_red {
                let standing = if carry.red { "; the RED already open stands" } else { "" };
                say(&mut acts, "blind/", format!("CANNOT TELL: the off-box copy cannot be measured, for {}s, past {}s: {}{standing}", secs(blind), cfg.max_age_s, short(e)));
                next.blind_red = true;
            }
            None
        }
    };
    if let Some((red, current)) = verdict {
        if carry.blind_red {
            say(&mut acts, "blind/", "CLEAR: the off-box copy is measured again".into());
            next.blind_red = false;
        }
        match (&red, carry.red) {
            (Some(line), false) => say(&mut acts, "", line.clone()),
            (None, true) => say(&mut acts, "", format!("CLEAR: the off-box copy is current again: {current}")),
            _ => {}
        }
        next.red = red.is_some();
    }
    (next, acts)
}

pub struct WatchOffBox {
    pub config: Result<Config, String>,
}

impl WatchOffBox {
    pub fn new(config: Config) -> Self {
        WatchOffBox { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchOffBox { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.off_box/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("waiting"), runtime::rows::Part::Field("blind_since_ms"), runtime::rows::Part::Field("red"), runtime::rows::Part::Field("snapshot_red"), runtime::rows::Part::Field("blind_red")];

impl Handler for WatchOffBox {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let cfg = match &self.config {
            Ok(c) => c,
            Err(e) => {
                ctx.event().set("config.refused", e.as_str());
                return vec![];
            }
        };
        let seen = match ev.kind.tag.as_str() {
            EFFECT_SUCCEEDED => serde_json::from_str::<Snapshot>(&ev.kind.body).map_err(|e| format!("the observe answer did not parse: {e}")),
            EFFECT_FAILED => Err(ev.kind.body.clone()),
            _ => Err("the observe timed out".to_string()),
        };
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let (next, acts) = decide(&carry, &seen, ev.at, cfg);
        pattern::commands(STATE, &carry, &next, acts)
    }
}
