//! The store's hot reads, held in memory (law-a-loops-hot-read-is-in-process): the rounds and
//! the open forge batch of each repo, and the ready counts, built from the store's own reads over
//! [`LcClient`] and kept current by its `watch` long-poll from the last revision seen. Every read
//! answers with the revision it reflects, and refuses once the store has not confirmed that
//! revision within the configured staleness: an old picture is never passed off as the present.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spira_config::lc_call::{LcClient, LcError};
use spira_config::lc_state::{self, BatchState, Row};

/// How old a view's last confirmed revision may be before its reads refuse.
pub const STALE_KEY: &str = "SPIRA_STORE_VIEW_STALE_MS";
/// One long-poll's wait: under the store's 2.5 s cap and the service's 3 s request deadline.
pub const WATCH_WAIT_MS: u64 = 2_000;
/// The pause after a poll the store did not answer, so a down service is not spun on.
const RETRY: Duration = Duration::from_secs(1);
/// The most ids one `list --ids` re-read names.
const IDS_PER_READ: usize = 200;

const BATCHES: &str = "batches/";
const LIFECYCLE: &str = "lifecycle/";
const READINESS: &str = "readiness/";
/// The queue's fields of a round, in the order `round open` writes them; any other follows by name.
const FIELD_ORDER: [&str; 7] = ["head", "base", "parent", "members", "worktree", "actor", "opened"];
const FORGE_PHASE: &str = "forge.phase";
const ENDED: &str = "ended";

/// A view's answer and the store revision it reflects.
#[derive(Debug, Clone, PartialEq)]
pub struct Read<T> {
    pub rev: u64,
    pub value: T,
}

/// One round record as the queue reads it: `batch_id`, `repo`, then the queue's own fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    /// The batch machine's word (`OPEN`, `CI_RUNNING`).
    pub state: String,
    pub fields: Vec<(String, String)>,
}

impl Record {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.as_str())
    }

    /// `key=value` lines, as `queue batch show` prints them.
    pub fn render(&self) -> String {
        self.fields.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
    }

    fn terminal(&self) -> bool {
        BatchState::from_str(&self.state).is_some_and(BatchState::is_terminal)
    }

    /// Why the queue ended it while the machine had not: no reader's open round.
    pub fn ended(&self) -> Option<&str> {
        self.get(ENDED).filter(|e| !e.is_empty())
    }
}

/// A repo's open forge batch: what `queue batch show --repo <repo>` answers from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchView {
    pub repo: String,
    pub open: Option<Record>,
}

impl BatchView {
    /// `batch show`'s stdout: the record's lines, nothing when none is open.
    pub fn shown(&self) -> String {
        self.open.as_ref().map(Record::render).unwrap_or_default()
    }
}

/// One open round and its phase as `round status` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Round {
    pub record: Record,
    pub phase: String,
}

/// A repo's open rounds, oldest first: what `queue round status` reads from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundView {
    pub repo: String,
    pub rounds: Vec<Round>,
}

/// The lifecycle rows by state, and how many a builder could take now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyCounts {
    pub by_state: BTreeMap<String, usize>,
    /// READY or REWORK and holding nothing.
    pub claimable: usize,
    /// Claimable, and the dispatcher's last pass judged it so.
    pub judged_claimable: usize,
    /// Claimable and express.
    pub express: usize,
}

#[derive(Clone)]
struct Batch {
    create_rev: u64,
    data: Value,
}

#[derive(Clone)]
struct Bead {
    row: Row,
    verdict: Option<String>,
}

#[derive(Default)]
struct State {
    rev: Option<u64>,
    confirmed: Option<Instant>,
    batches: BTreeMap<String, Batch>,
    beads: BTreeMap<String, Bead>,
}

pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The in-memory views and the watch that keeps them current.
pub struct Projection {
    client: LcClient,
    stale_after: Duration,
    clock: Clock,
    state: Mutex<State>,
}

