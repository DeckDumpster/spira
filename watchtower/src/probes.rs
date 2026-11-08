//! `--conditions-check` — the four standing conditions someone used to watch by hand:
//! a prod unit off the current release, a unit failing run after run, the box short of
//! room or under sustained pressure, and a release store grown past its keep. Each probe
//! gathers, decides purely, and hands [`conditions`](crate::conditions) what stands.

use crate::conditions::{Cond, Reading};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use runtime::units::{Exit, UnitDetail, UnitHandle, UnitState, Units};

pub struct Cfg {
    pub units: Arc<dyn Units>,
    pub releases: Option<String>,
    pub keep: usize,
    pub store_slack: usize,
    pub unit_glob: String,
    pub stale_release_secs: i64,
    pub failed_runs: i64,
    pub oom_glob: String,
    pub oom_hold_secs: i64,
    pub tmp_path: String,
    pub tmp_floor_mib: i64,
    pub disk_floor_pct: i64,
    pub psi_dir: String,
    pub psi_full_avg60: f64,
    pub psi_sustain_secs: i64,
}

fn sha_of(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

// ---- (a) units whose rendered release is not current -------------------------------

pub fn units_off_current(units: &[UnitDetail], current: &str, running: &dyn Fn(u32) -> Option<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for u in units {
        let Some(release) = u.environment.iter().find_map(|e| e.strip_prefix("SPIRA_RELEASE=")) else { continue };
        let sha = sha_of(release);
        let started = if sha == "current" { u.main_pid.and_then(running) } else { Some(sha.to_string()) };
        if let Some(started) = started.filter(|s| s != current) {
            out.push((u.name.clone(), started));
        }
    }
    out
}

fn process_release(releases: &str, pid: u32) -> Option<String> {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let canon_root = std::fs::canonicalize(releases).ok()?;
    let rest = exe.strip_prefix(&canon_root).ok().or_else(|| exe.strip_prefix(releases).ok())?;
    rest.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned())
}

pub fn release_currency(cfg: &Cfg) -> Reading {
    let Some(rel) = &cfg.releases else { return Reading::Unknown };
    let Some(current) = std::fs::read_link(Path::new(rel).join("current")).ok().map(|t| sha_of(&t.to_string_lossy()).to_string()) else {
        return Reading::Unknown;
    };
    let Ok(listed) = cfg.units.list(&cfg.unit_glob) else { return Reading::Unknown };
    Reading::Standing(
        units_off_current(&listed, &current, &|pid| process_release(rel, pid))
            .into_iter()
            .map(|(unit, sha)| Cond {
                key: unit.clone(),
                reference: format!("incident:release-stale-{unit}"),
                title: format!("STALE RELEASE: {unit} renders {sha}, current is {current}"),
                body: format!("{unit} has rendered release {sha} for over {}s; the current release is {current}.\n\nLanded fixes are not in force in this unit. Re-render and restart it (`release status`, then `release activate {current}`).\n", cfg.stale_release_secs),
                priority: 1,
                sustain_secs: cfg.stale_release_secs,
            })
            .collect(),
    )
}

// ---- (b) a unit failed for N consecutive runs ---------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub active_state: String,
    pub invocation: String,
    pub result: String,
    pub exit: Option<Exit>,
}

const SIGTERM: i32 = 15;

/// `Some((invocation, count))` is the new tally; `None` drops the unit.
pub fn tally(prev: Option<(&str, i64)>, run: &Run) -> Option<(String, i64)> {
    match run.active_state.as_str() {
        "failed" if run.result == "signal" && run.exit == Some(Exit::Signal(SIGTERM)) => None,
        "failed" => match prev {
            Some((inv, n)) if inv == run.invocation => Some((inv.to_string(), n)),
            Some((_, n)) => Some((run.invocation.clone(), n + 1)),
            None => Some((run.invocation.clone(), 1)),
        },
        "activating" | "deactivating" | "reloading" => prev.map(|(i, n)| (i.to_string(), n)),
        _ => None,
    }
}

fn parse_tally(text: &str) -> BTreeMap<String, (String, i64)> {
    text.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            if p.len() != 3 {
                return None;
            }
            Some((p[0].to_string(), (p[1].to_string(), p[2].parse().ok()?)))
        })
        .collect()
}

fn render_tally(m: &BTreeMap<String, (String, i64)>) -> String {
    m.iter().fold(String::new(), |mut acc, (u, (i, n))| {
        acc.push_str(&format!("{u} {i} {n}\n"));
        acc
    })
}

fn unit_run(cfg: &Cfg, unit: &str) -> Option<Run> {
    let d = cfg.units.detail(&UnitHandle::named(unit)).ok()?;
    Some(Run { active_state: d.active_state.unwrap_or_default(), invocation: d.invocation_id.unwrap_or_default(), result: d.result.unwrap_or_default(), exit: d.exit })
}

