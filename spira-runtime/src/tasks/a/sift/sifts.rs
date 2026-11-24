//! The Sift's reads over every queue repository. Each SUBMITTED or
//! CERTIFIED bead whose recorded tip is still its branch's live tip is screened by the `sift`
//! crate: its pass is recorded on the lifecycle store, or it is sent back, superseded or held
//! out. On one pool its verdicts are `batcher sifts`'s, line for line.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

use serde_json::Value;
use sift::{Candidate, GitProbe};
use spira_store::lc_call::LcClient;
use spira_config::repos::Registry;

const WHO: &str = "sift";

struct Repo {
    name: String,
    path: PathBuf,
    base: String,
    local: bool,
}

fn find_repo(reg: &Registry, name: &str, local: bool) -> Result<Repo, String> {
    let path = reg.root(name).unwrap_or_default();
    if path.is_empty() {
        return Err(format!("{name}: no checkout path is registered"));
    }
    let base = spira_config::repos::landref(reg, name).unwrap_or_default();
    if base.is_empty() {
        return Err(format!(
            "{name}: spira_landref could not resolve a base ref"
        ));
    }
    Ok(Repo {
        name: name.to_string(),
        path: PathBuf::from(path),
        base,
        local,
    })
}

fn run(cmd: &mut impl spira_config::bounded::Runnable, what: &str) -> Result<String, String> {
    let o = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let err = err.lines().last().unwrap_or("").trim();
        return Err(format!(
            "{what} exited {}{}",
            o.status.code().unwrap_or(-1),
            if err.is_empty() {
                String::new()
            } else {
                format!(": {err}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}

fn lcq_with(lc: &LcClient, args: &[&str]) -> Result<String, String> {
    lc_answer(lc.call(args))
}

/// An answer as the `spira-lc` child's was read: its stdout when it applied; otherwise its
/// exit and the last line it said.
fn lc_answer(
    r: Result<spira_store::lc_call::Reply, spira_store::lc_call::LcError>,
) -> Result<String, String> {
    let r = r.map_err(|e| format!("spira-lc: {e}"))?;
    if r.code != 0 {
        let said = if r.err.trim().is_empty() { &r.out } else { &r.err };
        let last = said.lines().last().unwrap_or("").trim();
        return Err(format!(
            "spira-lc exited {}{}",
            r.code,
            if last.is_empty() {
                String::new()
            } else {
                format!(": {last}")
            }
        ));
    }
    Ok(r.out)
}

fn lcq_held_with(lc: &LcClient, args: &[&str]) -> Result<String, String> {
    match store_view::shared() {
        Some(views) => lc_answer(views.answer(args).map(|r| r.value)),
        None => lcq_with(lc, args),
    }
}

/// The held lifecycle rows `q` keeps, as `list` printed them; `None` when the process keeps no
/// watch or `q` reaches rows it does not hold.
fn held_rows(q: &store_view::RowQuery) -> Option<Result<Vec<Value>, String>> {
    let rows = store_view::shared()?.rows_held(q)?;
    Some(rows.map(|r| r.value).map_err(|e| format!("could not say: {e}")))
}

fn parsed(text: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("{what}: unparsed reply: {e}"))
}

fn str_of<'a>(r: &'a Value, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str)
}

fn in_pool_states(r: &Value) -> bool {
    matches!(str_of(r, "state"), Some("CERTIFIED" | "SUBMITTED"))
}

fn holds_empty(r: &Value) -> bool {
    match r.get("holds") {
        None | Some(Value::Null) => true,
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::String(t)) => serde_json::from_str::<Vec<Value>>(t).map(|a| a.is_empty()).unwrap_or(false),
        Some(_) => false,
    }
}

/// Any hold but manual keeps a bead out of the screen; an unreadable `holds` fails closed.
fn holds_block_screen(r: &Value) -> bool {
    let blocks = |a: &[Value]| {
        a.iter().any(|h| h.as_str().and_then(spira_store::lc_state::hold_kind).map(|k| k.as_str()) != Some("manual"))
    };
    match r.get("holds") {
        None | Some(Value::Null) => false,
        Some(Value::Array(a)) => blocks(a),
        Some(Value::String(t)) => serde_json::from_str::<Vec<Value>>(t).map(|a| blocks(&a)).unwrap_or(true),
        Some(_) => true,
    }
}

fn ejected_at_tip(r: &Value) -> bool {
    str_of(r, "ejected_red_tip").is_some() && r.get("ejected_red_tip") == r.get("tip")
}

/// `(id, tip)` of every pool row not held other than manual, not ejected at its tip, by id.
fn read_pool(rows: &[Value]) -> Vec<(String, String)> {
    let mut pool: Vec<(String, String)> = rows
        .iter()
        .filter(|r| in_pool_states(r) && !holds_block_screen(r))
        .filter_map(|r| {
            let id = str_of(r, "bead_id")?.to_string();
            let tip = str_of(r, "tip").unwrap_or("none").to_string();
            (str_of(r, "ejected_red_tip") != Some(tip.as_str())).then_some((id, tip))
        })
        .collect();
    pool.sort();
    pool.dedup_by(|a, b| a.0 == b.0);
    pool
}

/// `refs/heads/spira/*` of the repository's own checkout, by bead id: what scopes the
/// machine's pool to this repository, and the live tip a recorded tip must still be.
fn branch_tips(repo: &Repo) -> Result<BTreeMap<String, String>, String> {
    if let Some(tips) = spira_config::git_refs::spira_tips(&repo.path) {
        return Ok(tips);
    }
    let out = run(
        spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&repo.path)
            .args([
                "for-each-ref",
                "--format=%(refname:short) %(objectname)",
                "refs/heads/spira/*",
            ]),
        "git for-each-ref",
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let (name, sha) = l.split_once(' ')?;
            Some((
                name.strip_prefix("spira/")?.to_string(),
                sha.trim().to_string(),
            ))
        })
        .collect())
}

