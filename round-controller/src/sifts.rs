//! `round-controller sifts`: the Sift over every queue repository. Each unheld SUBMITTED or
//! CERTIFIED bead whose recorded tip is still its branch's live tip is screened by the `sift`
//! crate: its pass is recorded on the lifecycle store, or it is sent back, superseded or held
//! out. On one pool its verdicts are `batcher sifts`'s, line for line.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use sift::{Acts, Candidate, FileStore, GitProbe};
use spira_config::lc_call::LcClient;
use spira_config::repos::Registry;

const WHO: &str = "round-controller";

/// What one pass reads besides the registry, resolved once.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub run: PathBuf,
    pub lc_bin: String,
    pub lc_timeout: u64,
    pub wiki: String,
    pub identity_projects: String,
    /// The Concierge's inbox, or why there is none: a verdict is told there as well as logged.
    pub inbox: Result<String, String>,
}

impl Env {
    pub fn for_process() -> Result<Env, String> {
        use spira_config::process::{cfg, cfg_parse};
        let home = spira_config::resolve::locate_home_for_process()?;
        let run = cfg("SPIRA_RUN")?;
        if run.trim().is_empty() {
            return Err("SPIRA_RUN unset".into());
        }
        Ok(Env {
            home,
            run: PathBuf::from(run),
            lc_bin: spira_config::lifecycle_row::lc_bin()?,
            lc_timeout: cfg_parse::<u64>("SPIRA_LC_TIMEOUT")?,
            wiki: cfg("SPIRA_WIKI").unwrap_or_default(),
            identity_projects: cfg("SPIRA_IDENTITY_PROJECTS").unwrap_or_default(),
            inbox: cfg("SPIRA_CONCIERGE_INBOX"),
        })
    }
}

/// The `sifts` verb: this process's config and registry, every verdict line on `out`.
pub fn for_process(out: &mut dyn Write) -> Result<(), String> {
    let env = Env::for_process()?;
    let reg = Registry::from_env(std::env::vars().collect(), &env.home);
    sifts(&env, &reg, out)
}

/// Screens every `queue` and `queue.local` repository; one that cannot be screened is named in
/// the error and does not stop the others.
pub fn sifts(env: &Env, reg: &Registry, out: &mut dyn Write) -> Result<(), String> {
    let mut failed = Vec::new();
    for name in reg.all() {
        let land = reg.land(&name);
        if !matches!(land.as_str(), "queue" | "queue.local") {
            continue;
        }
        if let Err(e) = sift_one(env, reg, &name, land == "queue.local", out) {
            eprintln!("{WHO} sifts: {name}: {e}");
            failed.push(name);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("sift failed for: {}", failed.join(", ")))
    }
}

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

fn sift_one(
    env: &Env,
    reg: &Registry,
    name: &str,
    local: bool,
    out: &mut dyn Write,
) -> Result<(), String> {
    let repo = find_repo(reg, name, local)?;
    let open = open_round(&|r| batch_show(env, r), &repo.name)?;
    let o = sift_repo(env, reg, &repo, &open, out)?;
    let _ = writeln!(
        out,
        "{WHO} {}: sift: {} sifted, {} sent back, {} superseded, {} held",
        repo.name,
        o.sifted.len(),
        o.sent_back.len(),
        o.superseded.len(),
        o.capped.len() + o.held.len()
    );
    Ok(())
}

fn sift_repo(
    env: &Env,
    reg: &Registry,
    repo: &Repo,
    open: &[Candidate],
    out: &mut dyn Write,
) -> Result<sift::Outcome, String> {
    let rows = pool_rows(env)?;
    let pool = certified_pool(env, repo, &rows)?;
    if pool.is_empty() {
        return Ok(sift::Outcome::default());
    }
    fetch_base(repo).map_err(|e| format!("base not fetched ({e})"))?;
    let work = env.run.join("sift");
    let probe = GitProbe {
        repo: repo.path.clone(),
        name: repo.name.clone(),
        registered: reg.names(),
        wiki: env.wiki.clone(),
        identity_projects: env.identity_projects.clone(),
        base_ref: repo.base.clone(),
        work: work.clone(),
        state: Box::new({
            let env = env.clone();
            move |id| lc_state(&env, id)
        }),
        shown: Box::new({
            let env = env.clone();
            move |id| bd_show_text(&env, id)
        }),
    };
    let mut live = Live {
        env,
        sifted: sifted_tips(&rows),
        out,
    };
    let o = sift::screen(&probe, &FileStore::new(work), &mut live, &pool, open);
    for e in &o.errors {
        eprintln!("{WHO} {}: {e}", repo.name);
    }
    Ok(o)
}

