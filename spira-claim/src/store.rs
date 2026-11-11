//! The only I/O: reading the store through bdq and the lifecycle service ([`LcClient`],
//! in-process over its socket), and config through spira-config. Every read is bounded in
//! time and in argv size. DESIGN.md §2 "Data in".

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::events::{self, EventRow};
use lifecycle::bead::BeadState;
use spira_config::lc_call::{LcClient, LcError};

use crate::rank::{self, LifecycleRow, ReadyRow};

/// Ids per `bd sql` events query and per `plan items --ids` call: keeps the argv bounded.
pub const EVENT_CHUNK: usize = 200;
pub const LIST_CHUNK: usize = 100;

#[derive(Clone)]
pub struct Store {
    pub bdq: String,
    /// The lifecycle service: every verb it serves goes here, no process launched.
    pub lc: LcClient,
    /// The `spira-lc` CLI, for the verbs the socket does not serve (`close`, `reopen`) and
    /// the one that takes stdin (`claim-verdicts`).
    pub lc_bin: String,
    pub timeout: Duration,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("bdq", &self.bdq).field("lc_socket", &self.lc.is_socket()).field("lc_bin", &self.lc_bin).field("timeout", &self.timeout).finish()
    }
}

/// Values from spira.toml through spira-config's ONE DOOR (per Ryan 2026-10-05, one source
/// of config): `spira_config::process::cfg`/`cfg_parse`, `$SPIRA_TOML` resolved once per
/// process — never parsed here, never an env override, never a caller-supplied default.
/// Every field here is a registered key (`spira/conf.d/SPIRA_*`); [`load_config`] reads each
/// one exactly once, at construction, and is the ONLY place in this crate that may.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// `spira.db` (`SPIRA_DB`) — `None` when declared "" ("no database for this run"). bdq
    /// reads it itself; this copy only gates writes and keys the ready cache.
    pub db: Option<String>,
    pub stack_max_depth: u32,
    #[allow(dead_code)]
    pub submitted_label: String,
    /// `spira.run` — the runtime directory (`unpoison`: ask history, audit log).
    pub run: String,
    /// `spira.scope_label`.
    pub scope_label: String,
    /// `spira.no_loop_label` (conf.d/SPIRA_NO_LOOP_LABEL).
    pub no_loop_label: String,
    /// `spira.queue_wait_label` — one of `rune_exclude`'s shared exclusions.
    pub queue_wait_label: String,
    /// `spira.open_children_label` — ditto.
    pub open_children_label: String,
    /// `spira.claim_retries` — `SPIRA_CLAIM_RETRIES`.
    pub claim_retries: u32,
    /// `spira.claim_retry_delay_s` — `SPIRA_CLAIM_RETRY_DELAY_S`.
    pub claim_retry_delay_s: u32,
    /// `spira.runes` (`SPIRA_RUNES`, a list) — the roster override `rune-exclude`/
    /// `rune-ready`/`bulk-ready-by-rune` pass to `spira_config::runes::spira_runes`;
    /// "" means no override (per Ryan 2026-10-05: no direct env read — `roster` used to
    /// read `SPIRA_RUNES` itself).
    pub runes: String,
    /// `SPIRA_INCIDENT_LABEL` — an incident never stacks on a fix that has not landed.
    pub incident_label: String,
    /// `SPIRA_QUEUE_DIR` — where each repo's `integrations` record lives; a member it names is not claimable.
    pub queue_dir: String,
    /// `SPIRA_READY_TTL_S` — how long `ready-count` shares one computed ready set; 0 = never.
    pub ready_ttl_s: u32,
    /// `SPIRA_LC_STORE_PATH` is set: the control store is where readiness is published.
    pub control_store: bool,
}