pub fn failing_units(cfg: &Cfg, run_dir: &Path) -> Reading {
    let Some(failed) = crate::failed_units::gather(&*cfg.units) else { return Reading::Unknown };
    let path = run_dir.join("conditions").join("failed-runs.tally");
    let prev = parse_tally(&std::fs::read_to_string(&path).unwrap_or_default());
    let mut units: Vec<String> = failed;
    for u in prev.keys() {
        if !units.contains(u) {
            units.push(u.clone());
        }
    }
    let units = crate::ctrl_gate::without_suspended(units);
    let mut next = BTreeMap::new();
    let mut standing = Vec::new();
    for unit in units {
        let Some(run) = unit_run(cfg, &unit) else {
            if let Some(p) = prev.get(&unit) {
                next.insert(unit, p.clone());
            }
            continue;
        };
        let Some((inv, n)) = tally(prev.get(&unit).map(|(i, n)| (i.as_str(), *n)), &run) else { continue };
        if n >= cfg.failed_runs {
            standing.push(Cond {
                key: unit.clone(),
                reference: format!("incident:failed-unit-{unit}"),
                title: format!("FAILED UNIT: {unit} failed {n} consecutive runs"),
                body: format!("{unit} has failed {n} consecutive runs.\n\nCheck: systemctl --user status {unit}\n"),
                priority: 1,
                sustain_secs: 0,
            });
        }
        next.insert(unit, (inv, n));
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(&path, render_tally(&next));
    Reading::Standing(standing)
}

// ---- (b2) a Restart= unit killed by the OOM killer ------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OomRow {
    pub restarts: i64,
    pub last_oom: i64,
}

/// What the manager reports for one unit: a restart counter that survives the restart
/// and the result of its latest stop, which the restart itself erases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OomShow {
    pub unit: String,
    pub restarts: i64,
    pub result: String,
}

/// Only units that count restarts (services) can be restart-looping.
pub fn oom_reads(units: &[UnitDetail]) -> Vec<OomShow> {
    units.iter().filter(|u| u.state != UnitState::Absent).filter_map(|u| Some(OomShow { unit: u.name.clone(), restarts: i64::from(u.n_restarts?), result: u.result.clone().unwrap_or_default() })).collect()
}

pub fn oom_decide(now: i64, prev: Option<&OomRow>, s: &OomShow) -> OomRow {
    let last = prev.map_or(0, |p| p.last_oom);
    let hit = s.result == "oom-kill";
    OomRow { restarts: s.restarts, last_oom: if hit { now } else { last } }
}

fn parse_oom_state(text: &str) -> BTreeMap<String, OomRow> {
    text.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            if p.len() != 3 {
                return None;
            }
            Some((p[0].to_string(), OomRow { restarts: p[1].parse().ok()?, last_oom: p[2].parse().ok()? }))
        })
        .collect()
}

pub fn oom_restarts(cfg: &Cfg, now: i64, run_dir: &Path) -> Reading {
    let Ok(listed) = cfg.units.list(&cfg.oom_glob) else { return Reading::Unknown };
    let path = run_dir.join("conditions").join("oom-restarts.state");
    let prev = parse_oom_state(&std::fs::read_to_string(&path).unwrap_or_default());
    let mut next = BTreeMap::new();
    let mut standing = Vec::new();
    for s in oom_reads(&listed) {
        let row = oom_decide(now, prev.get(&s.unit), &s);
        if row.last_oom > 0 && now - row.last_oom < cfg.oom_hold_secs {
            standing.push(Cond {
                key: s.unit.clone(),
                reference: format!("incident:oom-killed-{}", s.unit),
                title: format!("OOM KILLED: {} was killed for memory and restarted ({} restarts)", s.unit, s.restarts),
                body: format!(
                    "{} was last OOM-killed {}s ago and has restarted {} times; Restart= hides this from the failed-unit list.\n\nIts MemoryMax is too low for what it does: `systemctl --user show {} -p MemoryMax -p MemoryPeak`.\n",
                    s.unit, now - row.last_oom, s.restarts, s.unit
                ),
                priority: 2,
                sustain_secs: 0,
            });
        }
        next.insert(s.unit, row);
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(&path, next.iter().fold(String::new(), |mut a, (u, r)| {
        a.push_str(&format!("{u} {} {}\n", r.restarts, r.last_oom));
        a
    }));
    Reading::Standing(standing)
}

// ---- (c) room and pressure ----------------------------------------------------------

/// `(available MiB, Use%, size MiB)` of the filesystem holding `path`.
fn df(path: &str) -> Option<(i64, i64, Option<i64>)> {
    crate::fsstat::usage(path).map(|u| (u.avail_mib, u.used_pct, Some(u.size_mib)))
}

/// The `full avg60` of a `/proc/pressure/<resource>` file.
pub fn psi_full_avg60(text: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with("full "))?;
    line.split_whitespace().find_map(|w| w.strip_prefix("avg60="))?.parse().ok()
}

