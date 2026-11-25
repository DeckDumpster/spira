//! The reads behind the ops panel's document, in two groups the scheduler runs on its own tiers:
//! the fast group (rows, rounds, asks, live dwarves, VMs, tails, config) and the slow group (the
//! state machine's edges and dwell, and the 24 h counts). Each keeps its last answer in [`Held`], so
//! a source that fails shows what it last read and names the error. Nothing here is time-dependent
//! but `now`, which the caller passes in.

use crate::lcview::{
    apply_claim_views, carry_forward, holder_pid, latest_gate_runs, live_row, num, own_ids, repo_column, tail_lines, text, BatchRow, DwellRow, EdgeRow, GraphEdge, LiveDwarf, MenderRow, Meta, PassProgress, Row, Snapshot, Tail, VmRun, TAIL_BYTES,
};
use spira_store::lc_call::LcError;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// What one gather reads through: the store's served reads, the unit manager, the VM pool.
pub struct Env<'a> {
    pub run_dir: &'a Path,
    pub lc: &'a dyn Fn(&[&str]) -> Result<String, LcError>,
    pub units: Option<&'a dyn runtime::units::Units>,
    pub vm_status: Option<&'a dyn Fn() -> Result<String, String>>,
}

#[derive(Default, Clone)]
struct GateRead {
    signature: Option<Option<(u64, SystemTime)>>,
    error: Option<String>,
}

/// The last answer of each group, and what the cheap re-reads are keyed on.
#[derive(Default, Clone)]
pub struct Held {
    fast: Option<Snapshot>,
    slow: Option<Snapshot>,
    gate: GateRead,
    ejects: Option<((String, usize), Vec<i64>)>,
}

impl Held {
    /// The document the fast group's `Held` and the slow group's make together; `None` until each
    /// has been read once.
    pub fn document(fast: &Held, slow: &Held) -> Option<Snapshot> {
        let (fast, slow) = (fast.fast.as_ref()?, slow.slow.as_ref()?);
        let mut s = fast.clone();
        s.graph = slow.graph.clone();
        s.edges = slow.edges.clone();
        s.dwell = slow.dwell.clone();
        s.closed = slow.closed.clone();
        s.now = fast.now.max(slow.now);
        s.good_at = [fast.good_at, slow.good_at].into_iter().filter(|g| *g > 0).min().unwrap_or(0);
        s.errors.extend(slow.errors.iter().cloned());
        s.store_errors.extend(slow.store_errors.iter().cloned());
        s.not_running |= slow.not_running;
        s.gather_ms += slow.gather_ms;
        s.failed = s.errors.iter().chain(s.store_errors.iter()).cloned().collect();
        Some(s)
    }
}

/// Whether `new` shows what `old` shows, the clock fields aside.
pub fn same_but_clock(old: &Snapshot, new: &Snapshot) -> bool {
    let mut o = old.clone();
    (o.now, o.good_at, o.gather_ms) = (new.now, new.good_at, new.gather_ms);
    serde_json::to_string(&o).ok() == serde_json::to_string(new).ok()
}

fn store_failed(s: &mut Snapshot, e: LcError) {
    s.not_running |= matches!(e, LcError::NotRunning(_));
    s.store_errors.push(e.to_string());
}

fn lc_json(env: &Env, args: &[&str]) -> Result<Vec<serde_json::Value>, LcError> {
    let name = format!("spira-lc {}", args.join(" "));
    (env.lc)(args)
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| LcError::Fault(format!("{name}: {e}"))))
        .map(|v| v.as_array().cloned().unwrap_or_default())
}

fn plan_items(env: &Env, args: &[&str]) -> Result<Vec<serde_json::Value>, LcError> {
    let name = format!("spira-lc {}", args.join(" "));
    let v: serde_json::Value = (env.lc)(args).and_then(|t| serde_json::from_str(&t).map_err(|e| LcError::Fault(format!("{name}: {e}"))))?;
    v["items"].as_array().cloned().ok_or_else(|| LcError::Fault(format!("{name}: no items in the answer")))
}

fn world_plane() -> Result<String, String> {
    let stamp = spira_world::halt_stamp()?;
    let text = stamp.is_file().then(|| std::fs::read_to_string(&stamp).unwrap_or_default());
    Ok(crate::lcview::world_line(text.as_deref()))
}

