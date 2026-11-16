//! Wall time of every Dolt query and of every served request. One over the threshold appends a
//! line to the slow-query log; watchtower's `--slow-query-check` turns the log into one
//! incident per shape. Line: `<epoch>\t<verb>\t<caller>\t<millis>\t<shape>`; a whole request is
//! the shape `REQUEST`. The caller is the peer's `/proc/<pid>/comm`, never its cmdline.
//! `stats_json` is the per-verb and per-caller counter of what this process has served.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
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
    tasks: BTreeMap<Key, (u64, u64)>,
    past: BTreeMap<Key, Past>,
}

/// What a key's reads behind the head came to since the last successful push.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Past {
    too_old: u64,
    max_revs: Option<u64>,
    max_ms: Option<u64>,
}

/// (verb, caller, task): what a request is counted under.
type Key = (String, String, String);

static COUNTERS: Mutex<Counters> = Mutex::new(Counters { verbs: BTreeMap::new(), tasks: BTreeMap::new(), past: BTreeMap::new() });

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

pub fn request_done(verb: &str, method: &str, caller: &str, task: Option<&str>, took: Duration) {
    let (verb, caller, task) = (clean(verb), clean(caller), clean(task.unwrap_or("")));
    if let Ok(mut c) = COUNTERS.lock() {
        let v = c.verbs.entry(verb.clone()).or_default();
        v.0 += 1;
        v.1 += took.as_millis() as u64;
        let t = c.tasks.entry((verb.clone(), caller.clone(), task)).or_default();
        t.0 += 1;
        t.1 += took.as_millis() as u64;
    }
    if !is_batch_method(method) && !is_long_poll(method) {
        append(took, &verb, &caller, "REQUEST");
    }
}

/// Counts one request's reads behind the head under its key; a request that read none adds nothing.
pub fn past_read_done(verb: &str, caller: &str, task: Option<&str>, lag_revs: Option<u64>, lag_ms: Option<u64>, too_old: bool) {
    if lag_revs.is_none() && lag_ms.is_none() && !too_old {
        return;
    }
    let key = (clean(verb), clean(caller), clean(task.unwrap_or("")));
    if let Ok(mut c) = COUNTERS.lock() {
        let p = c.past.entry(key).or_default();
        p.too_old += u64::from(too_old);
        p.max_revs = p.max_revs.max(lag_revs);
        p.max_ms = p.max_ms.max(lag_ms);
    }
}

/// The p95 a served method must stay under (law-hot-reads-answer-in-500ms).
pub const P95_LIMIT_MS: u64 = 500;
const LATENCY_WINDOW: Duration = Duration::from_secs(300);
const LATENCY_MIN_SAMPLES: usize = 10;
const LATENCY_REALARM: Duration = Duration::from_secs(900);

#[derive(Default)]
pub struct Latency {
    samples: BTreeMap<String, VecDeque<(std::time::Instant, u64)>>,
    alarmed: BTreeMap<String, std::time::Instant>,
}

static LATENCY: Mutex<Latency> = Mutex::new(Latency { samples: BTreeMap::new(), alarmed: BTreeMap::new() });

/// The method a request is measured under: the verb, and for `work` the lane verb after the
/// bead id, so a slow brokered tool does not hide behind `work show`.
pub fn method_of(argv: &[String]) -> String {
    match argv.first().map(String::as_str) {
        Some("work") => format!("work:{}", argv.get(2).map(String::as_str).unwrap_or("-")),
        Some(v) => v.to_string(),
        None => "-".to_string(),
    }
}

/// Brokered batch jobs run a harness tool or aggregate the whole fact history; they are bounded
/// by their own deadline, not the store-call ceiling.
fn is_batch_method(method: &str) -> bool {
    method == "facts-query" || method.strip_prefix("work:").is_some_and(crate::work::brokers_tool)
}

fn is_long_poll(method: &str) -> bool {
    method == "watch"
}

pub fn sample(method: &str, took: Duration) {
    if let Ok(mut l) = LATENCY.lock() {
        l.record(method, std::time::Instant::now(), took.as_millis() as u64);
    }
}

/// One alarm line per method whose p95 over the window is past the limit, each at most once per
/// re-alarm interval.
pub fn latency_breaches(now: std::time::Instant) -> Vec<String> {
    LATENCY.lock().map(|mut l| l.breaches(now, crate::db::now_epoch())).unwrap_or_default()
}