/// Every field of [`Config`], each its own `cfg`/`cfg_parse` call — `Err` names the first
/// key that would not resolve and refuses; the caller (`dispatch`) turns that into
/// `Outcome::cannot_tell` rather than guessing a value for any of the rest.
pub fn load_config() -> Result<Config, String> {
    let db = spira_config::process::cfg("SPIRA_DB")?;
    Ok(Config {
        db: (!db.is_empty()).then_some(db),
        stack_max_depth: spira_config::process::cfg_parse("SPIRA_STACK_MAX_DEPTH")?,
        submitted_label: spira_config::process::cfg("SPIRA_SUBMITTED_LABEL")?,
        run: spira_config::process::cfg("SPIRA_RUN")?,
        scope_label: spira_config::process::cfg("SPIRA_SCOPE_LABEL")?,
        no_loop_label: spira_config::process::cfg("SPIRA_NO_LOOP_LABEL")?,
        queue_wait_label: spira_config::process::cfg("SPIRA_QUEUE_WAIT_LABEL")?,
        open_children_label: spira_config::process::cfg("SPIRA_OPEN_CHILDREN_LABEL")?,
        claim_retries: spira_config::process::cfg_parse("SPIRA_CLAIM_RETRIES")?,
        claim_retry_delay_s: spira_config::process::cfg_parse("SPIRA_CLAIM_RETRY_DELAY_S")?,
        runes: spira_config::process::cfg("SPIRA_RUNES")?,
        incident_label: spira_config::process::cfg("SPIRA_INCIDENT_LABEL")?,
        queue_dir: spira_config::process::cfg("SPIRA_QUEUE_DIR")?,
        ready_ttl_s: spira_config::process::cfg_parse("SPIRA_READY_TTL_S")?,
        control_store: !spira_config::process::cfg("SPIRA_LC_STORE_PATH")?.is_empty(),
    })
}

impl Store {
    pub fn new(timeout_s: u64) -> Result<Store, String> {
        let lc_bin = spira_config::lifecycle_row::lc_bin()?;
        let timeout = Duration::from_secs(timeout_s.max(1));
        Ok(Store { bdq: spira_config::release_env::tool("bdq")?, lc: LcClient::for_bin(&lc_bin)?.within(timeout), lc_bin, timeout })
    }

    /// One lifecycle request; any exit code is an answer. `Err` only when there is none.
    pub fn lc_full<S: AsRef<str>>(&self, args: &[S]) -> Result<Ran, String> {
        match self.lc.call(args) {
            Ok(r) => Ok(Ran { code: r.code, stdout: r.out, stderr: r.err }),
            Err(LcError::Refused(m) | LcError::Fault(m)) => Err(m),
        }
    }

    /// One lifecycle request's stdout on exit 0, else an error naming its first line of reason.
    pub fn lc_run<S: AsRef<str>>(&self, args: &[S]) -> Result<String, String> {
        let r = self.lc_full(args)?;
        if r.code != 0 {
            return Err(format!("exit {}: {}", r.code, first_reason(&r)));
        }
        Ok(r.stdout)
    }