struct Member {
    id: String,
    tip: String,
    stacked_on: Vec<String>,
}

/// The ids of the pool rows not held other than manual, whose recorded tip is their branch's live tip.
fn at_live_tips(rows: &[Value], branches: &BTreeMap<String, String>) -> Vec<String> {
    read_pool(rows).into_iter().filter(|(id, tip)| branches.get(id) == Some(tip)).map(|(id, _)| id).collect()
}

/// The pool, pure: of the `rows` not held other than manual, not ejected at their tip, those at their branch's live
/// tip with a content row (`shown`), less every member stacked (by `stacks`, its unfinished
/// prerequisites) on a bead held or ejected at its tip in `rows` or `outside`, on one whose tip
/// moved, or on a member dropped for that reason.
pub fn select_pool(rows: &[Value], outside: &[Value], branches: &BTreeMap<String, String>, shown: &BTreeSet<String>, stacks: &BTreeMap<String, Vec<String>>) -> Vec<Candidate> {
    let (current, moved): (Vec<_>, Vec<_>) = read_pool(rows).into_iter().partition(|(id, tip)| branches.get(id) == Some(tip));
    let mut barred: BTreeSet<String> = moved.into_iter().map(|(id, _)| id).collect();
    let members: Vec<Member> = current.into_iter().filter(|(id, _)| shown.contains(id)).map(|(id, tip)| Member { stacked_on: stacks.get(&id).cloned().unwrap_or_default(), id, tip }).collect();
    barred.extend(rows.iter().chain(outside).filter(|r| !holds_empty(r) || ejected_at_tip(r)).filter_map(|r| str_of(r, "bead_id").map(str::to_string)));
    drop_stacked_on_barred(members, barred).into_iter().map(|m| Candidate { id: m.id, tip: m.tip }).collect()
}

/// The prerequisites of `stack` not yet finished by `state` (`spira_config::edges::blocker_finished`);
/// an unread prerequisite is live, the safe side.
pub fn live_prereqs(stack: &[String], state: &BTreeMap<String, String>) -> Vec<String> {
    stack.iter().filter(|k| !spira_config::edges::blocker_finished(state.get(*k).and_then(|s| lifecycle::bead::BeadState::from_str(s)), None)).cloned().collect()
}

/// The prerequisite ids a `show` answer's recorded stack names.
fn stack_of(show: &Value) -> Vec<String> {
    let stack = match show.get("bead").and_then(|b| b.get("stack")) {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => return Vec::new(),
    };
    stack.as_object().map(|o| o.iter().filter(|(_, v)| v.is_string()).map(|(k, _)| k.clone()).collect::<BTreeSet<String>>().into_iter().collect()).unwrap_or_default()
}

/// Drops every member stacked on a barred bead, or on a member dropped for that reason.
fn drop_stacked_on_barred(mut kept: Vec<Member>, mut barred: BTreeSet<String>) -> Vec<Member> {
    while let Some(i) = kept
        .iter()
        .position(|m| m.stacked_on.iter().any(|p| barred.contains(p)))
    {
        let m = kept.remove(i);
        let p = m
            .stacked_on
            .iter()
            .find(|p| barred.contains(*p))
            .cloned()
            .unwrap_or_default();
        eprintln!("{WHO}: skipped {}: stacked on held {p}", m.id);
        barred.insert(m.id);
    }
    kept
}

/// The `items` of a `plan items` answer.
fn plan_items(text: &str) -> Result<Value, String> {
    let mut v: Value =
        serde_json::from_str(text).map_err(|e| format!("plan items: unparsed output: {e}"))?;
    match v.get_mut("items").map(Value::take) {
        Some(items @ Value::Array(_)) => Ok(items),
        _ => Err(format!(
            "plan items: no items in {}",
            text.chars().take(200).collect::<String>()
        )),
    }
}