fn fault(e: impl std::fmt::Display) -> LcError {
    LcError::Fault(format!("cannot tell: {e}"))
}

fn json_of(text: &str, what: &str) -> Result<Value, LcError> {
    serde_json::from_str(text.trim()).map_err(|e| fault(format!("{what}: not JSON: {e}")))
}

fn main_rev(v: &Value) -> Option<u64> {
    v.get("main").and_then(Value::as_u64)
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// The machine's word for a stored batch state: serde's variant name or the word itself.
fn state_word(stored: &str) -> Option<&'static str> {
    BatchState::from_str(stored)
        .or_else(|| serde_json::from_value(Value::String(stored.to_string())).ok())
        .map(BatchState::as_str)
}

/// The round id of a round's own key in `repo`; `None` for any key beneath one.
fn round_id<'a>(repo: &str, key: &'a str) -> Option<&'a str> {
    let id = key.strip_prefix(BATCHES)?.strip_prefix(repo)?.strip_prefix('/')?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

fn decode(key: &str, d: &Value) -> Result<Record, LcError> {
    let undecodable = |what: &str| fault(format!("{key}: undecodable round: no {what}"));
    let id = text(d, "batch_id").ok_or_else(|| undecodable("batch_id"))?;
    let stored = text(d, "state").ok_or_else(|| undecodable("state"))?;
    let state = state_word(&stored).ok_or_else(|| undecodable(&format!("batch state (it holds {stored:?})")))?;
    let mut fields = vec![("batch_id".to_string(), id.clone()), ("repo".to_string(), text(d, "repo").unwrap_or_default())];
    match d.get("round").and_then(Value::as_object).filter(|m| !m.is_empty()) {
        Some(own) => {
            let rank = |k: &str| FIELD_ORDER.iter().position(|f| *f == k).unwrap_or(FIELD_ORDER.len());
            let mut own: Vec<(&String, &Value)> = own.iter().collect();
            own.sort_by(|a, b| (rank(a.0), a.0).cmp(&(rank(b.0), b.0)));
            for (k, v) in own {
                fields.push((k.clone(), v.as_str().ok_or_else(|| undecodable(&format!("text for {k}")))?.to_string()));
            }
        }
        None => {
            for k in ["head", "base", "parent"] {
                if let Some(v) = text(d, k) {
                    fields.push((k.into(), v));
                }
            }
            let members: Vec<String> = d
                .get("members")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|m| Some(format!("{}:{}", text(m, "bead_id")?, text(m, "tip")?)))
                .collect();
            fields.push(("members".into(), members.join(" ")));
            if let Some(at) = d.get("opened_at").and_then(Value::as_i64) {
                fields.push(("opened".into(), at.to_string()));
            }
        }
    }
    Ok(Record { id, state: state.to_string(), fields })
}

/// `round status`'s name for a machine state.
fn phase_of(state: &str) -> String {
    match state {
        "OPEN" => "opened".into(),
        "CI_RUNNING" => "certifying".into(),
        other => other.to_lowercase(),
    }
}

/// The beads `list` answered, with the dispatcher's verdict on each.
fn beads_of(raw: &str) -> Result<Vec<(String, Bead)>, LcError> {
    let rows = lc_state::parse_rows(raw).map_err(fault)?;
    let verdicts: BTreeMap<String, String> = match json_of(raw, "list")? {
        Value::Array(a) => a.iter().filter_map(|r| Some((text(r, "bead_id")?, text(r, "claim_verdict")?))).collect(),
        _ => BTreeMap::new(),
    };
    Ok(rows.into_iter().map(|row| (row.bead_id.clone(), Bead { verdict: verdicts.get(&row.bead_id).cloned(), row })).collect())
}

impl Projection {
    pub fn new(client: LcClient, stale_after: Duration, clock: Clock) -> Self {
        Projection { client, stale_after, clock, state: Mutex::new(State::default()) }
    }

