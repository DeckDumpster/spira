//! The reap-log and dwarf-ledger keys: two log files in, flat `KEY=value` pairs out. Pure
//! over its inputs so fixtures can drive it; a source that cannot be read renders `?` for the
//! keys that depend on it, never 0.

use std::collections::{BTreeMap, BTreeSet};

use crate::timeutil::parse_strict;

pub type Keys = BTreeMap<String, String>;

fn token_split(line: &str) -> Option<(&str, &str)> {
    let end = line.find(char::is_whitespace)?;
    if end == 0 || line.as_bytes()[end] != b' ' {
        return None;
    }
    Some((&line[..end], &line[end + 1..]))
}

fn ledger(line: &str) -> Option<(&str, &str, &str, &str)> {
    let (ts, rest) = token_split(line)?;
    let (event, rest) = ["born", "awake", "done"]
        .iter()
        .find_map(|e| rest.strip_prefix(e).and_then(|r| r.strip_prefix(' ')).map(|r| (*e, r)))?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        return None;
    }
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    match rest[end..].strip_prefix(' ') {
        Some(tail) => Some((ts, event, name, tail)),
        None if end == rest.len() => Some((ts, event, name, "")),
        None => None,
    }
}

pub fn ledger_metrics(lines: &[&str], since: i64) -> Keys {
    let (mut born, mut lived, mut worked, mut thrash) = (0i64, 0i64, 0i64, 0i64);
    for line in lines {
        let Some((ts, event, _, rest)) = ledger(line) else { continue };
        if !parse_strict(ts).is_some_and(|t| t >= since) {
            continue;
        }
        let rest = rest.trim();
        match event {
            "born" => born += 1,
            "awake" => {
                lived += 1;
                if !matches!(rest, "capacity" | "paused" | "idle" | "") {
                    worked += 1;
                }
            }
            _ if rest.contains("status=requeue-thrash") => thrash += 1,
            _ => {}
        }
    }
    [
        ("SP_DWARF_BORN", born),
        ("SP_DWARF_LIVED", lived),
        ("SP_DWARF_STILLBORN", (born - lived).max(0)),
        ("SP_DWARF_WORKED", worked),
        ("SP_DWARF_THRASH", thrash),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

const LEDGER_KEYS: &[&str] = &["SP_DWARF_BORN", "SP_DWARF_LIVED", "SP_DWARF_STILLBORN", "SP_DWARF_WORKED", "SP_DWARF_THRASH"];
const REAP_KEYS: &[&str] = &["SP_SENT_FAILED", "SP_SENT_FAILED_AGE_M"];

fn read(path: &str) -> Option<String> {
    std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn unknown(keys: &[&str]) -> Keys {
    keys.iter().map(|k| (k.to_string(), "?".to_string())).collect()
}

/// Distinct beads with a `FAILED` line in `reap.log` inside the window, and minutes since the newest.
pub fn reap_metrics(lines: &[&str], since: i64, now: i64) -> Keys {
    let mut failed = BTreeSet::new();
    let mut newest: Option<i64> = None;
    for line in lines {
        let mut f = line.split_whitespace();
        let (Some(ts), Some("FAILED"), Some(id)) = (f.next(), f.next(), f.next()) else { continue };
        let Some(t) = parse_strict(ts).filter(|t| *t >= since) else { continue };
        failed.insert(id);
        newest = Some(newest.map_or(t, |n| n.max(t)));
    }
    let age = newest.map_or("?".to_string(), |n| ((now - n) / 60).to_string());
    [("SP_SENT_FAILED", failed.len().to_string()), ("SP_SENT_FAILED_AGE_M", age)].into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

pub fn collect(reap_log: &str, ledger_log: &str, hours: f64, now: i64) -> Keys {
    let since = now - (hours * 3600.0) as i64;
    let reap_text = read(reap_log);
    let ledger_text = read(ledger_log);
    let mut out = Keys::new();
    out.extend(match &reap_text {
        Some(t) => reap_metrics(&t.lines().collect::<Vec<_>>(), since, now),
        None => unknown(REAP_KEYS),
    });
    out.extend(match &ledger_text {
        Some(t) => ledger_metrics(&t.lines().collect::<Vec<_>>(), since),
        None => unknown(LEDGER_KEYS),
    });
    out.insert("SP_WINDOW_HOURS".into(), format!("{hours}"));
    out
}

pub fn render(keys: &Keys) -> String {
    keys.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1788998400 + 3600;

    fn ts(off: i64) -> String {
        let t = NOW + off;
        let days = t.div_euclid(86400);
        let s = t.rem_euclid(86400);
        let (y, m, d) = civil_from_days(days);
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
    }

    fn civil_from_days(z: i64) -> (i64, i64, i64) {
        let z = z + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (yoe + era * 400 + i64::from(m <= 2), m, d)
    }

    // covers: UC-dwarf-execution-09
    #[test_id(t0iseki)]
    #[test]
    fn requeue_thrash_ledger_lines_count_as_thrash_and_inflate_nothing_else() {
        let line = |ev: &str, rest: &str| format!("{} {ev} rune sp-x {rest}", ts(-10));
        let none = ledger_metrics(&[], NOW - 3600);
        assert_eq!(none["SP_DWARF_THRASH"], "0");
        let lines = [
            line("awake", "worked"),
            line("done", "rc=0 status=requeue-thrash"),
            line("done", "rc=0 status=requeue-thrash"),
            line("done", "rc=0 status=requeue-slain"),
        ];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let m = ledger_metrics(&refs, NOW - 3600);
        assert_eq!((m["SP_DWARF_THRASH"].as_str(), m["SP_DWARF_WORKED"].as_str()), ("2", "1"));
    }

    #[test_id(t1d107e)]
    #[test]
    fn failed_deletes_count_distinct_beads_inside_the_window_and_age_from_the_newest() {
        let l = |off: i64, verb: &str, id: &str| format!("{} {verb:<9} {id:<22} branch b survived deletion: x [by hel]", ts(off));
        let lines = [l(-7200, "FAILED", "sp-a"), l(-3000, "FAILED", "sp-a"), l(-600, "FAILED", "sp-b"), l(-90000, "FAILED", "sp-old"), l(-60, "REMOVED", "sp-c")];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let k = reap_metrics(&refs, NOW - 86400, NOW);
        assert_eq!((k["SP_SENT_FAILED"].as_str(), k["SP_SENT_FAILED_AGE_M"].as_str()), ("2", "10"));
        assert_eq!(reap_metrics(&[], NOW - 86400, NOW)["SP_SENT_FAILED_AGE_M"], "?");
    }

    #[test_id(tk2x1jt)]
    #[test]
    fn unreadable_sources_render_question_marks() {
        let k = collect("nonexistent/a", "nonexistent/b", 24.0, NOW);
        assert_eq!(k["SP_SENT_FAILED"], "?");
        assert_eq!(k["SP_DWARF_BORN"], "?");
        assert_eq!(k["SP_WINDOW_HOURS"], "24");
    }
}