fn cond(key: &str, title: String, body: String, sustain: i64) -> Cond {
    Cond { key: key.into(), reference: format!("incident:pressure-{key}"), title, body, priority: 1, sustain_secs: sustain }
}

pub fn pressure(cfg: &Cfg) -> Reading {
    let mut standing = Vec::new();
    let mut readable = false;
    if let Some((avail, _, size)) = df(&cfg.tmp_path) {
        readable = true;
        // A filesystem smaller than the floor can never satisfy it: that is a prerequisite
        // for install-time checking, not a condition a tick can clear.
        let unsatisfiable = size.is_some_and(|sz| sz < cfg.tmp_floor_mib);
        if avail < cfg.tmp_floor_mib && !unsatisfiable {
            standing.push(cond(
                "tmp-free",
                format!("LOW SPACE: {} has {avail} MiB free, below the {} MiB testenv slots need", cfg.tmp_path, cfg.tmp_floor_mib),
                format!("{} has {avail} MiB free; warm testenv slots shed below {} MiB and suites fault on quota.\n\nFind what is holding it: `du -xsh {}/* | sort -h | tail`.\n", cfg.tmp_path, cfg.tmp_floor_mib, cfg.tmp_path),
                0,
            ));
        }
    }
    if let Some((_, used, _)) = df("/") {
        readable = true;
        let free = 100 - used;
        if free < cfg.disk_floor_pct {
            standing.push(cond(
                "root-disk",
                format!("LOW DISK: root has {free}% free, below the {}% floor", cfg.disk_floor_pct),
                format!("The root filesystem has {free}% free (floor {}%).\n\nCheck release trees, worktrees and logs: `du -xh --max-depth=2 / | sort -h | tail -30`.\n", cfg.disk_floor_pct),
                0,
            ));
        }
    }
    for res in ["io", "memory"] {
        if let Some(v) = std::fs::read_to_string(Path::new(&cfg.psi_dir).join(res)).ok().and_then(|t| psi_full_avg60(&t)) {
            readable = true;
            if v >= cfg.psi_full_avg60 {
                standing.push(cond(
                    &format!("psi-{res}"),
                    format!("PRESSURE: {res} full avg60 {v} for over {}s", cfg.psi_sustain_secs),
                    format!("{res} pressure (PSI full avg60) is {v}, at or above {}, and has been for over {}s.\n\nSee what is running: `systemd-cgtop`, `cat /proc/pressure/{res}`.\n", cfg.psi_full_avg60, cfg.psi_sustain_secs),
                    cfg.psi_sustain_secs,
                ));
            }
        }
    }
    if readable {
        Reading::Standing(standing)
    } else {
        Reading::Unknown
    }
}

/// Two or more cold builds running while the host's io `full avg60` is at the threshold: the
/// admission that serialises cold builds has been bypassed or raised past what the box bears.
pub fn cold_overlap(cfg: &Cfg, run: &Path) -> Reading {
    use spira_config::admission as adm;
    let io = std::fs::read_to_string(Path::new(&cfg.psi_dir).join("io")).ok().and_then(|t| psi_full_avg60(&t));
    let held = adm::read_occupancy(run, adm::Pool::Cold, adm::size_configured(adm::Pool::Cold), &adm::RealProcs).map(|o| o.used());
    cold_overlap_reading(io, held, cfg)
}

pub fn cold_overlap_reading(io: Option<f64>, held: Option<u64>, cfg: &Cfg) -> Reading {
    let (Some(io), Some(held)) = (io, held) else { return Reading::Unknown };
    if held < 2 || io < cfg.psi_full_avg60 {
        return Reading::Standing(Vec::new());
    }
    Reading::Standing(vec![cond(
        "cold-overlap",
        format!("COLD BUILDS: {held} running at once with io full avg60 {io}"),
        format!("{held} cold builds hold the cold-build pool while io pressure (PSI full avg60) is {io}, at or above {}.\n\nSee the pool: `spira-admit status`; the cold-build slot is meant to admit one.\n", cfg.psi_full_avg60),
        cfg.psi_sustain_secs,
    )])
}

// ---- (d) the release store ----------------------------------------------------------

pub fn store_count(releases: &Path) -> Option<usize> {
    let rd = std::fs::read_dir(releases).ok()?;
    Some(
        rd.flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.len() == 40 && n.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) && e.file_type().map(|t| t.is_dir()).unwrap_or(false)
            })
            .count(),
    )
}

pub fn release_store(cfg: &Cfg) -> Reading {
    let Some(count) = cfg.releases.as_deref().and_then(|r| store_count(Path::new(r))) else { return Reading::Unknown };
    let limit = cfg.keep + cfg.store_slack;
    Reading::Standing(if count > limit {
        vec![Cond {
            key: "release-store".into(),
            reference: "incident:release-store-growth".into(),
            title: format!("RELEASE STORE: {count} trees held, keep is {} (+{} slack)", cfg.keep, cfg.store_slack),
            body: format!("The release store holds {count} release trees; prune keeps {} and the alarm allows {} more.\n\nPrune is not keeping up: run `release prune` and read why it failed.\n", cfg.keep, cfg.store_slack),
            priority: 2,
            sustain_secs: 0,
        }]
    } else {
        Vec::new()
    })
}