struct Live<'a> {
    env: &'a Env,
    sifted: BTreeMap<String, String>,
    out: &'a mut dyn Write,
}

impl Acts for Live<'_> {
    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        bead_note(self.env, id, text)
    }
    fn gate_red(&mut self, id: &str, tip: &str, reason: &str) -> Result<(), String> {
        lc_gate_red(&client_of(self.env)?, id, tip, reason)
    }
    fn supersede(&mut self, id: &str, keeper: &str) -> Result<(), String> {
        let reason = format!("Sift: identical patch (patch-id) to {keeper}.");
        // FORK-EDGE: unserved verb close (the spira-lc CLI answers it; the socket does not)
        spira_config::lifecycle_row::close_with(&self.env.lc_bin, id, &reason, "sift", Some(keeper))
    }
    fn pass(&mut self, id: &str, tip: &str) -> Result<(), String> {
        if self.sifted.get(id).map(String::as_str) == Some(tip) {
            return Ok(());
        }
        lc_sifted(&client_of(self.env)?, id, tip, self.out)
    }
    fn tell(&mut self, msg: &str) {
        let _ = writeln!(self.out, "{WHO}: {msg}");
        let line = format!(
            "{} [watch:sift] {msg}\n",
            stamp(spira_config::vtime::now_epoch())
        );
        let wrote = self.env.inbox.clone().and_then(|p| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .and_then(|mut f| f.write_all(line.as_bytes()))
                .map_err(|e| e.to_string())
        });
        if let Err(e) = wrote {
            eprintln!("{WHO}: sift: concierge not told ({e}): {msg}");
        }
    }
}

/// `%Y%m%dT%H%M%SZ`, the stamp the inbox's other writers use.
fn stamp(epoch: u64) -> String {
    crate::outcome::utc_stamp(epoch * 1000).replace(['-', ':'], "")
}

fn run(cmd: &mut Command, what: &str) -> Result<String, String> {
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

fn lcq(env: &Env, args: &[&str]) -> Result<String, String> {
    lcq_with(&client_of(env)?, args)
}

fn client_of(env: &Env) -> Result<LcClient, String> {
    Ok(LcClient::for_bin(&env.lc_bin)?.within(std::time::Duration::from_secs(env.lc_timeout)))
}

fn lcq_with(lc: &LcClient, args: &[&str]) -> Result<String, String> {
    lc_answer(lc.call(args))
}

/// An answer as the `spira-lc` child's was read: its stdout when it applied; otherwise its
/// exit and the last line it said.
fn lc_answer(
    r: Result<spira_config::lc_call::Reply, spira_config::lc_call::LcError>,
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

/// [`lcq`] for a read the process's store views hold (content, planning): the held answer when the
/// process keeps the store's watch.
fn lcq_held(env: &Env, args: &[&str]) -> Result<String, String> {
    match store_view::shared() {
        Some(views) => lc_answer(views.answer(args).map(|r| r.value)),
        None => lcq(env, args),
    }
}

fn lcq_held_with(lc: &LcClient, args: &[&str]) -> Result<String, String> {
    match store_view::shared() {
        Some(views) => lc_answer(views.answer(args).map(|r| r.value)),
        None => lcq_with(lc, args),
    }
}

/// The held lifecycle rows `q` keeps, as `list` printed them, when the process keeps the store's
/// watch; `None` when it does not.
fn held_rows(q: &store_view::RowQuery) -> Option<Result<Vec<Value>, String>> {
    store_view::shared().map(|views| views.rows(q).map(|r| r.value).map_err(|e| format!("could not say: {e}")))
}

/// `show <id>`'s answer: from the held row (it carries the `state`, `stack` and `tip` a show does)
/// when the process keeps the store's watch.
fn show_text(lc: &LcClient, id: &str) -> Result<String, String> {
    let ids = [id.to_string()];
    match held_rows(&store_view::RowQuery { ids: Some(&ids), ..Default::default() }) {
        Some(rows) => rows?.into_iter().next().map(|row| serde_json::json!({ "bead": row }).to_string()).ok_or_else(|| format!("spira-lc exited 1: no row for {id}")),
        None => lcq_with(lc, &["show", id]),
    }
}

fn parsed(text: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("{what}: unparsed reply: {e}"))
}

/// The SUBMITTED and CERTIFIED rows, read once per repository: the pool and the sifted tips
/// are both filters of this one snapshot.
fn pool_rows(env: &Env) -> Result<Vec<Value>, String> {
    if let Some(rows) = held_rows(&store_view::RowQuery { states: &["SUBMITTED", "CERTIFIED"], ..Default::default() }) {
        return rows;
    }
    let text = lcq(
        env,
        &["list", "--state", "SUBMITTED,CERTIFIED", "--snapshot"],
    )?;
    serde_json::from_str(&text).map_err(|e| format!("spira-lc list: unparsed reply: {e}"))
}

fn rows_of(env: &Env, ids: &[String]) -> Result<Vec<Value>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(rows) = held_rows(&store_view::RowQuery { ids: Some(ids), ..Default::default() }) {
        return rows;
    }
    let text = lcq(env, &["list", "--ids", &ids.join(",")])?;
    serde_json::from_str(&text).map_err(|e| format!("spira-lc list --ids: unparsed reply: {e}"))
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
        Some(Value::String(t)) => serde_json::from_str::<Vec<Value>>(t)
            .map(|a| a.is_empty())
            .unwrap_or(false),
        Some(_) => false,
    }
}

