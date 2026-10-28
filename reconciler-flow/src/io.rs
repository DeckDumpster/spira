//! The IO seam: everywhere this crate touches a clock, a file, a subprocess or the network.
//! Every function returns a `Result` and never guesses — a query that cannot run comes back
//! `Err`, and [`crate::main`] turns that into `RawStatus::Unobservable`, never `Satisfied`
//! (law-a-control-that-cannot-check-must-refuse). Nothing in [`crate::core`] calls any of
//! this directly, so the pure half never needs a duckdb binary, a bead store or a mailbox to
//! run its own tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;

/// One duckdb -json -c invocation, parsed. Empty stdout (a `SELECT` with no matching rows in
/// an old duckdb, or a query that legitimately returns nothing) is `Ok(vec![])`, not an
/// error — the caller decides whether "no rows" is itself a gap.
pub fn duckdb_json(bin: &str, sql: &str) -> Result<Vec<Value>, String> {
    let out = spira_config::bounded::bounded(bin)
        .args(["-json", "-c", sql])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{bin}: exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<Value>>(text).map_err(|e| format!("{bin}: unparsable json: {e}"))
}

fn family_path(root: &Path, family: &str) -> PathBuf {
    tsd::family_path(root, family)
}

fn f64_field(row: &Value, key: &str) -> f64 {
    row.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The count of beads in the backlog right now: every work bead whose lifecycle row is not
/// terminal (sp-mve9i: a bead's state is spira-lc's, never bd's `status`). Scoped by
/// `scope_label` when the harness serves more than one repository, same convention
/// czar-pass uses for its own detectors; bd supplies only which beads the scope holds.
pub fn backlog_count(bd_bin: &str, spira_db: &str, scope_label: &str) -> Result<u64, String> {
    let mut cmd = spira_config::bounded::bounded(bd_bin);
    cmd.arg("-C").arg(spira_db).args([
        "list", "--all",
        "--exclude-type", "epic,event", "--brief", "--json", "--limit", "0",
    ]);
    if !scope_label.is_empty() {
        cmd.args(["--label", scope_label]);
    }
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{bd_bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{bd_bin} list: exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<Value> = serde_json::from_str(text.trim())
        .map_err(|e| format!("{bd_bin} list: unparsable json: {e}"))?;
    let ids: Vec<String> = rows.iter().filter_map(|r| r.get("id").and_then(Value::as_str).map(str::to_string)).collect();
    let lc = spira_config::lc_state::list().map_err(|e| format!("lifecycle state unreadable: {e}"))?;
    Ok(backlog_of(&ids, &spira_config::lc_state::index(lc)))
}

/// How many of `ids` are still in the backlog: a lifecycle row that is not terminal. A bead
/// with no row can never be worked, so it is not backlog.
pub fn backlog_of(ids: &[String], lc: &std::collections::HashMap<String, spira_config::lc_state::Row>) -> u64 {
    ids.iter().filter(|id| lc.get(*id).is_some_and(|r| !r.terminal())).count() as u64
}

/// The bead-machine states a bead is still in flight in — after these, it has either landed
/// or left the pipeline some other terminal way. CERTIFIED and the terminal states are
/// deliberately excluded (design reconciler-time-series-2026-09-27 §3: the dwell invariant
/// names exactly READY, WORKING, SUBMITTED, IN_DELIVERY, REWORK).
const BEAD_DWELL_STATES: [&str; 5] = ["READY", "WORKING", "SUBMITTED", "IN_DELIVERY", "REWORK"];

/// The batch-machine states a round is still in flight in — LANDED, SETTLED and ABANDONED are
/// terminal, so nothing dwells in them.
const BATCH_DWELL_STATES: [&str; 5] = ["OPEN", "CI_RUNNING", "GREEN", "ATTRIBUTING", "REBUILDING"];

/// Every `bead-stage` row the store holds, once per pass, shared by the stage queries below. The
/// whole retention is read: [`rework_metrics`] reports how much history exists. A store that
/// cannot be reached, or holds no row, is the `Err` every one of them then returns by name.
pub fn stage_rows(endpoint: Option<&str>) -> Result<Vec<Value>, String> {
    let reader = tsd::store::Reader::new(endpoint).map_err(|e| e.to_string())?;
    reader.rows("bead-stage", 0).map_err(|e| e.to_string())
}

fn ts_secs(row: &Value) -> Option<f64> {
    tsd::query::ts_of(row).map(|t| t as f64)
}

fn applied(row: &Value) -> bool {
    tsd::query::flag(row, "applied")
}

fn machine(row: &Value) -> &str {
    tsd::query::text(row, "machine").unwrap_or("")
}

fn to_state(row: &Value) -> &str {
    tsd::query::text(row, "to_state").unwrap_or("")
}

fn seq(row: &Value) -> f64 {
    tsd::query::num(row, "seq").unwrap_or(f64::INFINITY)
}

fn cutoff(now: u64, hours: f64) -> f64 {
    now as f64 - hours * 3600.0
}

/// How many beads are in a state that still needs to land — submitted, certified or already
/// handed to delivery, but not there yet — read from each bead's latest applied `bead-stage` row.
pub fn waiting_to_land(rows: &[Value]) -> u64 {
    let mut latest: std::collections::HashMap<&str, (f64, &str)> = std::collections::HashMap::new();
    for r in rows.iter().filter(|r| machine(r) == "bead" && applied(r)) {
        let Some(key) = tsd::query::text(r, "key") else { continue };
        let q = seq(r);
        if latest.get(key).is_none_or(|(s, _)| q > *s) {
            latest.insert(key, (q, to_state(r)));
        }
    }
    latest.values().filter(|(_, st)| ["SUBMITTED", "CERTIFIED", "IN_DELIVERY"].contains(st)).count() as u64
}

/// Current-window and trailing-baseline land rate (events/hour) — a bead reaching LANDED, the
/// lifecycle machine's own terminal state (design §2a: states are the machine's).
pub fn velocity_metrics(rows: &[Value], now: u64, window_hours: f64, baseline_hours: f64) -> (f64, f64) {
    let merged_in = |hours: f64| rows.iter().filter(|r| machine(r) == "bead" && applied(r) && to_state(r) == "LANDED" && ts_secs(r).is_some_and(|t| t >= cutoff(now, hours))).count() as f64;
    (merged_in(window_hours) / window_hours.max(1e-9), merged_in(baseline_hours) / baseline_hours.max(1e-9))
}

/// Each applied row's previous state and the seconds since it, per `part` of the rows ordered by seq.
fn transitions(rows: &[Value], part: impl Fn(&Value) -> String, keep: impl Fn(&Value) -> bool) -> Vec<(&Value, Option<(&str, f64)>)> {
    let mut by: std::collections::BTreeMap<String, Vec<&Value>> = Default::default();
    for r in rows.iter().filter(|r| applied(r) && keep(r)) {
        by.entry(part(r)).or_default().push(r);
    }
    let mut out = Vec::new();
    for (_, mut v) in by {
        v.sort_by(|a, b| seq(a).partial_cmp(&seq(b)).unwrap_or(std::cmp::Ordering::Equal));
        let mut prev: Option<&Value> = None;
        for r in v {
            let before = prev.and_then(|p| Some((tsd::query::text(p, "to_state")?, ts_secs(p)?)));
            out.push((r, before));
            prev = Some(r);
        }
    }
    out
}

/// p95 dwell (seconds) across every in-flight bead state (READY, WORKING, SUBMITTED,
/// IN_DELIVERY, REWORK) and every in-flight batch state (OPEN, CI_RUNNING, GREEN,
/// ATTRIBUTING, REBUILDING), pooled into one series — for the current window and the
/// trailing baseline, plus how many transitions each is drawn from. `bead` and `batch` rows
/// share one `bead-stage` family, so dwell is taken per (machine, key) to keep a bead's own
/// timeline from a batch's.
pub fn dwell_metrics(rows: &[Value], now: u64, window_hours: f64, baseline_hours: f64) -> (f64, u64, f64, u64) {
    let states: Vec<&str> = [BEAD_DWELL_STATES.as_slice(), BATCH_DWELL_STATES.as_slice()].concat();
    let dwells: Vec<(f64, f64)> = transitions(rows, |r| format!("{}\u{0}{}", machine(r), tsd::query::text(r, "key").unwrap_or("")), |_| true)
        .into_iter()
        .filter_map(|(r, before)| {
            let (prev_state, prev_ts) = before?;
            let t = ts_secs(r)?;
            states.contains(&prev_state).then_some((t, t - prev_ts))
        })
        .collect();
    let p95 = |hours: f64| {
        let v: Vec<f64> = dwells.iter().filter(|(t, _)| *t >= cutoff(now, hours)).map(|(_, d)| *d).collect();
        (tsd::query::quantile(&v, 0.95).unwrap_or(0.0), v.len() as u64)
    };
    let (cur, cur_n) = p95(window_hours);
    let (base, base_n) = p95(baseline_hours);
    (cur, cur_n, base, base_n)
}

/// Flips (a round reaching ATTRIBUTING — the batch machine's own Red transition, legal only
/// from CI_RUNNING) and total batch transitions, for the current window and the trailing
/// baseline, both read from the `bead-stage` family's `machine = 'batch'` rows.
pub fn round_health_metrics(rows: &[Value], now: u64, window_hours: f64, baseline_hours: f64) -> (u64, u64, u64, u64) {
    let moves: Vec<(f64, bool)> = transitions(rows, |r| tsd::query::text(r, "key").unwrap_or("").to_string(), |r| machine(r) == "batch")
        .into_iter()
        .filter_map(|(r, before)| {
            let (prev_state, _) = before?;
            Some((ts_secs(r)?, to_state(r) == "ATTRIBUTING" && prev_state == "CI_RUNNING"))
        })
        .collect();
    let count = |hours: f64| {
        let in_window: Vec<&(f64, bool)> = moves.iter().filter(|(t, _)| *t >= cutoff(now, hours)).collect();
        (in_window.iter().filter(|(_, flip)| *flip).count() as u64, in_window.len() as u64)
    };
    let (cur_flips, cur_transitions) = count(window_hours);
    let (base_flips, base_transitions) = count(baseline_hours);
    (cur_flips, cur_transitions, base_flips, base_transitions)
}

/// Trailing-baseline average backlog size, from the `backlog` family this crate's own pass
/// appends to every run (see [`append_backlog_sample`]) — there is no other producer of a
/// backlog series, so the first several passes on a fresh install have little history and a
/// baseline near the current value, which is safe: [`crate::core::backlog_trend_raw`] treats
/// a zero baseline as "nothing to compare against" rather than a gap.
pub fn backlog_baseline(duckdb_bin: &str, root: &Path, baseline_hours: f64) -> Result<f64, String> {
    let path = family_path(root, "backlog");
    if !path.exists() {
        return Ok(0.0);
    }
    let path_str = path.to_string_lossy();
    let sql = format!(
        "SELECT avg(count) AS baseline
         FROM read_ndjson_auto('{path_str}')
         WHERE CAST(ts AS TIMESTAMP) >= now() - INTERVAL '{baseline_hours} hours';"
    );
    let rows = duckdb_json(duckdb_bin, &sql)?;
    Ok(rows.first().map(|r| f64_field(r, "baseline")).unwrap_or(0.0))
}

/// Appends this pass's backlog count to the `backlog` family via `tsd-write`, the one writer
/// every run/tsd/ producer shells out to (sp-sbc6o), by name on the launcher's PATH
/// (sp-gypjk) — best-effort: a failed write leaves the family unwritten, never fails the
/// pass that is trying to observe it. An empty program (a unit test's "no writer") skips.
pub fn append_backlog_sample(tsd_bin: &str, root: &Path, count: u64) {
    if tsd_bin.is_empty() {
        return;
    }
    let _ = spira_config::bounded::bounded(tsd_bin)
        .args(["--family", "backlog", "--root"])
        .arg(root)
        .args(["--field", &format!("count={count}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The last two `slots` samples, most recent second — "two samples a pass apart" (design §3).
/// `Err` when fewer than two rows exist yet (a fresh install, or the collector not running
/// long enough), or when a sample has a field the collector could not read (written as the
/// literal string `"?"` by `_tsd_slots_sample`, never fabricated as a number).
pub fn slots_samples(duckdb_bin: &str, root: &Path) -> Result<(crate::core::SlotsSample, crate::core::SlotsSample), String> {
    let path = family_path(root, "slots");
    if !path.exists() {
        return Err(format!("{}: no rows yet", path.display()));
    }
    let path_str = path.to_string_lossy();
    let sql = format!(
        "SELECT TRY_CAST(live AS BIGINT) AS live, TRY_CAST(ceiling AS BIGINT) AS ceiling,
                TRY_CAST(ready AS BIGINT) AS ready, TRY_CAST(capacity_paused AS BIGINT) AS capacity_paused
         FROM read_ndjson_auto('{path_str}')
         ORDER BY CAST(ts AS TIMESTAMP) DESC
         LIMIT 2;"
    );
    let rows = duckdb_json(duckdb_bin, &sql)?;
    if rows.len() < 2 {
        return Err(format!("{}: fewer than two samples yet", path.display()));
    }
    let sample = |row: &Value| -> Result<crate::core::SlotsSample, String> {
        let live = row.get("live").and_then(Value::as_u64).ok_or_else(|| "slots: unreadable live field".to_string())?;
        let ceiling = row.get("ceiling").and_then(Value::as_u64).ok_or_else(|| "slots: unreadable ceiling field".to_string())?;
        let ready = row.get("ready").and_then(Value::as_u64).ok_or_else(|| "slots: unreadable ready field".to_string())?;
        let capacity_paused = row.get("capacity_paused").and_then(Value::as_u64).unwrap_or(0) != 0;
        Ok(crate::core::SlotsSample { live, ceiling, ready, capacity_paused })
    };
    // rows[0] is the most recent sample (DESC); the pure function takes (prev, current).
    Ok((sample(&rows[1])?, sample(&rows[0])?))
}

/// The most recently observed sentinel pass's total wall time: every `sentinel-phase` row for
/// the pass with the latest timestamp, summed — sentinel.sh's own `_phase` gives every second
/// of a pass to exactly one CHECK row, so the sum over one `pass` id is that pass's wall time.
/// Only rows within `window_secs` are read: a sentinel that has not run in that long is `Err`,
/// not a stale pass read as healthy.
pub fn sentinel_pass_wall_seconds(endpoint: Option<&str>, now: u64, window_secs: u64) -> Result<u64, String> {
    let reader = tsd::store::Reader::new(endpoint).map_err(|e| e.to_string())?;
    let rows = reader.rows("sentinel-phase", now.saturating_sub(window_secs)).map_err(|e| e.to_string())?;
    sentinel_wall(&rows).ok_or_else(|| "sentinel-phase: no row carries a pass id".to_string())
}

pub fn sentinel_wall(rows: &[Value]) -> Option<u64> {
    let latest = rows.iter().filter_map(|r| Some((tsd::query::ts_of(r)?, tsd::query::text(r, "pass")?))).max_by_key(|(t, _)| *t)?.1;
    let total: f64 = rows.iter().filter(|r| tsd::query::text(r, "pass") == Some(latest)).filter_map(|r| tsd::query::num(r, "secs")).sum();
    Some(total as u64)
}

/// Reopens (transitions to REWORK) and beads landed over `window_hours`, plus how many hours
/// of `bead-stage` history exist at all — the caller holds the rework invariant report-only
/// until that history passes 24h (design: "report-only for 24h, then alerting"), since a
/// ratio computed from a few hours of a brand-new series has no baseline behind it yet.
pub fn rework_metrics(rows: &[Value], now: u64, window_hours: f64) -> (u64, u64, f64) {
    let in_window = |r: &&Value| applied(r) && ts_secs(r).is_some_and(|t| t >= cutoff(now, window_hours));
    let reopens = rows.iter().filter(in_window).filter(|r| to_state(r) == "REWORK").count() as u64;
    let landed = rows.iter().filter(in_window).filter(|r| to_state(r) == "LANDED").count() as u64;
    let oldest = rows.iter().filter_map(ts_secs).fold(f64::INFINITY, f64::min);
    (reopens, landed, (now as f64 - oldest).max(0.0) / 3600.0)
}

/// Per lifecycle state, the p90 dwell (seconds) for the current window and the trailing
/// baseline, plus how many completed transitions (a bead entering the state, then leaving it
/// for any next state) each is drawn from. A state with no completed transition in either
/// window is simply absent from the result — the caller (main.rs) evaluates every state in
/// [`crate::core::DWELL_REGRESSION_STATES`] regardless, defaulting an absent one to "no
/// history", the same as a fresh install.
pub fn dwell_regression_metrics(rows: &[Value], now: u64, window_hours: f64, baseline_hours: f64) -> Vec<(String, f64, u64, f64, u64)> {
    // Bucketed by when a dwell COMPLETED (the row leaving the state), not when it started —
    // the same convention `dwell_metrics` uses for CERTIFIED->LANDED: a dwell is only knowable
    // once it is over, so "the current window" means "finished recently", and a dwell that
    // started long ago but only just ended still counts as current.
    let mut by_state: std::collections::BTreeMap<&str, Vec<(f64, f64)>> = Default::default();
    for (r, before) in transitions(rows, |r| tsd::query::text(r, "key").unwrap_or("").to_string(), |_| true) {
        let Some((prev_state, prev_ts)) = before else { continue };
        let Some(t) = ts_secs(r) else { continue };
        by_state.entry(prev_state).or_default().push((t, t - prev_ts));
    }
    let p90 = |dwells: &[(f64, f64)], hours: f64| {
        let v: Vec<f64> = dwells.iter().filter(|(t, _)| *t >= cutoff(now, hours)).map(|(_, d)| *d).collect();
        (tsd::query::quantile(&v, 0.9).unwrap_or(0.0), v.len() as u64)
    };
    by_state
        .into_iter()
        .map(|(state, dwells)| {
            let (cur, cur_n) = p90(&dwells, window_hours);
            let (base, base_n) = p90(&dwells, baseline_hours);
            (state.to_string(), cur, cur_n, base, base_n)
        })
        .collect()
}

/// The optional per-stage overrides from the desired-state document (sp-xqhog): a velocity
/// floor for the "queue" stage (events/hour) and a dwell limit for the "review" stage
/// (seconds) — the same stage names the document's own shipped example fragment uses. Any
/// failure to read or parse the document falls back to `(None, None)`: these are optional
/// overrides layered on top of a baseline comparison that works without them, not a required
/// observation, so an absent or malformed document narrows coverage rather than blinding the
/// invariant entirely.
pub fn flow_floors(desired_dir: &Path) -> (Option<f64>, Option<u64>) {
    read_flow_floors(desired_dir).unwrap_or((None, None))
}

fn read_flow_floors(desired_dir: &Path) -> Option<(Option<f64>, Option<u64>)> {
    let current = fs::read_to_string(desired_dir.join("current")).ok()?;
    let version: u64 = current.trim().parse().ok()?;
    let doc_path = desired_dir.join("versions").join(format!("{version:06}.toml"));
    let text = fs::read_to_string(&doc_path).ok()?;
    let doc: toml::Value = toml::from_str(&text).ok()?;
    let resources = doc.get("resource")?.as_array()?;
    let flow = resources.iter().find(|r| r.get("kind").and_then(|k| k.as_str()) == Some("Flow"))?;
    let spec = flow.get("spec")?;
    let velocity_floor = spec
        .get("velocity_floor")
        .and_then(|v| v.get("queue"))
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
    let dwell_limit = spec
        .get("dwell_limit_seconds")
        .and_then(|v| v.get("review"))
        .and_then(|v| v.as_integer())
        .map(|i| i as u64);
    Some((velocity_floor, dwell_limit))
}

#[cfg(test)]
mod backlog_tests {
    use super::*;
    use spira_config::lc_state::Row;

    /// sp-mve9i: the backlog is the beads whose lifecycle row is not terminal.
    #[test]
    fn the_backlog_is_the_non_terminal_lifecycle_rows() {
        let lc = ["READY", "WORKING", "SUBMITTED", "LANDED", "DROPPED"]
            .iter()
            .enumerate()
            .map(|(i, st)| (format!("sp-{i}"), Row { bead_id: format!("sp-{i}"), state: st.to_string(), ..Default::default() }))
            .collect();
        let ids: Vec<String> = (0..6).map(|i| format!("sp-{i}")).collect();
        assert_eq!(backlog_of(&ids, &lc), 3);
    }
}
