//! `claimable`: the claim rule answered from lifecycle records when it is asked. Nothing is
//! read from a stored verdict and nothing is written.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lifecycle::bead::{rework_claim_stack_unchanged, BeadState, HoldKind, Stack};
use serde::Serialize;
use serde_json::json;
use spira_config::edges::{self, Gate};
use spira_config::nonwork::{self, SPLIT_LABEL};
use spira_config::runes::{self, Partition, PartitionsStamp};
use state_store::Backend;

use super::records::LifecycleRecord;
use super::state::{self, Call};
use super::store::Store;
use crate::{flag, CANNOT_TELL};

pub const VERBS: &[&str] = &["claimable"];

/// The hard ceiling the config schema enforces on `SPIRA_STACK_MAX_DEPTH`.
const STACK_CEILING: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Claimable,
    Blocked,
    Held,
    Snoozed,
    TooDeep,
    StackUnchanged,
    Carried,
    Stopped,
    OutOfScope,
    CannotTell,
}

#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub now: i64,
    pub stack_max: u32,
    pub incident_label: String,
    pub pilgrim_label: String,
    pub skuld_label: String,
    pub scope_label: String,
    pub no_loop_label: String,
    /// `(prerequisite, tip)` pairs a stack-conflict marker holds for.
    pub conflicts: BTreeSet<(String, String)>,
    pub reserved: BTreeSet<String>,
    pub stopped_repos: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Judgement {
    pub verdict: Verdict,
    pub blocker: Option<String>,
    pub missing: Vec<&'static str>,
    pub depth: u32,
    pub stack: Stack,
    pub gates: BTreeMap<String, Option<Gate>>,
}

fn csv(s: &str) -> Vec<&str> {
    s.split(',').map(str::trim).filter(|t| !t.is_empty()).collect()
}

fn rowless_done(issue_type: &str, status: &str) -> bool {
    let kind = match issue_type {
        "gate" => nonwork::Kind::Hold,
        "epic" | "decision" | "event" | "molecule" => nonwork::Kind::Epic,
        _ => return true,
    };
    nonwork::is_closed(kind, status)
}

/// What `cand` waits on: its parent, then its declared blockers, which never repeat the parent.
fn blockers(cand: &LifecycleRecord) -> Vec<String> {
    let Some(claim) = &cand.claim else { return Vec::new() };
    let id = &cand.row.bead_id;
    claim
        .parent
        .iter()
        .filter(|p| !p.is_empty() && *p != id)
        .chain(claim.blocks.iter().filter(|b| claim.parent.as_ref() != Some(*b)))
        .cloned()
        .collect()
}