fn ejected_at_tip(r: &Value) -> bool {
    str_of(r, "ejected_red_tip").is_some() && r.get("ejected_red_tip") == r.get("tip")
}

/// `(id, tip)` of every unheld pool row not ejected at its tip, by id.
fn read_pool(rows: &[Value]) -> Vec<(String, String)> {
    let mut pool: Vec<(String, String)> = rows
        .iter()
        .filter(|r| in_pool_states(r) && holds_empty(r))
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

fn sifted_tips(rows: &[Value]) -> BTreeMap<String, String> {
    rows.iter()
        .filter(|r| in_pool_states(r))
        .filter_map(|r| {
            Some((
                str_of(r, "bead_id")?.to_string(),
                str_of(r, "sifted_tip")?.to_string(),
            ))
        })
        .collect()
}

/// `refs/heads/spira/*` of the repository's own checkout, by bead id: what scopes the
/// machine's pool to this repository, and the live tip a recorded tip must still be.
fn branch_tips(repo: &Repo) -> Result<BTreeMap<String, String>, String> {
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

/// The pool the screen judges: every unheld pool row of this repository at its branch's live
/// tip, with a content row, not stacked on a bead that is held, ejected at its tip or moved.
fn certified_pool(env: &Env, repo: &Repo, rows: &[Value]) -> Result<Vec<Candidate>, String> {
    let branches = branch_tips(repo)?;
    let ids: Vec<String> = at_live_tips(rows, &branches);
    let shown: BTreeSet<String> = match bd_show(env, &ids)? {
        Value::Array(a) => a,
        o => vec![o],
    }
    .iter()
    .filter_map(|r| str_of(r, "id").map(str::to_string))
    .collect();
    let stacks: BTreeMap<String, Vec<String>> = ids.iter().filter(|id| shown.contains(*id)).map(|id| (id.clone(), live_stack(env, id))).collect();
    let pooled: BTreeSet<&str> = rows.iter().filter_map(|r| str_of(r, "bead_id")).collect();
    let outside: Vec<String> = stacks.values().flatten().filter(|id| !pooled.contains(id.as_str())).cloned().collect::<BTreeSet<_>>().into_iter().collect();
    let outside = rows_of(env, &outside)?;
    Ok(select_pool(rows, &outside, &branches, &shown, &stacks))
}

/// The ids of the unheld pool rows whose recorded tip is their branch's live tip.
fn at_live_tips(rows: &[Value], branches: &BTreeMap<String, String>) -> Vec<String> {
    read_pool(rows).into_iter().filter(|(id, tip)| branches.get(id) == Some(tip)).map(|(id, _)| id).collect()
}

/// The pool, pure: of the unheld `rows` not ejected at their tip, those at their branch's live
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

/// The prerequisites of `stack` not yet finished by `state` (LANDED, SUPERSEDED, DROPPED); an
/// unread prerequisite is live, the safe side.
pub fn live_prereqs(stack: &[String], state: &BTreeMap<String, String>) -> Vec<String> {
    stack.iter().filter(|k| !matches!(state.get(*k).map(String::as_str), Some("LANDED" | "SUPERSEDED" | "DROPPED"))).cloned().collect()
}

/// The prerequisites `id`'s recorded stack names that are not yet finished. A failed or
/// unparsed read is unstacked, said on stderr: the machine already answered the pool's read.
fn live_stack(env: &Env, id: &str) -> Vec<String> {
    let text = match client_of(env).and_then(|lc| show_text(&lc, id)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{WHO}: LIFECYCLE: spira-lc show {id} failed (read as unstacked): {e}");
            return Vec::new();
        }
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        eprintln!("{WHO}: LIFECYCLE: spira-lc show {id}: unparsed reply (read as unstacked)");
        return Vec::new();
    };
    let stack = match v.get("bead").and_then(|b| b.get("stack")) {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => return Vec::new(),
    };
    stack
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(_, v)| v.is_string())
                .map(|(k, _)| k.clone())
                .collect::<BTreeSet<String>>()
                .into_iter()
                .filter(|k| !prereq_is_finished(env, k))
                .collect()
        })
        .unwrap_or_default()
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