/// `repo`'s open batch record from `views`, its batch records current (read afresh unless a watch keeps them so). A read the store
/// refused, or a view it cannot vouch for, could not say: it is never read as no batch.
fn shown_in(views: &store_view::Projection, repo: &str) -> Result<String, String> {
    views.batches_current().map_err(|e| format!("could not say: {e}"))?;
    views.batch_view(repo).map(|r| r.value.shown()).map_err(|e| format!("could not say: {e}"))
}

/// The members of the repository's open round, whose patches a candidate may duplicate, read
/// from the queue's record of its open batch (`show`).
fn open_round(
    show: &dyn Fn(&str) -> Result<String, String>,
    repo: &str,
) -> Result<Vec<Candidate>, String> {
    let text = show(repo)?;
    let kv: BTreeMap<&str, &str> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    if kv.get("pr").is_none_or(|pr| pr.is_empty()) {
        return Ok(Vec::new());
    }
    Ok(kv
        .get("members")
        .copied()
        .unwrap_or("")
        .split_whitespace()
        .map(|t| {
            let (id, tip) = t.split_once(':').unwrap_or((t, ""));
            Candidate {
                id: id.to_string(),
                tip: tip.to_string(),
            }
        })
        .collect())
}

/// Fetches the base's remote so the screen judges against what the remote holds; a local
/// landing ref has none. A failed fetch refuses: an unrefreshed base is the wrong base.
fn fetch_base(repo: &Repo) -> Result<(), String> {
    if repo.local {
        return Ok(());
    }
    let remote = repo
        .base
        .split_once('/')
        .map(|(r, _)| r)
        .unwrap_or("origin");
    run(
        spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&repo.path)
            .args(["fetch", "-q", remote]),
        "git fetch base",
    )
    .map(|_| ())
}

struct Shown {
    state: String,
    version: Option<u64>,
    tip: Option<String>,
}

fn show(lc: &LcClient, id: &str) -> Result<Shown, String> {
    let v = parsed(&lcq_with(lc, &["show", id])?, "show")?; // BDQ_READ_CONSIDERED="LcClient is the spira-lc door; no bdq"
    let bead = v.get("bead").cloned().unwrap_or_default();
    Ok(Shown {
        state: str_of(&bead, "state").unwrap_or("").to_string(),
        version: bead.get("version").and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        }),
        tip: str_of(&bead, "tip").map(str::to_string),
    })
}

fn bead_event(lc: &LcClient, id: &str, state: &str, version: u64, kind: Value) -> Result<(), String> {
    let (version, kind) = (version.to_string(), kind.to_string());
    lcq_with(
        lc,
        &[
            "event",
            "bead",
            id,
            "--expect",
            state,
            "--version",
            &version,
            "--actor",
            "sift",
            "--kind",
            &kind,
        ],
    )
    .map(|_| ())
}

/// Keeps `reason` as a send-back log entry of `id`; a failure is said and never stops the send-back.
pub fn keep_sift_logs(lc: &LcClient, run: &std::path::Path, id: &str, reason: &str) {
    let ask = |a: &[String]| lcq_with(lc, &a.iter().map(String::as_str).collect::<Vec<_>>());
    if let Err(e) = spira_config::ejection::keep_sift(&ask, run, id, reason) {
        eprintln!("sift: {id} send-back logs not kept: {e}");
    }
}

/// Sends `id` back with `GateRed(reason)`, only while it is still the SUBMITTED or CERTIFIED
/// bead at the tip the screen judged.
pub fn lc_gate_red(lc: &LcClient, id: &str, tip: &str, reason: &str) -> Result<(), String> {
    let b = show(lc, id)?;
    let version = b.version.ok_or("show: no version")?;
    if !matches!(b.state.as_str(), "SUBMITTED" | "CERTIFIED") || b.tip.as_deref() != Some(tip) {
        return Err(format!("{id} moved since screening (state {})", b.state));
    }
    bead_event(
        lc,
        id,
        &b.state,
        version,
        serde_json::json!({"GateRed": {"tip": tip, "reason": reason}}),
    )
}

/// The `send-back` fact of a sift send-back: the note it stands on, at the tip it judged.
pub fn lc_send_back_fact(lc: &LcClient, id: &str, tip: &str, text: &str) -> Result<(), String> {
    let cause = spira_config::send_back::encode(tip, text);
    lcq_with(lc, &["fact", "add", id, "--kind", spira_config::send_back::KIND, "--actor", "sift", "--cause", &cause]).map(|_| ())
}

