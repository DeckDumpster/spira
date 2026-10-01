//! The only I/O: reading bd and spira-lc through their CLIs, and config through
//! spira-config. Every read is bounded in time and in argv size. DESIGN.md §2 "Data in".

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::events::{self, EventRow};
use crate::rank::{self, ReadyRow};

/// Ids per `bd sql` events query and per `bd list --id` call: keeps the argv bounded.
pub const EVENT_CHUNK: usize = 200;
pub const LIST_CHUNK: usize = 100;

#[derive(Debug, Clone)]
pub struct Store {
    pub bd: String,
    pub db: Option<String>,
    pub lc: String,
    pub timeout: Duration,
}

/// Values from spira.toml through the spira-config library (law-config-through-the-cli-only:
/// never parsed here).
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub db: Option<String>,
    pub stack_max_depth: Option<u32>,
    pub submitted_label: Option<String>,
    /// `spira.run` — the runtime directory (`unpoison`: ask history, audit log).
    pub run: Option<String>,
    /// `spira.ask_label` — the operator-ask label (`unpoison`: the ask it closes).
    pub ask_label: Option<String>,
    /// `spira.lifecycle_enforce` — whether the lifecycle machine is the poison's record.
    pub lifecycle_enforce: Option<bool>,
    /// `spira.scope_label` (`ready_args`/`fayth_ready`; conf.sh's own default for this one
    /// is procedural — `SPIRA_HOME_REPO` — so a caller with no toml value falls back to the
    /// already-exported `$SPIRA_SCOPE_LABEL` rather than this field, never to a constant).
    pub scope_label: Option<String>,
    /// `spira.no_loop_label` (default `no-loop`, conf.d/SPIRA_NO_LOOP_LABEL).
    pub no_loop_label: Option<String>,
    /// `spira.queue_wait_label` (default `spira-queue-waiting`) — one of `fayth_exclude`'s
    /// shared exclusions; UNEXPORTED by conf.sh, so this field (resolved in-process) is the
    /// only correct source for a separate process (the exec-boundary trap).
    pub queue_wait_label: Option<String>,
    /// `spira.open_children_label` (default `spira-open-children`) — ditto, unexported.
    pub open_children_label: Option<String>,
    /// `spira.claim_retries` (default 3) — `SPIRA_CLAIM_RETRIES`, unexported.
    pub claim_retries: Option<u32>,
    /// `spira.claim_retry_delay_s` (default 1) — `SPIRA_CLAIM_RETRY_DELAY_S`, unexported.
    pub claim_retry_delay_s: Option<u32>,
}

pub fn load_config() -> Config {
    let Some(path) = spira_config::discover(None) else { return Config::default() };
    match spira_config::load(&path) {
        Ok(doc) => {
            let s = doc.spira.unwrap_or_default();
            Config {
                db: s.db,
                stack_max_depth: s.stack_max_depth,
                submitted_label: s.submitted_label,
                run: s.run,
                ask_label: s.ask_label,
                lifecycle_enforce: s.lifecycle_enforce,
                scope_label: s.scope_label,
                no_loop_label: s.no_loop_label,
                queue_wait_label: s.queue_wait_label,
                open_children_label: s.open_children_label,
                claim_retries: s.claim_retries.as_deref().and_then(|v| v.trim().parse().ok()),
                claim_retry_delay_s: s.claim_retry_delay_s.as_deref().and_then(|v| v.trim().parse().ok()),
            }
        }
        Err(e) => {
            eprintln!("spira-claim: config {}: {e} — using defaults", path.display());
            Config::default()
        }
    }
}

impl Store {
    pub fn new(db_flag: Option<String>, timeout_s: u64, cfg: &Config) -> Store {
        Store {
            bd: std::env::var("SPIRA_BD").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "bd".into()),
            db: db_flag
                .or_else(|| std::env::var("SPIRA_DB").ok().filter(|s| !s.is_empty()))
                .or_else(|| cfg.db.clone()),
            lc: "spira-lc".into(), // by name, on the launcher's PATH (sp-gypjk)
            timeout: Duration::from_secs(timeout_s.max(1)),
        }
    }

    pub fn bd_cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.bd);
        if let Some(db) = &self.db {
            c.args(["-C", db]);
        }
        c.args(args);
        c
    }

    /// Run `bd <args>` to completion; `Err` names the verb and bd's own first stderr line
    /// (or the timeout/spawn failure). `pub` so `ready_count`/`cmd_claim_retry` (the
    /// generic bd-retry verb, which takes arbitrary caller argv main.rs never parses) can
    /// call it directly, same as every other read here.
    pub fn bd(&self, args: &[&str]) -> Result<String, String> {
        run(self.bd_cmd(args), self.timeout).map_err(|e| format!("bd {}: {e}", args.first().unwrap_or(&"")))
    }

    /// `ready_count <labels> <exclude-labels>` (lib.sh:459): one bd `ready` query, counted.
    /// A FAILED QUERY IS NOT A ZERO (sp-3ntca) — `Err` names the failure; the caller prints
    /// '0' to stdout regardless (every existing caller of the bash form only ever reads
    /// stdout, never the exit code, so stdout must still carry a number).
    pub fn ready_count(&self, ready_args: &[String], labels: &str, exclude: &str) -> Result<u64, String> {
        let mut args: Vec<&str> = ready_args.iter().map(String::as_str).collect();
        args.push("--label");
        args.push(labels);
        args.push("--exclude-label");
        args.push(exclude);
        args.push("--json");
        let text = self.bd(&args)?;
        Ok(bead::bdq::json_count(bead::bdq::json_only(&text)))
    }

    /// The live fetch `bulk_ready_by_fayth` makes when no `SPIRA_READY_SNAPSHOT` is
    /// cached: `bd <ready_args> --json`, `json_only`-stripped.
    pub fn ready_json(&self, ready_args: &[String]) -> Result<String, String> {
        let mut args: Vec<&str> = ready_args.iter().map(String::as_str).collect();
        args.push("--json");
        let text = self.bd(&args)?;
        Ok(bead::bdq::json_only(&text).to_string())
    }

    /// Every folded event for `ids`, one query per [`EVENT_CHUNK`] ids.
    pub fn events(&self, ids: &[String]) -> Result<Vec<EventRow>, String> {
        let mut out = Vec::new();
        for chunk in ids.chunks(EVENT_CHUNK) {
            let q = events_sql(chunk);
            let text = self.bd(&["sql", "--json", &q])?;
            out.extend(events::parse_rows(&text)?);
        }
        Ok(out)
    }

    /// bd rows by id, `--status all`, chunked.
    pub fn list_by_ids(&self, ids: &[String]) -> Result<Vec<ReadyRow>, String> {
        let mut out = Vec::new();
        for chunk in ids.chunks(LIST_CHUNK) {
            let csv = chunk.join(",");
            let text = self.bd(&["list", "--id", &csv, "--status", "all", "--limit", "0", "--json"])?;
            if text.trim().is_empty() {
                return Err("bd list --id returned nothing".into());
            }
            out.extend(rank::parse_ready(&text)?);
        }
        Ok(out)
    }

    pub fn children(&self, epic: &str) -> Result<Vec<ReadyRow>, String> {
        let text = self.bd(&["children", epic, "--json"])?;
        rank::parse_ready(&text)
    }

    pub fn lifecycle_snapshot(&self) -> Result<String, String> {
        let mut c = Command::new(&self.lc);
        c.arg("list");
        run(c, self.timeout).map_err(|e| format!("spira-lc list: {e}"))
    }
}