pub fn judge(
    cand: &LifecycleRecord,
    rows: &BTreeMap<String, LifecycleRecord>,
    rowless: &BTreeMap<String, (String, String)>,
    ctx: &Ctx,
) -> Judgement {
    let own = &cand.row;
    let mut j = Judgement { verdict: Verdict::Claimable, blocker: None, missing: Vec::new(), depth: 0, stack: Stack::new(), gates: BTreeMap::new() };
    if own.holds.iter().any(|h| *h != HoldKind::Wait) {
        j.verdict = Verdict::Held;
        return j;
    }
    let snoozed = own.holds.contains(&HoldKind::Wait)
        && own.reason.as_deref().is_some_and(|r| spira_store::lc_state::snooze_until(r).is_some() && !spira_store::lc_state::wait_expired(r, ctx.now));
    if snoozed {
        j.verdict = Verdict::Snoozed;
        return j;
    }
    if cand.claim.is_none() {
        j.missing.push("claim");
    }
    if cand.repo.is_none() {
        j.missing.push("repo");
    }
    let (Some(claim), Some(own_repo)) = (&cand.claim, &cand.repo) else {
        j.verdict = Verdict::CannotTell;
        return j;
    };
    let has = |set: &str, l: &String| csv(set).contains(&l.as_str());
    let labelled = csv(&ctx.scope_label).iter().all(|l| claim.labels.iter().any(|x| x == l))
        && !claim.labels.iter().any(|l| has(&ctx.no_loop_label, l));
    if !labelled {
        j.verdict = Verdict::OutOfScope;
        return j;
    }
    let never_stacks = [&ctx.incident_label, &ctx.pilgrim_label, &ctx.skuld_label].into_iter().any(|l| !l.is_empty() && claim.labels.contains(l));
    let stack_max = if never_stacks { 0 } else { ctx.stack_max };
    let split = claim.labels.iter().any(|l| l == SPLIT_LABEL);
    let mut stacked_max: Option<u32> = None;
    let mut blocked = None;
    for bid in blockers(cand) {
        let rec = rows.get(&bid);
        j.gates.insert(bid.clone(), rec.map(LifecycleRecord::gate));
        if blocked.is_some() {
            continue;
        }
        let is_parent = claim.parent.as_deref() == Some(bid.as_str());
        if is_parent && !edges::blocks(edges::Edge::Parent { parent: rec.map(|r| (r.row.state, r.row.tip.is_some())), split }) {
            continue;
        }
        let ok = match rec {
            None => rowless.get(&bid).is_some_and(|(ty, status)| rowless_done(ty, status)),
            Some(b) => {
                let stackable = stack_max > 0
                    && !is_parent
                    && !lifecycle::bead::provisional(b.row.state, b.row.reason.as_deref())
                    && b.repo.as_ref() == Some(own_repo);
                let conflict = b.row.tip.as_ref().is_some_and(|t| ctx.conflicts.contains(&(bid.clone(), t.clone())));
                match b.row.state {
                    BeadState::Landed | BeadState::Done => true,
                    BeadState::Certified | BeadState::InDelivery if stackable && conflict => false,
                    BeadState::Certified | BeadState::InDelivery if stackable => {
                        stacked_max = Some(stacked_max.unwrap_or(0).max(b.row.stack_depth));
                        if let Some(tip) = &b.row.tip {
                            j.stack.insert(bid.clone(), tip.clone());
                        }
                        true
                    }
                    s => s.is_terminal(),
                }
            }
        };
        if !ok {
            blocked = Some(bid);
        }
    }
    if let Some(b) = blocked {
        j.verdict = Verdict::Blocked;
        j.blocker = Some(b);
        j.stack.clear();
        return j;
    }
    j.depth = stacked_max.map_or(0, |d| d + 1);
    if j.depth > stack_max {
        j.verdict = Verdict::TooDeep;
        return j;
    }
    if own.state == BeadState::Rework && rework_claim_stack_unchanged(&own.stack, own.reason.as_deref(), &j.stack) {
        j.verdict = Verdict::StackUnchanged;
        return j;
    }
    if ctx.reserved.contains(&own.bead_id) || ctx.stopped_repos.contains(own_repo) {
        j.verdict = Verdict::Stopped;
        return j;
    }
    if let Some(carrier) = &cand.carried_by {
        if rows.get(carrier).is_none_or(|r| !r.row.state.is_terminal()) {
            j.verdict = Verdict::Carried;
            j.blocker = Some(carrier.clone());
        }
    }
    j
}

/// What the verb reads besides the control store.
pub trait Inputs {
    fn config(&self) -> Result<Config, String>;
    /// `(issue_type, status)` of each id the content store holds.
    fn statuses(&self, ids: &[String]) -> Result<BTreeMap<String, (String, String)>, String>;
    /// The plan rows of each id the content store holds, as `plan items --ids` prints them.
    fn items(&self, ids: &[String]) -> Result<Vec<serde_json::Value>, String>;
    fn run_dir(&self) -> Result<PathBuf, String>;
    /// The rune partitions that decide which runes would take a claimable bead.
    fn partitions(&self, config: &Config) -> Result<Vec<Partition>, String>;
    /// What `partitions` would read; equal stamps mean an equal answer.
    fn partitions_stamp(&self) -> Result<PartitionsStamp, String>;
}

#[derive(Clone)]
pub struct Config {
    pub queue_wait: String,
    pub stack_max: u32,
    pub incident_label: String,
    pub pilgrim_label: String,
    pub skuld_label: String,
    pub scope_label: String,
    pub no_loop_label: String,
}

pub struct Live;

impl Inputs for Live {
    fn config(&self) -> Result<Config, String> {
        use spira_config::process::{cfg, cfg_parse};
        Ok(Config {
            queue_wait: cfg("SPIRA_QUEUE_WAIT_LABEL")?,
            stack_max: cfg_parse::<u32>("SPIRA_STACK_MAX_DEPTH")?.min(STACK_CEILING),
            incident_label: cfg("SPIRA_INCIDENT_LABEL")?,
            pilgrim_label: cfg("SPIRA_PILGRIM_LABEL")?,
            skuld_label: cfg("SPIRA_SKULD_LABEL")?,
            scope_label: cfg("SPIRA_SCOPE_LABEL")?,
            no_loop_label: cfg("SPIRA_NO_LOOP_LABEL")?,
        })
    }