/// Records the pass for `tip`. A bead no longer SUBMITTED or CERTIFIED records none, said on
/// `out`; the machine refuses one whose tip moved.
pub fn lc_sifted(lc: &LcClient, id: &str, tip: &str, out: &mut dyn Write) -> Result<(), String> {
    let b = show(lc, id)?;
    if !matches!(b.state.as_str(), "SUBMITTED" | "CERTIFIED") {
        let _ = writeln!(
            out,
            "{WHO}: sift: {id} is {}, not screened at {tip}; no pass recorded",
            b.state
        );
        return Ok(());
    }
    let version = b.version.ok_or("show: no version")?;
    bead_event(
        lc,
        id,
        &b.state,
        version,
        serde_json::json!({"Sifted": {"tip": tip}}),
    )
}

// ---------------------------------------------------------------------------------------------
// The scheduler's `sift` task: one probe reads every fact the screen judges, so its decide does
// no IO, and its writes go through one store client.

/// One SUBMITTED or CERTIFIED row, as the pool reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PoolRow {
    pub id: String,
    pub state: String,
    pub version: u64,
    pub tip: String,
    pub holds: Vec<String>,
    pub ejected_red_tip: Option<String>,
    pub sifted_tip: Option<String>,
}

impl PoolRow {
    fn of(r: &Value) -> Option<PoolRow> {
        Some(PoolRow {
            id: str_of(r, "bead_id")?.to_string(),
            state: str_of(r, "state").unwrap_or("").to_string(),
            version: r.get("version").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).unwrap_or(0),
            tip: str_of(r, "tip").unwrap_or("none").to_string(),
            holds: match r.get("holds") {
                Some(Value::Array(a)) => a.iter().filter_map(|h| h.as_str().map(str::to_string)).collect(),
                Some(Value::String(t)) => serde_json::from_str::<Vec<Value>>(t).map(|a| a.iter().map(|h| h.as_str().map_or_else(|| h.to_string(), str::to_string)).collect()).unwrap_or_else(|_| vec![t.clone()]),
                None | Some(Value::Null) => vec![],
                Some(other) => vec![other.to_string()],
            },
            ejected_red_tip: str_of(r, "ejected_red_tip").map(str::to_string),
            sifted_tip: str_of(r, "sifted_tip").map(str::to_string),
        })
    }

    /// The row as the store answered it, for [`select_pool`].
    pub fn value(&self) -> Value {
        serde_json::json!({"bead_id": self.id, "state": self.state, "tip": self.tip, "holds": self.holds, "ejected_red_tip": self.ejected_red_tip, "sifted_tip": self.sifted_tip})
    }
}

/// The git facts of one candidate (or open-round member), computed by the probe.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CandidateFacts {
    pub base: String,
    /// The conflicting files of `merge-tree --write-tree`; empty is a clean merge.
    pub conflicts: Vec<String>,
    pub patch_id: Option<String>,
    pub stacked_on: Vec<String>,
    /// The identity check's finding, when it found one.
    pub identity_gap: Option<String>,
    /// Why the identity check refused or could not run (an `INFRA_FAULT` prefix: could not run).
    pub identity_error: Option<String>,
    /// Why a git read of this tip failed: the screen cannot judge it and passes it unscreened.
    pub error: Option<String>,
}

/// One repository's facts.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RepoFacts {
    pub rows: Vec<PoolRow>,
    pub outside: Vec<PoolRow>,
    /// Each member's recorded prerequisites, and each prerequisite's state as read.
    pub stack: BTreeMap<String, Vec<String>>,
    pub prereq_state: BTreeMap<String, String>,
    /// Description and notes of each bead with a content row, for the target verdict.
    pub items: BTreeMap<String, String>,
    /// `id:tip` of the open round's members.
    pub open_round: Vec<String>,
    pub branch_tips: BTreeMap<String, String>,
    /// By bead id: the pool's candidates and the open round's members.
    pub candidates: BTreeMap<String, CandidateFacts>,
    /// `None` when the identity pages could not be read: the check holds the bead out.
    pub identity_pages: Option<Vec<String>>,
    /// Whether this repo's base was fetched; `false` refuses its pass.
    pub fetched: bool,
    /// When the probe read this repository (epoch ms): it stamps the screen's inbox lines.
    pub read_at_ms: u64,
}

impl RepoFacts {
    /// The pool [`select_pool`] judges from these facts.
    pub fn pool(&self) -> Vec<Candidate> {
        let rows: Vec<Value> = self.rows.iter().map(PoolRow::value).collect();
        let outside: Vec<Value> = self.outside.iter().map(PoolRow::value).collect();
        let shown: BTreeSet<String> = self.items.keys().cloned().collect();
        let stacks: BTreeMap<String, Vec<String>> = self.stack.iter().map(|(id, s)| (id.clone(), live_prereqs(s, &self.prereq_state))).collect();
        select_pool(&rows, &outside, &self.branch_tips, &shown, &stacks)
    }

    /// The open round's members as the screen takes them.
    pub fn open_members(&self) -> Vec<Candidate> {
        self.open_round.iter().map(|t| {
            let (id, tip) = t.split_once(':').unwrap_or((t, ""));
            Candidate { id: id.to_string(), tip: tip.to_string() }
        }).collect()
    }
}

