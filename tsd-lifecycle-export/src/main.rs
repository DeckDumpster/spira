// tsd-lifecycle-export — the one writer of run/tsd/'s `bead-stage` family (design
// reconciler-time-series-2026-09-27 §2/§2a). Four modes, one schema:
//
//   tsd-lifecycle-export legacy    [--since <ISO8601>]   bd events, ongoing tail
//   tsd-lifecycle-export backfill  [--since <ISO8601>]   the same, one-time, back to a date
//   tsd-lifecycle-export lifecycle                       the control store's event log
//   tsd-lifecycle-export check                           fails when the export trails the log by over 10 minutes
//
// Every mode tracks its own progress in $SPIRA_RUN/tsd/.bead-stage-checkpoint-<mode>.json, so
// re-running any of them exports nothing new. Rows are appended through tsd-write (by name)
// (tsd-write), the one IO seam every run/tsd/ producer already shells out to — never a
// second, in-process writer of the same file.

use std::path::{Path, PathBuf};

use tsd_lifecycle_export::{
    advance_legacy_checkpoint, fold_legacy, BdAuditEvent, LegacyCheckpoint, StageRow,
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
    write_rows(&run, &rows)?;
    write_checkpoint(&cp_path, &new_cp)
}

fn fetch_bd_events(since: &str) -> Result<Vec<BdAuditEvent>, String> {
    let bd = spira_config::process::cfg("SPIRA_BD")?;
    let db = spira_config::process::cfg("SPIRA_DB")?;
    let query = format!(
        "SELECT id, issue_id, event_type, actor, new_value, created_at FROM events \
         WHERE created_at >= {} ORDER BY created_at, id",
        sql_str(since)
    );
    let out = spira_config::bounded::bounded(&bd)
        .args(["-C", &db, "sql", "--json", &query])
        .output()
        .map_err(|e| format!("running {bd}: {e}"))?;
    if !out.status.success() {
        return Err(format!("bd sql: {}", String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parsing bd sql --json output: {e}"))
}

// ── lifecycle: the control store's event log ─────────────────────────────────────────────

const UNSERVED: &str = "the lifecycle event log lives in the control store, which this exporter has no reader for yet";

fn run_lifecycle() -> Result<(), String> {
    Err(UNSERVED.into())
}

fn run_check() -> Result<(), String> {
    Err(UNSERVED.into())
}

// ── the write path: every row through tsd-write, never a second in-process writer ─────────

fn write_rows(run: &Path, rows: &[StageRow]) -> Result<(), String> {
    // tsd-write, by name on the launcher's PATH (sp-gypjk); a missing one fails the spawn below.
    let tsd_bin = "tsd-write";
    for row in rows {
        let mut cmd = spira_config::bounded::bounded(tsd_bin);
        cmd.args(["--family", "bead-stage", "--root"])
            .arg(run)
            .args(["--ts", &row.ts])
            .args(["--field", &format!("seq={}", row.seq)])
            .args(["--field-str", &format!("machine={}", row.machine)])
            .args(["--field-str", &format!("key={}", row.key)])
            .args(["--field-str", &format!("event={}", row.event)])
            .args(["--field-str", &format!("from_state={}", row.from_state)])
            .args(["--field-str", &format!("to_state={}", row.to_state)])
            .args(["--field", &format!("applied={}", row.applied)])
            .args(["--field-str", &format!("actor={}", row.actor)])
            .args(["--field-str", &format!("source={}", row.source)]);
        if let Some(refusal) = &row.refusal {
            cmd.args(["--field-str", &format!("refusal={refusal}")]);
        }
        if let Some(reason) = &row.reason {
            cmd.args(["--field-str", &format!("reason={reason}")]);
        }
        let status = cmd.status().map_err(|e| format!("running {tsd_bin}: {e}"))?;
        if !status.success() {
            return Err(format!("{tsd_bin} exited {status} writing seq {} for {}", row.seq, row.key));
        }
    }
    Ok(())
}
