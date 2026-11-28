//! # lc.ship — contract
//!
//! Family C, ported. lc-serve stages the control store's revlog runs and snapshots under
//! `SPIRA_LC_SHIP_DIR/queue/`; this task uploads them in-process, so lc-serve starts no program.
//!
//! - **Tick:** every `SPIRA_LC_SHIP_SECS`. Single-flight per kind: a kind with a drain in flight
//!   is left alone, so at most one pass over `queue/revlog` and one over `queue/snapshot` runs at a
//!   time, and a long snapshot upload does not hold back the runs.
//! - **Observe:** `lc.ship.observe` counts what each kind of `queue/` holds. Nothing is asked of a
//!   store with no `SPIRA_LC_BACKUP_URL` or an empty queue.
//! - **Act:** `lc.ship.drain` runs `ship::drain_kind` on the runs, `lc.ship.drain_snapshot` on the
//!   snapshots: oldest first, stopping at the first refusal so a retry is the same object. Any
//!   outcome returns its kind to idle, and a request past its deadline counts as idle, so a lost
//!   outcome cannot wedge the task.
//! - **Record:** every pass, refused or not, leaves `last-drain.json` in the ship directory: when, which kind, how many objects went, and the refusal or none.
//! - **Credentials:** the backup key in this process's environment, which the unit loads from the
//!   backup's environment file; `check-config` refuses an `s3://` destination without it.

use std::path::Path;
use std::time::Duration;

use runtime::event::EventKindTag;
use runtime::ports::Clock as _;
use serde::{Deserialize, Serialize};
use state_store_sqlite::durability::put::{Dest, S3Creds, Uploader};
use state_store_sqlite::durability::revlog::{REVLOG, SNAPSHOT};
use state_store_sqlite::durability::ship;

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "lc.ship";
pub const STREAM: &str = "task.lc.ship";
pub const TICK: EventKindTag = "lc.ship.tick";
pub const STATE: EventKindTag = "lc.ship.state";

pub const OBSERVE: &str = "lc.ship.observe";
pub const DRAIN: &str = "lc.ship.drain";
pub const DRAIN_SNAPSHOT: &str = "lc.ship.drain_snapshot";
pub const EFFECTS: &[&str] = &[OBSERVE, DRAIN, DRAIN_SNAPSHOT];

pub const CADENCE_KEY: &str = "SPIRA_LC_SHIP_SECS";
pub const URL_KEY: &str = "SPIRA_LC_BACKUP_URL";
pub const DIR_KEY: &str = "SPIRA_LC_SHIP_DIR";
pub const KEYS: &[&str] = &[CADENCE_KEY, URL_KEY, DIR_KEY];

// batch-job: one upload, bounded by the object's size, not a round trip
pub const PUT_DEADLINE: Duration = Duration::from_secs(300);

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None, jitter_key: None };

/// What one kind's queue holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Queue {
    pub queued: u64,
    pub oldest: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub dir: String,
    pub revlog: Queue,
    pub snapshot: Queue,
}

/// Per kind, when its drain was asked for; `None` while idle.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub revlog: Option<u64>,
    pub snapshot: Option<u64>,
}

/// The last pass of either kind: `ship-dir/last-drain.json`, read by `watch.off_box` for the error.
pub const LAST_DRAIN: &str = "last-drain.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastDrain {
    pub at_ms: u64,
    pub kind: String,
    pub uploaded: u64,
    pub error: Option<String>,
}

const DRAIN_DEADLINE_MS: u64 = 3_600_000;

pub struct LcShip {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

fn in_flight(asked: Option<u64>, now_ms: u64) -> Option<u64> {
    asked.filter(|at| now_ms.checked_sub(*at).is_some_and(|age| age < DRAIN_DEADLINE_MS))
}

fn ask(kind: &str, effect: &str, dir: &str, q: &Queue, asked: Option<u64>, now_ms: u64) -> (Option<u64>, Option<Act>) {
    if let Some(at) = in_flight(asked, now_ms) {
        return (Some(at), None);
    }
    if q.queued == 0 {
        return (None, None);
    }
    (Some(now_ms), Some(Act { name: effect.into(), key: format!("lc.ship/{kind}/{}/{now_ms}", q.oldest), input: dir.into(), deadline_ms: DRAIN_DEADLINE_MS, probe: true }))
}

pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(s) => {
            let (revlog, runs) = ask(REVLOG, DRAIN, &s.dir, &s.revlog, carry.revlog, now_ms);
            let (snapshot, snaps) = ask(SNAPSHOT, DRAIN_SNAPSHOT, &s.dir, &s.snapshot, carry.snapshot, now_ms);
            Ok((Carry { revlog, snapshot }, runs.into_iter().chain(snaps).collect()))
        }
        Input::Outcome { effect, .. } if effect == DRAIN => Ok((Carry { revlog: None, ..carry.clone() }, vec![])),
        Input::Outcome { effect, .. } if effect == DRAIN_SNAPSHOT => Ok((Carry { snapshot: None, ..carry.clone() }, vec![])),
        Input::Outcome { effect, .. } => Err(format!("{NAME}: an outcome of {effect} is not expected")),
        Input::Lost(lost) => lost.as_failure(decide, cfg, carry, now_ms),
        Input::Ran(r) => Err(format!("{NAME}: no unit is expected, got {:?}", r.handle)),
    }
}

