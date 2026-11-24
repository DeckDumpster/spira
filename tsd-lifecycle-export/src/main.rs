// tsd-lifecycle-export — the one writer of run/tsd/'s `bead-stage` family (design
// reconciler-time-series-2026-09-27 §2/§2a). Four modes, one schema:
//
//   tsd-lifecycle-export legacy    [--since <ISO8601>]   bd events, ongoing tail
//   tsd-lifecycle-export backfill  [--since <ISO8601>]   the same, one-time, back to a date
//   tsd-lifecycle-export lifecycle                       the control store's event log
//   tsd-lifecycle-export check                           fails when the export trails the log by over 10 minutes
//
// Every mode tracks its own progress in $SPIRA_RUN/tsd/.bead-stage-checkpoint-<mode>.json, so
// re-running any of them exports nothing new. Rows are pushed to the metrics store in
// batches from this process; no child program is started.

use std::path::{Path, PathBuf};

use tsd_lifecycle_export::{
    advance_legacy_checkpoint, drain, fold_legacy, lag_verdict, lifecycle_batch, newest_event, BdAuditEvent, LegacyCheckpoint,
    LifecycleCheckpoint, StageRow,
};

fn main() {
    if let Err(e) = spira_config::release_env::ensure_release_path() {
        eprintln!("tsd-lifecycle-export: {e}");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str);
    let since = flag(&args, "--since");
    let sink = spira_otel::metrics::MetricsSink::for_component(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "spira-tsd-lifecycle-export");
    let result = spira_otel::metrics::instrument_result(&sink, "tsd-lifecycle-export", mode.unwrap_or("none"), || match mode {
        Some("legacy") => run_legacy("legacy", since.as_deref().unwrap_or("now")),
        Some("backfill") => run_legacy("backfill", since.as_deref().unwrap_or("2026-09-15T00:00:00Z")),
        Some("lifecycle") => run_lifecycle(),
        Some("check") => run_check(),
        _ => Err("usage: tsd-lifecycle-export <legacy|backfill|lifecycle|check> [--since <ISO8601>]".to_string()),
    });
    if let Err(e) = result {
        eprintln!("tsd-lifecycle-export: {e}");
        std::process::exit(1);
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn spira_run() -> Result<PathBuf, String> {
    Ok(PathBuf::from(spira_config::process::cfg("SPIRA_RUN")?))
}

fn checkpoint_path(run: &Path, mode: &str) -> PathBuf {
    run.join("tsd").join(format!(".bead-stage-checkpoint-{mode}.json"))
}

fn write_checkpoint<T: serde::Serialize>(path: &Path, cp: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string(cp).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn now_iso() -> String {
    spira_config::bounded::bounded("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

// ── legacy / backfill: bd's audit `events` table ─────────────

fn run_legacy(mode: &str, default_since: &str) -> Result<(), String> {
    let run = spira_run()?;
    let cp_path = checkpoint_path(&run, mode);
    let default_since = if default_since == "now" { now_iso() } else { default_since.to_string() };
    let cp: LegacyCheckpoint = {
        let loaded: Option<LegacyCheckpoint> =
            std::fs::read_to_string(&cp_path).ok().and_then(|s| serde_json::from_str(&s).ok());
        loaded.unwrap_or_else(|| LegacyCheckpoint::starting_at(&default_since))
    };

    let events = fetch_bd_events(&cp.last_created_at)?;
    let (fresh, new_cp) = advance_legacy_checkpoint(&events, &cp);
    if fresh.is_empty() {
        write_checkpoint(&cp_path, &new_cp)?;
        return Ok(());
    }

    let rows = fold_legacy(&fresh, mode, cp.next_seq);
    write_rows(&rows)?;
    write_checkpoint(&cp_path, &new_cp)
}

fn fetch_bd_events(since: &str) -> Result<Vec<BdAuditEvent>, String> {
    let lc = spira_store::lifecycle_row::lc_bin()?;
    let query = format!(
        "SELECT id, issue_id, event_type, actor, new_value, created_at FROM events \
         WHERE created_at >= {} ORDER BY created_at, id",
        sql_str(since)
    );
    let mut child = spira_config::bounded::bounded(&lc)
        .args(["admin-store", "sql", "--json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("running {lc}: {e}"))?;
    std::io::Write::write_all(&mut child.stdin.take().ok_or("no stdin")?, query.as_bytes()).map_err(|e| format!("writing the query: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("running {lc}: {e}"))?;
    if !out.status.success() {
        return Err(format!("admin-store sql: {}", String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parsing admin-store sql --json output: {e}"))
}

// ── lifecycle: the control store's event log ─────────────────────────────────────────────

const LAG_LIMIT_SECS: i64 = 600;

fn read_checkpoint<T: Default + serde::de::DeserializeOwned>(path: &Path) -> T {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn run_lifecycle() -> Result<(), String> {
    let run = spira_run()?;
    let cp_path = checkpoint_path(&run, "lifecycle");
    let cp: LifecycleCheckpoint = read_checkpoint(&cp_path);
    let lc = spira_store::lc_call::LcClient::configured()?;
    drain(cp, |after| lifecycle_batch(&lc, after), write_rows, |cp| write_checkpoint(&cp_path, cp)).map(|_| ())
}

/// Fails when the newest `bead-stage` row trails the newest lifecycle event by over [`LAG_LIMIT_SECS`].
fn run_check() -> Result<(), String> {
    let lc = spira_store::lc_call::LcClient::configured()?;
    let Some(newest_event) = newest_event(&lc)? else { return Ok(()) };
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    let store = spira_otel::query::Store::new(endpoint.as_deref()).map_err(|e| e.to_string())?;
    let from = u64::try_from(newest_event - 86_400).unwrap_or(0);
    let series = store.export("spira_tsd_value{tsd_family=\"bead-stage\"}", from).map_err(|e| e.to_string())?;
    let newest_stage = series.iter().flat_map(|s| s.samples.iter().map(|(ms, _)| ms / 1000)).max();
    lag_verdict(newest_stage, newest_event, LAG_LIMIT_SECS)
}

// ── the write path: rows go to the metrics store from this process ───────────────────────

fn write_rows(rows: &[StageRow]) -> Result<(), String> {
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT")?;
    tsd_lifecycle_export::push_stage_rows(&endpoint, rows)
}
