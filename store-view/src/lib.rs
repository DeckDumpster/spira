//! The store's hot reads, held in memory (law-a-loops-hot-read-is-in-process): the rounds, the
//! open and the ended forge batches of each repo, and the ready counts, built from the store's own reads over
//! [`LcClient`] and kept current by its `watch` long-poll from the last revision seen. Every read
//! answers with the revision it reflects, and refuses once the store has not confirmed that
//! revision within the configured staleness: an old picture is never passed off as the present.
//!
//! A process that keeps the watch running [`install`]s its projection; every reader in it then
//! answers from [`shared`] memory and asks the store nothing (S7b). A process without one builds
//! its own and reads the store afresh, as before.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spira_store::lc_call::{LcClient, LcError, Reply, WATCH_HOLD_MS};
use spira_store::lc_state::{self, BatchState, Row};

/// How old a view's last confirmed revision may be before its reads refuse; a view the watch
/// keeps current may also be quiet for one watch hold ([`WATCH_HOLD_MS`]) beyond it.
pub const STALE_KEY: &str = "SPIRA_STORE_VIEW_STALE_MS";
/// The mark [`Projection::hold_watching`] sets; dropping it clears the mark.
pub struct Watching<'a>(&'a AtomicBool);

impl Drop for Watching<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// The pause after a poll the store did not answer, so a down service is not spun on.
pub const RETRY: Duration = Duration::from_secs(1);
/// The most ids one `list --ids` re-read names.
const IDS_PER_READ: usize = 200;

const BATCHES: &str = "batches/";
/// Every spelling of a state the batch machine has not ended, as a record can hold it (serde's
/// variant name, or the machine's word), so no spelling hides an open round.
const OPEN_STATES: [&str; 16] = [
    "Staged", "Open", "CiRunning", "Green", "Landing", "Deploying", "Attributing", "Rebuilding",
    "STAGED", "OPEN", "CI_RUNNING", "GREEN", "LANDING", "DEPLOYING", "ATTRIBUTING", "REBUILDING",
];
const ENDED_STATES: [&str; 8] = ["Landed", "Settled", "Abandoned", "Discarded", "LANDED", "SETTLED", "ABANDONED", "DISCARDED"];
/// The most open rounds the store is asked for across every repo; more is refused, never cut short.
const OPEN_MAX: usize = 512;
/// The most ended rounds of one repo `batch_view_ended` is answered from.
const ENDED_MAX: usize = 20_000;
const LIFECYCLE: &str = "lifecycle/";
const READINESS: &str = "readiness/";
const PLANNING: &str = "planning/";
const HISTORY: &str = "history/";
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
    /// The row as `list` printed it, so a served row is the one the store sent.
    raw: Value,
}

/// A filter over the held lifecycle rows, with `list`'s meaning: every field narrows.
#[derive(Debug, Default, Clone, Copy)]
pub struct RowQuery<'a> {
    /// Empty: any state.
    pub states: &'a [&'a str],
    pub repo: Option<&'a str>,
    /// `list --live`: the non-terminal rows, which are all the views hold.
    pub live: bool,
    pub ids: Option<&'a [String]>,
    /// A row whose holds contain this kind.
    pub hold: Option<&'a str>,
    pub express: bool,
}

/// The longest a `--window` answer is held: rounds leave a window by the clock alone, which no
/// watch event reports.
pub const WINDOW_HOLD: Duration = Duration::from_secs(60);

#[derive(Default)]
struct State {
    rev: Option<u64>,
    confirmed: Option<Instant>,
    /// The last confirmation was a watch answer, so a quiet hold is not staleness.
    watched: bool,
    batches: BTreeMap<String, Batch>,
    /// `None` after a batches-only refresh: no lifecycle row is held at the revision.
    beads: Option<BTreeMap<String, Bead>>,
}

/// The kinds of change that end a held answer: a planning write (it moves the planning watermark)
/// and a change to a round or its history.
const CONTENT: u8 = 1;
const ROUNDS: u8 = 2;
/// A watcher's condition set or cleared.
const CONDITIONS: u8 = 4;
const ALL: u8 = CONTENT | ROUNDS | CONDITIONS;
/// The watch prefix of the conditions each watcher keeps (`spira_lc::keys::conditions`).
pub const CONDITIONS_PREFIX: &str = "conditions/";