/// The runner of [`OBSERVE`]: what the staging directory holds, `queued: 0` with no destination.
pub fn observe(run: &Path, url: &str, dir: &str) -> Result<String, String> {
    let dir = ship::dir_at(Some(dir.to_string()), run);
    let mut snap = Snapshot { dir: dir.display().to_string(), ..Snapshot::default() };
    if !url.is_empty() {
        for (kind, q) in [(REVLOG, &mut snap.revlog), (SNAPSHOT, &mut snap.snapshot)] {
            let names = ship::names(&runtime::ports::RealFs, &dir.join("queue").join(kind))?;
            *q = Queue { queued: names.len() as u64, oldest: names.first().cloned().unwrap_or_default() };
        }
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The runner of [`DRAIN`]: uploads the runs `dir` queues, in-process.
pub fn drain(url: &str, dir: &str, lookup: impl Fn(&str) -> Option<String>, part_bytes: u64) -> Result<String, String> {
    drain_of(REVLOG, url, dir, lookup, part_bytes)
}

/// The runner of [`DRAIN_SNAPSHOT`]: the same for the snapshots.
pub fn drain_snapshot(url: &str, dir: &str, lookup: impl Fn(&str) -> Option<String>, part_bytes: u64) -> Result<String, String> {
    drain_of(SNAPSHOT, url, dir, lookup, part_bytes)
}

fn drain_of(kind: &str, url: &str, dir: &str, lookup: impl Fn(&str) -> Option<String>, part_bytes: u64) -> Result<String, String> {
    let (uploaded, ran) = match upload(kind, url, dir, lookup, part_bytes) {
        Ok(n) => (n, Ok(format!("{n} object(s) uploaded"))),
        Err((n, e)) => (n, Err(e)),
    };
    let last = LastDrain { at_ms: runtime::ports::WallClock.now_ms(), kind: kind.into(), uploaded, error: ran.as_ref().err().cloned() };
    let recorded = serde_json::to_vec(&last).map_err(|e| e.to_string()).and_then(|b| ship::write_durably(&runtime::ports::RealFs, &Path::new(dir).join(LAST_DRAIN), &b));
    ran.and_then(|said| recorded.map(|()| said))
}

/// The objects uploaded, or the refusal and how many went first.
fn upload(kind: &str, url: &str, dir: &str, lookup: impl Fn(&str) -> Option<String>, part_bytes: u64) -> Result<u64, (u64, String)> {
    let early = |e: String| (0, e);
    let dest = Dest::parse(url).map_err(early)?;
    let creds = match dest {
        Dest::S3 { .. } => Some(S3Creds::from_vars(lookup).and_then(|c| c.with_part_bytes(part_bytes)).map_err(early)?),
        Dest::File(_) => None,
    };
    let put = Uploader { dest, creds, deadline: PUT_DEADLINE };
    let sent = ship::drain_kind(&runtime::ports::RealFs, Path::new(dir), kind, &put).map_err(early)?;
    let n = sent.uploaded.len() as u64;
    match sent.failed {
        Some(e) => Err((n, format!("{n} object(s) uploaded, then: {e}"))),
        None => Ok(n),
    }
}

/// Whether this process could sign the uploads `lc.ship` would make: `Ok` unless the task acts on
/// an `s3://` destination and `lookup` lacks the key. No credential value is in the message.
pub fn check_credentials(mode: runtime::Mode, url: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<(), String> {
    if mode != runtime::Mode::Act || url.is_empty() {
        return Ok(());
    }
    match Dest::parse(url).map_err(|e| format!("{NAME}: {URL_KEY}: {e}"))? {
        Dest::File(_) => Ok(()),
        Dest::S3 { .. } => S3Creds::from_vars(lookup).map(|_| ()).map_err(|e| format!("{NAME}: {e}")),
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[];

crate::tasks::b::hosted!(LcShip, off_journal);