// ---- (e) open beads with no lifecycle row -------------------------------------------

pub const ROWLESS_REFERENCE: &str = "incident:lifecycle-rowless-beads";

pub struct RowlessCfg {
    pub lc: spira_config::lc_call::LcClient,
    pub cap: usize,
    pub sustain_secs: i64,
    pub state: std::path::PathBuf,
    pub full_secs: i64,
}

pub fn rowless(open: &[String], rows: &[String]) -> Vec<String> {
    let have: std::collections::BTreeSet<&str> = rows.iter().map(String::as_str).collect();
    let mut missing: Vec<String> = open.iter().filter(|id| !have.contains(id.as_str())).cloned().collect();
    missing.sort();
    missing.dedup();
    missing
}

pub fn rowless_cond(missing: &[String], cap: usize, sustain_secs: i64) -> Option<Cond> {
    if missing.is_empty() {
        return None;
    }
    let shown: Vec<&str> = missing.iter().take(cap.max(1)).map(String::as_str).collect();
    let more = missing.len() - shown.len();
    Some(Cond {
        key: "rowless-beads".into(),
        reference: ROWLESS_REFERENCE.into(),
        title: "LIFECYCLE: open beads have no lifecycle row and can never be claimed".into(),
        body: format!(
            "{} open bead(s) have no spira_lifecycle row so no builder can claim them: {}{}.\n\nA creation path did not run `spira-lc create-bead`. Create the rows (`spira-lc create-bead <id>`) and find the path that filed them.\n",
            missing.len(),
            shown.join(", "),
            if more > 0 { format!(" (+{more} more)") } else { String::new() }
        ),
        priority: 0,
        sustain_secs,
    })
}

/// Every lifecycle row's bead id, from the store's `list`; `None` when it did not answer.
pub fn lifecycle_ids(lc: &spira_config::lc_call::LcClient) -> Option<Vec<String>> {
    ids_of(lc.answer(&["list"]).ok()?.as_bytes(), "bead_id")
}

fn ids_of(stdout: &[u8], key: &str) -> Option<Vec<String>> {
    let rows: Vec<serde_json::Value> = serde_json::from_slice(stdout).ok()?;
    Some(rows.iter().filter_map(|r| r.get(key).and_then(|v| v.as_str()).map(String::from)).collect())
}

/// NAMED EXCEPTION to lifecycle-guard's bd-status-read rule (lifecycle-guard/DESIGN.md,
/// "The rowless controls"; the Concierge's ruling on sp-mve9i): this is the positive control
/// for "no bead is rowless". A bead with no lifecycle row has no state but bd's, so the only
/// way to find one is to ask bd which beads it considers live and look for each in the
/// machine; reading bd status here audits the machine's coverage and decides nothing about a
/// bead the machine holds. The rule names this function; nothing else may do this.
pub fn rowless_beads(cfg: &RowlessCfg, now: i64) -> Reading {
    let prior = load_rowless_state(&cfg.state);
    let plan = rowless_plan(prior.as_ref(), now, cfg.full_secs);
    let mut args: Vec<String> = ["--status", "open,in_progress,blocked,deferred", "--exclude-type", &spira_config::nonwork::no_row_types_arg(), "--json", "--limit", "0", "--brief"]
        .iter().map(|s| s.to_string()).collect();
    if let Some(since) = plan.created_after {
        args.extend(["--created-after".to_string(), crate::log::fmt_iso(since)]);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let open = crate::lc::content_list(&cfg.lc, &args).and_then(|o| ids_of(o.as_bytes(), "id"));
    let Some(mut candidates) = open else { return Reading::Unknown };
    candidates.extend(plan.carried);
    candidates.sort();
    candidates.dedup();
    let missing = if candidates.is_empty() {
        Vec::new()
    } else {
        let Some(rows) = lifecycle_ids(&cfg.lc) else { return Reading::Unknown };
        rowless(&candidates, &rows)
    };
    save_rowless_state(&cfg.state, &RowlessState { last_full: if plan.created_after.is_none() { now } else { prior.map_or(now, |p| p.last_full) }, last_check: now, missing: missing.clone() });
    Reading::Standing(rowless_cond(&missing, cfg.cap, cfg.sustain_secs).into_iter().collect())
}

#[derive(Debug, PartialEq)]
pub struct RowlessState {
    pub last_full: i64,
    pub last_check: i64,
    pub missing: Vec<String>,
}

pub struct RowlessPlan {
    /// `None`: a full sweep of every live bead; `Some(t)`: only beads created after `t`.
    pub created_after: Option<i64>,
    pub carried: Vec<String>,
}

/// A rowless bead is a creation-path miss, so between full sweeps only beads created since
/// the last check (with an overlap for clock skew) plus those already flagged need a look.
const ROWLESS_OVERLAP_SECS: i64 = 120;

pub fn rowless_plan(prior: Option<&RowlessState>, now: i64, full_secs: i64) -> RowlessPlan {
    match prior {
        Some(p) if now >= p.last_full && now - p.last_full < full_secs && now >= p.last_check => RowlessPlan {
            created_after: Some(p.last_check - ROWLESS_OVERLAP_SECS),
            carried: p.missing.clone(),
        },
        _ => RowlessPlan { created_after: None, carried: Vec::new() },
    }
}

fn load_rowless_state(path: &Path) -> Option<RowlessState> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let last_full = lines.next()?.parse().ok()?;
    let last_check = lines.next()?.parse().ok()?;
    Some(RowlessState { last_full, last_check, missing: lines.filter(|l| !l.is_empty()).map(String::from).collect() })
}