/// The reads [`Projection::answer`] holds, with the changes that end each; `None` for any other.
fn classes_of(argv: &[&str]) -> Option<u8> {
    match argv {
        ["content", "list" | "show", ..] | ["plan", "items" | "edges" | "children" | "watermark", ..] => Some(CONTENT),
        ["show-batch", _] => Some(ROUNDS),
        ["condition", "list", ..] => Some(CONDITIONS),
        ["list", rest @ ..] if rest.contains(&"--batches") => Some(ROUNDS | CONTENT),
        _ => None,
    }
}

/// The store's answers held under the changes that would end them. A generation per kind only
/// rises, so a read that started before a change cannot be kept after it.
#[derive(Default)]
struct Memo {
    held: HashMap<String, (u8, Reply, Instant)>,
    generation: [u64; 3],
}

impl Memo {
    fn seen(&self, classes: u8) -> u64 {
        (0..3).filter(|i| classes & (1 << i) != 0).map(|i| self.generation[i]).sum()
    }

    fn end(&mut self, classes: u8) {
        for i in (0..3).filter(|i| classes & (1 << i) != 0) {
            self.generation[i] += 1;
        }
        self.held.retain(|_, (held, ..)| *held & classes == 0);
    }
}

/// The most terminal rows kept: at the bound the set is dropped whole and refilled by what is asked next.
const TERMINAL_KEPT: usize = 4_096;

/// The terminal rows the store was asked for, kept until a change names them or the views reload:
/// the live set never holds one, and a reader that names one would otherwise ask the store each time.
#[derive(Default)]
struct Terminal {
    generation: u64,
    rows: HashMap<String, Value>,
}

impl Terminal {
    fn forget(&mut self, ids: Option<&[String]>) {
        self.generation += 1;
        match ids {
            Some(ids) => ids.iter().for_each(|id| drop(self.rows.remove(id))),
            None => self.rows.clear(),
        }
    }
}

pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The in-memory views and the watch that keeps them current. A read refuses once the last
/// confirmation is older than the configured staleness, plus one watch hold ([`WATCH_HOLD_MS`])
/// while that confirmation was a watch answer; a load, or a poll that failed, gets no extension.
pub struct Projection {
    client: LcClient,
    stale_after: Duration,
    clock: Clock,
    state: Mutex<State>,
    memo: Mutex<Memo>,
    terminal: Mutex<Terminal>,
    watching: AtomicBool,
    /// Why the last poll failed; cleared by one that applied.
    lost: Mutex<Option<String>>,
    /// Prefixes watched beside the fixed ones this view applies; their events are only reported.
    extra: Vec<String>,
}

/// What one [`Projection::poll`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Polled {
    /// The watch timed out with nothing new; the views are confirmed at `rev`.
    Quiet { rev: u64 },
    /// The watch's events were applied up to `through`; `keys` are their distinct keys, in answer order.
    Applied { through: u64, keys: Vec<String> },
    /// Every view was read afresh (the first load, nothing held, or a feed too old), at `rev`.
    Reloaded { rev: u64 },
}

static SHARED: OnceLock<Arc<Projection>> = OnceLock::new();

/// Makes `p` this process's projection for [`shared`]. Once only; the first stands.
pub fn install(p: Arc<Projection>) -> Arc<Projection> {
    Arc::clone(SHARED.get_or_init(|| p))
}

/// The projection this process keeps current, when it installed one.
pub fn shared() -> Option<Arc<Projection>> {
    SHARED.get().cloned()
}

/// [`Projection::list_answer`] on this process's projection; `None` when it keeps none.
pub fn held_list(argv: &[&str]) -> Option<Result<String, LcError>> {
    shared()?.list_answer(argv)
}

/// Any read this process's projection holds (a `list` or a memoised content, planning or round
/// read), as the store prints it; `None` for any other read or when it keeps no projection. A
/// stale view answers `Some(Err)`: the caller must not fall back to the store.
pub fn held_read(argv: &[&str]) -> Option<Result<String, LcError>> {
    let p = shared()?;
    if argv.first() == Some(&"list") {
        return p.list_answer(argv).or_else(|| ids_only(argv).map(|ids| p.ids_answer(&ids).map(|r| Value::Array(r.value).to_string())));
    }
    classes_of(argv)?;
    Some(p.answer(argv).and_then(|r| match r.value.code {
        0 => Ok(r.value.out),
        _ => Err(LcError::Fault(r.value.why())),
    }))
}

