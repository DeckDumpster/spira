//! Wall time of every Dolt query and of every served request. One over the threshold appends a
//! line to the slow-query log; watchtower's `--slow-query-check` turns the log into one
//! incident per shape. Line: `<epoch>\t<verb>\t<caller>\t<millis>\t<shape>`; a whole request is
//! the shape `REQUEST`. The caller is the peer's `/proc/<pid>/comm`, never its cmdline.
//! `stats_json` is the per-verb and per-caller counter of what this process has served.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;

thread_local! {
    static VERB: RefCell<String> = const { RefCell::new(String::new()) };
    static CALLER: RefCell<String> = const { RefCell::new(String::new()) };
}

#[derive(Default)]
struct Counters {
    verbs: BTreeMap<String, (u64, u64)>,
    tasks: BTreeMap<Key, Counts>,
    reads: BTreeMap<String, ReadCounts>,
    gate_max: BTreeMap<String, Duration>,
}

/// Reads the store ended at its limit, and reads that ran past it without being ended.
type ReadCounts = (u64, u64);

/// What a key has served: calls, milliseconds, and calls over the answer limit.
type Counts = (u64, u64, u64);

/// (verb, caller, task): what a request is counted under.
type Key = (String, String, String);

static COUNTERS: Mutex<Counters> = Mutex::new(Counters { verbs: BTreeMap::new(), tasks: BTreeMap::new(), reads: BTreeMap::new(), gate_max: BTreeMap::new() });

/// The longest wait at the store's read gate by one request of a verb, per push interval.
pub const GATE_WAIT_MAX_SECONDS: &str = "spira_lc_gate_wait_max_seconds";

/// How often the counts are pushed over OTLP.
pub const METRICS_EVERY: Duration = Duration::from_secs(60);

/// Who the request on this thread is for, as [`caller_of_pid`] named it.
pub fn current_caller() -> String {
    CALLER.with(|c| c.borrow().clone())
}

pub fn set_caller(caller: &str) {
    CALLER.with(|c| *c.borrow_mut() = clean(caller));
}

fn clean(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_whitespace() || c.is_control() { '_' } else { c }).take(32).collect();
    if s.is_empty() { "-".to_string() } else { s }
}

const CLIENT: &str = "spira-lc";
const INTERPRETERS: [&str; 6] = ["bash", "sh", "dash", "python3", "python", "env"];
const WRAPPERS: [&str; 10] = ["timeout", "env", "nice", "ionice", "nohup", "setsid", "stdbuf", "flock", "chrt", "spira-supervise"];
const MAX_ANCESTORS: usize = 8;

/// A process that only launches another: a known wrapper program, or an interpreter run with
/// `-c` (no script of its own to name).
fn is_wrapper(cmdline: &[u8]) -> bool {
    let text = String::from_utf8_lossy(cmdline);
    let mut args = text.split('\0').filter(|a| !a.is_empty());
    let Some(prog) = args.next().map(basename) else { return false };
    WRAPPERS.contains(&prog) || (INTERPRETERS.contains(&prog) && args.any(|a| a == "-c" || (a.starts_with('-') && !a.starts_with("--") && a.contains('c'))))
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// `SPIRA_CALLER` from a NUL-separated environment block.
fn env_caller(environ: &[u8]) -> Option<String> {
    environ.split(|b| *b == 0).find_map(|kv| kv.strip_prefix(b"SPIRA_CALLER=")).map(|v| String::from_utf8_lossy(v).into_owned()).filter(|v| !v.is_empty())
}

/// `<program>/<subcommand>` from a NUL-separated cmdline; an interpreter names its script.
fn cmdline_caller(cmdline: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(cmdline);
    let mut args = text.split('\0').filter(|a| !a.is_empty());
    let mut prog = basename(args.next()?);
    if INTERPRETERS.contains(&prog) {
        match args.by_ref().find(|a| !a.starts_with('-')) {
            Some(script) => prog = basename(script),
            None => return Some(prog.to_string()),
        }
    }
    Some(match args.find(|a| !a.starts_with('-')) {
        Some(sub) => format!("{prog}/{sub}"),
        None => prog.to_string(),
    })
}

fn parent_pid(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("{}/{pid}/stat", system_paths::PROC)).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

/// Who a served request is for. The peer is almost always the `spira-lc` client, which names
/// nobody, so its `SPIRA_CALLER` or else its parent process stands in; any other peer is itself.
pub fn caller_of_pid(pid: i32) -> String {
    let comm = std::fs::read_to_string(format!("{}/{pid}/comm", system_paths::PROC)).map(|c| c.trim().to_string()).unwrap_or_default();
    if comm != CLIENT {
        return clean(&comm);
    }
    let named = std::fs::read(format!("{}/{pid}/environ", system_paths::PROC)).ok().and_then(|e| env_caller(&e));
    let parent = || {
        let mut at = parent_pid(pid)?;
        for _ in 0..MAX_ANCESTORS {
            let cmdline = std::fs::read(format!("{}/{at}/cmdline", system_paths::PROC)).ok()?;
            if !is_wrapper(&cmdline) {
                return cmdline_caller(&cmdline);
            }
            at = parent_pid(at).filter(|p| *p > 1)?;
        }
        None
    };
    clean(&named.or_else(parent).unwrap_or(comm))
}

pub fn reads_done(verb: &str, ended: u64, overran: u64) {
    if ended == 0 && overran == 0 {
        return;
    }
    if let Ok(mut c) = COUNTERS.lock() {
        let r = c.reads.entry(clean(verb)).or_default();
        r.0 += ended;
        r.1 += overran;
    }
}

/// A request's wait at the store's read gate; the longest per verb is kept until pushed.
pub fn gate_waited(verb: &str, waited: Duration) {
    if let Ok(mut c) = COUNTERS.lock() {
        let longest = c.gate_max.entry(clean(verb)).or_default();
        *longest = (*longest).max(waited);
    }
}

fn take_gate_wait_series(at: std::time::SystemTime) -> Vec<spira_otel::metrics::Series> {
    let Ok(mut c) = COUNTERS.lock() else { return Vec::new() };
    std::mem::take(&mut c.gate_max)
        .into_iter()
        .map(|(verb, longest)| spira_otel::metrics::Series { name: GATE_WAIT_MAX_SECONDS.into(), value: longest.as_secs_f64(), labels: vec![("verb".to_string(), verb)], at })
        .collect()
}

/// Pushes the longest gate wait per verb since the last push, and forgets it.
pub fn push_gate_waits(sink: &spira_otel::metrics::MetricsSink) {
    let series = take_gate_wait_series(std::time::SystemTime::now());
    if !series.is_empty() {
        let _ = sink.try_push_series(&series);
    }
}

pub fn request_done(verb: &str, method: &str, caller: &str, task: Option<&str>, took: Duration) {
    let (verb, caller, task) = (clean(verb), clean(caller), clean(task.unwrap_or("")));
    let over = limits().is_some_and(|l| l.is_over(method, took.as_millis() as u64));
    if let Ok(mut c) = COUNTERS.lock() {
        let v = c.verbs.entry(verb.clone()).or_default();
        v.0 += 1;
        v.1 += took.as_millis() as u64;
        let t = c.tasks.entry((verb.clone(), caller.clone(), task)).or_default();
        t.0 += 1;
        t.1 += took.as_millis() as u64;
        t.2 += u64::from(over);
    }
    if !is_batch_method(method) && !limits().is_some_and(|l| l.waits(method)) {
        append(took, &verb, &caller, "REQUEST");
    }
}

const ANSWER_KEY: &str = "SPIRA_LC_ANSWER_MS";
const WAITING_KEY: &str = "SPIRA_LC_WAITING_VERBS";

/// The answer-time alarm's settings, pinned at serve start.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    pub answer_ms: u64,
    pub waiting: Vec<String>,
}