fn read_tail(path: &Path) -> Option<Tail> {
    let mut f = std::fs::File::open(path).ok()?;
    let meta = f.metadata().ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let start = meta.len().saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(TAIL_BYTES).read_to_end(&mut buf).ok()?;
    Some(Tail { lines: tail_lines(&buf, start > 0, 2), mtime })
}

fn repo_root_and_base(name: &str) -> Result<(String, String), String> {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let home = spira_config::resolve::locate_home_for_process().unwrap_or_default();
    let reg = spira_config::repos::Registry::from_env_checkout(env, &home);
    let root = reg.root(name).ok_or_else(|| format!("spira-config repo root {name}: exit 1 "))?;
    let base = reg.base(name).ok_or_else(|| format!("spira-config repo base {name}: exit 1 "))?;
    Ok((root, base))
}

/// The base's name and the commits that name their own beads, re-read only when the base moves.
fn base_commits() -> Result<(String, HashMap<String, String>), String> {
    let (root, base) = repo_root_and_base("spira")?;
    let (root, base) = (root.trim().to_string(), base.trim().to_string());
    let repo = Path::new(&root);
    let log = || -> Result<HashMap<String, String>, String> {
        let sha = spira_config::git_refs::sha_of(repo, &base).ok_or_else(|| format!("{root}: cannot read {base}"))?;
        let out = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(repo)
            .args(["log", "--format=%H %s", "-n", "4000", &sha])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("git: {e}"))?;
        if !out.status.success() {
            return Err(format!("git log {base}: exit {}", out.status.code().unwrap_or(-1)));
        }
        let mut m = HashMap::new();
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((h, subj)) = l.split_once(' ') {
                for id in own_ids(subj) {
                    m.entry(id).or_insert_with(|| h.to_string());
                }
            }
        }
        Ok(m)
    };
    let commits = match spira_config::git_refs::sha_of(repo, &base) {
        Some(tip) => spira_config::git_refs::held(repo, &format!("own-bead commits {base}"), &tip, log),
        None => log(),
    }?;
    Ok((base, commits))
}

fn live_dwarves(env: &Env) -> Result<Vec<LiveDwarf>, String> {
    let units = env.units.ok_or("no unit manager to list the dwarves")?;
    let mut found = std::collections::BTreeMap::new();
    for glob in runtime::units::dwarf_globs() {
        for d in units.list(&glob).map_err(|e| e.to_string())? {
            if runtime::units::is_live_dwarf(&d.name, d.state) && d.main_pid.is_some_and(|p| p > 0) {
                found.insert(d.name.clone(), LiveDwarf { unit: d.name, pid: d.main_pid.unwrap_or(0) as i64, started: d.since_ms.map_or(0, |ms| (ms / 1000) as i64), phase: String::new() });
            }
        }
    }
    Ok(found.into_values().collect())
}

fn gather_vm_runs(env: &Env, s: &mut Snapshot, last: Option<&Snapshot>) {
    let status = match env.vm_status {
        Some(read) => read().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| format!("round-vm status: {e}"))),
        None => Err("round-vm status: no pool configured".to_string()),
    };
    let v = match status {
        Ok(v) => v,
        Err(e) => {
            s.errors.push(e);
            s.vm_runs = last.map(|l| l.vm_runs.clone()).unwrap_or_default();
            return;
        }
    };
    let strs = |x: &serde_json::Value| x.as_array().map(|a| a.iter().map(text).collect()).unwrap_or_default();
    for l in v["leases"].as_array().cloned().unwrap_or_default() {
        s.vm_runs.push(VmRun { vm: text(&l["vm"]), since: num(&l["since"]).unwrap_or(0), owner: text(&l["owner"]), cmd: strs(&l["cmd"]), idle: false, run: text(&l["run"]), held: l["held"].as_bool().unwrap_or(false), held_since: num(&l["held_since"]).unwrap_or(0) });
    }
    if let Some(r) = v["ready"].as_object().filter(|r| !r.is_empty()) {
        let handle = r.get("vm").map(text).unwrap_or_default();
        let since = v["vms"].as_array().and_then(|a| a.iter().find(|x| text(&x["vm"]) == handle)).and_then(|x| num(&x["since"])).unwrap_or(0);
        s.vm_runs.push(VmRun { vm: handle, since, idle: true, ..Default::default() });
    }
}