/// What the probe reads one repository with.
pub struct Reads<'a> {
    pub lc: &'a LcClient,
    pub views: &'a store_view::Projection,
    pub reg: &'a Registry,
    pub wiki: &'a str,
    pub identity_projects: &'a str,
    pub work: PathBuf,
}

/// Every `queue` and `queue.local` repository, with whether it is local.
pub fn screened_repos(reg: &Registry) -> Vec<(String, bool)> {
    reg.all().into_iter().filter_map(|name| {
        let land = reg.land(&name);
        matches!(land.as_str(), "queue" | "queue.local").then(|| (name, land == "queue.local"))
    }).collect()
}

/// The row as it stands at `now`: a timed `wait` hold that ran out no longer holds it.
fn without_expired_wait(mut row: Value, now: i64) -> Value {
    let expired = str_of(&row, "reason").is_some_and(|r| spira_store::lc_state::wait_expired(r, now));
    let drop_wait = |a: &mut Vec<Value>| a.retain(|h| h.as_str() != Some("wait"));
    if expired {
        match row.get_mut("holds") {
            Some(Value::Array(a)) => drop_wait(a),
            Some(Value::String(t)) => {
                if let Ok(mut a) = serde_json::from_str::<Vec<Value>>(t) {
                    drop_wait(&mut a);
                    row["holds"] = Value::Array(a);
                }
            }
            _ => {}
        }
    }
    row
}

/// One repository's facts, read in-process: the store over `lc`, the open round from the
/// views, and git for the branch tips, the base fetch and each candidate. An empty pool reads
/// no git past the branch list and fetches nothing.
pub fn observe_repo(r: &Reads, name: &str, local: bool) -> Result<RepoFacts, String> {
    let repo = find_repo(r.reg, name, local)?;
    let rows: Vec<Value> = match held_rows(&store_view::RowQuery { states: &["SUBMITTED", "CERTIFIED"], ..Default::default() }) {
        Some(rows) => rows?,
        None => serde_json::from_str(&lcq_with(r.lc, &["list", "--state", "SUBMITTED,CERTIFIED"])?).map_err(|e| format!("spira-lc list: unparsed reply: {e}"))?, // BDQ_READ_CONSIDERED="LcClient is the spira-lc door; no bdq"
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let rows: Vec<Value> = rows.into_iter().map(|row| without_expired_wait(row, now)).collect();
    let mut f = RepoFacts { rows: rows.iter().filter_map(PoolRow::of).collect(), branch_tips: branch_tips(&repo)?, ..RepoFacts::default() };
    let ids = at_live_tips(&rows, &f.branch_tips);
    if !ids.is_empty() {
        let items = plan_items(&lcq_held_with(r.lc, &["plan", "items", "--full", "--ids", &ids.join(",")])?)?;
        for row in items.as_array().into_iter().flatten() {
            if let Some(id) = str_of(row, "id") {
                f.items.insert(id.to_string(), format!("{}\n{}", str_of(row, "description").unwrap_or(""), str_of(row, "notes").unwrap_or("")));
            }
        }
    }
    let by_id: BTreeMap<&str, &Value> = rows.iter().filter_map(|row| Some((str_of(row, "bead_id")?, row))).collect();
    for id in ids.iter().filter(|id| f.items.contains_key(*id)) {
        let stack = by_id.get(id.as_str()).map(|row| stack_of(&serde_json::json!({ "bead": row }))).unwrap_or_default();
        f.stack.insert(id.clone(), stack);
    }
    let prereqs: Vec<String> = f.stack.values().flatten().cloned().collect::<BTreeSet<_>>().into_iter().collect();
    let prereq_rows = rows_of_with(r.lc, &prereqs)?;
    for row in &prereq_rows {
        if let (Some(id), Some(state)) = (str_of(row, "bead_id"), str_of(row, "state")) {
            f.prereq_state.insert(id.to_string(), state.to_string());
        }
    }
    let pooled: BTreeSet<&str> = f.rows.iter().map(|p| p.id.as_str()).collect();
    f.outside = prereq_rows.iter().filter_map(PoolRow::of).filter(|p| !pooled.contains(p.id.as_str())).collect();
    f.open_round = open_round(&|repo| shown_in(r.views, repo), name)?.into_iter().map(|c| format!("{}:{}", c.id, c.tip)).collect();
    let pool = f.pool();
    if pool.is_empty() {
        return Ok(f);
    }
    f.fetched = fetch_base(&repo).is_ok();
    if !f.fetched {
        return Ok(f);
    }
    f.identity_pages = sift::identity::load(r.wiki, r.identity_projects, &r.reg.names()).ok().map(|m| m.keys().cloned().collect());
    let probe = GitProbe {
        repo: repo.path.clone(),
        name: repo.name.clone(),
        registered: r.reg.names(),
        wiki: r.wiki.to_string(),
        identity_projects: r.identity_projects.to_string(),
        base_ref: repo.base.clone(),
        work: r.work.clone(),
        state: Box::new(|_| Err("the probe reads states from the store".into())),
        shown: Box::new(|_| Err("the probe reads items from the store".into())),
    };
    let base = sift::Probe::base(&probe).unwrap_or_default();
    for c in pool.iter().chain(&f.open_members()) {
        f.candidates.insert(c.id.clone(), facts_of(&probe, &base, c));
    }
    Ok(f)
}

fn facts_of(probe: &GitProbe, base: &str, c: &Candidate) -> CandidateFacts {
    use sift::Probe;
    let mut f = CandidateFacts { base: base.to_string(), ..CandidateFacts::default() };
    match probe.merge(&c.tip) {
        Ok(sift::Merge::Conflict(files)) => f.conflicts = files,
        Ok(sift::Merge::Clean(_)) => {}
        Err(e) => f.error = Some(e),
    }
    match probe.patch_id(&c.tip) {
        Ok(p) => f.patch_id = Some(p),
        Err(e) => f.error = f.error.take().or(Some(e)),
    }
    match probe.stacked_on(&c.id, &c.tip) {
        Ok(s) => f.stacked_on = s,
        Err(e) => f.error = f.error.take().or(Some(e)),
    }
    match probe.identity_gap(&c.tip) {
        Ok(g) => f.identity_gap = g,
        Err(e) => f.identity_error = Some(e),
    }
    f
}

fn rows_of_with(lc: &LcClient, ids: &[String]) -> Result<Vec<Value>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(rows) = held_rows(&store_view::RowQuery { ids: Some(ids), ..Default::default() }) {
        return rows;
    }
    serde_json::from_str(&lcq_with(lc, &["list", "--ids", &ids.join(",")])?).map_err(|e| format!("spira-lc list --ids: unparsed reply: {e}")) // BDQ_READ_CONSIDERED="LcClient is the spira-lc door; no bdq"
}