    /// The views on `client` with the configured staleness, on the wall clock. Refused when the
    /// lifecycle is not on the control store: the watch sees that store only, and would confirm a
    /// picture the dolt path had moved past.
    pub fn configured(client: LcClient) -> Result<Self, String> {
        let stale_ms: u64 = spira_config::process::cfg_parse(STALE_KEY)?;
        if spira_config::process::cfg_parse::<u8>("SPIRA_LC_LIFECYCLE_ON_STORE")? == 0 {
            return Err("the store views watch the control store, and the lifecycle is not on it (SPIRA_LC_LIFECYCLE_ON_STORE=0)".into());
        }
        Ok(Projection::new(client, Duration::from_millis(stale_ms), Arc::new(Instant::now)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn rc_list(&self, prefix: &str) -> Result<(Vec<Value>, u64), LcError> {
        let body = json!({ "prefixes": [prefix], "selector": "everything" }).to_string();
        let v = json_of(&self.client.answer(&["rc", "list", "--json", &body])?, "rc list")?;
        let ok = &v["ok"];
        let rev = ok["list_rev"].as_u64().ok_or_else(|| fault(format!("rc list: no list_rev in {v}")))?;
        Ok((ok["items"].as_array().cloned().unwrap_or_default(), rev))
    }

    fn list_ids(&self, ids: &[String]) -> Result<Vec<(String, Bead)>, LcError> {
        let mut out = Vec::new();
        for chunk in ids.chunks(IDS_PER_READ) {
            out.extend(beads_of(&self.client.answer(&["list", "--ids", &chunk.join(",")])?)?);
        }
        Ok(out)
    }

    /// Reads every view afresh: the batch records and the revision they were listed at, then
    /// every lifecycle row. A change after that revision is redelivered by the next watch.
    pub fn load(&self) -> Result<(), LcError> {
        let (items, rev) = self.rc_list(BATCHES)?;
        let beads = beads_of(&self.client.answer(&["list"])?)?;
        let mut batches = BTreeMap::new();
        for i in items {
            let key = text(&i, "key").unwrap_or_default();
            let create_rev = main_rev(&i["create_rev"]).unwrap_or(0);
            batches.insert(key, Batch { create_rev, data: i["value"]["data"].clone() });
        }
        let mut s = self.lock();
        *s = State { rev: Some(rev), confirmed: Some((self.clock)()), batches, beads: beads.into_iter().collect() };
        Ok(())
    }

    /// One step of keeping current: a load when nothing is held, else one watch from the held
    /// revision. Nothing moves unless the whole answer applied.
    pub fn poll(&self) -> Result<(), LcError> {
        let held = self.lock().rev;
        let Some(from) = held else { return self.load() };
        let (from_s, wait) = (from.to_string(), WATCH_WAIT_MS.to_string());
        let w = json_of(&self.client.answer(&["watch", "", "--from-rev", &from_s, "--wait-ms", &wait])?, "watch")?;
        if let Some(p) = w["progress_rev"].as_u64() {
            let mut s = self.lock();
            s.rev = Some(p.max(from));
            s.confirmed = Some((self.clock)());
            return Ok(());
        }
        if w.get("too_old").is_some() {
            return self.load();
        }
        let (Some(events), Some(through)) = (w["events"].as_array(), w["through"].as_u64()) else {
            return Err(fault(format!("watch: unreadable answer {w}")));
        };
        let mut batch_puts: Vec<(String, Option<Value>, Option<u64>)> = Vec::new();
        let mut dirty = BTreeSet::new();
        for e in events {
            let key = text(e, "key").unwrap_or_default();
            if key.starts_with(BATCHES) {
                let value = e.get("value").filter(|v| !v.is_null()).map(|v| v["data"].clone());
                batch_puts.push((key, value, e.get("create_rev").and_then(main_rev)));
            } else if let Some(id) = key.strip_prefix(LIFECYCLE).or_else(|| key.strip_prefix(READINESS)) {
                dirty.insert(id.to_string());
            }
        }
        let dirty: Vec<String> = dirty.into_iter().collect();
        let fresh: BTreeMap<String, Bead> = self.list_ids(&dirty)?.into_iter().collect();
        let mut s = self.lock();
        for (key, value, create_rev) in batch_puts {
            match value {
                None => {
                    s.batches.remove(&key);
                }
                Some(data) => {
                    let create_rev = create_rev.or_else(|| s.batches.get(&key).map(|b| b.create_rev)).unwrap_or(through);
                    s.batches.insert(key, Batch { create_rev, data });
                }
            }
        }
        for id in dirty {
            match fresh.get(&id) {
                Some(b) => s.beads.insert(id, b.clone()),
                None => s.beads.remove(&id),
            };
        }
        s.rev = Some(through.max(from));
        s.confirmed = Some((self.clock)());
        Ok(())
    }

    /// Polls until `stop` is set, pausing after a poll the store did not answer.
    pub fn run(&self, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            if self.poll().is_err() {
                std::thread::sleep(RETRY);
            }
        }
    }

    fn read<T>(&self, f: impl FnOnce(&State) -> Result<T, LcError>) -> Result<Read<T>, LcError> {
        let s = self.lock();
        let (Some(rev), Some(at)) = (s.rev, s.confirmed) else {
            return Err(LcError::Refused("the store views are not loaded yet".into()));
        };
        let age = (self.clock)().saturating_duration_since(at);
        if age > self.stale_after {
            return Err(LcError::Refused(format!(
                "stale past {}ms: the views hold revision {rev}, last confirmed {}ms ago",
                self.stale_after.as_millis(),
                age.as_millis()
            )));
        }
        Ok(Read { rev, value: f(&s)? })
    }

    /// The repo's rounds the machine has not ended and the queue has not ended, oldest first.
    fn open_rounds(s: &State, repo: &str) -> Result<Vec<Record>, LcError> {
        let mut rounds: Vec<(u64, i64, Record)> = Vec::new();
        for (key, b) in s.batches.iter().filter(|(k, _)| round_id(repo, k).is_some()) {
            let r = decode(key, &b.data)?;
            if r.terminal() || r.ended().is_some() {
                continue;
            }
            let opened = r.get("opened").and_then(|o| o.parse().ok()).unwrap_or(0);
            rounds.push((b.create_rev, opened, r));
        }
        rounds.sort_by(|a, b| (a.0, a.1, &a.2.id).cmp(&(b.0, b.1, &b.2.id)));
        Ok(rounds.into_iter().map(|(_, _, r)| r).collect())
    }

    pub fn batch_view(&self, repo: &str) -> Result<Read<BatchView>, LcError> {
        self.read(|s| {
            let open = Self::open_rounds(s, repo)?.into_iter().find(|r| r.get(FORGE_PHASE).is_some());
            Ok(BatchView { repo: repo.to_string(), open })
        })
    }

    pub fn round_view(&self, repo: &str) -> Result<Read<RoundView>, LcError> {
        self.read(|s| {
            let rounds = Self::open_rounds(s, repo)?.into_iter().map(|record| Round { phase: phase_of(&record.state), record }).collect();
            Ok(RoundView { repo: repo.to_string(), rounds })
        })
    }

    pub fn ready_counts(&self) -> Result<Read<ReadyCounts>, LcError> {
        self.read(|s| {
            let mut c = ReadyCounts::default();
            for b in s.beads.values() {
                *c.by_state.entry(b.row.state.clone()).or_default() += 1;
                if b.row.claimable() && b.row.holds.is_empty() {
                    c.claimable += 1;
                    c.judged_claimable += usize::from(b.verdict.as_deref() == Some("claimable"));
                    c.express += usize::from(b.row.express);
                }
            }
            Ok(c)
        })
    }
}