fn gather_gate_runs(env: &Env, s: &mut Snapshot, held: &mut GateRead, last: Option<&Snapshot>) {
    let path = tsd::family_path(env.run_dir, "gate-run");
    let signature = std::fs::metadata(&path).ok().and_then(|m| Some((m.len(), m.modified().ok()?)));
    if held.signature != Some(signature) {
        let src = tsd::query::Source { reader: None, root: env.run_dir };
        let from = (s.now.max(0) as u64).saturating_sub(7 * 86_400);
        match src.rows("gate-run", from) {
            Ok(rows) => (s.gate_runs, held.error) = (latest_gate_runs(&rows), None),
            Err(f) if f.code == tsd::query::ERR_NO_FAMILY => (s.gate_runs, held.error) = (Default::default(), None),
            Err(f) => (s.gate_runs, held.error) = (last.map(|l| l.gate_runs.clone()).unwrap_or_default(), Some(f.message)),
        }
        held.signature = Some(signature);
    } else {
        s.gate_runs = last.map(|l| l.gate_runs.clone()).unwrap_or_default();
    }
    if let Some(e) = &held.error {
        s.errors.push(e.clone());
    }
}

fn gather_batches(env: &Env, s: &mut Snapshot, held: &mut Held) {
    match lc_json(env, &["list", "--batches"]) { // BDQ_READ_CONSIDERED="the spira-lc lifecycle door (LcClient), not a bd read"
        Ok(rows) => {
            let mut by_time: Vec<(i64, String, String)> = Vec::new();
            for b in &rows {
                let at = num(&b["last_at"]).or_else(|| num(&b["opened_at"])).unwrap_or(0);
                for e in b["ejected"].as_array().cloned().unwrap_or_default() {
                    if let (Some(id), Some(why)) = (e["bead_id"].as_str(), e["reason"].as_str()) {
                        by_time.push((at, id.to_string(), why.to_string()));
                    }
                }
            }
            by_time.sort();
            for (_, id, why) in by_time {
                s.eject_why.insert(id, why);
            }
            s.batches = rows
                .iter()
                .map(|b| BatchRow {
                    id: text(&b["batch_id"]),
                    state: text(&b["state"]),
                    last_at: num(&b["last_at"]).or_else(|| num(&b["opened_at"])).unwrap_or(0),
                    members: b["members"].as_array().map(|arr| arr.iter().map(|x| text(&x["bead_id"])).collect()).unwrap_or_default(),
                    opened_at: num(&b["opened_at"]).unwrap_or(0),
                    parent: text(&b["parent"]),
                    repo: text(&b["repo"]),
                    pr: num(&b["pr"]).unwrap_or(0),
                    phase: text(&b["phase"]),
                    progress: PassProgress::from_json(&b["progress"]),
                    run_url: b["progress"]["run_url"].as_str().unwrap_or("").to_string(),
                    log: b["progress"]["log"].as_str().unwrap_or("").to_string(),
                    eject_at: Vec::new(),
                    ejected: b["ejected"].as_array().map(|arr| arr.iter().map(|x| x["bead_id"].as_str().map(String::from).unwrap_or_else(|| text(x))).collect()).unwrap_or_default(),
                })
                .collect()
        }
        Err(e) => store_failed(s, e),
    }
    if let Some(b) = s.batches.iter_mut().max_by_key(|b| b.opened_at.max(b.last_at)).filter(|b| !b.ejected.is_empty()) {
        let key = (b.id.clone(), b.ejected.len());
        if let Some((_, at)) = held.ejects.as_ref().filter(|(k, _)| *k == key) {
            b.eject_at = at.clone();
        } else {
            match lc_json(env, &["history", &b.id, "--machine", "batch"]) { // BDQ_READ_CONSIDERED="the spira-lc lifecycle door (LcClient), not a bd read"
                Ok(evs) => {
                    b.eject_at = evs.iter().filter(|e| e["evidence"].as_str().is_some_and(|x| x.starts_with("{\"Eject\""))).filter_map(|e| num(&e["at"])).collect();
                    held.ejects = Some((key, b.eject_at.clone()));
                }
                Err(e) => store_failed(s, e),
            }
        }
    }
}