    /// bdq for what the lifecycle service's `content` verb does not serve: `sql` (the events
    /// query), `note --stdin`, `update --set-metadata` and `claim-retry`'s arbitrary ready
    /// queries. Content reads and label writes go through [`Store::lc`].
    pub fn bdq_cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.bdq);
        c.args(args);
        c
    }

    /// Run `bdq <args>` to completion; `Err` names the verb and bd's own first stderr line
    /// (or the timeout/spawn failure).
    pub fn bdq(&self, args: &[&str]) -> Result<String, String> {
        run(self.bdq_cmd(args), self.timeout).map_err(|e| format!("bd {}: {e}", args.first().unwrap_or(&"")))
    }

    /// Every folded event for `ids`, one query per [`EVENT_CHUNK`] ids: bd's own rows (its
    /// claim/close/reopen events and the history written before the move) and the facts the
    /// harness has appended to the lifecycle log since.
    pub fn events(&self, ids: &[String]) -> Result<Vec<EventRow>, String> {
        let mut out = Vec::new();
        for chunk in ids.chunks(EVENT_CHUNK) {
            let q = events_sql(chunk);
            let text = self.bdq(&["sql", "--json", &q])?;
            out.extend(events::parse_rows(&text)?);
            out.extend(self.facts(chunk)?);
        }
        Ok(out)
    }

    pub fn facts(&self, ids: &[String]) -> Result<Vec<EventRow>, String> {
        let args = ["facts".to_string(), "--ids".into(), ids.join(",")];
        let text = self.lc_run(&args).map_err(|e| format!("spira-lc facts: {e}"))?;
        events::parse_rows(&text)
    }

    /// `spira-lc plan <args>`'s `items`, as JSON array text. No `items` is an error, never
    /// zero rows.
    pub fn plan(&self, args: &[&str]) -> Result<String, String> {
        let what = format!("spira-lc plan {}", args.first().unwrap_or(&""));
        let argv: Vec<String> = std::iter::once("plan").chain(args.iter().copied()).map(str::to_string).collect();
        let text = self.lc_run(&argv).map_err(|e| format!("{what}: {e}"))?;
        let v: serde_json::Value = serde_json::from_str(text.trim()).map_err(|e| format!("{what}: not JSON: {e}"))?;
        match v.get("items") {
            Some(items @ serde_json::Value::Array(_)) => Ok(items.to_string()),
            _ => Err(format!("{what}: no items array")),
        }
    }

    /// `spira-lc list-asks`: the open asks, from their control-plane records.
    pub fn open_asks(&self) -> Result<String, String> {
        self.lc_run(&["list-asks"]).map_err(|e| format!("spira-lc list-asks: {e}"))
    }

    /// The plan rows of `ids`, every status, chunked.
    pub fn items_by_ids(&self, ids: &[String]) -> Result<Vec<ReadyRow>, String> {
        let mut out = Vec::new();
        for chunk in ids.chunks(LIST_CHUNK) {
            out.extend(rank::parse_ready(&self.plan(&["items", "--ids", &chunk.join(",")])?)?);
        }
        Ok(out)
    }

    pub fn children(&self, epic: &str) -> Result<Vec<ReadyRow>, String> {
        rank::parse_ready(&self.plan(&["children", "--ids", epic])?)
    }

    /// The lifecycle rows in `states`. Never the whole store: it holds every bead ever.
    pub fn lifecycle_in_states(&self, states: &[&str]) -> Result<HashMap<String, LifecycleRow>, String> {
        self.lc_list(&["--state", &states.join(","), "--snapshot"])
    }

    /// The lifecycle rows of exactly `ids`, one `spira-lc list --ids` per [`EVENT_CHUNK`].
    pub fn lifecycle_of(&self, ids: &[String]) -> Result<HashMap<String, LifecycleRow>, String> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(EVENT_CHUNK) {
            out.extend(self.lc_list(&["--ids", &chunk.join(",")])?);
        }
        Ok(out)
    }

    /// The dispatcher's last judgement of the READY and REWORK beads, in one read.
    pub fn ready_set(&self) -> Result<ReadySet, String> {
        let text = self.lc_run(&["ready-set"]).map_err(|e| format!("spira-lc ready-set: {e}"))?;
        let v: serde_json::Value = serde_json::from_str(text.trim()).map_err(|e| format!("spira-lc ready-set: not JSON: {e}"))?;
        let part = |k: &str| v.get(k).filter(|x| x.is_array()).map(|x| x.to_string()).ok_or_else(|| format!("spira-lc ready-set: no {k} array"));
        let lifecycle = part("lifecycle")?;
        let stamp = |k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
        Ok(ReadySet {
            rows: rank::parse_ready(&part("items")?)?,
            express: rank::express_ids(&lifecycle)?,
            lifecycle: rank::parse_lifecycle(&lifecycle)?,
            planning_watermark: stamp("planning_watermark"),
            judged_at: stamp("judged_at"),
            unjudged: v.get("unjudged").and_then(serde_json::Value::as_u64).ok_or("spira-lc ready-set: no unjudged count")?,
        })
    }

    pub fn express_ids(&self) -> Result<std::collections::BTreeSet<String>, String> {
        Ok(self.lc_list(&["--express", "--live", "--snapshot"])?.into_keys().collect())
    }

    fn lc_list(&self, filter: &[&str]) -> Result<HashMap<String, LifecycleRow>, String> {
        let argv: Vec<&str> = std::iter::once("list").chain(filter.iter().copied()).collect();
        let text = self.lc_run(&argv).map_err(|e| format!("spira-lc list: {e}"))?;
        crate::rank::parse_lifecycle(&text)
    }

    /// The planning plane's watermark, read before the rows it orders.
    pub fn planning_watermark(&self) -> Result<u64, String> {
        let text = self.lc_run(&["plan", "watermark"]).map_err(|e| format!("spira-lc plan watermark: {e}"))?;
        let v: serde_json::Value = serde_json::from_str(text.trim()).map_err(|e| format!("spira-lc plan watermark: not JSON: {e}"))?;
        v["watermark"].as_u64().ok_or_else(|| "spira-lc plan watermark: no watermark".to_string())
    }

    /// The beads that wait on, or descend from, any of `ids`: the sources of the planning
    /// plane's edges into them that block, and its parent-child edges.
    pub fn dependents_of(&self, ids: &[String]) -> Result<std::collections::BTreeSet<String>, String> {
        let mut out = std::collections::BTreeSet::new();
        for chunk in ids.chunks(LIST_CHUNK) {
            let text = self.lc_run(&["plan", "edges", "--ids", &chunk.join(","), "--dependents"]).map_err(|e| format!("spira-lc plan edges: {e}"))?;
            let v: serde_json::Value = serde_json::from_str(text.trim()).map_err(|e| format!("spira-lc plan edges: not JSON: {e}"))?;
            let edges = v.get("edges").and_then(serde_json::Value::as_array).ok_or("spira-lc plan edges: no edges array")?;
            for e in edges {
                let kind = e["type"].as_str().unwrap_or("");
                if kind == "parent-child" || spira_config::edges::blocks(spira_config::edges::Edge::Dep(kind)) {
                    out.extend(e["issue_id"].as_str().map(str::to_string));
                }
            }
        }
        Ok(out)
    }

    /// Hands one readiness pass to the control store.
    pub fn lc_publish_readiness(&self, payload: &str) -> Result<(), String> {
        let r = self.lc.call_stdin(&["publish-readiness"], payload).map_err(|e| format!("spira-lc publish-readiness: {e}"))?;
        match r.code {
            0 => Ok(()),
            c => Err(format!("spira-lc publish-readiness: exit {c}: {}", r.why())),
        }
    }

    /// Raises the alert for a refused readiness publish; the store counts each return as a flap.
    pub fn alert_readiness_refused(&self, why: &str) -> Result<(), String> {
        let message = format!("readiness not published\n\n{why}");
        self.lc_verb(&["condition", "set", READINESS_ALERT, "--type", "Alert", "--reason", "Firing", "--message", &message, "--status", "True", "--owner", "heimdall"], &[0])
    }

    /// Clears that alert once a publish is accepted; nothing firing is a no-op.
    pub fn clear_readiness_refused(&self) -> Result<(), String> {
        self.lc_verb(&["condition", "clear", READINESS_ALERT, "--reason", "Cleared", "--message", "readiness published again", "--owner", "heimdall"], &[0])
    }

    /// Hands one pass's verdicts to the lifecycle store, which keeps them on the bead rows.
    pub fn lc_claim_verdicts(&self, file: &spira_claim::verdicts::VerdictFile) -> Result<(), String> {
        let r = self.lc.call_stdin(&["claim-verdicts"], &file.payload()).map_err(|e| format!("spira-lc claim-verdicts: {e}"))?;
        match r.code {
            0 => Ok(()),
            c => Err(format!("spira-lc claim-verdicts: exit {c}: {}", r.why())),
        }
    }

    /// The machine's return-to-rework, through the one client of `spira-lc reopen`.
    pub fn lc_reopen(&self, id: &str, cause: &str, actor: &str) -> Result<(), String> {
        spira_config::lifecycle_row::reopen_with(&self.lc_bin, id, cause, actor)
    }

    /// `spira-lc release <id> <actor>`: hand back a claim this caller holds. Only a WORKING
    /// row has a claim to hand back; any other state is left unwritten, because the machine
    /// would refuse the Release and record the refusal on the bead's timeline.
    pub fn release_claim(&self, id: &str, actor: &str) -> Result<(), String> {
        let rows = self.lifecycle_of(&[id.to_string()])?;
        if rows.get(id).is_some_and(|r| r.state == BeadState::Working) {
            return self.lc_verb(&["release", id, actor], &[0, 1, 3]);
        }
        Ok(())
    }

    fn lc_verb(&self, args: &[&str], ok: &[i32]) -> Result<(), String> {
        let r = self.lc_full(args)?;
        if ok.contains(&r.code) {
            return Ok(());
        }
        Err(format!("spira-lc {} exit {}: {}", args[0], r.code, first_reason(&r)))
    }
}