    fn statuses(&self, ids: &[String]) -> Result<BTreeMap<String, (String, String)>, String> {
        let beads = crate::bd::store(|sql, db| crate::content_sql::statuses(sql, db, ids))?;
        Ok(beads.into_iter().map(|b| (b.id, (b.issue_type, b.status))).collect())
    }

    fn items(&self, ids: &[String]) -> Result<Vec<serde_json::Value>, String> {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        crate::bd::store(|sql, _| crate::plan::items_held(sql, &ids).map_err(crate::content_sql::Failure::Store))
    }

    fn run_dir(&self) -> Result<PathBuf, String> {
        crate::lifecycle_run_dir()
    }

    fn partitions(&self, config: &Config) -> Result<Vec<Partition>, String> {
        use spira_config::process::cfg;
        let home = spira_config::resolve::locate_home_for_process()?;
        let roster = cfg("SPIRA_RUNES")?;
        runes::partitions(&home, Some(roster.as_str()), &config.queue_wait, &cfg("SPIRA_OPEN_CHILDREN_LABEL")?)
    }

    fn partitions_stamp(&self) -> Result<PartitionsStamp, String> {
        let home = spira_config::resolve::locate_home_for_process()?;
        let roster = spira_config::process::cfg("SPIRA_RUNES")?;
        Ok(runes::partitions_stamp(&home, Some(roster.as_str())))
    }
}

/// The process-wide inputs: config pinned at the first read, partitions re-read when a rune changes.
pub static LIVE: std::sync::LazyLock<Pinned<Live>> = std::sync::LazyLock::new(|| Pinned::new(Live));

/// `inner` with its config pinned at the first successful read and its partitions kept until
/// their stamp changes. A failed read is not kept.
pub struct Pinned<I> {
    inner: I,
    config: std::sync::OnceLock<Config>,
    parts: std::sync::Mutex<Option<(PartitionsStamp, Vec<Partition>)>>,
}

impl<I: Inputs> Pinned<I> {
    pub fn new(inner: I) -> Self {
        Pinned { inner, config: std::sync::OnceLock::new(), parts: std::sync::Mutex::new(None) }
    }
}

impl<I: Inputs> Inputs for Pinned<I> {
    fn config(&self) -> Result<Config, String> {
        if let Some(c) = self.config.get() {
            return Ok(c.clone());
        }
        let read = self.inner.config()?;
        Ok(self.config.get_or_init(|| read).clone())
    }

    fn statuses(&self, ids: &[String]) -> Result<BTreeMap<String, (String, String)>, String> {
        self.inner.statuses(ids)
    }

    fn items(&self, ids: &[String]) -> Result<Vec<serde_json::Value>, String> {
        self.inner.items(ids)
    }

    fn run_dir(&self) -> Result<PathBuf, String> {
        self.inner.run_dir()
    }

    fn partitions(&self, config: &Config) -> Result<Vec<Partition>, String> {
        let stamp = self.inner.partitions_stamp()?;
        let mut kept = self.parts.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((s, parts)) = kept.as_ref() {
            if *s == stamp {
                return Ok(parts.clone());
            }
        }
        let parts = self.inner.partitions(config)?;
        *kept = Some((stamp, parts.clone()));
        Ok(parts)
    }

    fn partitions_stamp(&self) -> Result<PartitionsStamp, String> {
        self.inner.partitions_stamp()
    }
}

pub fn run<B: Backend>(store: &Store<B>, args: &[String], inputs: &dyn Inputs, call: &Call) -> (i32, String) {
    match answer(store, args, inputs, call) {
        Ok(out) => (0, out),
        Err(e) => (CANNOT_TELL, format!("cannot tell: claimable: {e}")),
    }
}