fn gather_menders(env: &Env, s: &mut Snapshot) {
    match lc_json(env, &["mending", "list", "--live"]) {
        Ok(rows) => s.menders = rows.iter().map(|m| MenderRow { batch_id: text(&m["batch_id"]), pass: num(&m["pass"]).unwrap_or(0), suite: text(&m["suite"]), state: text(&m["state"]), deadline: num(&m["deadline"]) }).collect(),
        Err(e) => store_failed(s, e),
    }
}

fn gather_rows(env: &Env, s: &mut Snapshot) -> HashMap<i64, String> {
    let mut phases = HashMap::new();
    match lc_json(env, &["list", "--live"]) { // BDQ_READ_CONSIDERED="the spira-lc lifecycle door (LcClient), not a bd read"
        Ok(rows) => {
            let mut read = true;
            for r in rows {
                let (row, meta) = match live_row(&r) {
                    Ok(x) => x,
                    Err(e) => {
                        s.errors.push(format!("spira-lc list --live: {e}"));
                        read = false;
                        break;
                    }
                };
                if let (Some(pid), Some(ph)) = (r["holder"].as_str().and_then(holder_pid), r["dwarf_phase"].as_str()) {
                    phases.insert(pid, ph.to_string());
                }
                s.meta.insert(row.id.clone(), meta);
                s.rows.push(row);
            }
            if read {
                apply_claim_views(s);
            }
        }
        Err(e) => store_failed(s, e),
    }
    match (env.lc)(&["ops-view", "ops_recent", "--epics"])
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| LcError::Fault(format!("spira-lc ops-view ops_recent: {e}"))))
        .map(|v| match v {
            serde_json::Value::Array(rows) => rows,
            mut o => {
                s.epics = serde_json::from_value(o["epics"].take()).unwrap_or_default();
                o["rows"].as_array().cloned().unwrap_or_default()
            }
        }) {
        Ok(rows) => {
            for r in rows {
                let repo = match repo_column(&r) {
                    Ok(repo) => repo,
                    Err(e) => {
                        s.errors.push(format!("spira-lc ops-view ops_recent: {e}"));
                        break;
                    }
                };
                let id = text(&r["bead_id"]);
                let updated_at = num(&r["updated_at"]).unwrap_or(0);
                s.meta.entry(id.clone()).or_insert_with(|| Meta { title: text(&r["title"]), priority: num(&r["priority"]), ..Default::default() });
                s.rows.push(Row { id, state: "LANDED".into(), updated_at, since: num(&r["since"]).unwrap_or(updated_at), repo, ..Default::default() });
            }
        }
        Err(e) => store_failed(s, e),
    }
    phases
}

fn gather_asks(env: &Env, s: &mut Snapshot) {
    match (env.lc)(&["list-asks"]).and_then(|t| crate::lcview::ask_rows(&t).map_err(LcError::Fault)) {
        Ok(asks) => {
            let label = spira_config::process::cfg("SPIRA_ASK_LABEL").map(|v| v.trim().to_string()).unwrap_or_default();
            if !label.is_empty() {
                for a in &asks {
                    s.meta.entry(a.id.clone()).or_default().labels.push(label.clone());
                }
            }
            s.asks = asks;
        }
        Err(e) => {
            s.asks_error = Some(e.to_string());
            store_failed(s, e);
        }
    }
    if s.asks.iter().any(|a| a.title.is_empty()) {
        let ids = s.asks.iter().filter(|a| a.title.is_empty()).map(|a| a.id.as_str()).collect::<Vec<_>>().join(",");
        match plan_items(env, &["plan", "items", "--ids", &ids]) {
            Ok(rows) => crate::lcview::fill_ask_titles(&mut s.asks, &rows),
            Err(e) => store_failed(s, e),
        }
    }
}

fn gather_config(s: &mut Snapshot) {
    s.release = spira_config::release_env::release_root_for_process()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "?".into());
    s.ask_label = spira_config::process::cfg("SPIRA_ASK_LABEL").map(|v| v.trim().to_string()).unwrap_or_default();
    s.forge_base = spira_config::process::cfg("SPIRA_FORGE_REPO").ok().and_then(|r| r.trim().split_once('/').map(|(o, _)| format!("https://github.com/{o}"))).unwrap_or_default();
    s.loom_base = spira_config::process::cfg("SPIRA_LOOM_ADDR").ok().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).map(|a| format!("http://{}", a.replacen("0.0.0.0", "localhost", 1)));
    let file_ceiling = spira_config::process::cfg("SPIRA_MAX_LIVE_DWARVES").ok().map(|v| v.trim().to_string());
    let limit = spira_config::scheduler_link::Limit::read("SPIRA_MAX_LIVE_DWARVES", file_ceiling.as_deref());
    s.ceiling = limit.shown().and_then(|v| v.parse().ok()).unwrap_or(0);
    if let Some(p) = limit.pending() {
        s.ceiling_pending = p.config.clone();
        s.ceiling_waits = p.waits.clone();
    }
}

