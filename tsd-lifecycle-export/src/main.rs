// tsd-lifecycle-export — the one writer of run/tsd/'s `bead-stage` family (design
// reconciler-time-series-2026-09-27 §2/§2a). Three modes, one schema:
//
//   tsd-lifecycle-export legacy    [--since <ISO8601>]   bd events, ongoing tail
//   tsd-lifecycle-export backfill  [--since <ISO8601>]   the same, one-time, back to a date
//   tsd-lifecycle-export lifecycle                       spira_lifecycle.event, post-cutover
//
// Every mode tracks its own progress in $SPIRA_RUN/tsd/.bead-stage-checkpoint-<mode>.json, so
// re-running any of them exports nothing new. Rows are appended through tsd-write (by name)
// (tsd-write), the one IO seam every run/tsd/ producer already shells out to — never a
// second, in-process writer of the same file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tsd_lifecycle_export::{
    advance_legacy_checkpoint, fold_legacy, map_lifecycle_row, BdAuditEvent, LegacyCheckpoint, LifecycleCheckpoint, LifecycleEventRow, StageRow,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str);
    let since = flag(&args, "--since");
    let result = match mode {
        Some("legacy") => run_legacy("legacy", since.as_deref().unwrap_or("now")),
        Some("backfill") => run_legacy("backfill", since.as_deref().unwrap_or("2026-09-15T00:00:00Z")),
        Some("lifecycle") => run_lifecycle(),
        _ => {
            eprintln!("usage: tsd-lifecycle-export <legacy|backfill|lifecycle> [--since <ISO8601>]");
            std::process::exit(2);
        }
    };
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