/// Fetches `name`'s base remote: what the screen's pass is refused without.
pub fn fetch_repo_base(reg: &Registry, name: &str, local: bool) -> Result<(), String> {
    fetch_base(&find_repo(reg, name, local)?)
}

/// Supersedes `id` by `keeper` through the store's close.
pub fn lc_supersede(lc: &LcClient, id: &str, keeper: &str) -> Result<(), String> {
    let reason = format!("Sift: identical patch (patch-id) to {keeper}.");
    let r = lc.call_stdin(&["close", id, "--reason-file", "-", "--actor", "sift", "--superseded-by", keeper], &reason).map_err(|e| format!("spira-lc close {id}: {e}"))?;
    match r.code {
        0 => Ok(()),
        code => Err(format!("spira-lc close {id} exited {code}: {}", r.text())),
    }
}

/// The evidence a send-back stands on, written on the bead's notes through the store's `work`
/// door (`work <id> note -`, the text on stdin).
pub fn lc_note(lc: &LcClient, id: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("sift: {id} has no evidence to write; not sent back"));
    }
    let r = lc.call_stdin(&["work", id, "note", "-"], text).map_err(|e| format!("spira-lc work {id} note: {e}"))?;
    match r.code {
        0 => Ok(()),
        code => Err(format!("spira-lc work {id} note exited {code}: {}", r.text())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tjo5yst)]
    #[test]
    fn an_expired_timed_wait_no_longer_blocks_the_screen_and_a_live_one_does() {
        let row = |holds: Value, reason: &str| serde_json::json!({"bead_id": "sp-a", "state": "SUBMITTED", "tip": "t1", "holds": holds, "reason": reason});
        let branches: BTreeMap<String, String> = [("sp-a".to_string(), "t1".to_string())].into();
        let pool = |r: Value| select_pool(&[without_expired_wait(r, 1000)], &[], &branches, &BTreeSet::from(["sp-a".to_string()]), &BTreeMap::new()).len();
        assert_eq!(pool(row(serde_json::json!(["wait"]), "snooze-until:900")), 1);
        assert_eq!(pool(row(serde_json::json!("[\"wait\"]"), "snooze-until:900")), 1);
        assert_eq!(pool(row(serde_json::json!(["wait"]), "snooze-until:1100")), 0, "an unexpired wait still blocks");
        assert_eq!(pool(row(serde_json::json!(["wait"]), "sp-blocker")), 0, "an untimed wait still blocks");
        assert_eq!(pool(row(serde_json::json!(["wait", "ask"]), "snooze-until:900")), 0, "another blocking hold stays");
    }

    #[test_id(tfnlj4y)]
    #[test]
    fn a_done_prerequisite_is_dropped_and_a_working_or_unread_one_stays() {
        let stack: Vec<String> = ["sp-done", "sp-working", "sp-unread", "sp-landed"].map(String::from).into();
        let state = BTreeMap::from([("sp-done", "DONE"), ("sp-working", "WORKING"), ("sp-landed", "LANDED")].map(|(k, v)| (k.to_string(), v.to_string())));
        assert_eq!(live_prereqs(&stack, &state), ["sp-working", "sp-unread"]);
    }

    #[test_id(tnvtcuj)]
    #[test]
    fn a_row_held_only_manual_is_screened_and_any_other_hold_keeps_it_out() {
        let row = |holds: serde_json::Value| serde_json::json!({"bead_id": "sp-a", "state": "SUBMITTED", "tip": "t1", "holds": holds});
        let branches = BTreeMap::from([("sp-a".to_string(), "t1".to_string())]);
        let shown = BTreeSet::from(["sp-a".to_string()]);
        let pool = |holds| select_pool(&[row(holds)], &[], &branches, &shown, &BTreeMap::new()).len();
        assert_eq!(pool(serde_json::json!([])), 1);
        assert_eq!(pool(serde_json::json!(["manual"])), 1);
        assert_eq!(pool(serde_json::json!(["wait"])), 0);
        assert_eq!(pool(serde_json::json!(["manual", "wait"])), 0);
        assert_eq!(pool(serde_json::json!("[\"manual\"]")), 1);
        assert_eq!(pool(serde_json::json!("[\"wait\"]")), 0);
        assert_eq!(pool(serde_json::json!("not json")), 0);
        assert_eq!(pool(serde_json::json!(7)), 0);
        assert_eq!(pool(serde_json::json!(["no-such-hold"])), 0);

        let dependant = serde_json::json!({"bead_id": "sp-b", "state": "SUBMITTED", "tip": "t2", "holds": []});
        let branches = BTreeMap::from([("sp-a".to_string(), "t1".to_string()), ("sp-b".to_string(), "t2".to_string())]);
        let shown = BTreeSet::from(["sp-a".to_string(), "sp-b".to_string()]);
        let stacks = BTreeMap::from([("sp-b".to_string(), vec!["sp-a".to_string()])]);
        let ids: Vec<_> = select_pool(&[row(serde_json::json!(["manual"])), dependant], &[], &branches, &shown, &stacks).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["sp-a"], "a member stacked on a held bead stays out");
    }

    struct NeverCalled;

    impl spira_store::lc_call::Transport for NeverCalled {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            panic!("the store was called with {argv:?}");
        }
    }

    #[test_id(t52ommm)]
    #[test]
    fn an_empty_evidence_text_is_refused_before_any_store_call() {
        let lc = spira_store::lc_call::LcClient::over(std::sync::Arc::new(NeverCalled), true);
        let e = lc_note(&lc, "sp-a", "  \n").unwrap_err();
        assert!(e.contains("sp-a") && e.contains("no evidence"), "{e}");
    }

    #[test_id(td6mc4t)]
    #[test]
    fn the_open_round_is_the_store_record_the_queue_shows() {
        let show = |repo: &str| -> Result<String, String> {
            assert_eq!(repo, "deck");
            Ok("batch_id=deck-1\nforge.phase=open\npr=12\nmembers=sp-a:ta sp-b:tb\n".into())
        };
        let open = open_round(&show, "deck").unwrap();
        assert_eq!(
            open.iter().map(|c| (c.id.as_str(), c.tip.as_str())).collect::<Vec<_>>(),
            [("sp-a", "ta"), ("sp-b", "tb")]
        );
        let opening = |_: &str| Ok("batch_id=deck-1\nforge.phase=opening\nmembers=sp-a:ta\n".to_string());
        assert!(open_round(&opening, "deck").unwrap().is_empty(), "no PR yet: nothing to duplicate");
        assert!(open_round(&|_| Ok(String::new()), "deck").unwrap().is_empty());
        assert!(open_round(&|_| Err("queue batch show exited 1".into()), "deck").is_err(), "cannot tell is not no round");
    }

    struct Store(std::sync::Mutex<Vec<Result<spira_store::lc_call::Reply, String>>>);

    impl spira_store::lc_call::Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            assert_eq!(argv[..2], ["rc", "list"], "only the batch records are read");
            self.0.lock().unwrap().remove(0)
        }
    }

    /// Views on `answers` whose clock moves a second at every look, so a zero staleness is stale
    /// by the first read.
    fn views(answers: Vec<Result<spira_store::lc_call::Reply, String>>, stale: std::time::Duration) -> store_view::Projection {
        let client = spira_store::lc_call::LcClient::over(std::sync::Arc::new(Store(std::sync::Mutex::new(answers))), true);
        let (t0, looks) = (std::time::Instant::now(), std::sync::atomic::AtomicU64::new(0));
        let clock = move || t0 + std::time::Duration::from_secs(looks.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        store_view::Projection::new(client, stale, std::sync::Arc::new(clock))
    }

    #[test_id(tlex74n)]
    #[test]
    fn the_open_batch_is_read_from_the_store_views_and_a_refused_read_could_not_say() {
        let listed = serde_json::json!({"ok": {"list_rev": 4, "items": [
            {"key": "batches/deck/deck-1", "create_rev": {"main": 2}, "value": {"data": {
                "batch_id": "deck-1", "state": "OPEN", "repo": "deck",
                "round": {"members": "sp-a:ta sp-b:tb", "forge.phase": "armed", "pr": "12"}}}}]}});
        let ok = Ok(spira_store::lc_call::Reply { code: 0, out: listed.to_string(), err: String::new(), read_ended: false });
        let v = views(vec![ok.clone(), ok.clone()], std::time::Duration::from_secs(60));
        let open = open_round(&|r| shown_in(&v, r), "deck").unwrap();
        assert_eq!(open.iter().map(|c| (c.id.as_str(), c.tip.as_str())).collect::<Vec<_>>(), [("sp-a", "ta"), ("sp-b", "tb")]);
        assert!(open_round(&|r| shown_in(&v, r), "other").unwrap().is_empty(), "no batch open: nothing to duplicate");

        let down = views(vec![Err("the lifecycle service is not answering".into())], std::time::Duration::from_secs(60));
        let e = open_round(&|r| shown_in(&down, r), "deck").unwrap_err();
        assert!(e.starts_with("could not say:"), "{e}");
        let stale = views(vec![ok], std::time::Duration::ZERO);
        let e = open_round(&|r| shown_in(&stale, r), "deck").unwrap_err();
        assert!(e.starts_with("could not say:") && e.contains("stale past"), "{e}");
    }

    #[test_id(tderzi2)]
    #[test]
    fn a_pool_row_held_ejected_at_its_tip_or_out_of_the_pool_states_is_not_screened() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"bead_id":"sp-b","state":"SUBMITTED","tip":"t2"},
                {"bead_id":"sp-a","state":"CERTIFIED","tip":"t1","holds":"[]"},
                {"bead_id":"sp-h","state":"SUBMITTED","tip":"t3","holds":["x"]},
                {"bead_id":"sp-e","state":"SUBMITTED","tip":"t4","ejected_red_tip":"t4"},
                {"bead_id":"sp-r","state":"REWORK","tip":"t5"},
                {"bead_id":"sp-n","state":"SUBMITTED","holds":7}]"#,
        )
        .unwrap();
        assert_eq!(
            read_pool(&rows),
            [
                ("sp-a".to_string(), "t1".to_string()),
                ("sp-b".to_string(), "t2".to_string())
            ]
        );
    }

    #[test_id(tlxb0c3)]
    #[test]
    fn a_member_stacked_on_a_dropped_member_is_dropped_too() {
        let m = |id: &str, on: &[&str]| Member {
            id: id.into(),
            tip: "t".into(),
            stacked_on: on.iter().map(|s| s.to_string()).collect(),
        };
        let kept = drop_stacked_on_barred(
            vec![
                m("sp-c", &["sp-b"]),
                m("sp-b", &["sp-held"]),
                m("sp-a", &[]),
            ],
            ["sp-held".to_string()].into(),
        );
        assert_eq!(
            kept.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["sp-a"]
        );
    }

    #[test_id(te4blqf)]
    #[test]
    fn branch_tips_are_read_from_ref_files_loose_packed_and_loose_over_packed() {
        let d = testkit::TempDir::new("sifts-ref-files");
        let sha = |c: char| c.to_string().repeat(40);
        let g = d.join(".git");
        std::fs::create_dir_all(g.join("refs/heads/spira")).unwrap();
        std::fs::write(g.join("refs/heads/spira/sp-loose"), format!("{}\n", sha('1'))).unwrap();
        std::fs::write(g.join("refs/heads/spira/sp-both"), format!("{}\n", sha('2'))).unwrap();
        std::fs::write(g.join("packed-refs"), format!("# pack-refs\n{} refs/heads/spira/sp-packed\n{} refs/heads/spira/sp-both\n", sha('3'), sha('4'))).unwrap();
        let repo = Repo { name: "r".into(), path: d.path().to_path_buf(), base: "main".into(), local: true };
        let want = BTreeMap::from([("sp-loose".to_string(), sha('1')), ("sp-both".to_string(), sha('2')), ("sp-packed".to_string(), sha('3'))]);
        assert_eq!(branch_tips(&repo), Ok(want));
        std::fs::write(g.join("refs/heads/spira/sp-loose"), format!("{}\n", sha('5'))).unwrap();
        assert_eq!(branch_tips(&repo).unwrap()["sp-loose"], sha('5'), "nothing is cached between reads");
    }
}