/// A prerequisite recorded LANDED, SUPERSEDED or DROPPED sequences nothing; an unreadable row
/// is live, the safe side.
fn prereq_is_finished(env: &Env, id: &str) -> bool {
    let Ok(text) = client_of(env).and_then(|lc| show_text(&lc, id)) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    matches!(
        v.get("bead").and_then(|b| str_of(b, "state")),
        Some("LANDED" | "SUPERSEDED" | "DROPPED")
    )
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

/// The item rows of `ids`, from `spira-lc plan items --full`, the planning plane's read door.
fn bd_show(env: &Env, ids: &[String]) -> Result<Value, String> {
    if ids.is_empty() {
        return Ok(Value::Array(vec![]));
    }
    plan_items(&lcq_held(env, &["plan", "items", "--full", "--ids", &ids.join(",")])?)
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

/// The description and notes of `id`, where a target and its verdict live.
fn bd_show_text(env: &Env, id: &str) -> Result<String, String> {
    let rows = bd_show(env, &[id.to_string()])?;
    let row = rows
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| format!("plan items: no row for {id}"))?;
    Ok(format!(
        "{}\n{}",
        str_of(row, "description").unwrap_or(""),
        str_of(row, "notes").unwrap_or("")
    ))
}

/// The repository's open forge batch record from the store's views, as `queue batch show --repo
/// <repo>` printed it: `key=value` lines; nothing when none is open.
fn batch_show(env: &Env, repo: &str) -> Result<String, String> {
    let client = spira_config::lc_call::LcClient::for_bin(&env.lc_bin)
        .map_err(|e| format!("could not say: {e}"))?
        .within(std::time::Duration::from_secs(env.lc_timeout));
    let views = store_view::Projection::hot(client).map_err(|e| format!("could not say: {e}"))?;
    shown_in(&views, repo)
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
        Command::new("git")
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
    let v = parsed(&lcq_with(lc, &["show", id])?, "show")?;
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

fn lc_state(env: &Env, id: &str) -> Result<String, String> {
    let v = parsed(&lcq(env, &["show", id])?, "show")?;
    v.get("bead")
        .and_then(|b| str_of(b, "state"))
        .map(str::to_string)
        .ok_or_else(|| "show: no state".to_string())
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

/// The evidence a send-back stands on, written on the bead before the GateRed.
fn bead_note(env: &Env, id: &str, text: &str) -> Result<(), String> {
    // batch-job: bead.sh amend is a bead-store write that takes as long as the store does
    // FORK-EDGE: bead.sh amend is the operator tool (it also mails the dwarf at work on the bead); no library entry does both
    let mut cmd = Command::new("bash");
    cmd.arg(env.home.join("bead.sh"))
        .args(["amend", id, "--note", text])
        .envs(spira_config::release_env::child_path_env_for_process()?);
    run(&mut cmd, "bead.sh amend").map(|_| ())
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

/// One repository's facts, read in-process: the store over `lc`, the open round from the
/// views, and git for the branch tips, the base fetch and each candidate. An empty pool reads
/// no git past the branch list and fetches nothing.
pub fn observe_repo(r: &Reads, name: &str, local: bool) -> Result<RepoFacts, String> {
    let repo = find_repo(r.reg, name, local)?;
    let rows: Vec<Value> = match held_rows(&store_view::RowQuery { states: &["SUBMITTED", "CERTIFIED"], ..Default::default() }) {
        Some(rows) => rows?,
        None => serde_json::from_str(&lcq_with(r.lc, &["list", "--state", "SUBMITTED,CERTIFIED", "--snapshot"])?).map_err(|e| format!("spira-lc list: unparsed reply: {e}"))?,
    };
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
    for id in ids.iter().filter(|id| f.items.contains_key(*id)) {
        let stack = show_text(r.lc, id).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| stack_of(&v)).unwrap_or_default();
        for k in &stack {
            if let Some(state) = show_text(r.lc, k).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.get("bead").and_then(|b| str_of(b, "state")).map(str::to_string)) {
                f.prereq_state.insert(k.clone(), state);
            }
        }
        f.stack.insert(id.clone(), stack);
    }
    let pooled: BTreeSet<&str> = f.rows.iter().map(|p| p.id.as_str()).collect();
    let outside: Vec<String> = f.stack.values().flatten().filter(|k| !pooled.contains(k.as_str())).cloned().collect::<BTreeSet<_>>().into_iter().collect();
    f.outside = rows_of_with(r.lc, &outside)?.iter().filter_map(PoolRow::of).collect();
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
    serde_json::from_str(&lcq_with(lc, &["list", "--ids", &ids.join(",")])?).map_err(|e| format!("spira-lc list --ids: unparsed reply: {e}"))
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
    let r = lc.call_stdin(&["work", id, "note", "-"], text).map_err(|e| format!("spira-lc work {id} note: {e}"))?;
    match r.code {
        0 => Ok(()),
        code => Err(format!("spira-lc work {id} note exited {code}: {}", r.text())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    struct Store(std::sync::Mutex<Vec<Result<spira_config::lc_call::Reply, String>>>);

    impl spira_config::lc_call::Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_config::lc_call::Reply, String> {
            assert_eq!(argv[..2], ["rc", "list"], "only the batch records are read");
            crate::lock(&self.0).remove(0)
        }
    }

    /// Views on `answers` whose clock moves a second at every look, so a zero staleness is stale
    /// by the first read.
    fn views(answers: Vec<Result<spira_config::lc_call::Reply, String>>, stale: std::time::Duration) -> store_view::Projection {
        let client = spira_config::lc_call::LcClient::over(std::sync::Arc::new(Store(std::sync::Mutex::new(answers))), true);
        let (t0, looks) = (std::time::Instant::now(), std::sync::atomic::AtomicU64::new(0));
        let clock = move || t0 + std::time::Duration::from_secs(looks.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        store_view::Projection::new(client, stale, std::sync::Arc::new(clock))
    }

    #[test]
    fn the_open_batch_is_read_from_the_store_views_and_a_refused_read_could_not_say() {
        let listed = serde_json::json!({"ok": {"list_rev": 4, "items": [
            {"key": "batches/deck/deck-1", "create_rev": {"main": 2}, "value": {"data": {
                "batch_id": "deck-1", "state": "OPEN", "repo": "deck",
                "round": {"members": "sp-a:ta sp-b:tb", "forge.phase": "armed", "pr": "12"}}}}]}});
        let ok = Ok(spira_config::lc_call::Reply { code: 0, out: listed.to_string(), err: String::new() });
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

    #[test]
    fn the_inbox_stamp_is_the_compact_utc_form() {
        assert_eq!(stamp(1_700_000_000), "20231114T221320Z");
    }

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
}