impl Limits {
    fn waits(&self, method: &str) -> bool {
        self.waiting.iter().any(|v| v == method)
    }

    /// Whether a call of `ms` on `method` is over the limit: batch jobs and waiting verbs never are.
    fn is_over(&self, method: &str, ms: u64) -> bool {
        ms > self.answer_ms && !is_batch_method(method) && !self.waits(method)
    }
}

pub fn answer_limits_from(read: impl Fn(&str) -> Result<String, String>) -> Result<Limits, String> {
    let ms = read(ANSWER_KEY).map_err(|e| format!("spira.lc_answer_ms: {e}"))?;
    let answer_ms = spira_config::process::parse_declared::<u32>(ANSWER_KEY, &ms).map_err(|e| format!("spira.lc_answer_ms: {e}"))?;
    let waiting = read(WAITING_KEY).map_err(|e| format!("spira.lc_waiting_verbs: {e}"))?;
    let waiting = waiting.split(',').map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect();
    Ok(Limits { answer_ms: u64::from(answer_ms), waiting })
}

#[cfg(not(test))]
static LIMITS: std::sync::OnceLock<Limits> = std::sync::OnceLock::new();

#[cfg(test)]
thread_local! {
    static TEST_LIMITS: RefCell<Limits> = RefCell::new(Limits { answer_ms: 100, waiting: vec!["watch".to_string()] });
}

pub fn pin_limits(limits: Limits) {
    #[cfg(not(test))]
    let _ = LIMITS.set(limits);
    #[cfg(test)]
    TEST_LIMITS.with(|t| *t.borrow_mut() = limits);
}

fn limits() -> Option<Limits> {
    #[cfg(test)]
    return Some(TEST_LIMITS.with(|t| t.borrow().clone()));
    #[cfg(not(test))]
    LIMITS.get().cloned()
}

const LATENCY_WINDOW: Duration = Duration::from_secs(300);

type Pair = (String, String);

#[derive(Default)]
pub struct Latency {
    samples: BTreeMap<Pair, VecDeque<(std::time::Instant, u64)>>,
    red: BTreeSet<Pair>,
}

static LATENCY: Mutex<Latency> = Mutex::new(Latency { samples: BTreeMap::new(), red: BTreeSet::new() });

/// The method a request is measured under: the verb, and for `work` the lane verb after the
/// bead id, so a slow brokered tool does not hide behind `work show`; for `ops-view` the view.
pub fn method_of(argv: &[String]) -> String {
    match argv.first().map(String::as_str) {
        Some("work") => format!("work:{}", argv.get(2).map(String::as_str).unwrap_or("-")),
        Some("ops-view") => match argv.get(1).and_then(|a| crate::ops::VIEWS.iter().find(|v| **v == a.as_str())) {
            Some(view) => format!("ops-view:{view}"),
            None => "ops-view".to_string(),
        },
        Some(v) => v.to_string(),
        None => "-".to_string(),
    }
}