fn answer<B: Backend>(store: &Store<B>, args: &[String], inputs: &dyn Inputs, call: &Call) -> Result<String, String> {
    let repo = flag(args, "--repo");
    if args.len() != 1 + 2 * usize::from(repo.is_some()) {
        return Err("takes only --repo <repo>".into());
    }
    let rev = store.contract().head().map_err(|e| e.to_string())?;
    let states = [BeadState::Ready, BeadState::Rework];
    let cands = store
        .records_page(rev, Some(&states), None, None, None, |r| repo.as_ref().is_none_or(|x| r.repo.as_ref() == Some(x)))
        .map_err(|e| e.to_string())?;
    let (rows, views) = view(store, rev, cands.into_iter().map(|r| r.value).collect(), inputs, call.now)?;
    let cannot_tell = views.values().filter(|v| v.verdict == Verdict::CannotTell).count();
    let ids: Vec<String> = views.keys().cloned().collect();
    // An unreadable content store leaves `content` null and says why: the lifecycle judgement stands without it, and a reader that ranks names the cause.
    let (items, content_error) = match inputs.items(&ids) {
        Ok(items) => (items, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let mut content: BTreeMap<String, serde_json::Value> = items
        .into_iter()
        .filter_map(|r| Some((r.get("id")?.as_str()?.to_string(), r)))
        .collect();
    let beads: Vec<_> = views
        .iter()
        .map(|(id, v)| {
            json!({
                "bead_id": id, "verdict": v.verdict, "blocker": v.blocker, "runes": v.runes, "missing": v.missing,
                "depth": v.depth, "stack": v.stack, "gates": v.gates, "row": state::listed(&rows[id], None),
                "content": content.remove(id),
            })
        })
        .collect();
    Ok(json!({ "rev": rev, "cannot_tell": cannot_tell, "content_error": content_error, "beads": beads }).to_string())
}

/// One candidate's answer: the judgement and the runes that would take it.
pub struct Viewed {
    pub verdict: Verdict,
    pub blocker: Option<String>,
    pub runes: Vec<String>,
    pub missing: Vec<&'static str>,
    pub depth: u32,
    pub stack: Stack,
    pub gates: BTreeMap<String, Option<Gate>>,
}

/// The records a view consulted, and its answer per candidate.
pub type ViewRead = (BTreeMap<String, LifecycleRecord>, BTreeMap<String, Viewed>);

/// The claim view of `cands` (READY and REWORK records), read from the records at `rev` and
/// the inputs, with the records it consulted. Only `Pinned` inputs keep anything between calls.
pub fn view<B: Backend>(
    store: &Store<B>,
    rev: u64,
    cands: Vec<LifecycleRecord>,
    inputs: &dyn Inputs,
    now: i64,
) -> Result<ViewRead, String> {
    if cands.is_empty() {
        return Ok((BTreeMap::new(), BTreeMap::new()));
    }
    let config = inputs.config()?;
    let mut rows: BTreeMap<String, LifecycleRecord> = cands.into_iter().map(|r| (r.row.bead_id.clone(), r)).collect();
    let cand_ids: Vec<String> = rows.keys().cloned().collect();
    let wanted: BTreeSet<String> = cand_ids.iter().filter_map(|id| rows.get(id)).flat_map(blockers).collect();
    let carriers: BTreeSet<String> = cand_ids.iter().filter_map(|id| rows[id].carried_by.clone()).collect();
    let unread: Vec<String> = wanted.union(&carriers).filter(|i| !rows.contains_key(*i)).cloned().collect();
    if !unread.is_empty() {
        for r in store.records_page(rev, None, Some(&unread), None, None, |_| true).map_err(|e| e.to_string())? {
            rows.insert(r.value.row.bead_id.clone(), r.value);
        }
    }
    let rowless_ids: Vec<String> = wanted.iter().filter(|i| !rows.contains_key(*i)).cloned().collect();
    let rowless = if rowless_ids.is_empty() { BTreeMap::new() } else { inputs.statuses(&rowless_ids)? };
    let run = inputs.run_dir()?;
    let ctx = context(&config, &run, &rows, &cand_ids, now);
    let judged: Vec<(String, Judgement)> = cand_ids.iter().map(|id| (id.clone(), judge(&rows[id], &rows, &rowless, &ctx))).collect();
    let parts = if judged.iter().any(|(_, j)| j.verdict == Verdict::Claimable) { inputs.partitions(&config)? } else { Vec::new() };
    let views = judged
        .into_iter()
        .map(|(id, j)| {
            let runes = match (j.verdict, &rows[&id].claim) {
                (Verdict::Claimable, Some(c)) => runes::taking(&c.labels, &parts, &config.queue_wait),
                _ => Vec::new(),
            };
            let v = Viewed { verdict: j.verdict, blocker: j.blocker, runes, missing: j.missing, depth: j.depth, stack: j.stack, gates: j.gates };
            (id, v)
        })
        .collect();
    Ok((rows, views))
}

fn context(config: &Config, run: &Path, rows: &BTreeMap<String, LifecycleRecord>, cands: &[String], now: i64) -> Ctx {
    let conflicts = rows
        .values()
        .filter(|r| matches!(r.row.state, BeadState::Certified | BeadState::InDelivery))
        .filter_map(|r| Some((r.row.bead_id.clone(), r.row.tip.clone()?)))
        .filter(|(id, tip)| spira_config::stack_conflict::holds(run, id, tip))
        .collect();
    let repos: BTreeSet<&String> = cands.iter().filter_map(|id| rows[id].repo.as_ref()).collect();
    let stopped_repos = repos
        .into_iter()
        .filter(|r| spira_config::scoped_stop::stop_for(run, r, "claims").is_some())
        .cloned()
        .collect();
    Ctx {
        now,
        stack_max: config.stack_max,
        incident_label: config.incident_label.clone(),
        pilgrim_label: config.pilgrim_label.clone(),
        skuld_label: config.skuld_label.clone(),
        scope_label: config.scope_label.clone(),
        no_loop_label: config.no_loop_label.clone(),
        conflicts,
        reserved: spira_config::scoped_stop::reserved_ids(run),
        stopped_repos,
    }
}

/// Inputs for a unit test: a fixed config, no content store and no run directory.
#[cfg(test)]
pub(crate) struct Fixed;

#[cfg(test)]
pub(crate) static FIXED: Fixed = Fixed;

#[cfg(test)]
impl Inputs for Fixed {
    fn config(&self) -> Result<Config, String> {
        Ok(Config {
            queue_wait: "queue-wait".into(),
            stack_max: 4,
            incident_label: "incident".into(),
            pilgrim_label: "pilgrimage".into(),
            skuld_label: "draft".into(),
            scope_label: String::new(),
            no_loop_label: String::new(),
        })
    }

    fn statuses(&self, _: &[String]) -> Result<BTreeMap<String, (String, String)>, String> {
        Ok(BTreeMap::new())
    }

    fn items(&self, ids: &[String]) -> Result<Vec<serde_json::Value>, String> {
        Ok(ids.iter().map(|id| json!({ "id": id, "title": format!("title of {id}"), "priority": 2, "issue_type": "task", "status": "open", "created_at": "2026-01-01T00:00:00Z", "defer_until": null, "dependencies": [] })).collect())
    }

    fn run_dir(&self) -> Result<PathBuf, String> {
        Ok(PathBuf::from("nonexistent/run"))
    }

    fn partitions(&self, _: &Config) -> Result<Vec<Partition>, String> {
        Ok(Vec::new())
    }

    fn partitions_stamp(&self) -> Result<PartitionsStamp, String> {
        Ok(PartitionsStamp::default())
    }
}

/// Inputs that cannot be read.
#[cfg(test)]
pub(crate) struct Unreadable;

#[cfg(test)]
pub(crate) static UNREADABLE: Unreadable = Unreadable;

#[cfg(test)]
impl Inputs for Unreadable {
    fn config(&self) -> Result<Config, String> {
        Err("SPIRA_STACK_MAX_DEPTH is unreadable".into())
    }

    fn statuses(&self, _: &[String]) -> Result<BTreeMap<String, (String, String)>, String> {
        Err("the content store is unreadable".into())
    }

    fn items(&self, _: &[String]) -> Result<Vec<serde_json::Value>, String> {
        Err("the content store is unreadable".into())
    }

    fn run_dir(&self) -> Result<PathBuf, String> {
        Err("the run directory is unreadable".into())
    }

    fn partitions(&self, _: &Config) -> Result<Vec<Partition>, String> {
        Err("the runes are unreadable".into())
    }

    fn partitions_stamp(&self) -> Result<PartitionsStamp, String> {
        Err("the runes are unreadable".into())
    }
}

#[cfg(test)]
mod tests;
