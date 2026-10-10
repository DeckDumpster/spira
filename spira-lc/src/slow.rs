//! Wall time of every Dolt query and of every served request. One over the threshold appends a
//! line to the slow-query log; watchtower's `--slow-query-check` turns the log into one
//! incident per shape. Line: `<epoch>\t<verb>\t<caller>\t<millis>\t<shape>`; a whole request is
//! the shape `REQUEST`. The caller is the peer's `/proc/<pid>/comm`, never its cmdline.
//! `stats_json` is the per-verb and per-caller counter of what this process has served.

use std::cell::RefCell;
use std::collections::BTreeMap;
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
    callers: BTreeMap<(String, String), u64>,
}

static COUNTERS: Mutex<Counters> = Mutex::new(Counters { verbs: BTreeMap::new(), callers: BTreeMap::new() });

pub fn set_caller(caller: &str) {
    CALLER.with(|c| *c.borrow_mut() = clean(caller));
}

fn clean(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_whitespace() || c.is_control() { '_' } else { c }).take(32).collect();
    if s.is_empty() { "-".to_string() } else { s }
}

pub fn caller_of_pid(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|c| clean(c.trim())).unwrap_or_else(|_| "-".to_string())
}

pub fn request_done(verb: &str, caller: &str, took: Duration) {
    let (verb, caller) = (clean(verb), clean(caller));
    if let Ok(mut c) = COUNTERS.lock() {
        let v = c.verbs.entry(verb.clone()).or_default();
        v.0 += 1;
        v.1 += took.as_millis() as u64;
        *c.callers.entry((verb.clone(), caller.clone())).or_default() += 1;
    }
    append(took, &verb, &caller, "REQUEST");
}

pub fn stats_json() -> String {
    let Ok(c) = COUNTERS.lock() else { return "{}".to_string() };
    let verbs: serde_json::Map<String, serde_json::Value> =
        c.verbs.iter().map(|(v, (n, ms))| (v.clone(), serde_json::json!({"count": n, "total_ms": ms}))).collect();
    let callers: Vec<serde_json::Value> =
        c.callers.iter().map(|((v, who), n)| serde_json::json!({"verb": v, "caller": who, "count": n})).collect();
    serde_json::json!({"verbs": verbs, "callers": callers}).to_string()
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
        request_done("probe-verb", "probe\tcaller", Duration::from_millis(40));
        request_done("probe-verb", "probe\tcaller", Duration::from_millis(60));
        let got = std::fs::read_to_string(&log).unwrap();
        assert!(got.lines().all(|l| l.split('\t').collect::<Vec<_>>()[1..] == ["probe-verb", "probe_caller", l.split('\t').nth(3).unwrap(), "REQUEST"][..]), "{got}");
        let s: serde_json::Value = serde_json::from_str(&stats_json()).unwrap();
        assert_eq!(s["verbs"]["probe-verb"], serde_json::json!({"count": 2, "total_ms": 100}));
        assert!(s["callers"].as_array().unwrap().iter().any(|c| c["verb"] == "probe-verb" && c["caller"] == "probe_caller" && c["count"] == 2));
    }

    #[test]
    fn a_caller_name_is_the_comm_of_the_pid_and_unknown_pids_are_dashes() {
        let comm = std::fs::read_to_string("/proc/self/comm").unwrap();
        assert_eq!(caller_of_pid(std::process::id() as i32), clean(comm.trim()));
        assert_eq!(caller_of_pid(-1), "-");
    }
}