/// Brokered batch jobs run a harness tool; they are bounded by their own deadline, not the
/// store-call ceiling.
fn is_batch_method(method: &str) -> bool {
    method.strip_prefix("work:").is_some_and(|v| crate::work::brokers_tool(v) && !crate::work::exec_verb(v))
}

pub fn sample(method: &str, caller: &str, took: Duration) {
    let Some(limits) = limits() else { return };
    if let Ok(mut l) = LATENCY.lock() {
        l.record(&limits, method, caller, std::time::Instant::now(), took.as_millis() as u64);
    }
}

/// One line per (method, caller) that went over the answer limit or came back under it.
pub fn latency_transitions(now: std::time::Instant) -> Vec<String> {
    let Some(limits) = limits() else { return Vec::new() };
    LATENCY.lock().map(|mut l| l.transitions(&limits, now, crate::db::now_epoch())).unwrap_or_default()
}

impl Latency {
    fn record(&mut self, limits: &Limits, method: &str, caller: &str, now: std::time::Instant, ms: u64) {
        if !is_batch_method(method) && !limits.waits(method) {
            self.samples.entry((clean(method), clean(caller))).or_default().push_back((now, ms));
        }
    }

    fn transitions(&mut self, limits: &Limits, now: std::time::Instant, epoch: i64) -> Vec<String> {
        let mut out = Vec::new();
        let mut keys: BTreeSet<Pair> = self.samples.keys().cloned().collect();
        keys.extend(self.red.iter().cloned());
        for key in keys {
            let window = self.samples.entry(key.clone()).or_default();
            while window.front().is_some_and(|(t, _)| now.saturating_duration_since(*t) > LATENCY_WINDOW) {
                window.pop_front();
            }
            let mut ms: Vec<u64> = window.iter().map(|(_, m)| *m).collect();
            ms.sort_unstable();
            let over = ms.iter().filter(|m| **m > limits.answer_ms).count();
            let (method, caller) = (&key.0, &key.1);
            let limit = limits.answer_ms;
            if over > 0 && self.red.insert(key.clone()) {
                let at = |pct: usize| ms[(ms.len() * pct).div_ceil(100) - 1];
                out.push(format!(
                    "{epoch}\tLC-LATENCY\tRED\t{method}\t{caller}\tover={over}/{}\tp50={}ms\tp95={}ms\tmax={}ms\tlimit={limit}ms",
                    ms.len(),
                    at(50),
                    at(95),
                    ms[ms.len() - 1]
                ));
            } else if over == 0 && self.red.remove(&key) {
                out.push(format!("{epoch}\tLC-LATENCY\tCLEAR\t{method}\t{caller}\tlimit={limit}ms"));
            }
            if ms.is_empty() {
                self.samples.remove(&key);
            }
        }
        out
    }
}

pub fn stats_json() -> String {
    let Ok(c) = COUNTERS.lock() else { return "{}".to_string() };
    let verbs: serde_json::Map<String, serde_json::Value> =
        c.verbs.iter().map(|(v, (n, ms))| (v.clone(), serde_json::json!({"count": n, "total_ms": ms}))).collect();
    let callers: Vec<serde_json::Value> =
        c.tasks.iter().map(|((v, who, task), (n, ms, _))| serde_json::json!({"verb": v, "caller": who, "task": task, "count": n, "total_ms": ms})).collect();
    serde_json::json!({"verbs": verbs, "callers": callers}).to_string()
}

/// The counts that moved since `pushed`, as one request series and one store-time series per
/// moved key, both holding the delta.
fn series_since(c: &Counters, pushed: &BTreeMap<Key, Counts>, at: std::time::SystemTime) -> Vec<spira_otel::metrics::Series> {
    let mut out = Vec::new();
    for (key, &(n, ms, over)) in &c.tasks {
        let (pn, pms, pover) = pushed.get(key).copied().unwrap_or_default();
        if n == pn {
            continue;
        }
        let labels = vec![("verb".to_string(), key.0.clone()), ("caller".to_string(), key.1.clone()), ("task".to_string(), key.2.clone())];
        let series = |name: &str, value: f64| spira_otel::metrics::Series { name: name.into(), value, labels: labels.clone(), at };
        out.push(series("spira_lc_requests", (n - pn) as f64));
        out.push(series("spira_lc_request_seconds", (ms - pms) as f64 / 1000.0));
        if over > pover {
            out.push(series("spira_lc_requests_over", (over - pover) as f64));
        }
    }
    out
}

fn read_series_since(c: &Counters, pushed: &BTreeMap<String, ReadCounts>, at: std::time::SystemTime) -> Vec<spira_otel::metrics::Series> {
    let mut out = Vec::new();
    for (verb, &(ended, overran)) in &c.reads {
        let (pe, po) = pushed.get(verb).copied().unwrap_or_default();
        let series = |name: &str, value: u64| spira_otel::metrics::Series { name: name.into(), value: value as f64, labels: vec![("verb".to_string(), verb.clone())], at };
        if ended > pe {
            out.push(series("spira_lc_reads_ended", ended - pe));
        }
        if overran > po {
            out.push(series("spira_lc_reads_overran", overran - po));
        }
    }
    out
}