/// The ids of a `list --ids a,b` that names nothing else.
pub fn ids_only(argv: &[&str]) -> Option<Vec<String>> {
    match argv {
        ["list", "--ids", ids] => Some(ids.split(',').map(str::to_string).collect()),
        _ => None,
    }
}

/// `argv` answered by this process's projection when it holds the read, else by `client`. A stale
/// view is an error, never a fall back to the store.
pub fn call_held(client: &LcClient, argv: &[&str]) -> Result<Reply, String> {
    match held_read(argv) {
        Some(held) => held.map(|out| Reply { code: 0, out, err: String::new(), read_ended: false }).map_err(|e| e.to_string()),
        None => client.call(argv).map_err(String::from),
    }
}

/// `list --ids` rows, answered by this process's projection when it keeps one.
pub fn rows_by_ids(client: &LcClient, ids: &[String]) -> Result<Vec<Row>, String> {
    let Some(p) = shared() else { return lc_state::list_ids_on(client, ids) };
    let rows = p.ids_answer(ids).map_err(|e| e.to_string())?.value;
    lc_state::parse_rows(&Value::Array(rows).to_string())
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
            for (k, v) in own.into_iter().filter(|(k, _)| !matches!(k.as_str(), "batch_id" | "repo")) {
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

/// The batch records `rc list` answered, by key.
fn batches_of(items: Vec<Value>) -> BTreeMap<String, Batch> {
    items
        .into_iter()
        .map(|i| (text(&i, "key").unwrap_or_default(), Batch { create_rev: main_rev(&i["create_rev"]).unwrap_or(0), data: i["value"]["data"].clone() }))
        .collect()
}

/// What a watch delivery ends among the held answers: a planning write, a change to a round, or a
/// history entry of one (a bead's history ends nothing held).
fn ended_by(s: &State, events: &[Value]) -> u8 {
    let rounds: Vec<String> = s.batches.values().filter_map(|b| text(&b.data, "batch_id")).collect();
    events.iter().fold(0, |ended, e| {
        let key = e.get("key").and_then(Value::as_str).unwrap_or_default();
        ended
            | if key.starts_with(PLANNING) {
                CONTENT
            } else if key.starts_with(CONDITIONS_PREFIX) {
                CONDITIONS
            } else if key.starts_with(BATCHES) || key.strip_prefix(HISTORY).is_some_and(|h| rounds.iter().any(|r| h.strip_prefix(r.as_str()).is_some_and(|rest| rest.starts_with('/')))) {
                ROUNDS
            } else {
                0
            }
    })
}

/// The beads `raw` waits on as `list` printed it: its parent, declared blockers and judged blocker.
fn waits_on(raw: &Value) -> impl Iterator<Item = &str> {
    fn ids(v: &Value) -> impl Iterator<Item = &str> {
        v.as_array().into_iter().flatten().filter_map(Value::as_str)
    }
    let claim = &raw["claim"];
    claim["parent"].as_str().into_iter().chain(ids(&claim["blocks"])).chain(ids(&raw["blocked_by"])).chain(raw["claim_view"]["blocker"].as_str())
}

/// `changed` and every held bead that waits on one of them, however deep: a bead's verdict is
/// judged from its blockers, so their change is its change though its own keys never move.
fn with_waiters(beads: &BTreeMap<String, Bead>, mut changed: BTreeSet<String>) -> BTreeSet<String> {
    loop {
        let more: Vec<String> = beads.iter().filter(|(id, b)| !changed.contains(*id) && waits_on(&b.raw).any(|w| changed.contains(w))).map(|(id, _)| id.clone()).collect();
        if more.is_empty() {
            return changed;
        }
        changed.extend(more);
    }
}

/// The beads `list` answered, with the claim view's verdict on each.
fn beads_of(raw: &str) -> Result<Vec<(String, Bead)>, LcError> {
    let rows = lc_state::parse_rows(raw).map_err(fault)?;
    let verdicts: BTreeMap<String, String> = match json_of(raw, "list")? {
        Value::Array(a) => a.iter().filter_map(|r| Some((text(r, "bead_id")?, text(r.get("claim_view")?, "verdict")?))).collect(),
        _ => BTreeMap::new(),
    };
    let raws: BTreeMap<String, Value> = match json_of(raw, "list")? {
        Value::Array(a) => a.into_iter().filter_map(|r| Some((text(&r, "bead_id")?, r))).collect(),
        _ => BTreeMap::new(),
    };
    Ok(rows
        .into_iter()
        .map(|row| {
            let (verdict, raw) = (verdicts.get(&row.bead_id).cloned(), raws.get(&row.bead_id).cloned().unwrap_or(Value::Null));
            (row.bead_id.clone(), Bead { verdict, raw, row })
        })
        .collect())
}

impl Projection {
    pub fn new(client: LcClient, stale_after: Duration, clock: Clock) -> Self {
        Projection { client, stale_after, clock, state: Mutex::new(State::default()), memo: Mutex::new(Memo::default()), terminal: Mutex::new(Terminal::default()), watching: AtomicBool::new(false), lost: Mutex::new(None), extra: Vec::new() }
    }

    /// Also watches `prefixes`: events under them are reported by [`poll`](Self::poll) and not applied.
    pub fn with_prefixes(mut self, prefixes: &[&str]) -> Self {
        for p in prefixes {
            if ![BATCHES, LIFECYCLE, READINESS, PLANNING, HISTORY, CONDITIONS_PREFIX].contains(p) && !self.extra.iter().any(|e| e == p) {
                self.extra.push(p.to_string());
            }
        }
        self
    }

    /// This process's installed projection, else a fresh one on `client`. Pair it with
    /// [`batches_current`](Self::batches_current).
    pub fn hot(client: LcClient) -> Result<Arc<Projection>, String> {
        match shared() {
            Some(p) => Ok(p),
            None => Projection::configured(client).map(Arc::new),
        }
    }

    /// Marks this projection as kept current by a watch until the guard drops.
    pub fn hold_watching(&self) -> Watching<'_> {
        self.watching.store(true, Ordering::Relaxed);
        Watching(&self.watching)
    }

    /// Whether a watch is keeping this projection current.
    pub fn watching(&self) -> bool {
        self.watching.load(Ordering::Relaxed)
    }

    /// The batch views current: nothing to do when the watch keeps them so (a read then refuses
    /// if the watch fell behind); otherwise the batch records are read afresh.
    pub fn batches_current(&self) -> Result<(), LcError> {
        if self.watching() {
            Ok(())
        } else {
            self.refresh_batches()
        }
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

    fn terminal(&self) -> std::sync::MutexGuard<'_, Terminal> {
        self.terminal.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn end_held(&self, classes: u8) {
        self.memo.lock().unwrap_or_else(|e| e.into_inner()).end(classes);
    }

    fn rc_list(&self, prefix: &str, states: &[&str], limit: usize) -> Result<(Vec<Value>, u64), LcError> {
        let body = json!({ "prefixes": [prefix], "selector": "everything", "states": states, "limit": limit }).to_string();
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
    /// the live lifecycle rows. A change after that revision is redelivered by the next watch.
    pub fn load(&self) -> Result<(), LcError> {
        self.load_at(false).map(|_| ())
    }

    fn load_at(&self, watched: bool) -> Result<u64, LcError> {
        let (items, rev) = self.rc_list(BATCHES, &OPEN_STATES, OPEN_MAX)?;
        let beads = beads_of(&self.client.answer(&["list", "--live"])?)?;
        let batches = batches_of(items);
        let mut s = self.lock();
        self.end_held(ALL);
        self.terminal().forget(None);
        *s = State { rev: Some(rev), confirmed: Some((self.clock)()), watched, batches, beads: Some(beads.into_iter().collect()) };
        Ok(rev)
    }

    /// Reads the batch records afresh and nothing else: the round and batch views answer at the
    /// revision they were listed at, and the ready counts refuse until a full [`load`](Self::load),
    /// since rows read earlier are not of that revision. A poll on such a picture loads every view.
    pub fn refresh_batches(&self) -> Result<(), LcError> {
        let (items, rev) = self.rc_list(BATCHES, &OPEN_STATES, OPEN_MAX)?;
        let batches = batches_of(items);
        let mut s = self.lock();
        self.end_held(ALL);
        self.terminal().forget(None);
        *s = State { rev: Some(rev), confirmed: Some((self.clock)()), watched: false, batches, beads: None };
        Ok(())
    }

    /// One step of keeping current: a load when nothing is held, else one watch from the held
    /// revision. Nothing moves unless the whole answer applied.
    pub fn poll(&self) -> Result<Polled, LcError> {
        let answer = self.watch_argv().map(|argv| self.client.answer(&argv));
        self.apply_watch(answer)
    }

    /// The watch to send from the held revision; `None` when nothing is held and a load comes first.
    pub fn watch_argv(&self) -> Option<Vec<String>> {
        let from = {
            let s = self.lock();
            s.rev.filter(|_| s.beads.is_some())
        }?;
        let hold = if self.lock().watched { WATCH_HOLD_MS } else { 0 };
        let mut argv = vec!["watch".to_string()];
        argv.extend(self.watch_prefixes().into_iter().map(String::from));
        argv.extend(["--from-rev".into(), from.to_string(), "--wait-ms".into(), hold.to_string(), "--rows".into()]);
        Some(argv)
    }

    /// Applies the answer to [`watch_argv`](Self::watch_argv)'s watch (`None`: it had none, so load).
    /// A failed answer or apply marks the watch lost and moves nothing, so the next watch resumes
    /// from the held revision.
    pub fn apply_watch(&self, from: Option<Result<String, LcError>>) -> Result<Polled, LcError> {
        let polled = self.apply_once(from);
        if polled.is_err() {
            self.lock().watched = false;
        }
        *self.lost.lock().unwrap_or_else(|e| e.into_inner()) = polled.as_ref().err().map(ToString::to_string);
        polled
    }

    /// The prefixes the watch names: the fixed six, then those added by [`Self::with_prefixes`].
    pub fn watch_prefixes(&self) -> Vec<&str> {
        let mut all = vec![BATCHES, LIFECYCLE, READINESS, PLANNING, HISTORY, CONDITIONS_PREFIX];
        all.extend(self.extra.iter().map(String::as_str));
        all
    }

    fn apply_once(&self, answer: Option<Result<String, LcError>>) -> Result<Polled, LcError> {
        let Some(answer) = answer else { return self.load_at(true).map(|rev| Polled::Reloaded { rev }) };
        let from = {
            let s = self.lock();
            s.rev.filter(|_| s.beads.is_some())
        };
        let answer = answer?;
        let Some(from) = from else { return self.load_at(true).map(|rev| Polled::Reloaded { rev }) };
        let w = json_of(&answer, "watch")?;
        if let Some(p) = w["progress_rev"].as_u64() {
            let mut s = self.lock();
            s.rev = Some(p.max(from));
            s.confirmed = Some((self.clock)());
            s.watched = true;
            return Ok(Polled::Quiet { rev: p.max(from) });
        }
        if w.get("too_old").is_some() || w.get("future_rev").is_some() {
            return self.load_at(true).map(|rev| Polled::Reloaded { rev });
        }
        let (Some(events), Some(through)) = (w["events"].as_array(), w["through"].as_u64()) else {
            return Err(fault(format!("watch: unreadable answer {w}")));
        };
        let mut batch_puts: Vec<(String, Option<Value>, Option<u64>)> = Vec::new();
        let mut dirty = BTreeSet::new();
        let mut keys: Vec<String> = Vec::new();
        for e in events {
            let key = text(e, "key").unwrap_or_default();
            if !keys.contains(&key) {
                keys.push(key.clone());
            }
            if key.starts_with(BATCHES) {
                let value = e.get("value").filter(|v| !v.is_null()).map(|v| v["data"].clone());
                batch_puts.push((key, value, e.get("create_rev").and_then(main_rev)));
            } else if let Some(id) = key.strip_prefix(LIFECYCLE).or_else(|| key.strip_prefix(READINESS)) {
                dirty.insert(id.to_string());
            }
        }
        let named = if w.get("rows").is_some() { dirty.clone() } else { BTreeSet::new() };
        let carried: BTreeMap<String, Bead> = match w.get("rows") {
            Some(rows) => beads_of(&rows.to_string())?.into_iter().collect(),
            None => BTreeMap::new(),
        };
        if let Some(beads) = self.lock().beads.as_ref() {
            dirty = with_waiters(beads, dirty);
        }
        let dirty: Vec<String> = dirty.into_iter().collect();
        let mut fresh = carried;
        let wanted: Vec<String> = dirty.iter().filter(|id| !named.contains(*id)).cloned().collect();
        fresh.extend(self.list_ids(&wanted)?);
        let mut s = self.lock();
        self.end_held(ended_by(&s, events));
        self.terminal().forget(Some(&dirty));
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
        if let Some(beads) = s.beads.as_mut() {
            for id in dirty {
                match fresh.get(&id).filter(|b| !b.row.terminal()) {
                    Some(b) => beads.insert(id, b.clone()),
                    None => beads.remove(&id),
                };
            }
        }
        s.rev = Some(through.max(from));
        s.confirmed = Some((self.clock)());
        s.watched = true;
        Ok(Polled::Applied { through: through.max(from), keys })
    }

    /// Polls until `stop` is set, each poll one watch of [`WATCH_HOLD_MS`]; pausing after a poll the store did not answer; `observe` sees
    /// each poll that succeeded.
    pub fn run(&self, stop: &AtomicBool, observe: &mut dyn FnMut(&Polled)) {
        let _watching = self.hold_watching();
        while !stop.load(Ordering::Relaxed) {
            match self.poll() {
                Ok(polled) => observe(&polled),
                Err(_) => std::thread::sleep(RETRY),
            }
        }
    }

    fn read<T>(&self, f: impl FnOnce(&State) -> Result<T, LcError>) -> Result<Read<T>, LcError> {
        let s = self.lock();
        let (Some(rev), Some(at)) = (s.rev, s.confirmed) else {
            return Err(LcError::Refused("the store views are not loaded yet".into()));
        };
        let age = (self.clock)().saturating_duration_since(at);
        let bound = if s.watched { self.stale_after + Duration::from_millis(WATCH_HOLD_MS) } else { self.stale_after };
        if age > bound {
            let lost = self.lost.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|why| format!("; the watch is lost: {why}")).unwrap_or_default();
            return Err(LcError::Refused(format!(
                "stale past {}ms: the views hold revision {rev}, last confirmed {}ms ago{lost}",
                bound.as_millis(),
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

    /// The repo's forge batches the machine or the queue has ended, newest opened first: what
    /// `queue batch show --ended` reads from the store (the queue's closed-PR archives on disk are
    /// not the store's, and are not here).
    pub fn batch_view_ended(&self, repo: &str) -> Result<Read<Vec<Record>>, LcError> {
        let held = self.read(|s| Ok(s.batches.clone()))?;
        let (items, _) = self.rc_list(&format!("{BATCHES}{repo}/"), &ENDED_STATES, ENDED_MAX)?;
        let mut batches = batches_of(items);
        batches.extend(held.value);
        let mut ended: Vec<(i64, u64, Record)> = Vec::new();
        for (key, b) in batches.iter().filter(|(k, _)| round_id(repo, k).is_some()) {
            let r = decode(key, &b.data)?;
            if r.get(FORGE_PHASE).is_none() || !(r.terminal() || r.ended().is_some()) {
                continue;
            }
            let opened = r.get("opened").and_then(|o| o.parse().ok()).unwrap_or(0);
            ended.push((opened, b.create_rev, r));
        }
        ended.sort_by(|a, b| (b.0, b.1, &b.2.id).cmp(&(a.0, a.1, &a.2.id)));
        Ok(Read { rev: held.rev, value: ended.into_iter().map(|(_, _, r)| r).collect() })
    }

    pub fn round_view(&self, repo: &str) -> Result<Read<RoundView>, LcError> {
        self.read(|s| {
            let rounds = Self::open_rounds(s, repo)?.into_iter().map(|record| Round { phase: phase_of(&record.state), record }).collect();
            Ok(RoundView { repo: repo.to_string(), rounds })
        })
    }

    pub fn ready_counts(&self) -> Result<Read<ReadyCounts>, LcError> {
        self.read(|s| {
            let beads = s.beads.as_ref().ok_or_else(|| LcError::Refused("the lifecycle rows are not loaded: the views hold the batch records only".into()))?;
            let mut c = ReadyCounts::default();
            for b in beads.values() {
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

    /// The held lifecycle rows `q` keeps, in id order, as `list` printed them; `None` when `q`
    /// reaches a row the views do not hold (a terminal one), which the caller asks the store.
    pub fn rows(&self, q: &RowQuery) -> Option<Result<Read<Vec<Value>>, LcError>> {
        let live_only = q.live || (!q.states.is_empty() && q.states.iter().all(|s| !lc_state::is_terminal(s)));
        if !live_only {
            let ids = q.ids?;
            let s = self.lock();
            if s.beads.as_ref().is_some_and(|held| ids.iter().any(|i| !held.contains_key(i))) {
                return None;
            }
        }
        Some(self.read(|s| {
            let beads = s.beads.as_ref().ok_or_else(|| LcError::Refused("the lifecycle rows are not loaded: the views hold the batch records only".into()))?;
            let kept = |b: &&Bead| {
                (q.states.is_empty() || q.states.contains(&b.row.state.as_str()))
                    && q.repo.is_none_or(|r| b.raw.get("repo").and_then(Value::as_str) == Some(r))
                    && q.ids.is_none_or(|ids| ids.contains(&b.row.bead_id))
                    && q.hold.is_none_or(|h| b.row.held(h))
                    && (!q.express || b.row.express)
            };
            Ok(beads.values().filter(kept).map(|b| b.raw.clone()).collect())
        }))
    }

    /// [`rows`](Self::rows), and for a query that only names ids, the terminal rows among them too
    /// ([`ids_answer`](Self::ids_answer)): `None` only for a read the views cannot answer.
    pub fn rows_held(&self, q: &RowQuery) -> Option<Result<Read<Vec<Value>>, LcError>> {
        let by_ids_only = q.states.is_empty() && q.repo.is_none() && !q.live && q.hold.is_none() && !q.express;
        self.rows(q).or_else(|| q.ids.filter(|_| by_ids_only).map(|ids| self.ids_answer(ids)))
    }

    /// `list --ids`'s rows for `ids`, in id order: the held live rows, the terminal rows already
    /// asked for, and the store for the rest, once. A terminal row is kept until a change names it.
    pub fn ids_answer(&self, ids: &[String]) -> Result<Read<Vec<Value>>, LcError> {
        let (rev, held, unheld) = {
            let r = self.read(|s| {
                let beads = s.beads.as_ref().ok_or_else(|| LcError::Refused("the lifecycle rows are not loaded: the views hold the batch records only".into()))?;
                let (held, unheld): (Vec<&String>, Vec<&String>) = ids.iter().partition(|id| beads.contains_key(*id));
                Ok((held.iter().map(|id| beads[*id].raw.clone()).collect::<Vec<_>>(), unheld.into_iter().cloned().collect::<Vec<_>>()))
            })?;
            (r.rev, r.value.0, r.value.1)
        };
        let mut rows = held;
        let (remembered, generation) = {
            let t = self.terminal();
            (unheld.iter().filter_map(|id| t.rows.get(id).cloned()).collect::<Vec<_>>(), t.generation)
        };
        let known: BTreeSet<&str> = remembered.iter().filter_map(|r| r["bead_id"].as_str()).collect();
        let asked: Vec<String> = unheld.iter().filter(|id| !known.contains(id.as_str())).cloned().collect();
        rows.extend(remembered.iter().cloned());
        let mut learned = Vec::new();
        for chunk in asked.chunks(IDS_PER_READ) {
            let text = self.client.answer(&["list", "--ids", &chunk.join(",")])?;
            match json_of(&text, "list")? {
                Value::Array(found) => learned.extend(found),
                other => return Err(fault(format!("list: not an array: {other}"))),
            }
        }
        {
            let mut t = self.terminal();
            if t.generation == generation {
                for row in learned.iter().filter(|r| r["state"].as_str().is_some_and(lc_state::is_terminal)) {
                    if t.rows.len() >= TERMINAL_KEPT {
                        t.rows.clear();
                    }
                    if let Some(id) = row["bead_id"].as_str() {
                        t.rows.insert(id.to_string(), row.clone());
                    }
                }
            }
        }
        rows.extend(learned);
        rows.sort_by(|a, b| a["bead_id"].as_str().cmp(&b["bead_id"].as_str()));
        Ok(Read { rev, value: rows })
    }

    /// `argv`'s answer when it is a `list` wholly inside the held live rows, rendered as the
    /// store prints it; `None` for any other read, which the caller asks of the store. A stale
    /// view answers `Some(Err)`: the caller must not fall back to the store.
    pub fn list_answer(&self, argv: &[&str]) -> Option<Result<String, LcError>> {
        let [verb, rest @ ..] = argv else { return None };
        if *verb != "list" {
            return None;
        }
        if rest.contains(&"--batches") {
            return Some(self.answer(argv).and_then(|r| match r.value.code {
                0 => Ok(r.value.out),
                _ => Err(LcError::Fault(r.value.why())),
            }));
        }
        let mut q = RowQuery::default();
        let mut states: Vec<&str> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let mut has_ids = false;
        let mut it = rest.iter();
        while let Some(&tok) = it.next() {
            match tok {
                "--live" => q.live = true,
                "--express" => q.express = true,
                "--state" => states = it.next()?.split(',').collect(),
                "--repo" => q.repo = Some(it.next()?),
                "--hold" => q.hold = Some(it.next()?),
                "--ids" => {
                    has_ids = true;
                    ids = it.next()?.split(',').map(str::to_string).collect();
                }
                _ => return None,
            }
        }
        q.states = &states;
        q.ids = has_ids.then_some(ids.as_slice());
        Some(self.rows(&q)?.map(|r| Value::Array(r.value).to_string()))
    }

    /// The store's answer to a content, planning, `show-batch` or `list --batches` read: held until
    /// a change the watch delivers could alter it, else asked of the store once and held. A process
    /// without a watch asks the store each time; one whose watch fell behind is refused.
    pub fn answer(&self, argv: &[&str]) -> Result<Read<Reply>, LcError> {
        let classes = classes_of(argv).ok_or_else(|| LcError::Refused(format!("`{}` is not a read the views hold", argv.join(" "))))?;
        if !self.watching() {
            return Ok(Read { rev: 0, value: self.client.call(argv)? });
        }
        let fresh = self.read(|_| Ok(()))?;
        let key = argv.join("\u{1f}");
        let before = {
            let m = self.memo.lock().unwrap_or_else(|e| e.into_inner());
            let aged_out = |at: &Instant| argv.contains(&"--window") && (self.clock)().saturating_duration_since(*at) > WINDOW_HOLD;
            if let Some((_, reply, _)) = m.held.get(&key).filter(|(_, _, at)| !aged_out(at)) {
                return Ok(Read { rev: fresh.rev, value: reply.clone() });
            }
            m.seen(classes)
        };
        let reply = self.client.call(argv)?;
        if reply.code == 0 {
            let mut m = self.memo.lock().unwrap_or_else(|e| e.into_inner());
            if m.seen(classes) == before {
                m.held.insert(key, (classes, reply.clone(), (self.clock)()));
            }
        }
        Ok(Read { rev: fresh.rev, value: reply })
    }

    /// `show <id>`'s answer, `{"bead": row}`, from the held row (it carries the `state`, `stack` and
    /// `tip` a show does); `None` when the row is not held, which the caller asks the store.
    pub fn show(&self, id: &str) -> Option<Result<Read<Value>, LcError>> {
        let ids = [id.to_string()];
        let rows = self.rows(&RowQuery { ids: Some(&ids), ..Default::default() })?;
        Some(rows.map(|r| Read { rev: r.rev, value: json!({ "bead": r.value.into_iter().next().unwrap_or(Value::Null) }) }))
    }

    /// The ids of the rows in `state` for `repo` that hold nothing.
    pub fn unheld(&self, state: &str, repo: &str) -> Result<Read<Vec<String>>, LcError> {
        self.read(|s| {
            let beads = s.beads.as_ref().ok_or_else(|| LcError::Refused("the lifecycle rows are not loaded: the views hold the batch records only".into()))?;
            Ok(beads
                .values()
                .filter(|b| b.row.state == state && b.row.holds.is_empty() && b.raw.get("repo").and_then(Value::as_str) == Some(repo))
                .map(|b| b.row.bead_id.clone())
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tt3dtmn)]
    #[test]
    fn the_state_lists_name_every_spelling_of_each_batch_state_the_store_writes() {
        let words = ["STAGED", "OPEN", "CI_RUNNING", "GREEN", "LANDING", "DEPLOYING", "ATTRIBUTING", "REBUILDING", "LANDED", "SETTLED", "ABANDONED", "DISCARDED"];
        for word in words {
            let state = BatchState::from_str(word).unwrap();
            let serde_name = serde_json::to_value(state).unwrap();
            let list: &[&str] = if state.is_terminal() { &ENDED_STATES } else { &OPEN_STATES };
            let other: &[&str] = if state.is_terminal() { &OPEN_STATES } else { &ENDED_STATES };
            for spelling in [word, serde_name.as_str().unwrap()] {
                assert!(list.contains(&spelling), "{spelling} missing from its list");
                assert!(!other.contains(&spelling), "{spelling} in the wrong list");
            }
        }
        assert_eq!(OPEN_STATES.len() + ENDED_STATES.len(), words.len() * 2);
    }
}
