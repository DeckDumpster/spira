//! # lc.ship — contract
//!
//! Family C, ported. lc-serve stages the control store's revlog runs and snapshots under
//! `SPIRA_LC_SHIP_DIR/queue/`; this task uploads them, so lc-serve starts no `curl`.
//!
//! - **Tick:** every `SPIRA_LC_SHIP_SECS`. Single-flight: a tick with a drain in flight re-arms.
//! - **Observe:** `lc.ship.observe` counts what `queue/` holds. Nothing is asked of a store with
//!   no `SPIRA_LC_BACKUP_URL` or an empty queue.
//! - **Act:** `lc.ship.drain` runs `ship::drain` on the directory: oldest first, stopping at the
//!   first refusal so a retry is the same object. A refusal is the task's error; the object
//!   stays queued and the next tick sends it again.
//! - **Credentials:** `S3Creds::from_env`, so this process's environment carries the backup key.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use runtime::event::EventKindTag;
use runtime::ports::Proc;
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
pub const EFFECTS: &[&str] = &[OBSERVE, DRAIN];

pub const CADENCE_KEY: &str = "SPIRA_LC_SHIP_SECS";
pub const URL_KEY: &str = "SPIRA_LC_BACKUP_URL";
pub const DIR_KEY: &str = "SPIRA_LC_SHIP_DIR";
pub const KEYS: &[&str] = &[CADENCE_KEY, URL_KEY, DIR_KEY];

// batch-job: one upload, bounded by the object's size, not a round trip
const PUT_DEADLINE: Duration = Duration::from_secs(300);

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None, jitter_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub dir: String,
    pub queued: u64,
    pub oldest: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Draining,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
}

pub struct LcShip {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

pub fn decide(_cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(_) if carry.phase == Phase::Draining => Ok((Carry::default(), vec![])),
        Input::Observed(s) if s.queued == 0 => Ok((carry.clone(), vec![])),
        Input::Observed(s) => Ok((Carry { phase: Phase::Draining }, vec![Act { name: DRAIN.into(), key: format!("lc.ship/{}/{now_ms}", s.oldest), input: s.dir.clone(), deadline_ms: 3_600_000, probe: false }])),
        Input::Outcome { effect, ok: true, .. } if effect == DRAIN => Ok((Carry::default(), vec![])),
        Input::Outcome { effect, ok: false, body } if effect == DRAIN => Err(format!("{NAME}: the upload was refused and stays queued: {body}")),
        Input::Outcome { effect, .. } => Err(format!("{NAME}: an outcome of {effect} is not expected")),
        Input::Ran(r) => Err(format!("{NAME}: no unit is expected, got {:?}", r.handle)),
    }
}

/// The runner of [`OBSERVE`]: what the staging directory holds, `queued: 0` with no destination.
pub fn observe(run: &Path, url: &str, dir: &str) -> Result<String, String> {
    let dir = ship::dir_at(Some(dir.to_string()), run);
    let mut snap = Snapshot { dir: dir.display().to_string(), ..Snapshot::default() };
    if !url.is_empty() {
        for kind in [REVLOG, SNAPSHOT] {
            let names = ship::names(&dir.join("queue").join(kind))?;
            snap.oldest = if snap.oldest.is_empty() { names.first().cloned().unwrap_or_default() } else { snap.oldest };
            snap.queued += names.len() as u64;
        }
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The runner of [`DRAIN`]: uploads what `dir` queues through `proc`, which starts the `curl`.
pub fn drain(proc: Arc<dyn Proc>, url: &str, dir: &str) -> Result<String, String> {
    let dest = Dest::parse(url)?;
    let creds = match dest {
        Dest::S3 { .. } => Some(S3Creds::from_env()?),
        Dest::File(_) => None,
    };
    let put = Uploader { dest, creds, proc, deadline: PUT_DEADLINE };
    let sent = ship::drain(Path::new(dir), &put)?;
    match sent.failed {
        Some(e) => Err(format!("{} object(s) uploaded, then: {e}", sent.uploaded.len())),
        None => Ok(format!("{} object(s) uploaded", sent.uploaded.len())),
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("phase")];

crate::tasks::b::hosted!(LcShip);