/// Pushes the read-limit counts by verb; `pushed` advances only when the push succeeds.
pub fn push_reads(sink: &spira_otel::metrics::MetricsSink, pushed: &mut BTreeMap<String, ReadCounts>) {
    let Ok(c) = COUNTERS.lock() else { return };
    let (series, now) = (read_series_since(&c, pushed, std::time::SystemTime::now()), c.reads.clone());
    drop(c);
    if !series.is_empty() && sink.try_push_series(&series).is_ok() {
        *pushed = now;
    }
}

/// Pushes what moved since `pushed`; `pushed` advances only when the push succeeds. The
/// counters are not locked during the post.
pub fn push_counts(sink: &spira_otel::metrics::MetricsSink, pushed: &mut BTreeMap<Key, Counts>) {
    let Ok(c) = COUNTERS.lock() else { return };
    let (series, now) = (series_since(&c, pushed, std::time::SystemTime::now()), c.tasks.clone());
    drop(c);
    if !series.is_empty() && sink.try_push_series(&series).is_ok() {
        *pushed = now;
    }
}

type HeldKey = (String, Vec<(String, String)>);

static HELD: Mutex<BTreeMap<HeldKey, spira_otel::metrics::Series>> = Mutex::new(BTreeMap::new());

/// Holds gauges for the metrics thread: a new value for a (name, labels) replaces the old.
pub fn hold_gauges(series: Vec<spira_otel::metrics::Series>) {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    for s in series {
        held.insert((s.name.clone(), s.labels.clone()), s);
    }
}

/// Replaces every held gauge named in `names` with `series`, so a label that has gone away stops
/// being pushed.
pub fn replace_gauges(names: &[&str], series: Vec<spira_otel::metrics::Series>) {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).retain(|(name, _), _| !names.contains(&name.as_str()));
    hold_gauges(series);
}

#[cfg(test)]
pub fn held() -> Vec<spira_otel::metrics::Series> {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
}

/// Pushes every held gauge in one request.
pub fn push_held(sink: &spira_otel::metrics::MetricsSink) {
    let series: Vec<_> = HELD.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
    if !series.is_empty() {
        let _ = sink.try_push_series(&series);
    }
}

/// Resident bytes from a `/proc/<pid>/status` text: its `VmRSS` line, which the kernel reports
/// in kB, so no page size is assumed.
fn rss_bytes_of(status: &str) -> Option<u64> {
    let kb = status.lines().find_map(|l| l.strip_prefix("VmRSS:"))?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()?;
    Some(kb * 1024)
}

/// Anonymous resident bytes from a `/proc/<pid>/status` text: its `RssAnon` line (kB).
fn rss_anon_bytes_of(status: &str) -> Option<u64> {
    let kb = status.lines().find_map(|l| l.strip_prefix("RssAnon:"))?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()?;
    Some(kb * 1024)
}

/// One log line: this process's RssAnon at `at`, or nothing where `/proc` does not say.
pub fn rss_anon_line(at: &str) -> Option<String> {
    let bytes = std::fs::read_to_string(format!("{}/status", system_paths::PROC_SELF)).ok().as_deref().and_then(rss_anon_bytes_of)?;
    Some(format!("spira-lc serve: rss-anon {at} {} MB", bytes / (1024 * 1024)))
}

static VERBS_SEEN: Mutex<std::collections::BTreeSet<String>> = Mutex::new(std::collections::BTreeSet::new());

/// True the first time this process is asked about `verb`.
pub fn first_of_verb(verb: &str) -> bool {
    VERBS_SEEN.lock().map(|mut seen| seen.insert(verb.to_string())).unwrap_or(false)
}

/// Pushes this process's resident memory as the gauge `spira_lc_rss_bytes`.
pub fn push_rss(sink: &spira_otel::metrics::MetricsSink) {
    let Some(bytes) = std::fs::read_to_string(format!("{}/status", system_paths::PROC_SELF)).ok().as_deref().and_then(rss_bytes_of) else { return };
    sink.push(&[spira_otel::metrics::Point::Gauge { name: "spira_lc_rss_bytes", value: bytes as f64 }], &[]);
}

/// Pushes the start-up total, once, as the gauge `spira_lc_ready_ms`.
pub fn push_ready(sink: &spira_otel::metrics::MetricsSink, ready_ms: u64) {
    sink.push(&[spira_otel::metrics::Point::Gauge { name: "spira_lc_ready_ms", value: ready_ms as f64 }], &[]);
}

pub fn set_verb(verb: &str) {
    VERB.with(|v| *v.borrow_mut() = verb.to_string());
}

/// The slow-query threshold from the declared `SPIRA_SLOW_QUERY_MS`; a missing or unparsable key is an `Err`, never a default.
fn threshold_from(declared: Result<String, String>) -> Result<Duration, String> {
    let v = declared?;
    v.trim().parse::<u64>().map(Duration::from_millis).map_err(|_| format!("SPIRA_SLOW_QUERY_MS is not a whole number of milliseconds: {v:?}"))
}

#[cfg(test)]
thread_local! {
    static THRESHOLD_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(1000) };
}

