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
use spira_config::repos::Registry;

const WHO: &str = "round-controller";

/// What one pass reads besides the registry, resolved once.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub run: PathBuf,
    pub queue_bin: String,
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
            queue_bin: spira_config::release_env::tool("queue")?,
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
        lc_gate_red(self.env, id, tip, reason)
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
        lc_sifted(self.env, id, tip, self.out)
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
    let client = spira_config::lc_call::LcClient::for_bin(&env.lc_bin)?
        .within(std::time::Duration::from_secs(env.lc_timeout));
    lc_answer(client.call(args))
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

fn parsed(text: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("{what}: unparsed reply: {e}"))
}

/// The SUBMITTED and CERTIFIED rows, read once per repository: the pool and the sifted tips
/// are both filters of this one snapshot.
fn pool_rows(env: &Env) -> Result<Vec<Value>, String> {
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
    let (current, moved): (Vec<_>, Vec<_>) = read_pool(rows)
        .into_iter()
        .partition(|(id, tip)| branches.get(id) == Some(tip));
    let mut barred: BTreeSet<String> = moved.into_iter().map(|(id, _)| id).collect();
    let ids: Vec<String> = current.iter().map(|(id, _)| id.clone()).collect();
    let shown: BTreeSet<String> = match bd_show(env, &ids)? {
        Value::Array(a) => a,
        o => vec![o],
    }
    .iter()
    .filter_map(|r| str_of(r, "id").map(str::to_string))
    .collect();
    let members: Vec<Member> = current
        .into_iter()
        .filter(|(id, _)| shown.contains(id))
        .map(|(id, tip)| Member {
            stacked_on: live_stack(env, &id),
            id,
            tip,
        })
        .collect();
    let stacked_on: Vec<String> = members
        .iter()
        .flat_map(|m| m.stacked_on.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    barred.extend(barred_ids(env, rows, &stacked_on)?);
    Ok(drop_stacked_on_barred(members, barred)
        .into_iter()
        .map(|m| Candidate {
            id: m.id,
            tip: m.tip,
        })
        .collect())
}

/// The prerequisites `id`'s recorded stack names that are not yet finished. A failed or
/// unparsed read is unstacked, said on stderr: the machine already answered the pool's read.
fn live_stack(env: &Env, id: &str) -> Vec<String> {
    let text = match lcq(env, &["show", id]) {
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

/// A prerequisite recorded LANDED, SUPERSEDED or DROPPED sequences nothing; an unreadable row
/// is live, the safe side.
fn prereq_is_finished(env: &Env, id: &str) -> bool {
    let Ok(text) = lcq(env, &["show", id]) else {
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

/// Beads that may not ride in a round: held, or ejected at their current tip.
fn barred_ids(
    env: &Env,
    rows: &[Value],
    stacked_on: &[String],
) -> Result<BTreeSet<String>, String> {
    let pooled: BTreeSet<&str> = rows.iter().filter_map(|r| str_of(r, "bead_id")).collect();
    let outside: Vec<String> = stacked_on
        .iter()
        .filter(|id| !pooled.contains(id.as_str()))
        .cloned()
        .collect();
    let more = rows_of(env, &outside)?;
    Ok(rows
        .iter()
        .chain(&more)
        .filter(|r| !holds_empty(r) || ejected_at_tip(r))
        .filter_map(|r| str_of(r, "bead_id").map(str::to_string))
        .collect())
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
    plan_items(&lcq(env, &["plan", "items", "--full", "--ids", &ids.join(",")])?)
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

/// `queue batch show --repo <repo>`: the repository's open forge batch record, as `key=value`
/// lines; nothing when none is open.
fn batch_show(env: &Env, repo: &str) -> Result<String, String> {
    // FORK-EDGE: needs the store projection (spira_runtime::store) in a crate round-controller can depend on — spira-runtime → install → release → round-controller is a cycle
    let mut cmd = Command::new("timeout");
    cmd.arg(env.lc_timeout.to_string())
        .arg(&env.queue_bin)
        .args(["batch", "show", "--repo", repo]);
    run(&mut cmd, "queue batch show")
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

fn show(env: &Env, id: &str) -> Result<Shown, String> {
    let v = parsed(&lcq(env, &["show", id])?, "show")?;
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

fn bead_event(env: &Env, id: &str, state: &str, version: u64, kind: Value) -> Result<(), String> {
    let (version, kind) = (version.to_string(), kind.to_string());
    lcq(
        env,
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
fn lc_gate_red(env: &Env, id: &str, tip: &str, reason: &str) -> Result<(), String> {
    let b = show(env, id)?;
    let version = b.version.ok_or("show: no version")?;
    if !matches!(b.state.as_str(), "SUBMITTED" | "CERTIFIED") || b.tip.as_deref() != Some(tip) {
        return Err(format!("{id} moved since screening (state {})", b.state));
    }
    bead_event(
        env,
        id,
        &b.state,
        version,
        serde_json::json!({"GateRed": {"tip": tip, "reason": reason}}),
    )
}

/// Records the pass for `tip`. A bead no longer SUBMITTED or CERTIFIED records none, said on
/// `out`; the machine refuses one whose tip moved.
fn lc_sifted(env: &Env, id: &str, tip: &str, out: &mut dyn Write) -> Result<(), String> {
    let b = show(env, id)?;
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
        env,
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