impl Latency {
    fn record(&mut self, method: &str, now: std::time::Instant, ms: u64) {
        if !is_batch_method(method) && !is_long_poll(method) {
            self.samples.entry(clean(method)).or_default().push_back((now, ms));
        }
    }

    fn breaches(&mut self, now: std::time::Instant, epoch: i64) -> Vec<String> {
        let mut out = Vec::new();
        for (method, window) in self.samples.iter_mut() {
            while window.front().is_some_and(|(t, _)| now.saturating_duration_since(*t) > LATENCY_WINDOW) {
                window.pop_front();
            }
            if window.len() < LATENCY_MIN_SAMPLES {
                continue;
            }
            let mut ms: Vec<u64> = window.iter().map(|(_, m)| *m).collect();
            ms.sort_unstable();
            let p95 = ms[(ms.len() * 95).div_ceil(100) - 1];
            if p95 <= P95_LIMIT_MS || self.alarmed.get(method).is_some_and(|t| now.saturating_duration_since(*t) < LATENCY_REALARM) {
                continue;
            }
            self.alarmed.insert(method.clone(), now);
            out.push(format!("{epoch}\tLC-LATENCY\t{method}\tp95={p95}ms\tn={}\tlimit={P95_LIMIT_MS}ms", ms.len()));
        }
        out
    }
}

pub fn stats_json() -> String {
    let Ok(c) = COUNTERS.lock() else { return "{}".to_string() };
    let verbs: serde_json::Map<String, serde_json::Value> =
        c.verbs.iter().map(|(v, (n, ms))| (v.clone(), serde_json::json!({"count": n, "total_ms": ms}))).collect();
    let callers: Vec<serde_json::Value> =
        c.tasks.iter().map(|((v, who, task), (n, ms))| serde_json::json!({"verb": v, "caller": who, "task": task, "count": n, "total_ms": ms})).collect();
    serde_json::json!({"verbs": verbs, "callers": callers, "writes": crate::db::write_stats()}).to_string()
}

/// The counts that moved since `pushed`, as one request series and one store-time series per
/// moved key, both holding the delta.
fn series_since(c: &Counters, pushed: &BTreeMap<Key, (u64, u64)>, at: std::time::SystemTime) -> Vec<spira_otel::metrics::Series> {
    let mut out = Vec::new();
    for (key, &(n, ms)) in &c.tasks {
        let (pn, pms) = pushed.get(key).copied().unwrap_or_default();
        if n == pn {
            continue;
        }
        let labels = vec![("verb".to_string(), key.0.clone()), ("caller".to_string(), key.1.clone()), ("task".to_string(), key.2.clone())];
        let series = |name: &str, value: f64| spira_otel::metrics::Series { name: name.into(), value, labels: labels.clone(), at };
        out.push(series("spira_lc_requests", (n - pn) as f64));
        out.push(series("spira_lc_request_seconds", (ms - pms) as f64 / 1000.0));
    }
    for (key, p) in &c.past {
        let labels = vec![("verb".to_string(), key.0.clone()), ("caller".to_string(), key.1.clone()), ("task".to_string(), key.2.clone())];
        let series = |name: &str, value: f64| spira_otel::metrics::Series { name: name.into(), value, labels: labels.clone(), at };
        if p.too_old > 0 {
            out.push(series("spira_lc_store_too_old", p.too_old as f64));
        }
        if let Some(v) = p.max_revs {
            out.push(series("spira_lc_store_lag_revs_max", v as f64));
        }
        if let Some(v) = p.max_ms {
            out.push(series("spira_lc_store_lag_ms_max", v as f64));
        }
    }
    out
}

/// Pushes what moved since `pushed`; `pushed` advances only when the push succeeds. The
/// counters are not locked during the post.
pub fn push_counts(sink: &spira_otel::metrics::MetricsSink, pushed: &mut BTreeMap<Key, (u64, u64)>) {
    let Ok(c) = COUNTERS.lock() else { return };
    let (series, now, past) = (series_since(&c, pushed, std::time::SystemTime::now()), c.tasks.clone(), c.past.clone());
    drop(c);
    if !series.is_empty() && sink.try_push_series(&series).is_ok() {
        *pushed = now;
        if let Ok(mut c) = COUNTERS.lock() {
            c.past = settle(std::mem::take(&mut c.past), &past);
        }
    }
}