/// The first non-empty line a failed answer gave, stderr first (a socket answer has only stdout).
fn first_reason(r: &Ran) -> &str {
    r.stderr.lines().chain(r.stdout.lines()).find(|l| !l.trim().is_empty()).unwrap_or("no output")
}

/// What [`Store::ready_set`] read: the beads the dispatcher judged claimable, the planning
/// watermark and time of the oldest judgement among them, and how many READY or REWORK beads
/// no pass has judged yet.
#[derive(Debug)]
pub struct ReadySet {
    pub rows: Vec<ReadyRow>,
    pub lifecycle: HashMap<String, LifecycleRow>,
    pub express: std::collections::BTreeSet<String>,
    pub planning_watermark: Option<String>,
    pub judged_at: Option<String>,
    pub unjudged: u64,
}

/// The condition key a refused readiness publish raises.
pub const READINESS_ALERT: &str = "claim-readiness-refused";

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
    use spira_config::lc_call::{Reply, Transport};
    use std::sync::{Arc, Mutex};

    /// The lifecycle service as a test double: every request recorded, each answered by `reply`.
    struct Scripted {
        seen: Mutex<Vec<Vec<String>>>,
        reply: fn(&[String]) -> Reply,
    }

    impl Transport for Scripted {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            let mut argv = argv.to_vec();
            argv.extend(stdin.map(|b| format!("stdin={b}")));
            self.seen.lock().unwrap().push(argv.clone());
            Ok((self.reply)(&argv))
        }
    }

    fn store_over(reply: fn(&[String]) -> Reply) -> (Store, Arc<Scripted>) {
        let t = Arc::new(Scripted { seen: Mutex::new(Vec::new()), reply });
        let st = Store { bdq: "/nonexistent/bdq".into(), lc: LcClient::over(t.clone(), true), lc_bin: "/nonexistent/spira-lc".into(), timeout: Duration::from_secs(1) };
        (st, t)
    }

    #[test]
    fn lifecycle_reads_and_writes_go_through_the_service_not_a_program() {
        let (st, t) = store_over(|argv| match argv[0].as_str() {
            "list" => Reply { code: 0, out: r#"[{"bead_id":"sp-a","state":"WORKING","version":1,"holds":[]}]"#.into(), err: String::new() },
            "facts" | "list-asks" => Reply { code: 0, out: "[]".into(), err: String::new() },
            _ => Reply { code: 0, out: String::new(), err: String::new() },
        });
        assert!(st.lifecycle_of(&["sp-a".into()]).unwrap().contains_key("sp-a"));
        assert!(st.facts(&["sp-a".into()]).unwrap().is_empty());
        assert_eq!(st.open_asks().unwrap(), "[]");
        st.release_claim("sp-a", "dwarf-x").unwrap();
        let seen = t.seen.lock().unwrap().clone();
        assert_eq!(seen[0], ["list", "--ids", "sp-a"]);
        assert_eq!(seen[1], ["facts", "--ids", "sp-a"]);
        assert_eq!(seen[2], ["list-asks"]);
        assert_eq!(seen.last().unwrap(), &["release", "sp-a", "dwarf-x"]);
    }

    #[test]
    fn claim_verdicts_rides_the_service_with_its_payload_as_the_body() {
        let (st, t) = store_over(|_| Reply { code: 0, out: String::new(), err: String::new() });
        let file = spira_claim::verdicts::VerdictFile { computed_at: "2026-11-03T00:00:00Z".into(), entries: Vec::new() };
        st.lc_claim_verdicts(&file).unwrap();
        assert_eq!(t.seen.lock().unwrap()[0], ["claim-verdicts".to_string(), format!("stdin={}", file.payload())]);
        let (st, _) = store_over(|_| Reply { code: 2, out: String::new(), err: "cannot tell: down\n".into() });
        assert_eq!(st.lc_claim_verdicts(&file).unwrap_err(), "spira-lc claim-verdicts: exit 2: cannot tell: down");
    }

    #[test]
    fn a_socket_refusal_names_its_reason_from_stdout() {
        let (st, _) = store_over(|_| Reply { code: 3, out: "refused: no such bead\n".into(), err: String::new() });
        let e = st.lc_run(&["list-asks"]).unwrap_err();
        assert_eq!(e, "exit 3: refused: no such bead");
    }

    #[test]
    fn bead_metadata_reads_content_through_the_service() {
        let (st, t) = store_over(|_| Reply { code: 0, out: r#"[{"id":"sp-a","metadata":{"thrash_tip":"abc"}}]"#.into(), err: String::new() });
        assert_eq!(crate::counters::bead_metadata(&st, "sp-a", "thrash_tip"), "abc");
        assert_eq!(t.seen.lock().unwrap()[0], ["content", "show", "sp-a", "--long", "--json"]);
    }

    #[test]
    fn events_sql_quotes_and_bounds() {
        let q = events_sql(&["sp-a".into(), "o'hara".into()]);
        assert!(q.contains("'sp-a','o\\'hara'"));
        assert!(q.contains("substring(coalesce(new_value,''),1,120)"));
        // 200 ids stay far below MAX_ARG_STRLEN (128 KiB).
        let ids: Vec<String> = (0..EVENT_CHUNK).map(|i| format!("sp-{i:08}.12")).collect();
        assert!(events_sql(&ids).len() < 16 * 1024);
    }
}