fn finish(s: &mut Snapshot, started: std::time::Instant) {
    s.failed = s.errors.iter().chain(s.store_errors.iter()).cloned().collect();
    s.gather_ms = started.elapsed().as_millis() as u64;
}

/// Reads the fast group at `now` into `held`.
pub fn gather_fast(env: &Env, now: i64, held: &mut Held) {
    let started = std::time::Instant::now();
    let last = held.fast.take();
    let mut s = Snapshot { now, ..Default::default() };
    gather_config(&mut s);
    let phases = gather_rows(env, &mut s);
    gather_asks(env, &mut s);
    gather_batches(env, &mut s, held);
    gather_menders(env, &mut s);
    match base_commits() {
        Ok((base, m)) => (s.base, s.on_base) = (base, m),
        Err(e) => {
            s.errors.push(e);
            if let Some(l) = &last {
                (s.base, s.on_base) = (l.base.clone(), l.on_base.clone());
            }
        }
    }
    s.world = world_plane().unwrap_or_else(|e| {
        s.errors.push(e);
        "?".into()
    });
    s.live = match live_dwarves(env) {
        Ok(mut l) => {
            for a in l.iter_mut() {
                a.phase = phases.get(&a.pid).cloned().unwrap_or_else(|| "working".into());
            }
            Some(l)
        }
        Err(e) => {
            s.errors.push(e);
            None
        }
    };
    let live_pids: Vec<i64> = s.live.as_ref().map(|l| l.iter().map(|a| a.pid).collect()).unwrap_or_default();
    for r in s.rows.iter().filter(|r| (r.state == "WORKING" && r.holder.is_some()) || r.holder.as_deref().and_then(holder_pid).is_some_and(|p| live_pids.contains(&p))) {
        if let Some(t) = read_tail(&env.run_dir.join(format!("{}.log", r.id))) {
            s.tails.insert(r.id.clone(), t);
        }
    }
    s.inflight = std::fs::read_to_string(env.run_dir.join("cockpit.env")).map(|t| crate::lcview::Inflight::from_cockpit_env(&t)).unwrap_or_default();
    gather_vm_runs(env, &mut s, last.as_ref());
    gather_gate_runs(env, &mut s, &mut held.gate, last.as_ref());
    if let Some(l) = &last {
        carry_forward(&mut s, l);
    }
    finish(&mut s, started);
    held.fast = Some(s);
}