fn threshold() -> Result<Duration, String> {
    #[cfg(test)]
    return Ok(Duration::from_millis(THRESHOLD_MS.with(|t| t.get())));
    #[cfg(not(test))]
    threshold_from(spira_config::process::cfg("SPIRA_SLOW_QUERY_MS"))
}

fn log_path() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var("SPIRA_SLOW_QUERY_LOG").ok().filter(|p| !p.is_empty()) {
        return Some(p.into());
    }
    // SPIRA_RUN is declared config (spira/conf.d): the one source is $SPIRA_TOML (per Ryan
    // 2026-10-05), never this process's own environment.
    spira_config::process::cfg("SPIRA_RUN").ok().filter(|r| !r.is_empty()).map(|r| std::path::Path::new(&r).join("slow-queries.log"))
}

/// SQL with every literal replaced by `?`, whitespace collapsed, and value lists folded, so
/// the same query with different values is one shape.
pub fn shape(sql: &str) -> String {
    let mut out = String::new();
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                while let Some(d) = chars.next() {
                    if d == '\\' {
                        chars.next();
                    } else if d == c {
                        break;
                    }
                }
                out.push('?');
            }
            d if d.is_ascii_digit() && !out.ends_with(|p: char| p.is_alphanumeric() || p == '_') => {
                while chars.peek().is_some_and(|n| n.is_ascii_digit() || *n == '.') {
                    chars.next();
                }
                out.push('?');
            }
            d if d.is_whitespace() => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            d => out.push(d),
        }
    }
    let mut folded = out.trim().to_string();
    while folded.contains("?, ?") || folded.contains("?,?") {
        folded = folded.replace("?, ?", "?").replace("?,?", "?");
    }
    folded.chars().take(300).collect()
}

pub fn line(epoch: i64, verb: &str, caller: &str, took: Duration, sql: &str) -> String {
    format!("{epoch}\t{verb}\t{caller}\t{}\t{}\n", took.as_millis(), shape(sql))
}

pub fn record(took: Duration, sql: &str) {
    let verb = VERB.with(|v| v.borrow().clone());
    let verb = if verb.is_empty() { "-".to_string() } else { verb };
    let caller = CALLER.with(|c| c.borrow().clone());
    let caller = if caller.is_empty() { "-".to_string() } else { caller };
    append(took, &verb, &caller, sql);
}