fn read_checkpoint<T: Default + serde::de::DeserializeOwned>(path: &Path) -> T {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write_checkpoint<T: serde::Serialize>(path: &Path, cp: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string(cp).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn now_iso() -> String {
    Command::new("date")
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
    let out = Command::new(&bd)
        .args(["-C", &db, "sql", "--json", &query])
        .output()
        .map_err(|e| format!("running {bd}: {e}"))?;
    if !out.status.success() {
        return Err(format!("bd sql: {}", String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parsing bd sql --json output: {e}"))
}

// ── lifecycle: spira_lifecycle.event, post-cutover ────────────────────────────────────────

fn run_lifecycle() -> Result<(), String> {
    let run = spira_run()?;
    let cp_path = checkpoint_path(&run, "lifecycle");
    let cp: LifecycleCheckpoint = read_checkpoint(&cp_path);

    let conn = LcRoConn::from_env()?;
    let query = format!(
        "SELECT seq, machine, lc_key, event, from_state, to_state, applied, refusal, evidence, actor, at \
         FROM event WHERE seq > {} ORDER BY seq",
        cp.last_seq
    );
    let raw_rows = conn.query(&query)?;
    if raw_rows.is_empty() {
        return Ok(());
    }

    let mut events = Vec::with_capacity(raw_rows.len());
    let mut last_seq = cp.last_seq;
    for r in &raw_rows {
        let seq: i64 = field_str(r, "seq")?.parse().map_err(|e| format!("seq: {e}"))?;
        let at: i64 = field_str(r, "at")?.parse().map_err(|e| format!("at: {e}"))?;
        let applied = field_str(r, "applied")? != "0";
        let refusal = r.get("refusal").and_then(|v| v.as_str()).map(str::to_string);
        let evidence_raw = field_str(r, "evidence").unwrap_or_default();
        let evidence = serde_json::from_str(&evidence_raw).unwrap_or(serde_json::Value::Null);
        events.push(LifecycleEventRow {
            seq,
            machine: field_str(r, "machine")?,
            lc_key: field_str(r, "lc_key")?,
            event: field_str(r, "event")?,
            from_state: field_str(r, "from_state")?,
            to_state: field_str(r, "to_state")?,
            applied,
            refusal,
            evidence,
            actor: field_str(r, "actor")?,
            at,
        });
        last_seq = last_seq.max(seq);
    }

    let rows: Vec<StageRow> = events.iter().map(map_lifecycle_row).collect();
    write_rows(&run, &rows)?;
    write_checkpoint(&cp_path, &LifecycleCheckpoint { last_seq })
}

fn field_str(row: &serde_json::Value, key: &str) -> Result<String, String> {
    row.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("missing or non-string field {key:?} in {row}"))
}

/// A minimal, read-only connection to `spira_lifecycle`, deliberately separate from
/// `spira-lc`'s own `Conn` (which defaults to the `spira_lc` credential): this exporter must
/// never hold `spira_lc`'s INSERT/UPDATE authority, only `spira_lc_ro`'s SELECT (design row
/// 1). Same `SPIRA_LC_*` env vars as `spira-lc` reads, so an operator can point either at the
/// read-only user the same way, but the *default* user here is the read-only one.
struct LcRoConn {
    dolt_bin: String,
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
    data_dir: String,
}

impl LcRoConn {
    fn from_env() -> Result<Self, String> {
        // SPIRA_LC_DOLT_BIN/HOST/PORT/USER/PASSWORD/DB/DATA_DIR are not registered config keys
        // (no spira/conf.d entry) — left reading the raw environment. Only
        // SPIRA_LC_PASSWORD_FILE below is registered.
        let dolt_bin = std::env::var("SPIRA_LC_DOLT_BIN").unwrap_or_else(|_| "dolt".to_string());
        let host = std::env::var("SPIRA_LC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port: u16 = std::env::var("SPIRA_LC_PORT")
            .unwrap_or_else(|_| "3307".to_string())
            .parse()
            .map_err(|e| format!("SPIRA_LC_PORT: {e}"))?;
        let user = std::env::var("SPIRA_LC_USER").unwrap_or_else(|_| "spira_lc_ro".to_string());
        let password = match spira_config::process::cfg("SPIRA_LC_PASSWORD_FILE")?.as_str() {
            "" => std::env::var("SPIRA_LC_PASSWORD").unwrap_or_default(),
            path => std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?.trim().to_string(),
        };
        let database = std::env::var("SPIRA_LC_DB").unwrap_or_else(|_| "spira_lifecycle".to_string());
        let data_dir = std::env::var("SPIRA_LC_DATA_DIR").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| {
            // Never /tmp itself: dolt lstat()s every entry and vanishing ones fail the call.
            let d = std::env::temp_dir().join("spira-lc-data");
            let _ = std::fs::create_dir_all(&d);
            d.display().to_string()
        });
        Ok(LcRoConn { dolt_bin, host, port, user, password, database, data_dir })
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.dolt_bin);
        cmd.arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--host")
            .arg(&self.host)
            .arg("--port")
            .arg(self.port.to_string())
            .arg("-u")
            .arg(&self.user)
            .arg("--no-tls")
            .arg("--use-db")
            .arg(&self.database)
            .args(["sql", "-r", "json"])
            .env("DOLT_CLI_PASSWORD", &self.password);
        cmd
    }

    fn query(&self, sql: &str) -> Result<Vec<serde_json::Value>, String> {
        let stmt = if sql.trim_end().ends_with(';') { sql.to_string() } else { format!("{sql};") };
        let mut child = self
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawning {}: {e}", self.dolt_bin))?;
        child.stdin.take().expect("piped stdin").write_all(stmt.as_bytes()).map_err(|e| format!("writing query: {e}"))?;
        let out = child.wait_with_output().map_err(|e| format!("waiting on {}: {e}", self.dolt_bin))?;
        if !out.status.success() {
            return Err(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(serde_json::Deserializer::from_str(&text)
            .into_iter::<serde_json::Value>()
            .flatten()
            .filter_map(|v| v.get("rows").and_then(|r| r.as_array()).cloned())
            .flatten()
            .collect())
    }
}

// ── the write path: every row through tsd-write, never a second in-process writer ─────────

fn write_rows(run: &Path, rows: &[StageRow]) -> Result<(), String> {
    // tsd-write, by name on the launcher's PATH (sp-gypjk); a missing one fails the spawn below.
    let tsd_bin = "tsd-write";
    for row in rows {
        let mut cmd = Command::new(tsd_bin);
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

#[cfg(test)]
mod argv_secret_tests {
    use super::LcRoConn;

    #[test]
    fn the_password_is_never_an_argument_to_dolt() {
        let conn = LcRoConn {
            dolt_bin: "dolt".into(),
            host: "h".into(),
            port: 1,
            user: "u".into(),
            password: "s3cret-pw".into(),
            database: "d".into(),
            data_dir: "/x".into(),
        };
        let cmd = conn.command();
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(!args.iter().any(|a| a.contains("s3cret-pw") || a == "-p" || a == "--password"), "{args:?}");
        assert!(cmd.get_envs().any(|(k, v)| k == "DOLT_CLI_PASSWORD" && v.is_some_and(|v| v == "s3cret-pw")));
    }
}