/// `now` less what `sent` carried: counts are subtracted and a maximum is cleared only when
/// nothing larger arrived since, so a read made during the push is reported by the next one.
fn settle(now: BTreeMap<Key, Past>, sent: &BTreeMap<Key, Past>) -> BTreeMap<Key, Past> {
    now.into_iter()
        .filter_map(|(k, p)| {
            let s = sent.get(&k).copied().unwrap_or_default();
            let left = Past {
                too_old: p.too_old - s.too_old,
                max_revs: p.max_revs.filter(|v| Some(*v) != s.max_revs),
                max_ms: p.max_ms.filter(|v| Some(*v) != s.max_ms),
            };
            (left != Past::default()).then_some((k, left))
        })
        .collect()
}

pub fn set_verb(verb: &str) {
    VERB.with(|v| *v.borrow_mut() = verb.to_string());
}

fn threshold() -> Duration {
    let ms = std::env::var("SPIRA_SLOW_QUERY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1000);
    Duration::from_millis(ms)
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
    if took < threshold() {
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

    fn fed(method: &str, durations: &[u64]) -> (Latency, std::time::Instant) {
        let t0 = std::time::Instant::now();
        let mut l = Latency::default();
        for ms in durations {
            l.record(method, t0, *ms);
        }
        (l, t0)
    }

    #[test]
    fn a_slow_method_alarms_once_and_a_fast_one_never() {
        let (mut l, t0) = fed("event-continuity", &[2400; 12]);
        for _ in 0..12 {
            l.record("show", t0, 3);
        }
        let got = l.breaches(t0, 7);
        assert_eq!(got, ["7\tLC-LATENCY\tevent-continuity\tp95=2400ms\tn=12\tlimit=500ms"]);
        assert!(l.breaches(t0, 8).is_empty(), "re-alarm waits out the interval");
        assert_eq!(l.breaches(t0 + LATENCY_REALARM + Duration::from_secs(1), 9).len(), 0, "the window has emptied by then");
    }
    #[test]
    fn p95_not_the_mean_decides() {
        let mut ms = vec![10u64; 94];
        ms.extend([900; 6]);
        let (mut l, t0) = fed("ops-view", &ms);
        assert_eq!(l.breaches(t0, 1).len(), 1, "6 of 100 over the limit puts p95 over it");
        let mut ms = vec![10u64; 96];
        ms.extend([900; 4]);
        let (mut l, t0) = fed("ops-view", &ms);
        assert!(l.breaches(t0, 1).is_empty(), "4 of 100 over the limit leaves p95 under it");
    }

    #[test]
    fn too_few_samples_and_batch_methods_do_not_alarm() {
        let (mut l, t0) = fed("list", &[4000; 9]);
        assert!(l.breaches(t0, 1).is_empty());
        let (mut l, t0) = fed("work:groom", &[4000; 20]);
        assert!(l.breaches(t0, 1).is_empty());
        let (mut l, t0) = fed("work:submit", &[40000; 20]);
        assert!(l.breaches(t0, 1).is_empty());
        let (mut l, t0) = fed("work:note", &[4000; 20]);
        assert_eq!(l.breaches(t0, 1).len(), 1, "a store-only lane verb is held to the ceiling");
    }

    #[test]
    fn a_long_poll_does_not_alarm() {
        let (mut l, t0) = fed("watch", &[2000; 20]);
        assert!(l.breaches(t0, 1).is_empty());
    }

    #[test]
    fn a_work_request_is_measured_by_its_lane_verb() {
        let a = |v: &[&str]| method_of(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["work", "sp-x", "note", "hi"]), "work:note");
        assert_eq!(a(&["work", "sp-x", "groom", "sweep"]), "work:groom");
        assert_eq!(a(&["ops-view", "ops_live"]), "ops-view");
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
    fn digits_inside_identifiers_survive() {
        assert!(shape("SELECT a1 FROM t2").contains("a1 FROM t2"));
    }

    #[test]
    fn a_fast_query_writes_nothing_and_a_slow_one_one_line() {
        let d = testkit::TempDir::new("slow-rec");
        let log = d.join("s.log");
        let _env = testkit::env(&[("SPIRA_SLOW_QUERY_LOG", Some(log.to_str().unwrap())), ("SPIRA_SLOW_QUERY_MS", Some("1000"))]);
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
        let _env = testkit::env(&[("SPIRA_SLOW_QUERY_LOG", Some(log.to_str().unwrap())), ("SPIRA_SLOW_QUERY_MS", Some("0"))]);
        request_done("probe-verb", "probe-verb", "probe\tcaller", None, Duration::from_millis(40));
        request_done("other-verb", "other-verb", "other", None, Duration::from_millis(7));
        request_done("probe-verb", "probe-verb", "probe\tcaller", None, Duration::from_millis(60));
        request_done("work", "work:submit", "probe\tcaller", None, Duration::from_millis(9000));
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
        c.tasks.insert(key.clone(), (3, 1500));
        c.tasks.insert(idle.clone(), (2, 40));
        let sink = spira_otel::metrics::MetricsSink::new(None, "spira-lc", Duration::from_millis(1));
        let at = std::time::SystemTime::now();
        let mut pushed = BTreeMap::new();
        pushed.insert(idle.clone(), (2, 40));
        let got = series_since(&c, &pushed, at);
        let flat: Vec<(String, f64)> = got.iter().map(|s| (s.name.clone(), s.value)).collect();
        assert_eq!(flat, [("spira_lc_requests".to_string(), 3.0), ("spira_lc_request_seconds".to_string(), 1.5)], "the unmoved key yields nothing");
        let encoded = format!("{:?}", sink.encode_series(&got));
        for l in ["verb", "caller", "task", "asks", "who", "spira_lc_requests", "spira_lc_request_seconds"] {
            assert!(encoded.contains(l), "{l} missing from {encoded}");
        }
        pushed.insert(key.clone(), (3, 1500));
        c.tasks.insert(key, (4, 1700));
        let got = series_since(&c, &pushed, at);
        let flat: Vec<f64> = got.iter().map(|s| s.value).collect();
        assert_eq!(flat, [1.0, 0.2]);
    }

    #[test]
    fn past_reads_are_pushed_as_a_count_and_interval_maxima_and_a_quiet_key_pushes_none() {
        let probe = |name: &str| {
            let labels = [("verb", "watch"), ("caller", "past-probe"), ("task", "-")];
            let c = COUNTERS.lock().unwrap();
            series_since(&c, &BTreeMap::new(), std::time::SystemTime::now())
                .into_iter()
                .filter(|s| s.name == name && labels.iter().all(|(k, v)| s.labels.contains(&(k.to_string(), v.to_string()))))
                .map(|s| s.value)
                .collect::<Vec<_>>()
        };
        request_done("watch", "watch", "past-probe", None, Duration::from_millis(1));
        past_read_done("watch", "past-probe", None, Some(9000), Some(5), true);
        past_read_done("watch", "past-probe", None, Some(7), None, false);
        past_read_done("watch", "past-probe", None, None, None, false);
        assert_eq!(probe("spira_lc_store_too_old"), [1.0]);
        assert_eq!(probe("spira_lc_store_lag_revs_max"), [9000.0]);
        assert_eq!(probe("spira_lc_store_lag_ms_max"), [5.0]);
        let unreachable = spira_otel::metrics::MetricsSink::new(None, "spira-lc", Duration::from_millis(1));
        push_counts(&unreachable, &mut BTreeMap::new());
        assert_eq!(probe("spira_lc_store_too_old"), [1.0], "a failed push keeps the interval");
    }

    #[test]
    fn a_too_old_answer_with_no_audit_record_counts_without_a_lag_in_ms() {
        past_read_done("watch", "no-record-probe", None, Some(3), None, true);
        let c = COUNTERS.lock().unwrap();
        let got = series_since(&c, &BTreeMap::new(), std::time::SystemTime::now());
        let mine = |n: &str| got.iter().filter(|s| s.name == n && s.labels.contains(&("caller".into(), "no-record-probe".into()))).count();
        assert_eq!((mine("spira_lc_store_too_old"), mine("spira_lc_store_lag_revs_max"), mine("spira_lc_store_lag_ms_max")), (1, 1, 0));
    }

    #[test]
    fn a_past_read_made_during_a_push_survives_it() {
        let key = ("v".to_string(), "c".to_string(), "-".to_string());
        let sent = BTreeMap::from([(key.clone(), Past { too_old: 1, max_revs: Some(9), max_ms: Some(4) })]);
        let now = BTreeMap::from([(key.clone(), Past { too_old: 2, max_revs: Some(20), max_ms: Some(4) })]);
        assert_eq!(settle(now, &sent)[&key], Past { too_old: 1, max_revs: Some(20), max_ms: None });
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