pub fn sql_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The events query: only folded types, only needed columns, new_value truncated.
pub fn events_sql(ids: &[String]) -> String {
    let in_list = ids.iter().map(|i| sql_quote(i)).collect::<Vec<_>>().join(",");
    format!(
        "select issue_id, event_type, substring(coalesce(new_value,''),1,120) as new_value, created_at \
         from events where issue_id in ({in_list}) and event_type in \
         ('claimed','status_changed','closed','requeued','reopened','reopen','reclaimed','poison.cleared') \
         order by issue_id, created_at"
    )
}

/// Run a command to completion within `timeout`; stdout on exit 0, else an error naming
/// the first stderr line. A timed-out child is killed.
pub fn run(cmd: Command, timeout: Duration) -> Result<String, String> {
    let r = run_full(cmd, timeout, None)?;
    if r.code != 0 {
        let first = r.stderr.lines().next().unwrap_or("no stderr").to_string();
        return Err(format!("exit {}: {first}", r.code));
    }
    Ok(r.stdout)
}

/// What a finished child said: its exit code (-1 when killed by a signal) and both streams.
#[derive(Debug, Clone, Default)]
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run a command within `timeout`, feeding `input` on stdin (law-payloads-go-on-stdin) or
/// /dev/null when there is none. Err only when it could not start, or timed out (killed).
pub fn run_full(mut cmd: Command, timeout: Duration, input: Option<&[u8]>) -> Result<Ran, String> {
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("cannot start: {e}"))?;
    let t_in = input.map(|b| {
        let mut sin = child.stdin.take().unwrap();
        let b = b.to_vec();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = sin.write_all(&b);
        })
    });
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("wait: {e}")),
        }
    };
    if let Some(t) = t_in {
        let _ = t.join();
    }
    Ok(Ran {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_sql_quotes_and_bounds() {
        let q = events_sql(&["sp-a".into(), "o'hara".into()]);
        assert!(q.contains("'sp-a','o\\'hara'"));
        assert!(q.contains("substring(coalesce(new_value,''),1,120)"));
        // 200 ids stay far below MAX_ARG_STRLEN (128 KiB).
        let ids: Vec<String> = (0..EVENT_CHUNK).map(|i| format!("sp-{i:08}.12")).collect();
        assert!(events_sql(&ids).len() < 16 * 1024);
    }

    #[test]
    fn run_reports_failure_and_timeout() {
        assert!(run(Command::new("true"), Duration::from_secs(5)).is_ok());
        let e = run(Command::new("false"), Duration::from_secs(5)).unwrap_err();
        assert!(e.starts_with("exit 1"), "{e}");
        let mut sl = Command::new("sleep");
        sl.arg("5");
        let e = run(sl, Duration::from_millis(1100)).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(run(Command::new("/nonexistent/binary"), Duration::from_secs(1)).is_err());
    }

    #[test]
    fn run_full_feeds_stdin_and_reports_the_code() {
        let mut c = Command::new("sh");
        c.args(["-c", "cat; exit 3"]);
        let r = run_full(c, Duration::from_secs(5), Some(b"it's \"quoted\" text")).unwrap();
        assert_eq!(r.code, 3);
        assert_eq!(r.stdout, "it's \"quoted\" text");
        let r = run_full(Command::new("cat"), Duration::from_secs(5), None).unwrap();
        assert_eq!((r.code, r.stdout.as_str()), (0, ""));
    }

    #[test]
    fn run_drains_large_stdout() {
        // A child writing more than a pipe buffer must not deadlock.
        let mut c = Command::new("sh");
        c.args(["-c", "head -c 300000 /dev/zero | tr '\\0' x"]);
        assert_eq!(run(c, Duration::from_secs(10)).unwrap().len(), 300000);
    }
}