fn save_rowless_state(path: &Path, st: &RowlessState) {
    let mut text = format!("{}\n{}\n", st.last_full, st.last_check);
    for id in &st.missing {
        text.push_str(id);
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// ---- (f) content store open beads outrun the lifecycle's live rows ---------------------

pub const CONTENT_DRIFT_REFERENCE: &str = "incident:lifecycle-content-drift";

pub struct ContentDriftCfg {
    pub lc: spira_config::lc_call::LcClient,
    pub slack: usize,
    pub sustain_secs: i64,
    pub state: std::path::PathBuf,
}

pub fn content_drift_cond(drifted: usize, slack: usize, sustain_secs: i64) -> Option<Cond> {
    if drifted <= slack {
        return None;
    }
    Some(Cond {
        key: "content-drift".into(),
        reference: CONTENT_DRIFT_REFERENCE.into(),
        title: "LIFECYCLE: the content store holds open beads whose lifecycle row is terminal".into(),
        body: format!(
            "`spira-lc reconcile-content` (dry run) would close {drifted} bead(s) the store still holds open though their lifecycle row is terminal (allowed excess {slack}). Every reader of content status sees a backlog that is not there.\n\nRun `spira-lc reconcile-content --apply`; find the terminal path that did not close its content bead.\n"
        ),
        priority: 1,
        sustain_secs,
    })
}

/// What a `reconcile-content --since-seq` dry run said: the newest event seq it read through
/// and the beads it would close.
#[derive(Debug, PartialEq)]
pub struct DriftRead {
    pub through: u64,
    pub drifted: Vec<String>,
}

fn drift_read(stdout: &[u8]) -> Option<DriftRead> {
    let text = String::from_utf8_lossy(stdout);
    text.lines().rev().find(|l| !l.trim().is_empty())?.strip_prefix("reconcile-content: ")?;
    let through = text.lines().find_map(|l| l.strip_prefix("through-seq: "))?.trim().parse().ok()?;
    let drifted = text.lines().filter_map(|l| l.strip_prefix("would close ")?.split_whitespace().next().map(String::from)).collect();
    Some(DriftRead { through, drifted })
}

fn load_drift_state(path: &Path) -> Option<(u64, Vec<String>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    Some((lines.next()?.parse().ok()?, lines.filter(|l| !l.is_empty()).map(String::from).collect()))
}

fn save_drift_state(path: &Path, read: &DriftRead) {
    let mut text = format!("{}\n", read.through);
    for id in &read.drifted {
        text.push_str(id);
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Reads content rows only for beads with a lifecycle event since the last pass plus those
/// still drifted then; a pass over an unchanged store reads none.
pub fn content_drift(cfg: &ContentDriftCfg) -> Reading {
    let (since, carried) = load_drift_state(&cfg.state).unwrap_or((0, Vec::new()));
    let since = since.to_string();
    let argv: Vec<&str> = ["reconcile-content", "--since-seq", &since].into_iter().chain(carried.iter().map(String::as_str)).collect();
    let read = cfg.lc.answer(&argv).ok().and_then(|o| drift_read(o.as_bytes()));
    let Some(read) = read else { return Reading::Unknown };
    save_drift_state(&cfg.state, &read);
    Reading::Standing(content_drift_cond(read.drifted.len(), cfg.slack, cfg.sustain_secs).into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rowless_reads_bd_through_the_content_door_and_finds_the_bead_with_no_row() {
        let d = testkit::TempDir::new("wt-rowless-lc");
        let (t, lc) = crate::lc::scripted::scripted_by(&[
            ("content list", 0, r#"[{"id":"sp-a"},{"id":"sp-b"}]"#),
            ("list", 0, r#"[{"bead_id":"sp-a"}]"#),
        ]);
        let cfg = RowlessCfg { lc, cap: 5, sustain_secs: 0, state: d.join("rowless.state"), full_secs: 3600 };
        let conds = standing(rowless_beads(&cfg, 1_700_000_000));
        assert_eq!(conds.len(), 1);
        assert!(conds[0].body.contains("sp-b") && !conds[0].body.contains("sp-a,"), "{}", conds[0].body);
        let seen = t.seen.lock().unwrap();
        assert_eq!(&seen[0][..3], &["content", "list", "--status"]);
        let (_, down) = crate::lc::scripted::scripted(2, "cannot tell");
        let cfg = RowlessCfg { lc: down, cap: 5, sustain_secs: 0, state: d.join("rowless2.state"), full_secs: 3600 };
        assert!(matches!(rowless_beads(&cfg, 1_700_000_000), Reading::Unknown), "no answer is unknown, never no rowless beads");
    }

    #[test]
    fn content_drift_is_the_store_reconcile_content_dry_run() {
        let d = testkit::TempDir::new("wt-drift-lc");
        let out = "through-seq: 42\nwould close sp-x (closed in bd)\nwould close sp-y (closed in bd)\nreconcile-content: 2 would close\n";
        let (t, lc) = crate::lc::scripted::scripted(0, out);
        let cfg = ContentDriftCfg { lc, slack: 1, sustain_secs: 0, state: d.join("drift.state") };
        assert_eq!(standing(content_drift(&cfg)).len(), 1, "two drifted is past a slack of one");
        assert_eq!(t.seen.lock().unwrap()[0], vec!["reconcile-content", "--since-seq", "0"]);
        assert!(std::fs::read_to_string(d.join("drift.state")).unwrap().starts_with("42\n"));
        let cfg = ContentDriftCfg { lc: crate::lc::scripted::scripted(2, "cannot tell").1, slack: 1, sustain_secs: 0, state: d.join("drift2.state") };
        assert!(matches!(content_drift(&cfg), Reading::Unknown));
    }

    #[test]
    fn lifecycle_ids_come_from_the_store_client() {
        let (t, c) = crate::lc::scripted::scripted(0, r#"[{"bead_id":"sp-a"},{"bead_id":"sp-b"}]"#);
        assert_eq!(lifecycle_ids(&c), Some(vec!["sp-a".to_string(), "sp-b".to_string()]));
        assert_eq!(t.seen.lock().unwrap().as_slice(), &[vec!["list".to_string()]]);
        assert_eq!(lifecycle_ids(&crate::lc::scripted::scripted(2, "cannot tell").1), None, "no answer is unknown, never no rows");
    }

    fn restarted(name: &str, n: u32, result: &str) -> UnitDetail {
        UnitDetail { n_restarts: Some(n), result: Some(result.into()), load_state: Some("loaded".into()), state: UnitState::Active, ..UnitDetail::absent(name) }
    }

    fn rendering(name: &str, env: &[&str], pid: Option<u32>) -> UnitDetail {
        UnitDetail { environment: env.iter().map(|e| e.to_string()).collect(), main_pid: pid, state: UnitState::Active, ..UnitDetail::absent(name) }
    }

    #[test]
    fn the_probes_read_the_unit_manager_the_cfg_names_and_no_manager_is_unknown() {
        let d = testkit::TempDir::new("probes-units");
        let mut c = cfg(d.path());
        let scripted = runtime::units::ScriptedUnits::new(Arc::new(runtime::ports::VirtualClock::new(0)));
        scripted.install_detail(UnitDetail { exit: Some(Exit::Code(1)), active_state: Some("failed".into()), invocation_id: Some("i1".into()), result: Some("exit-code".into()), ..UnitDetail { state: UnitState::Failed, load_state: Some("loaded".into()), ..UnitDetail::absent("spira-bad.service") } });
        scripted.install_detail(UnitDetail { n_restarts: Some(2), result: Some("oom-kill".into()), ..restarted("spira-hog.service", 2, "oom-kill") });
        c.units = Arc::new(scripted);
        c.failed_runs = 1;
        match failing_units(&c, d.path()) {
            Reading::Standing(s) => assert_eq!(s.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["spira-bad.service"]),
            _ => panic!("not standing"),
        }
        match oom_restarts(&c, 100, d.path()) {
            Reading::Standing(s) => assert_eq!(s.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["spira-hog.service"]),
            _ => panic!("not standing"),
        }
        c.units = runtime::units::unreachable("no manager");
        assert!(matches!(failing_units(&c, d.path()), Reading::Unknown));
        assert!(matches!(oom_restarts(&c, 100, d.path()), Reading::Unknown));
        assert!(matches!(release_currency(&c), Reading::Unknown));
    }

    #[test]
    fn a_unit_without_a_restart_counter_is_not_an_oom_candidate() {
        let timer = UnitDetail { state: UnitState::Active, ..UnitDetail::absent("spira-t.timer") };
        assert_eq!(oom_reads(&[timer, UnitDetail::absent("spira-gone.service")]), vec![]);
    }

    #[test]
    fn an_oom_kill_is_read_from_the_unit_result_and_held() {
        let rows = oom_reads(&[restarted("spira-a.service", 4, "success"), restarted("spira-b.service", 0, "oom-kill")]);
        assert_eq!(rows.len(), 2);
        let prev = OomRow { restarts: 3, last_oom: 0 };
        assert_eq!(oom_decide(100, Some(&prev), &rows[0]), OomRow { restarts: 4, last_oom: 0 }, "a restart alone is not an OOM kill");
        assert_eq!(oom_decide(100, None, &rows[1]).last_oom, 100, "Result=oom-kill is evidence");
        assert_eq!(oom_decide(200, Some(&OomRow { restarts: 4, last_oom: 100 }), &rows[0]).last_oom, 100, "a past hit is held");
    }

    #[test]
    fn a_sigterm_stop_is_not_a_failed_run() {
        let term = Run { result: "signal".into(), exit: Some(Exit::Signal(15)), ..run("failed", "i2") };
        assert_eq!(tally(Some(("i1", 2)), &term), None);
        let kill = Run { exit: Some(Exit::Signal(9)), ..term };
        assert_eq!(tally(Some(("i1", 2)), &kill), Some(("i2".into(), 3)));
    }

    #[test]
    fn rowless_plan_is_incremental_between_full_sweeps_and_full_after() {
        let st = RowlessState { last_full: 1000, last_check: 1500, missing: vec!["sp-m".into()] };
        let p = rowless_plan(Some(&st), 1600, 3600);
        assert_eq!(p.created_after, Some(1500 - ROWLESS_OVERLAP_SECS));
        assert_eq!(p.carried, vec!["sp-m".to_string()]);
        assert_eq!(rowless_plan(Some(&st), 1000 + 3600, 3600).created_after, None, "an hour on: full sweep");
        assert_eq!(rowless_plan(None, 1600, 3600).created_after, None);
        assert_eq!(rowless_plan(Some(&st), 900, 3600).created_after, None, "a clock gone backwards: full sweep");
    }

    #[test]
    fn rowless_state_round_trips() {
        let t = testkit::TempDir::new("rowless-state");
        let f = t.path().join("sub").join("rowless.state");
        let st = RowlessState { last_full: 5, last_check: 9, missing: vec!["sp-a".into(), "sp-b".into()] };
        save_rowless_state(&f, &st);
        assert_eq!(load_rowless_state(&f), Some(st));
    }

    #[test]
    fn rowless_finds_an_injected_rowless_bead_and_caps_the_list() {
        let open: Vec<String> = ["sp-a", "sp-b", "sp-c"].iter().map(|s| s.to_string()).collect();
        let rows: Vec<String> = ["sp-a", "sp-c", "sp-gone"].iter().map(|s| s.to_string()).collect();
        assert_eq!(rowless(&open, &rows), vec!["sp-b".to_string()]);
        assert_eq!(rowless(&open, &open), Vec::<String>::new());
        assert!(rowless_cond(&[], 5, 0).is_none(), "a fully rowed store stands nothing");
        let many: Vec<String> = (0..30).map(|i| format!("sp-{i:02}")).collect();
        let c = rowless_cond(&many, 5, 300).unwrap();
        assert_eq!(c.reference, ROWLESS_REFERENCE, "one reference de-duplicates every filing");
        assert!(c.body.contains("30 open bead(s)") && c.body.contains("(+25 more)") && !c.body.contains("sp-05"), "{}", c.body);
        assert_eq!(c.sustain_secs, 300);
    }

    #[test]
    fn a_dry_run_reads_through_its_seq_and_the_beads_it_would_close() {
        let out = b"would close sp-a (lifecycle LANDED)\nthrough-seq: 9\nreconcile-content: 1 would close (dry run; --apply to write)\n";
        assert_eq!(drift_read(out), Some(DriftRead { through: 9, drifted: vec!["sp-a".into()] }));
        assert_eq!(drift_read(b"reconcile-content: 0 would close (dry run; --apply to write)\n"), None, "no seq, no state to advance");
    }

    #[test]
    fn units_off_current_names_only_units_rendering_another_release() {
        let show = [rendering("spira-a-prod.service", &["SPIRA_RELEASE=/r/aaa", "PATH=/x"], None), rendering("spira-b-prod.service", &["SPIRA_RELEASE=/r/bbb"], None), rendering("spira-c-prod.service", &[], None)];
        assert_eq!(units_off_current(&show, "bbb", &|_| None), vec![("spira-a-prod.service".to_string(), "aaa".to_string())]);
        assert!(units_off_current(&show, "aaa", &|_| None).iter().any(|(u, _)| u == "spira-b-prod.service"));
    }

    #[test]
    fn units_rendering_the_current_symlink_are_current() {
        let show = [rendering("spira-a-prod.service", &["SPIRA_RELEASE=/r/current"], None), rendering("spira-b-prod.service", &["SPIRA_RELEASE=/r/current/"], None), rendering("spira-c-prod.service", &["SPIRA_RELEASE=/r/aaa"], None)];
        assert_eq!(units_off_current(&show, "bbb", &|_| None), vec![("spira-c-prod.service".to_string(), "aaa".to_string())]);
    }

    #[test]
    fn a_unit_rendering_current_is_stale_only_when_its_process_started_from_another_release() {
        let cur = ["SPIRA_RELEASE=/r/current"];
        let show = [rendering("spira-a-prod.service", &cur, Some(11)), rendering("spira-b-prod.service", &cur, Some(22)), rendering("spira-c-prod.service", &cur, None), rendering("spira-d-prod.service", &cur, Some(33))];
        let running = |pid: u32| match pid { 11 => Some("old".to_string()), 22 => Some("new".to_string()), _ => None };
        assert_eq!(units_off_current(&show, "new", &running), vec![("spira-a-prod.service".to_string(), "old".to_string())]);
    }

    fn run(state: &str, inv: &str) -> Run {
        Run { active_state: state.into(), invocation: inv.into(), result: "exit-code".into(), exit: Some(Exit::Code(1)) }
    }

    #[test]
    fn tally_counts_each_new_failed_invocation_once() {
        assert_eq!(tally(None, &run("failed", "i1")), Some(("i1".into(), 1)));
        assert_eq!(tally(Some(("i1", 1)), &run("failed", "i1")), Some(("i1".into(), 1)), "the same failed run seen twice is one run");
        assert_eq!(tally(Some(("i1", 1)), &run("failed", "i2")), Some(("i2".into(), 2)));
    }

    #[test]
    fn tally_survives_a_restart_in_flight_and_resets_on_success() {
        assert_eq!(tally(Some(("i1", 2)), &run("activating", "i2")), Some(("i1".into(), 2)));
        assert_eq!(tally(Some(("i1", 2)), &run("inactive", "i2")), None);
        assert_eq!(tally(Some(("i1", 2)), &run("active", "i2")), None);
    }

    #[test]
    fn tally_file_round_trips() {
        let mut m = BTreeMap::new();
        m.insert("u.service".to_string(), ("abc".to_string(), 3));
        assert_eq!(parse_tally(&render_tally(&m)), m);
    }

    #[test]
    fn df_and_psi_parse_the_real_shapes() {
        let psi = "some avg10=1.00 avg60=2.00 avg300=0.50 total=1\nfull avg10=0.00 avg60=12.34 avg300=1.00 total=1\n";
        assert_eq!(psi_full_avg60(psi), Some(12.34));
        assert_eq!(psi_full_avg60("some avg60=9.0\n"), None);
    }

    fn cfg(dir: &Path) -> Cfg {
        Cfg {
            units: Arc::new(runtime::units::ScriptedUnits::new(Arc::new(runtime::ports::VirtualClock::new(0)))),
            releases: Some(dir.join("releases").to_string_lossy().into_owned()),
            keep: 2,
            store_slack: 1,
            unit_glob: "spira-*-prod.service".into(),
            stale_release_secs: 3600,
            failed_runs: 3,
            oom_glob: "spira-*".into(),
            oom_hold_secs: 21600,
            tmp_path: "/tmp".into(),
            tmp_floor_mib: 100,
            disk_floor_pct: 15,
            psi_dir: dir.join("psi").to_string_lossy().into_owned(),
            psi_full_avg60: 10.0,
            psi_sustain_secs: 300,
        }
    }

    fn standing(r: Reading) -> Vec<Cond> {
        match r {
            Reading::Standing(c) => c,
            Reading::Unknown => panic!("probe read as unknown"),
        }
    }

    #[test]
    fn release_store_crosses_only_past_keep_plus_slack() {
        let d = testkit::TempDir::new("wt-store");
        let c = cfg(&d);
        for i in 0..3 {
            std::fs::create_dir_all(d.join(format!("releases/{:040x}", i))).unwrap();
        }
        std::fs::create_dir_all(d.join("releases/not-a-sha")).unwrap();
        assert!(standing(release_store(&c)).is_empty(), "3 trees is keep 2 + slack 1");
        std::fs::create_dir_all(d.join(format!("releases/{:040x}", 9))).unwrap();
        let s = standing(release_store(&c));
        assert_eq!(s.len(), 1);
        assert!(s[0].title.contains("4 trees"));
    }

    #[test]
    fn release_store_is_unknown_when_the_directory_cannot_be_read() {
        let d = testkit::TempDir::new("wt-store-missing");
        assert!(matches!(release_store(&cfg(&d)), Reading::Unknown));
    }

    #[test]
    fn cold_overlap_stands_only_with_two_cold_builds_and_io_pressure() {
        let d = testkit::TempDir::new("wt-cold");
        let c = cfg(&d);
        let keys = |io, held| standing(cold_overlap_reading(io, held, &c)).into_iter().map(|c| c.key).collect::<Vec<_>>();
        assert_eq!(keys(Some(91.0), Some(2)), vec!["cold-overlap"]);
        assert!(keys(Some(91.0), Some(1)).is_empty());
        assert!(keys(Some(1.0), Some(3)).is_empty());
        assert!(matches!(cold_overlap_reading(None, Some(2), &c), Reading::Unknown));
    }

}
