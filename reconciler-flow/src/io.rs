//! The IO seam: everywhere this crate touches a clock, a file, a subprocess or the network.
//! Every function returns a `Result` and never guesses — a query that cannot run comes back
//! `Err`, and [`crate::main`] turns that into `RawStatus::Unobservable`, never `Satisfied`
//! (law-a-control-that-cannot-check-must-refuse). Nothing in [`crate::core`] calls any of
//! this directly, so the pure half never needs a duckdb binary, a bead store or a mailbox to
//! run its own tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use std::io::Write as _;

/// One duckdb -json -c invocation, parsed. Empty stdout (a `SELECT` with no matching rows in
/// an old duckdb, or a query that legitimately returns nothing) is `Ok(vec![])`, not an
/// error — the caller decides whether "no rows" is itself a gap.
pub fn duckdb_json(bin: &str, sql: &str) -> Result<Vec<Value>, String> {
    let out = Command::new(bin)
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

fn u64_field(row: &Value, key: &str) -> u64 {
    row.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The count of beads in the backlog right now: everything not yet closed. Scoped by
/// `scope_label` when the harness serves more than one repository, same convention
/// czar-pass uses for its own detectors.
pub fn backlog_count(bd_bin: &str, spira_db: &str, scope_label: &str) -> Result<u64, String> {
    let mut cmd = Command::new(bd_bin);
    cmd.arg("-C").arg(spira_db).args([
        "list", "--status", "open,in_progress,blocked,deferred",
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
    Ok(rows.len() as u64)
}

/// The bead-machine states a bead is still in flight in — after these, it has either landed
/// or left the pipeline some other terminal way. CERTIFIED and the terminal states are
/// deliberately excluded (design reconciler-time-series-2026-09-27 §3: the dwell invariant
/// names exactly READY, WORKING, SUBMITTED, IN_DELIVERY, REWORK).
const BEAD_DWELL_STATES: [&str; 5] = ["READY", "WORKING", "SUBMITTED", "IN_DELIVERY", "REWORK"];

/// The batch-machine states a round is still in flight in — LANDED, SETTLED and ABANDONED are
/// terminal, so nothing dwells in them.
const BATCH_DWELL_STATES: [&str; 5] = ["OPEN", "CI_RUNNING", "GREEN", "ATTRIBUTING", "REBUILDING"];

fn quoted_in_list(states: &[&str]) -> String {
    states.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ")
}

/// How many beads are in a state that still needs to land — submitted, certified or already
/// handed to delivery, but not there yet — read from each bead's latest `bead-stage` row. A
/// family that does not exist yet is a fresh install with nothing in flight, not a failure.
pub fn waiting_to_land(duckdb_bin: &str, root: &Path) -> Result<u64, String> {
    let path = family_path(root, "bead-stage");
    if !path.exists() {
        return Ok(0);
    }
    let path_str = path.to_string_lossy();
    let sql = format!(
        "WITH latest AS (
            SELECT to_state,
                   row_number() OVER (PARTITION BY key ORDER BY seq DESC) AS rn
            FROM read_ndjson_auto('{path_str}')
            WHERE machine = 'bead' AND applied
         )
         SELECT count(*) AS n FROM latest WHERE rn = 1 AND to_state IN ('SUBMITTED', 'CERTIFIED', 'IN_DELIVERY');"
    );
    let rows = duckdb_json(duckdb_bin, &sql)?;
    Ok(rows.first().map(|r| u64_field(r, "n")).unwrap_or(0))
}

/// Current-window and trailing-baseline land rate (events/hour), both read from the
/// `bead-stage` family in one query — a bead reaching LANDED, the lifecycle machine's own
/// terminal state (design §2a: states are the machine's). `Err` when the family has no rows
/// yet at all — a fresh install with nothing ever landed cannot say whether landing has
/// stalled.
pub fn velocity_metrics(
    duckdb_bin: &str,
    root: &Path,
    window_hours: f64,
    baseline_hours: f64,
) -> Result<(f64, f64), String> {
    let path = family_path(root, "bead-stage");
    if !path.exists() {
        return Err(format!("{}: no rows yet", path.display()));
    }
    let path_str = path.to_string_lossy();
    let sql = format!(
        "SELECT
            count(*) FILTER (WHERE machine = 'bead' AND applied AND to_state = 'LANDED' AND CAST(ts AS TIMESTAMP) >= now() - INTERVAL '{window_hours} hours') AS cur_n,
            count(*) FILTER (WHERE machine = 'bead' AND applied AND to_state = 'LANDED' AND CAST(ts AS TIMESTAMP) >= now() - INTERVAL '{baseline_hours} hours') AS base_n
         FROM read_ndjson_auto('{path_str}');"
    );
    let rows = duckdb_json(duckdb_bin, &sql)?;
    let row = rows.first().ok_or_else(|| "velocity query returned no row".to_string())?;
    let cur_n = f64_field(row, "cur_n");
    let base_n = f64_field(row, "base_n");
    Ok((cur_n / window_hours.max(1e-9), base_n / baseline_hours.max(1e-9)))
}

/// p95 dwell (seconds) across every in-flight bead state (READY, WORKING, SUBMITTED,
/// IN_DELIVERY, REWORK) and every in-flight batch state (OPEN, CI_RUNNING, GREEN,
/// ATTRIBUTING, REBUILDING), pooled into one series — for the current window and the
/// trailing baseline, plus how many transitions each is drawn from. `bead` and `batch` rows
/// share one `bead-stage` file, so dwell is windowed `PARTITION BY (machine, key)` to keep a
/// bead's own timeline from a batch's.
pub fn dwell_metrics(
    duckdb_bin: &str,
    root: &Path,
    window_hours: f64,
    baseline_hours: f64,
) -> Result<(f64, u64, f64, u64), String> {
    let path = family_path(root, "bead-stage");
    if !path.exists() {
        return Err(format!("{}: no rows yet", path.display()));
    }
    let path_str = path.to_string_lossy();
    let dwell_states = quoted_in_list(&[BEAD_DWELL_STATES.as_slice(), BATCH_DWELL_STATES.as_slice()].concat());
    let sql = format!(
        "WITH ordered AS (
            SELECT machine, key, to_state, CAST(ts AS TIMESTAMP) AS ts,
                   LAG(to_state) OVER (PARTITION BY machine, key ORDER BY seq) AS prev_state,
                   LAG(CAST(ts AS TIMESTAMP)) OVER (PARTITION BY machine, key ORDER BY seq) AS prev_ts
            FROM read_ndjson_auto('{path_str}')
            WHERE applied
         ), dwells AS (
            SELECT ts, epoch(ts) - epoch(prev_ts) AS dwell_s
            FROM ordered
            WHERE prev_state IN ({dwell_states})
         )
         SELECT
            quantile_cont(dwell_s, 0.95) FILTER (WHERE ts >= now() - INTERVAL '{window_hours} hours') AS cur_p95,
            count(*) FILTER (WHERE ts >= now() - INTERVAL '{window_hours} hours') AS cur_n,
            quantile_cont(dwell_s, 0.95) FILTER (WHERE ts >= now() - INTERVAL '{baseline_hours} hours') AS base_p95,
            count(*) FILTER (WHERE ts >= now() - INTERVAL '{baseline_hours} hours') AS base_n
         FROM dwells;"
    );
    let rows = duckdb_json(duckdb_bin, &sql)?;
    let row = rows.first().ok_or_else(|| "dwell query returned no row".to_string())?;
    Ok((f64_field(row, "cur_p95"), u64_field(row, "cur_n"), f64_field(row, "base_p95"), u64_field(row, "base_n")))
}

/// Flips (a round reaching ATTRIBUTING — the batch machine's own Red transition, legal only
/// from CI_RUNNING) and total batch transitions, for the current window and the trailing
/// baseline, both read from the `bead-stage` family's `machine = 'batch'` rows.
pub fn round_health_metrics(
    duckdb_bin: &str,
    root: &Path,
    window_hours: f64,
    baseline_hours: f64,
) -> Result<(u64, u64, u64, u64), String> {
    let path = family_path(root, "bead-stage");
    if !path.exists() {
        return Err(format!("{}: no rows yet", path.display()));
    }
    let path_str = path.to_string_lossy();
    let sql = format!(
        "WITH ordered AS (
            SELECT key, to_state, CAST(ts AS TIMESTAMP) AS ts,
                   LAG(to_state) OVER (PARTITION BY key ORDER BY seq) AS prev_state
            FROM read_ndjson_auto('{path_str}')
            WHERE machine = 'batch' AND applied
         )
         SELECT
            count(*) FILTER (WHERE prev_state IS NOT NULL AND to_state = 'ATTRIBUTING' AND prev_state = 'CI_RUNNING'
                              AND ts >= now() - INTERVAL '{window_hours} hours') AS cur_flips,
            count(*) FILTER (WHERE prev_state IS NOT NULL AND ts >= now() - INTERVAL '{window_hours} hours') AS cur_transitions,
            count(*) FILTER (WHERE prev_state IS NOT NULL AND to_state = 'ATTRIBUTING' AND prev_state = 'CI_RUNNING'
                              AND ts >= now() - INTERVAL '{baseline_hours} hours') AS base_flips,
            count(*) FILTER (WHERE prev_state IS NOT NULL AND ts >= now() - INTERVAL '{baseline_hours} hours') AS base_transitions
         FROM ordered;"
    );
    let rows = duckdb_json(duckdb_bin, &sql)?;
    let row = rows.first().ok_or_else(|| "round-health query returned no row".to_string())?;
    Ok((
        u64_field(row, "cur_flips"),
        u64_field(row, "cur_transitions"),
        u64_field(row, "base_flips"),
        u64_field(row, "base_transitions"),
    ))
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
/// every run/tsd/ producer shells out to (sp-sbc6o) — best-effort: a missing or unbuilt
/// binary leaves the family unwritten, never fails the pass that is trying to observe it.
pub fn append_backlog_sample(tsd_bin: &str, root: &Path, count: u64) {
    if tsd_bin.is_empty() || !Path::new(tsd_bin).is_file() {
        return;
    }
    let _ = Command::new(tsd_bin)
        .args(["--family", "backlog", "--root"])
        .arg(root)
        .args(["--field", &format!("count={count}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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

/// Sends one message to the Concierge mailbox — the alert path a flow gap has no
/// deterministic remedy to try instead of (per the design). Deduplication across passes is
/// explicitly a separate bead's mandate (sp-fufyb); this sends once per pass a gap is
/// confirmed (`is_gap`), relying on mail.sh's own settle/tidy handling in the meantime.
pub fn mail_concierge(mail_sh: &str, subject: &str, body: &str) -> Result<(), String> {
    let mut child = Command::new("bash")
        .arg(mail_sh)
        .args(["send", "concierge", "--from", "Reconciler <reconciler@spira>", "--subject", subject, "--kind", "note"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{mail_sh}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("{mail_sh}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{mail_sh} send concierge: exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}
