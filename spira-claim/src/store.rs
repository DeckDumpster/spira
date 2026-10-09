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

/// Values from spira.toml through spira-config's ONE DOOR (per Ryan 2026-10-05, one source
/// of config): `spira_config::process::cfg`/`cfg_parse`, `$SPIRA_TOML` resolved once per
/// process — never parsed here, never an env override, never a caller-supplied default.
/// Every field here is a registered key (`spira/conf.d/SPIRA_*`); [`load_config`] reads each
/// one exactly once, at construction, and is the ONLY place in this crate that may.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// `spira.bd` (`SPIRA_BD`) — the `bd` binary to exec. Registered but documented as
    /// carrying no default of its own (conf.d: "resolves empty unless set"); an empty
    /// resolved value is passed through as-is — `Store::new` no longer substitutes the
    /// literal `"bd"` for it (see the migration report: this is a behaviour change).
    pub bd: String,
    /// `spira.db` (`SPIRA_DB`) — `None` when the declared value is "" (conf.d: deliberately
    /// `=` not `:=`, "an explicitly empty SPIRA_DB means 'no database for this run'"), same
    /// as this field's own pre-cfg() emptiness check; never a substituted default.
    pub db: Option<String>,
    pub stack_max_depth: u32,
    pub submitted_label: String,
    /// `spira.run` — the runtime directory (`unpoison`: ask history, audit log).
    pub run: String,
    /// `spira.ask_label` — the operator-ask label (`unpoison`: the ask it closes).
    pub ask_label: String,
    /// `spira.scope_label`.
    pub scope_label: String,
    /// `spira.no_loop_label` (conf.d/SPIRA_NO_LOOP_LABEL).
    pub no_loop_label: String,
    /// `spira.queue_wait_label` — one of `fayth_exclude`'s shared exclusions.
    pub queue_wait_label: String,
    /// `spira.open_children_label` — ditto.
    pub open_children_label: String,
    /// `spira.overlap_defer_label` — ditto.
    pub overlap_defer_label: String,
    /// `spira.claim_retries` — `SPIRA_CLAIM_RETRIES`.
    pub claim_retries: u32,
    /// `spira.claim_retry_delay_s` — `SPIRA_CLAIM_RETRY_DELAY_S`.
    pub claim_retry_delay_s: u32,
    /// `spira.fayths` (`SPIRA_FAYTHS`, a list) — the roster override `fayth-exclude`/
    /// `fayth-ready`/`bulk-ready-by-fayth` pass to `spira_config::chamber::spira_fayths`;
    /// "" means no override (per Ryan 2026-10-05: no direct env read — `roster` used to
    /// read `SPIRA_FAYTHS` itself).
    pub fayths: String,
    /// `SPIRA_INCIDENT_LABEL` — an incident never stacks on a fix that has not landed.
    pub incident_label: String,
}

/// Every field of [`Config`], each its own `cfg`/`cfg_parse` call — `Err` names the first
/// key that would not resolve and refuses; the caller (`dispatch`) turns that into
/// `Outcome::cannot_tell` rather than guessing a value for any of the rest.
pub fn load_config() -> Result<Config, String> {
    let db = spira_config::process::cfg("SPIRA_DB")?;
    Ok(Config {
        bd: spira_config::process::cfg("SPIRA_BD")?,
        db: (!db.is_empty()).then_some(db),
        stack_max_depth: spira_config::process::cfg_parse("SPIRA_STACK_MAX_DEPTH")?,
        submitted_label: spira_config::process::cfg("SPIRA_SUBMITTED_LABEL")?,
        run: spira_config::process::cfg("SPIRA_RUN")?,
        ask_label: spira_config::process::cfg("SPIRA_ASK_LABEL")?,
        scope_label: spira_config::process::cfg("SPIRA_SCOPE_LABEL")?,
        no_loop_label: spira_config::process::cfg("SPIRA_NO_LOOP_LABEL")?,
        queue_wait_label: spira_config::process::cfg("SPIRA_QUEUE_WAIT_LABEL")?,
        open_children_label: spira_config::process::cfg("SPIRA_OPEN_CHILDREN_LABEL")?,
        overlap_defer_label: spira_config::process::cfg("SPIRA_OVERLAP_DEFER_LABEL")?,
        claim_retries: spira_config::process::cfg_parse("SPIRA_CLAIM_RETRIES")?,
        claim_retry_delay_s: spira_config::process::cfg_parse("SPIRA_CLAIM_RETRY_DELAY_S")?,
        fayths: spira_config::process::cfg("SPIRA_FAYTHS")?,
        incident_label: spira_config::process::cfg("SPIRA_INCIDENT_LABEL")?,
    })
}

impl Store {
    pub fn new(db_flag: Option<String>, timeout_s: u64, cfg: &Config) -> Store {
        Store {
            bd: cfg.bd.clone(),
            db: db_flag.or_else(|| cfg.db.clone()),
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

    pub fn lc_cmd(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.lc);
        c.args(args);
        c
    }

    /// Run `bd <args>` to completion; `Err` names the verb and bd's own first stderr line
    /// (or the timeout/spawn failure). `pub` so `cmd_claim_retry` (the
    /// generic bd-retry verb, which takes arbitrary caller argv main.rs never parses) can
    /// call it directly, same as every other read here.
    pub fn bd(&self, args: &[&str]) -> Result<String, String> {
        run(self.bd_cmd(args), self.timeout).map_err(|e| format!("bd {}: {e}", args.first().unwrap_or(&"")))
    }

    /// Every folded event for `ids`, one query per [`EVENT_CHUNK`] ids: bd's own rows (its
    /// claim/close/reopen events and the history written before the move) and the facts the
    /// harness has appended to the lifecycle log since.
    pub fn events(&self, ids: &[String]) -> Result<Vec<EventRow>, String> {
        let mut out = Vec::new();
        for chunk in ids.chunks(EVENT_CHUNK) {
            let q = events_sql(chunk);
            let text = self.bd(&["sql", "--json", &q])?;
            out.extend(events::parse_rows(&text)?);
            out.extend(self.facts(chunk)?);
        }
        Ok(out)
    }

    pub fn facts(&self, ids: &[String]) -> Result<Vec<EventRow>, String> {
        let args = ["facts".to_string(), "--ids".into(), ids.join(",")];
        let text = run(self.lc_cmd(&args), self.timeout).map_err(|e| format!("spira-lc facts: {e}"))?;
        events::parse_rows(&text)
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

    /// The machine's return-to-rework, through the one client of `spira-lc reopen`.
    pub fn lc_reopen(&self, id: &str, cause: &str, actor: &str) -> Result<(), String> {
        spira_config::lifecycle_row::reopen_with(&self.lc, id, cause, actor)
    }

    /// `spira-lc release <id> <actor>`: hand back a claim this caller holds. A row not WORKING
    /// has no claim to hand back (the machine refuses it, exit 3); that is not a failure.
    pub fn release_claim(&self, id: &str, actor: &str) -> Result<(), String> {
        self.lc_verb(&["release", id, actor], &[0, 1, 3])
    }

    fn lc_verb(&self, args: &[&str], ok: &[i32]) -> Result<(), String> {
        let mut c = Command::new(&self.lc);
        c.args(args);
        let r = run_full(c, self.timeout, None)?;
        if ok.contains(&r.code) {
            return Ok(());
        }
        let said = r.stderr.lines().chain(r.stdout.lines()).find(|l| !l.trim().is_empty()).unwrap_or("no output");
        Err(format!("spira-lc {} exit {}: {said}", args[0], r.code))
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
