//! Wall time of every Dolt query. One over the threshold appends a line to the slow-query
//! log; watchtower's `--slow-query-check` turns the log into one incident per shape.
//! Line: `<epoch>\t<verb>\t<millis>\t<shape>`.

use std::cell::RefCell;
use std::io::Write;
use std::time::Duration;

thread_local! {
    static VERB: RefCell<String> = const { RefCell::new(String::new()) };
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
    std::env::var("SPIRA_RUN").ok().filter(|r| !r.is_empty()).map(|r| std::path::Path::new(&r).join("slow-queries.log"))
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

pub fn line(epoch: i64, verb: &str, took: Duration, sql: &str) -> String {
    format!("{epoch}\t{verb}\t{}\t{}\n", took.as_millis(), shape(sql))
}

pub fn record(took: Duration, sql: &str) {
    if took < threshold() {
        return;
    }
    let Some(path) = log_path() else { return };
    let verb = VERB.with(|v| v.borrow().clone());
    let verb = if verb.is_empty() { "-".to_string() } else { verb };
    let l = line(crate::db::now_epoch(), &verb, took, sql);
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
        record(Duration::from_millis(500), "SELECT 1");
        assert!(!log.exists());
        record(Duration::from_millis(1500), "SELECT * FROM bead WHERE id = 'a'");
        let got = std::fs::read_to_string(&log).unwrap();
        assert_eq!(got.lines().count(), 1);
        let f: Vec<&str> = got.trim_end().split('\t').collect();
        assert_eq!((f[1], f[2], f[3]), ("list", "1500", "SELECT * FROM bead WHERE id = ?"));
    }
}