fn append(took: Duration, verb: &str, caller: &str, sql: &str) {
    let limit = match threshold() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("spira-lc: slow-query log off, refusing: {e}");
            return;
        }
    };
    if took < limit {
        return;
    }
    let Some(path) = log_path() else { return };
    let l = line(crate::db::now_epoch(), verb, caller, took, sql);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(l.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(waiting: &[&str]) -> Limits {
        Limits { answer_ms: 100, waiting: waiting.iter().map(|w| w.to_string()).collect() }
    }

    fn feed(l: &mut Latency, lim: &Limits, method: &str, caller: &str, at: std::time::Instant, ms: &[u64]) {
        for m in ms {
            l.record(lim, method, caller, at, *m);
        }
    }

    #[test]
    fn rss_anon_is_read_from_its_own_status_line_not_vmrss() {
        let status = "VmRSS:\t  900 kB\nRssAnon:\t  2048 kB\nRssFile:\t 10 kB\n";
        assert_eq!(rss_anon_bytes_of(status), Some(2048 * 1024));
        assert_eq!(rss_anon_bytes_of("VmRSS: 9 kB\n"), None);
    }

    #[test]
    fn a_verb_is_first_once() {
        assert!(first_of_verb("first-of-verb-test"));
        assert!(!first_of_verb("first-of-verb-test"));
    }

    #[test]
    fn one_call_over_the_limit_raises_one_red_for_its_own_pair() {
        let (lim, t0, mut l) = (limits(&["watch"]), std::time::Instant::now(), Latency::default());
        let mut sift = vec![30u64; 20];
        sift.push(150);
        feed(&mut l, &lim, "claimable", "sift", t0, &sift);
        feed(&mut l, &lim, "claimable", "queue", t0, &[30; 20]);
        assert_eq!(l.transitions(&lim, t0, 7), ["7\tLC-LATENCY\tRED\tclaimable\tsift\tover=1/21\tp50=30ms\tp95=30ms\tmax=150ms\tlimit=100ms"]);
        assert!(l.transitions(&lim, t0, 8).is_empty(), "a pair stays red without repeating");
    }

    #[test]
    fn a_red_pair_clears_once_the_window_holds_no_call_over_the_limit() {
        let (lim, t0, mut l) = (limits(&["watch"]), std::time::Instant::now(), Latency::default());
        feed(&mut l, &lim, "claimable", "sift", t0, &[30, 150]);
        assert_eq!(l.transitions(&lim, t0, 1).len(), 1);
        let later = t0 + LATENCY_WINDOW + Duration::from_secs(1);
        feed(&mut l, &lim, "claimable", "sift", later, &[30; 3]);
        assert_eq!(l.transitions(&lim, later, 9), ["9\tLC-LATENCY\tCLEAR\tclaimable\tsift\tlimit=100ms"]);
        assert!(l.transitions(&lim, later, 10).is_empty());
    }

    #[test]
    fn a_red_pair_that_goes_silent_still_clears() {
        let (lim, t0, mut l) = (limits(&[]), std::time::Instant::now(), Latency::default());
        feed(&mut l, &lim, "list", "lc-view", t0, &[500]);
        assert_eq!(l.transitions(&lim, t0, 1).len(), 1);
        let later = t0 + LATENCY_WINDOW + Duration::from_secs(1);
        assert_eq!(l.transitions(&lim, later, 2).len(), 1);
        assert!(l.transitions(&lim, later, 3).is_empty());
    }

    #[test]
    fn the_waiting_verbs_exempt_come_from_the_configuration() {
        let (lim, t0, mut l) = (limits(&["watch", "foo-poll"]), std::time::Instant::now(), Latency::default());
        feed(&mut l, &lim, "foo-poll", "x", t0, &[3000; 20]);
        feed(&mut l, &lim, "watch", "x", t0, &[3000; 20]);
        assert!(l.transitions(&lim, t0, 1).is_empty());
        let lim = limits(&["watch"]);
        feed(&mut l, &lim, "foo-poll", "x", t0, &[3000; 20]);
        assert_eq!(l.transitions(&lim, t0, 2).len(), 1, "a verb the list does not name is held to the limit");
    }

    #[test]
    fn brokered_batch_methods_are_not_alarmed() {
        let (lim, t0, mut l) = (limits(&[]), std::time::Instant::now(), Latency::default());
        feed(&mut l, &lim, "work:read", "x", t0, &[4000; 20]);
        feed(&mut l, &lim, "work:search", "x", t0, &[40000; 20]);
        assert!(l.transitions(&lim, t0, 1).is_empty());
        feed(&mut l, &lim, "work:submit", "x", t0, &[4000]);
        assert_eq!(l.transitions(&lim, t0, 2).len(), 1, "a verb recorded for the scheduler is a store write, held to the limit");
    }

    #[test]
    fn calls_over_the_limit_are_counted_by_verb_and_caller() {
        request_done("over-probe", "over-probe", "over-probe-caller", None, Duration::from_millis(150));
        request_done("over-probe", "over-probe", "over-probe-caller", None, Duration::from_millis(20));
        request_done("over-probe-wait", "watch", "over-probe-caller", None, Duration::from_millis(5000));
        let c = COUNTERS.lock().unwrap();
        let got = series_since(&c, &BTreeMap::new(), std::time::SystemTime::now());
        let over = |verb: &str| got.iter().find(|s| s.name == "spira_lc_requests_over" && s.labels.contains(&("verb".into(), verb.into())) && s.labels.contains(&("caller".into(), "over-probe-caller".into()))).map(|s| s.value);
        assert_eq!(over("over-probe"), Some(1.0));
        assert_eq!(over("over-probe-wait"), None, "a waiting verb is never over");
    }

    #[test]
    fn answer_limits_are_refused_by_key_when_missing_or_unparsable() {
        let ok = |k: &str| match k {
            ANSWER_KEY => Ok("250".to_string()),
            _ => Ok("watch, foo-poll".to_string()),
        };
        assert_eq!(answer_limits_from(ok).unwrap(), Limits { answer_ms: 250, waiting: vec!["watch".into(), "foo-poll".into()] });
        let e = answer_limits_from(|k| Err(format!("{k} is not a registered config key"))).unwrap_err();
        assert!(e.contains("SPIRA_LC_ANSWER_MS"), "{e}");
        let e = answer_limits_from(|k| if k == ANSWER_KEY { Ok("fast".into()) } else { Ok(String::new()) }).unwrap_err();
        assert!(e.contains("SPIRA_LC_ANSWER_MS"), "{e}");
        let e = answer_limits_from(|k| if k == ANSWER_KEY { Ok("100".into()) } else { Err("unset".into()) }).unwrap_err();
        assert!(e.contains("SPIRA_LC_WAITING_VERBS") || e.contains("lc_waiting_verbs"), "{e}");
        assert!(answer_limits_from(|k| Ok(if k == ANSWER_KEY { "100".into() } else { String::new() })).unwrap().waiting.is_empty(), "an empty list exempts nothing");
    }

    #[test]
    fn a_work_request_is_measured_by_its_lane_verb() {
        let a = |v: &[&str]| method_of(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["work", "sp-x", "note", "hi"]), "work:note");
        assert_eq!(a(&["work", "sp-x", "groom", "sweep"]), "work:groom");
        assert_eq!(a(&["ops-view", "ops_live"]), "ops-view:ops_live");
        assert_eq!(a(&["ops-view", "ops_dwell"]), "ops-view:ops_dwell");
        assert_eq!(a(&["ops-view", "not-a-view"]), "ops-view", "an unknown argument is no new label");
    }

    #[test]
    fn spira_caller_is_read_from_the_environment_block() {
        assert_eq!(env_caller(b"A=1\0SPIRA_CALLER=batcher\0B=2\0").as_deref(), Some("batcher"));
        assert_eq!(env_caller(b"A=1\0SPIRA_CALLER=\0"), None);
        assert_eq!(env_caller(b"XSPIRA_CALLER=y\0"), None);
    }

    #[test]
    fn a_parent_is_named_by_program_and_subcommand() {
        assert_eq!(cmdline_caller(b"cockpit-collect\0core\0--x\0").as_deref(), Some("cockpit-collect/core"));
        assert_eq!(cmdline_caller(b"usr/bin/batcher\0sifts\0").as_deref(), Some("batcher/sifts"));
        assert_eq!(cmdline_caller(b"bin/bash\0-e\0/x/bead-sweep.sh\0watch\0").as_deref(), Some("bead-sweep.sh/watch"));
        assert_eq!(cmdline_caller(b"queue\0").as_deref(), Some("queue"));
        assert_eq!(cmdline_caller(b""), None);
    }

    #[test]
    fn wrappers_are_recognised_and_programs_are_not() {
        for w in [&b"timeout\x0030\0spira-lc\0show\0"[..], b"usr/bin/env\0X=1\0y\0", b"nice\0-n\x0010\0z\0", b"bash\0-c\0. lib; f\0", b"spira-supervise\0x\0"] {
            assert!(is_wrapper(w), "{:?}", String::from_utf8_lossy(w));
        }
        for p in [&b"gate-worker\0run\0"[..], b"bash\0-e\0/x/bead-sweep.sh\0watch\0", b"batcher\0sifts\0", b""] {
            assert!(!is_wrapper(p), "{:?}", String::from_utf8_lossy(p));
        }
    }

    #[test]
    fn shape_drops_values_and_folds_lists() {
        let a = shape("SELECT * FROM bead WHERE id = 'sp-1' AND version = 12 AND s IN ('a','b', 'c')");
        let b = shape("SELECT  *\nFROM bead WHERE id = 'x\\'y' AND version = 7 AND s IN ('z')");
        assert_eq!(a, b);
        assert!(!a.contains("sp-1") && !a.contains("12"));
        assert!(a.contains("bead"));
    }

    #[test]
    fn a_threshold_the_config_does_not_declare_is_refused_by_name() {
        let missing = "SPIRA_SLOW_QUERY_MS is not a registered config key (spira/conf.d) — refusing".to_string();
        let e = threshold_from(Err(missing)).unwrap_err();
        assert!(e.contains("SPIRA_SLOW_QUERY_MS"), "{e}");
        assert_eq!(threshold_from(Ok("250".into())).unwrap(), Duration::from_millis(250));
        assert!(threshold_from(Ok("fast".into())).unwrap_err().contains("SPIRA_SLOW_QUERY_MS"));
    }

    #[test]
    fn digits_inside_identifiers_survive() {
        assert!(shape("SELECT a1 FROM t2").contains("a1 FROM t2"));
    }

    #[test]
    fn a_fast_query_writes_nothing_and_a_slow_one_one_line() {
        let d = testkit::TempDir::new("slow-rec");
        let log = d.join("s.log");
        let _env = testkit::env(&[("SPIRA_SLOW_QUERY_LOG", Some(log.to_str().unwrap()))]);
        THRESHOLD_MS.with(|t| t.set(1000));
        set_verb("list");
        set_caller("spira-lc");
        record(Duration::from_millis(500), "SELECT 1");
        assert!(!log.exists());
        record(Duration::from_millis(1500), "SELECT * FROM bead WHERE id = 'a'");
        let got = std::fs::read_to_string(&log).unwrap();
        assert_eq!(got.lines().count(), 1);
        let f: Vec<&str> = got.trim_end().split('\t').collect();
        assert_eq!((f[1], f[2], f[3], f[4]), ("list", "spira-lc", "1500", "SELECT * FROM bead WHERE id = ?"));
    }

    #[test]
    fn a_request_is_logged_whole_and_counted_per_verb_and_caller() {
        let d = testkit::TempDir::new("slow-req");
        let log = d.join("s.log");
        let _env = testkit::env(&[("SPIRA_SLOW_QUERY_LOG", Some(log.to_str().unwrap()))]);
        THRESHOLD_MS.with(|t| t.set(0));
        request_done("probe-verb", "probe-verb", "probe\tcaller", None, Duration::from_millis(40));
        request_done("other-verb", "other-verb", "other", None, Duration::from_millis(7));
        request_done("probe-verb", "probe-verb", "probe\tcaller", None, Duration::from_millis(60));
        request_done("work", "work:read", "probe\tcaller", None, Duration::from_millis(9000));
        let got: String = std::fs::read_to_string(&log).unwrap().lines().filter(|l| l.contains("\tprobe-verb\t")).map(|l| format!("{l}\n")).collect();
        assert_eq!(got.lines().count(), 2, "{got}");
        let all = std::fs::read_to_string(&log).unwrap();
        assert!(!all.contains("\twork\t"), "a brokered batch tool's runtime is not store latency: {all}");
        assert!(got.lines().all(|l| l.split('\t').collect::<Vec<_>>()[1..] == ["probe-verb", "probe_caller", l.split('\t').nth(3).unwrap(), "REQUEST"][..]), "{got}");
        let s: serde_json::Value = serde_json::from_str(&stats_json()).unwrap();
        assert_eq!(s["verbs"]["probe-verb"], serde_json::json!({"count": 2, "total_ms": 100}));
        assert!(s["callers"].as_array().unwrap().iter().any(|c| c["verb"] == "probe-verb" && c["caller"] == "probe_caller" && c["task"] == "-" && c["count"] == 2 && c["total_ms"] == 100));
    }

    #[test]
    fn series_carry_verb_caller_and_task_and_only_the_delta() {
        let mut c = Counters::default();
        let key = ("v".to_string(), "who".to_string(), "asks".to_string());
        let idle = ("w".to_string(), "who".to_string(), "-".to_string());
        c.tasks.insert(key.clone(), (3, 1500, 0));
        c.tasks.insert(idle.clone(), (2, 40, 0));
        let sink = spira_otel::metrics::MetricsSink::new(None, "spira-lc", Duration::from_millis(1));
        let at = std::time::SystemTime::now();
        let mut pushed = BTreeMap::new();
        pushed.insert(idle.clone(), (2, 40, 0));
        let got = series_since(&c, &pushed, at);
        let flat: Vec<(String, f64)> = got.iter().map(|s| (s.name.clone(), s.value)).collect();
        assert_eq!(flat, [("spira_lc_requests".to_string(), 3.0), ("spira_lc_request_seconds".to_string(), 1.5)], "the unmoved key yields nothing");
        let encoded = format!("{:?}", sink.encode_series(&got));
        for l in ["verb", "caller", "task", "asks", "who", "spira_lc_requests", "spira_lc_request_seconds"] {
            assert!(encoded.contains(l), "{l} missing from {encoded}");
        }
        pushed.insert(key.clone(), (3, 1500, 0));
        c.tasks.insert(key, (4, 1700, 0));
        let got = series_since(&c, &pushed, at);
        let flat: Vec<f64> = got.iter().map(|s| s.value).collect();
        assert_eq!(flat, [1.0, 0.2]);
    }

    #[test]
    fn reads_ended_and_overran_are_pushed_by_verb_summed_over_requests() {
        let mut c = Counters::default();
        for (ended, overran) in [(1, 0), (2, 0)] {
            let r = c.reads.entry("list".to_string()).or_default();
            r.0 += ended;
            r.1 += overran;
        }
        c.reads.entry("show".to_string()).or_default().1 += 4;
        let got = read_series_since(&c, &BTreeMap::new(), std::time::SystemTime::now());
        let total = |name: &str, verb: &str| got.iter().filter(|s| s.name == name && s.labels == [("verb".to_string(), verb.to_string())]).map(|s| s.value).sum::<f64>();
        assert_eq!(total("spira_lc_reads_ended", "list"), 3.0);
        assert_eq!(total("spira_lc_reads_overran", "show"), 4.0);
        assert_eq!(total("spira_lc_reads_overran", "list"), 0.0);
        let pushed = BTreeMap::from([("list".to_string(), (3, 0))]);
        assert!(read_series_since(&c, &pushed, std::time::SystemTime::now()).iter().all(|s| s.labels[0].1 != "list"), "only the delta");
    }

    #[test]
    fn the_longest_gate_wait_of_a_verb_is_pushed_once_per_interval() {
        gate_waited("list", Duration::from_millis(20));
        gate_waited("list", Duration::from_millis(70));
        gate_waited("list", Duration::from_millis(30));
        let got = take_gate_wait_series(std::time::SystemTime::now());
        let list: Vec<_> = got.iter().filter(|s| s.name == GATE_WAIT_MAX_SECONDS && s.labels == [("verb".to_string(), "list".to_string())]).collect();
        assert_eq!(list.len(), 1);
        assert!((list[0].value - 0.07).abs() < 1e-9, "{}", list[0].value);
        assert!(take_gate_wait_series(std::time::SystemTime::now()).iter().all(|s| s.labels[0].1 != "list"), "taken once");
    }

    #[test]
    fn reads_done_accumulates_into_the_counters() {
        reads_done("reads-probe", 1, 0);
        reads_done("reads-probe", 2, 5);
        assert_eq!(COUNTERS.lock().unwrap().reads["reads-probe"], (3, 5));
    }

    #[test]
    fn resident_memory_is_read_from_the_vmrss_line_in_kb() {
        assert_eq!(rss_bytes_of("Name:\tspira-lc\nVmRSS:\t  2048 kB\nThreads:\t4\n"), Some(2048 * 1024));
        assert_eq!(rss_bytes_of("Name:\tx\n"), None);
        let own = std::fs::read_to_string(format!("{}/status", system_paths::PROC_SELF)).unwrap();
        assert!(rss_bytes_of(&own).is_some_and(|b| b > 0), "this process's own status names its resident size");
    }

    #[test]
    fn a_failed_push_keeps_the_delta_for_the_next_period() {
        request_done("push-probe", "push-probe", "who", Some("t"), Duration::from_millis(3));
        let unreachable = spira_otel::metrics::MetricsSink::new(None, "spira-lc", Duration::from_millis(1));
        let mut pushed = BTreeMap::new();
        push_counts(&unreachable, &mut pushed);
        assert!(!pushed.keys().any(|k| k.0 == "push-probe"), "a failed push advances nothing");
        let c = COUNTERS.lock().unwrap();
        assert!(series_since(&c, &pushed, std::time::SystemTime::now()).iter().any(|s| s.labels.contains(&("verb".into(), "push-probe".into()))));
    }

    #[test]
    fn a_caller_name_is_the_comm_of_the_pid_and_unknown_pids_are_dashes() {
        let comm = std::fs::read_to_string(format!("{}/comm", system_paths::PROC_SELF)).unwrap();
        assert_eq!(caller_of_pid(std::process::id() as i32), clean(comm.trim()));
        assert_eq!(caller_of_pid(-1), "-");
    }
}
