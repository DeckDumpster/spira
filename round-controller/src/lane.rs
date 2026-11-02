//! `pass`'s lane step: `lanes/<repo>` is kept naming the commit git's landing ref is at and the
//! image and harness the repo's rounds run on, written only when one of them changed. A repo's
//! lane is what every cut is tested on and what `cutover-check` reads its base verdict against.

use serde_json::{json, Value};
use state_store::{Compare, Object, Request, Txn};

use crate::land::git_key;
use crate::orphans::HOLDER;
use crate::ports::Ports;
use crate::refusal::{self, Refusal};
use crate::stack::{self, lane_key, Lane};

pub const STEP: &str = "lane";

/// Where a refusal to read the installed release's round image or harness is keyed.
pub const INSTALLED_KEY: &str = "installed/round-image";

/// What a repo's rounds run on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneInputs {
    /// The hash of the recipe the round image is built from.
    pub image_digest: String,
    pub harness_version: String,
}

/// The installed release's round image and harness, for each repo that lands by rounds.
pub trait Installed {
    /// `None` for a repo that does not land by rounds: its lane is left as it stands.
    fn lane_inputs(&self, repo: &str) -> Result<Option<LaneInputs>, String>;
}

/// No repo lands by rounds: for a test whose code never records a lane.
pub struct NoRounds;

impl Installed for NoRounds {
    fn lane_inputs(&self, _: &str) -> Result<Option<LaneInputs>, String> {
        Ok(None)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    NotRounds,
    Unchanged,
    Written,
}

/// Writes `lanes/<repo>` from git's landing ref and the installed round image and harness, by a
/// CAS on the record, re-read when another writer moved it first. Fields it does not own
/// (`selection`, `concurrency`) are kept.
pub fn record(p: &Ports, repo: &str) -> Result<Recorded, Refusal> {
    const TRIES: usize = 3;
    let Some(inputs) = p
        .installed
        .lane_inputs(repo)
        .map_err(|e| Refusal::unreadable(INSTALLED_KEY, e))?
    else {
        return Ok(Recorded::NotRounds);
    };
    let at = p
        .git
        .landing_sha(repo)
        .map_err(|e| Refusal::unreadable(&git_key(repo), e))?;
    let key = lane_key(repo);
    for _ in 0..TRIES {
        let item = p
            .store
            .get(&key)
            .map_err(|e| Refusal::unreadable(&key, e))?;
        let rev = item.as_ref().map(|i| i.mod_rev);
        let mut value = item.map_or_else(|| Object::new(json!({})), |i| i.value);
        let lane = value
            .data
            .as_object_mut()
            .ok_or_else(|| Refusal::unreadable(&key, "the lane is not an object"))?;
        let landing = match lane.get("landing_ref").and_then(Value::as_str) {
            Some(was) if was != at && unrecorded_landing(p, repo)? => was.to_string(),
            _ => at.clone(),
        };
        let want = [
            ("landing_ref", landing),
            ("image_digest", inputs.image_digest.clone()),
            ("harness_version", inputs.harness_version.clone()),
        ];
        if want
            .iter()
            .all(|(k, v)| lane.get(*k).and_then(Value::as_str) == Some(v.as_str()))
        {
            return Ok(Recorded::Unchanged);
        }
        for (k, v) in want {
            lane.insert(k.into(), json!(v));
        }
        serde_json::from_value::<Lane>(value.data.clone())
            .map_err(|e| Refusal::unreadable(&key, e))?;
        let t = Txn::new()
            .when(Compare::ModRev(key.clone(), rev))
            .put(&key, value);
        let req = Request::new(
            &format!("rc-lane-{:016x}", p.next_u64()),
            HOLDER,
            "lane.record",
        );
        if p.store
            .txn(&t, &req)
            .map_err(|e| Refusal::unreadable(&key, e))?
            .succeeded
        {
            return Ok(Recorded::Written);
        }
    }
    Err(Refusal::unreadable(
        &key,
        format!("moved under {TRIES} tries to record the lane"),
    ))
}

/// A live round whose head git's landing ref already holds is the Lander's to record: moving the
/// lane past it first would leave it ungrounded, and supersede would take a round that landed.
fn unrecorded_landing(p: &Ports, repo: &str) -> Result<bool, Refusal> {
    for r in stack::live(p, repo)? {
        let Some(head) = r.round.row.head.as_deref() else {
            continue;
        };
        if p.git
            .under_landing_ref(repo, head)
            .map_err(|e| Refusal::unreadable(&git_key(repo), e))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `pass`'s lane step: [`record`], its refusal told once.
pub fn step(p: &Ports, repo: &str) {
    let r = record(p, repo);
    refusal::note(p, repo, STEP, r.as_ref().err());
}

#[cfg(test)]
mod tests;