/// Reads the slow group at `now` into `held`.
pub fn gather_slow(env: &Env, now: i64, held: &mut Held) {
    let started = std::time::Instant::now();
    let last = held.slow.take();
    let mut s = Snapshot { now, ..Default::default() };
    match lc_json(env, &["ops-graph"]) {
        Ok(rows) => s.graph = rows.iter().map(|g| GraphEdge { from: text(&g["from"]), event: text(&g["event"]), to: text(&g["to"]) }).collect(),
        Err(e) => store_failed(&mut s, e),
    }
    match lc_json(env, &["ops-view", "ops_edges"]) {
        Ok(rows) => {
            s.edges = rows
                .iter()
                .map(|e| EdgeRow { kind: text(&e["kind"]), from_state: text(&e["from_state"]), to_state: e["to_state"].as_str().map(String::from), event: e["event"].as_str().map(String::from), refusal: e["refusal"].as_str().map(String::from), n_1h: num(&e["n_1h"]).unwrap_or(0) })
                .collect()
        }
        Err(e) => store_failed(&mut s, e),
    }
    match lc_json(env, &["ops-view", "ops_dwell"]) {
        Ok(rows) => s.dwell = rows.iter().map(|d| DwellRow { bead_id: text(&d["bead_id"]), state: text(&d["state"]), entered_at: num(&d["entered_at"]).unwrap_or(0), p95_s: num(&d["p95_s"]) }).collect(),
        Err(e) => store_failed(&mut s, e),
    }
    let closed = spira_config::process::cfg("SPIRA_VICTORIALOGS_URL")
        .and_then(|url| spira_otel::vlogs::Logs::new(&url).map_err(|e| e.to_string()))
        .and_then(|logs| Ok((crate::lcview::closed_24h(&logs)?, crate::lcview::applied_edges_1h(&logs)?)));
    match closed {
        Ok((c, applied)) => {
            s.closed = Some(c);
            s.edges.retain(|e| e.kind != "applied");
            s.edges.extend(applied);
        }
        Err(e) => {
            s.errors.push(e);
            s.closed = last.as_ref().and_then(|l| l.closed.clone());
        }
    }
    if let Some(l) = &last {
        carry_forward(&mut s, l);
    }
    finish(&mut s, started);
    held.slow = Some(s);
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::units::{ScriptedUnits, UnitDetail, UnitState};
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn answered(_: &[&str]) -> Result<String, LcError> {
        Ok("[]".into())
    }

    fn env<'a>(run_dir: &'a Path, lc: &'a dyn Fn(&[&str]) -> Result<String, LcError>) -> Env<'a> {
        Env { run_dir, lc, units: None, vm_status: None }
    }

    #[test_id(ta6v4al)]
    #[test]
    fn a_store_read_that_found_no_store_running_marks_the_snapshot_and_a_fault_does_not() {
        let mut s = Snapshot::default();
        store_failed(&mut s, LcError::Fault("spira-lc list --live: exit 2 x".into()));
        assert!(!s.not_running && s.store_errors.len() == 1);
        store_failed(&mut s, LcError::NotRunning("store not running: nothing was sent".into()));
        assert!(s.not_running && s.store_errors.len() == 2 && s.errors.is_empty());
    }

    #[test_id(tqg0x7l)]
    #[test]
    fn the_live_dwarves_the_scheduler_started_are_listed_through_the_unit_manager() {
        let units = ScriptedUnits::new(Arc::new(runtime::ports::VirtualClock::new(0)));
        let unit = |name: &str, state, pid| UnitDetail { name: name.into(), state, main_pid: pid, since_ms: Some(1_794_000_000_000), ..Default::default() };
        let own = "spira-run-sentinel_2dcall-builder_2d1794009721297_2d1.service";
        units.install_detail(unit(own, UnitState::Active, Some(4242)));
        units.install_detail(unit("spira-run-sentinel_2dcall-builder_2d1794000000000_2d0.service", UnitState::Failed, None));
        units.install_detail(unit("spira-run-sentinel_2dpass-landing.service", UnitState::Active, Some(77)));
        units.install_detail(unit("spira-dwarf-builder-1794000000.service", UnitState::Active, Some(9)));
        let dir = testkit::TempDir::new("lcgather-units");
        let lc = answered;
        let e = Env { units: Some(&units), ..env(dir.path(), &lc) };
        let live = live_dwarves(&e).unwrap();
        assert_eq!(live.iter().map(|d| (d.unit.as_str(), d.pid)).collect::<Vec<_>>(), [("spira-dwarf-builder-1794000000.service", 9), (own, 4242)], "the live dwarves of either name, never a pass or a failed one");
        assert_eq!(live[1].started, 1_794_000_000);
        assert!(live_dwarves(&env(dir.path(), &lc)).is_err(), "no unit manager is an error, not no dwarves");
    }

    struct Counting(Mutex<Vec<String>>);

    impl Transport for Counting {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.join(" "));
            let out = match argv[0].as_str() {
                "rc" => r#"{"ok":{"items":[],"list_rev":5}}"#.to_string(),
                "watch" => {
                    // clock-safe: the double's long-poll, ending after a pause
                    std::thread::sleep(Duration::from_millis(5));
                    r#"{"progress_rev":5}"#.to_string()
                }
                _ => "[]".to_string(),
            };
            Ok(Reply { code: 0, out, err: String::new(), read_ended: false })
        }
    }

    #[test_id(tpnit22)]
    #[test]
    fn a_second_gather_with_no_store_change_sends_no_live_or_round_list() {
        let store = Arc::new(Counting(Mutex::new(Vec::new())));
        let views = store_view::install(Arc::new(store_view::Projection::new(LcClient::over(store.clone(), true), Duration::from_secs(30), Arc::new(std::time::Instant::now))));
        views.load().unwrap();
        let watcher = views.clone();
        std::thread::spawn(move || watcher.run(&std::sync::atomic::AtomicBool::new(false), &mut |_| {}));
        // clock-safe: a cap on the wait for the watch thread to start
        for _ in 0..6000 {
            let watched_long = store.0.lock().unwrap().iter().any(|a| a.starts_with("watch ") && a.contains("--wait-ms 30000"));
            if views.watching() && watched_long {
                break;
            }
            // clock-safe: a poll interval under that cap
            std::thread::sleep(Duration::from_millis(5));
        }
        let read = |a: &[&str]| store_view::held_read(a).unwrap_or_else(|| Ok("[]".to_string()));
        let dir = testkit::TempDir::new("lcgather-held");
        let mut held = Held::default();
        gather_fast(&env(dir.path(), &read), 1_000, &mut held);
        let loaded = store.0.lock().unwrap().len();
        gather_fast(&env(dir.path(), &read), 1_005, &mut held);
        let sent = store.0.lock().unwrap().clone();
        assert!(!sent[loaded..].iter().any(|a| a.starts_with("list --live") || a.starts_with("list --batches")), "{sent:?}");
        assert!(sent.iter().any(|a| a.starts_with("watch ") && a.contains("--wait-ms 30000")), "{sent:?}");
    }

    #[test_id(t3z239u)]
    #[test]
    fn the_gate_runs_are_read_again_only_when_their_family_file_changes() {
        let dir = testkit::TempDir::new("lcgather-gate");
        let path = tsd::family_path(dir.path(), "gate-run");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let row = |bead: &str| format!("{{\"ts\":\"2026-11-20T00:00:00Z\",\"bead\":\"{bead}\",\"phases\":\"build 1s\"}}\n");
        let now = tsd::parse_ts("2026-11-21T00:00:00Z").unwrap() as i64;
        std::fs::write(&path, row("sp-a")).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let lc = answered;
        let e = env(dir.path(), &lc);
        let mut held = Held::default();
        let beads = |h: &Held| h.fast.as_ref().unwrap().gate_runs.keys().cloned().collect::<Vec<_>>();
        gather_fast(&e, now, &mut held);
        assert_eq!(beads(&held), ["sp-a"]);
        std::fs::write(&path, row("sp-b")).unwrap();
        std::fs::File::open(&path).unwrap().set_modified(mtime).unwrap();
        gather_fast(&e, now + 5, &mut held);
        assert_eq!(beads(&held), ["sp-a"], "a file of the same length and time is not read again");
        std::fs::write(&path, format!("{}{}", row("sp-b"), row("sp-c"))).unwrap();
        gather_fast(&e, now + 10, &mut held);
        assert_eq!(beads(&held), ["sp-b", "sp-c"]);
    }

    #[test_id(tjg148t)]
    #[test]
    fn a_failed_vm_read_keeps_the_last_vms_and_names_the_error_and_a_clock_alone_changes_nothing() {
        let dir = testkit::TempDir::new("lcgather-vm");
        let lc = answered;
        let status = || Ok::<String, String>(serde_json::json!({"leases": [{"vm": "109", "since": 100, "owner": "5@1", "cmd": [], "run": "r-1", "held": false}], "ready": null, "vms": []}).to_string());
        let down = || Err::<String, String>("round-vm: state unreadable".into());
        let mut held = Held::default();
        gather_fast(&Env { vm_status: Some(&status), ..env(dir.path(), &lc) }, 1_000, &mut held);
        let first = held.fast.clone().unwrap();
        assert_eq!((first.vm_runs.len(), first.vm_runs[0].since), (1, 100));
        gather_fast(&Env { vm_status: Some(&down), ..env(dir.path(), &lc) }, 1_010, &mut held);
        let after = held.fast.clone().unwrap();
        assert_eq!(after.vm_runs.len(), 1, "the last read stands in");
        assert!(after.errors.iter().any(|e| e.contains("state unreadable")), "{:?}", after.errors);
        assert!(same_but_clock(&first, &{
            let mut moved = first.clone();
            (moved.now, moved.good_at, moved.gather_ms) = (9, 9, 9);
            moved
        }), "only the clock fields moved");
        let mut changed = first.clone();
        changed.vm_runs[0].since += 1;
        assert!(!same_but_clock(&first, &changed), "a VM's start is part of what the document shows");
    }
}
