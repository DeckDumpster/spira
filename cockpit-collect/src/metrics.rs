//! The HEL / SELF / aeon-ledger keys: two log files in, flat `KEY=value` pairs out. Pure
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

fn pass_ts(line: &str) -> Option<&str> {
    let (ts, rest) = token_split(line)?;
    rest.starts_with("spira: state: ").then_some(ts)
}

fn act(line: &str) -> Option<(&str, &str)> {
    let (ts, rest) = token_split(line)?;
    rest.strip_prefix("spira: ACT ").map(|t| (ts, t))
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

fn not_an_event(text: &str) -> bool {
    if text == "invoked reflection" {
        return true;
    }
    text.strip_prefix("summoned a ")
        .and_then(|r| r.strip_suffix(" aeon"))
        .is_some_and(|w| !w.is_empty() && !w.contains(char::is_whitespace))
}

fn digits(s: &str) -> Option<(i64, &str)> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    (n > 0).then(|| (s[..n].parse().unwrap_or(i64::MAX), &s[n..]))
}

fn counts_at(s: &str) -> Option<(i64, i64, i64)> {
    let (o, s) = digits(s.strip_prefix("open=")?)?;
    let (r, s) = digits(s.strip_prefix(" plan_ready=")?)?;
    let (i, _) = digits(s.strip_prefix(" in_progress=")?)?;
    Some((o, r, i))
}

/// open work, nothing ready, nothing running — CHECK 8's precondition.
fn starved(line: &str) -> bool {
    let mut from = 0;
    while let Some(i) = line[from..].find("state: ") {
        let rest = &line[from + i + 7..];
        let mut tries = vec![rest];
        if let Some((tok, tail)) = rest.split_once(' ') {
            let word = tok.split_once('=').map(|(k, _)| k);
            if word.is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')) {
                tries.push(tail);
            }
        }
        if tries.into_iter().filter_map(counts_at).any(|(o, r, ip)| o > 0 && r == 0 && ip == 0) {
            return true;
        }
        from += i + 7;
    }
    false
}

pub fn sentinel_metrics(lines: &[&str], since: i64) -> Keys {
    let mut starved_n = 0;
    let mut passes: Vec<Vec<String>> = Vec::new();
    let mut judged_at: Option<usize> = None;
    for line in lines {
        if let Some(ts) = pass_ts(line) {
            match parse_strict(ts) {
                Some(t) if t >= since => {
                    passes.push(Vec::new());
                    if starved(line) {
                        starved_n += 1;
                    }
                }
                _ => {
                    passes.clear();
                    judged_at = None;
                    starved_n = 0;
                }
            }
            continue;
        }
        if let (Some((_, text)), false) = (act(line), passes.is_empty()) {
            let text = text.trim().to_string();
            if text == "invoked reflection" {
                judged_at = Some(passes.len() - 1);
            }
            passes.last_mut().unwrap().push(text);
        }
    }
    let n = passes.len();
    let acts: usize = passes.iter().map(Vec::len).sum();
    let mut false_acts = 0;
    for i in 1..n {
        let prev: BTreeSet<&String> = passes[i - 1].iter().collect();
        false_acts += passes[i].iter().filter(|t| !not_an_event(t) && prev.contains(t)).count();
    }
    let mut out = Keys::new();
    out.insert("SP_PASSES".into(), n.to_string());
    out.insert("SP_ACTS".into(), acts.to_string());
    out.insert("SP_FALSE_ACTS".into(), false_acts.to_string());
    out.insert(
        "SP_FALSE_PER_PASS".into(),
        if n > 0 { format!("{:.2}", false_acts as f64 / n as f64) } else { "?".into() },
    );
    out.insert("SP_STARVED_PASSES".into(), starved_n.to_string());
    out.insert(
        "SP_SINCE_JUDGEMENT".into(),
        match judged_at {
            None if starved_n == 0 => "n/a".into(),
            None => format!("NEVER ({starved_n} starved)"),
            Some(j) => (n - 1 - j).to_string(),
        },
    );
    out
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
        ("SP_AEON_BORN", born),
        ("SP_AEON_LIVED", lived),
        ("SP_AEON_STILLBORN", (born - lived).max(0)),
        ("SP_AEON_WORKED", worked),
        ("SP_AEON_THRASH", thrash),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

pub fn hel_metrics(lines: &[&str], since: i64, now: i64) -> Keys {
    let (mut sent, mut failed, mut held, mut kept) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    let mut failed_last: Option<i64> = None;
    let mut cur: Option<i64> = None;
    for line in lines {
        if let Some(ts) = pass_ts(line).or_else(|| act(line).map(|(t, _)| t)) {
            cur = parse_strict(ts);
        }
        let Some(c) = cur.filter(|c| *c >= since) else { continue };
        let t = line.trim();
        let second = || t.split_whitespace().nth(1).map(str::to_string);
        if t.starts_with("SENT") {
            sent.extend(second());
        } else if t.starts_with("HELD") {
            held.extend(second());
        } else if t.starts_with("KEEP") {
            kept.extend(second());
        } else if t.starts_with("FAILED") {
            if let Some(id) = second() {
                failed.insert(id);
                failed_last = Some(failed_last.map_or(c, |f| f.max(c)));
            }
        }
    }
    let age = failed_last.map_or("?".to_string(), |f| ((now - f) / 60).to_string());
    [
        ("SP_SENT", sent.len().to_string()),
        ("SP_SENT_HELD", held.len().to_string()),
        ("SP_SENT_KEPT", kept.len().to_string()),
        ("SP_SENT_FAILED", failed.len().to_string()),
        ("SP_SENT_FAILED_AGE_M", age),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// What is repeating now, and the two regression tripwires, over the short SELF window.
pub fn self_metrics(sent: &[&str], ledger_lines: &[&str], self_since: i64, now: i64) -> Keys {
    let mut passes: Vec<(i64, Vec<String>)> = Vec::new();
    let mut starved_tss: Vec<i64> = Vec::new();
    let mut in_pass = false;
    for line in sent {
        if let Some(ts) = pass_ts(line) {
            match parse_strict(ts) {
                Some(t) if t >= self_since => {
                    passes.push((t, Vec::new()));
                    in_pass = true;
                    if starved(line) {
                        starved_tss.push(t);
                    }
                }
                _ => in_pass = false,
            }
            continue;
        }
        if let (Some((_, text)), true) = (act(line), in_pass) {
            let text = text.trim();
            if !not_an_event(text) {
                passes.last_mut().unwrap().1.push(text.to_string());
            }
        }
    }

    let mut out = Keys::new();
    let n = passes.len();
    let mut repeating: Vec<(usize, i64, String)> = Vec::new();
    if n >= 2 {
        let last: BTreeSet<&String> = passes[n - 1].1.iter().collect();
        for text in last {
            let mut run = 0;
            let mut first_ts = passes[n - 1].0;
            for (ts, acts) in passes.iter().rev() {
                if !acts.contains(text) {
                    break;
                }
                run += 1;
                first_ts = *ts;
            }
            if run >= 2 {
                repeating.push((run, ((passes[n - 1].0 - first_ts) / 60).max(1), text.clone()));
            }
        }
    }
    repeating.sort_by(|a, b| b.0.cmp(&a.0));
    out.insert("SP_SELF_REPEATING_N".into(), repeating.len().to_string());
    for (i, (run, dur, text)) in repeating.iter().enumerate() {
        out.insert(format!("SP_SELF_REPEATING{i}"), format!("{text} \u{d7} {run} passes ({dur}m)"));
    }

    out.insert("SP_SELF_STARVED_W".into(), starved_tss.len().to_string());
    out.insert("SP_SELF_STARVED_LAST".into(), ago(starved_tss.iter().max().copied(), now));

    let mut born: BTreeMap<&str, i64> = BTreeMap::new();
    for line in ledger_lines {
        let Some((ts, event, name, _)) = ledger(line) else { continue };
        let Some(t) = parse_strict(ts).filter(|t| *t >= self_since) else { continue };
        match event {
            "born" => {
                born.insert(name, t);
            }
            "awake" => {
                born.remove(name);
            }
            _ => {}
        }
    }
    out.insert("SP_SELF_STILLBORN_W".into(), born.len().to_string());
    out.insert("SP_SELF_STILLBORN_LAST".into(), ago(born.values().max().copied(), now));
    out
}

fn ago(latest: Option<i64>, now: i64) -> String {
    latest.map_or("-".into(), |t| format!("{}m ago", ((now - t) / 60).max(0)))
}

const SENTINEL_KEYS: &[&str] = &[
    "SP_PASSES", "SP_ACTS", "SP_FALSE_ACTS", "SP_FALSE_PER_PASS", "SP_SINCE_JUDGEMENT", "SP_STARVED_PASSES",
];
const LEDGER_KEYS: &[&str] = &["SP_AEON_BORN", "SP_AEON_LIVED", "SP_AEON_STILLBORN", "SP_AEON_WORKED", "SP_AEON_THRASH"];
const HEL_KEYS: &[&str] = &["SP_SENT", "SP_SENT_HELD", "SP_SENT_KEPT", "SP_SENT_FAILED", "SP_SENT_FAILED_AGE_M"];
const SELF_KEYS: &[&str] = &[
    "SP_SELF_REPEATING_N", "SP_SELF_STILLBORN_W", "SP_SELF_STILLBORN_LAST", "SP_SELF_STARVED_W", "SP_SELF_STARVED_LAST",
];

fn read(path: &str) -> Option<String> {
    std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn unknown(keys: &[&str]) -> Keys {
    keys.iter().map(|k| (k.to_string(), "?".to_string())).collect()
}

/// SPIRA_SELF_WINDOW is minutes, from the registered config.
pub fn self_window_minutes() -> Option<f64> {
    spira_config::process::cfg("SPIRA_SELF_WINDOW")
        .ok()
        .and_then(|v| v.trim().parse().ok())
}

pub fn collect(sentinel_log: &str, ledger_log: &str, hours: f64, self_window_min: Option<f64>, now: i64) -> Keys {
    let since = now - (hours * 3600.0) as i64;
    let sent_text = read(sentinel_log);
    let ledger_text = read(ledger_log);
    let sent: Option<Vec<&str>> = sent_text.as_deref().map(|t| t.lines().collect());
    let led: Option<Vec<&str>> = ledger_text.as_deref().map(|t| t.lines().collect());

    let mut out = Keys::new();
    out.extend(sent.as_deref().map_or_else(|| unknown(SENTINEL_KEYS), |l| sentinel_metrics(l, since)));
    out.extend(led.as_deref().map_or_else(|| unknown(LEDGER_KEYS), |l| ledger_metrics(l, since)));
    out.extend(sent.as_deref().map_or_else(|| unknown(HEL_KEYS), |l| hel_metrics(l, since, now)));
    out.extend(match (sent.as_deref(), led.as_deref(), self_window_min) {
        (Some(s), Some(l), Some(w)) => self_metrics(s, l, now - (w * 60.0) as i64, now),
        _ => unknown(SELF_KEYS),
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

    #[test]
    fn hel_counts_distinct_branches_and_attributes_by_preceding_timestamp() {
        let state = format!("{} spira: state: open=0 plan_ready=0 in_progress=0 aeons=0", ts(-300));
        let lines = [
            "SENT before-any-timestamp x",
            &state,
            "SENT sp-test1  myrepo mybranch  orphaned worktree",
            "SENT sp-test1  again",
            "FAILED sp-f x",
        ];
        let k = hel_metrics(&lines, NOW - 86400, NOW);
        assert_eq!(k["SP_SENT"], "1");
        assert_eq!(k["SP_SENT_FAILED"], "1");
        assert_eq!(k["SP_SENT_FAILED_AGE_M"], "5");
    }

    #[test]
    fn ledger_counts_thrash_without_inflating_worked() {
        let lines = [
            format!("{} awake rune sp-aaa worked", ts(-10)),
            format!("{} done rune sp-bbb rc=0 status=requeue-thrash", ts(-10)),
            format!("{} done rune sp-ccc rc=0 status=requeue-thrash", ts(-10)),
            format!("{} done rune sp-eee rc=0 status=requeue-slain", ts(-10)),
        ];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let k = ledger_metrics(&refs, NOW - 86400);
        assert_eq!(k["SP_AEON_THRASH"], "2");
        assert_eq!(k["SP_AEON_WORKED"], "1");
        assert_eq!(ledger_metrics(&[], NOW - 86400)["SP_AEON_THRASH"], "0");
    }

    #[test]
    fn repeating_act_run_is_found_and_old_burst_is_not() {
        let mk = |offs: &[i64]| -> Vec<String> {
            offs.iter()
                .flat_map(|o| {
                    vec![
                        format!("{} spira: state: open=1 plan_ready=1 in_progress=0", ts(*o)),
                        format!("{} spira: ACT handled 1 stranded item(s)", ts(*o)),
                    ]
                })
                .collect()
        };
        let recent = mk(&[-600, -480, -360]);
        let r: Vec<&str> = recent.iter().map(String::as_str).collect();
        let k = self_metrics(&r, &[], NOW - 5400, NOW);
        assert_eq!(k["SP_SELF_REPEATING_N"], "1");
        assert_eq!(k["SP_SELF_REPEATING0"], "handled 1 stranded item(s) \u{d7} 3 passes (4m)");
        let old = mk(&[-21600, -21540, -21480]);
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        assert_eq!(self_metrics(&o, &[], NOW - 5400, NOW)["SP_SELF_REPEATING_N"], "0");
    }

    #[test]
    fn starved_reads_old_prefixed_state_lines() {
        assert!(starved("t spira: state: open=2 plan_ready=0 in_progress=0 aeons=0"));
        assert!(starved("t spira: state: x=1 open=2 plan_ready=0 in_progress=0"));
        assert!(!starved("t spira: state: open=2 plan_ready=1 in_progress=0"));
    }

    #[test]
    fn unreadable_sources_render_question_marks() {
        let k = collect("/nonexistent/a", "/nonexistent/b", 24.0, Some(60.0), NOW);
        assert_eq!(k["SP_PASSES"], "?");
        assert_eq!(k["SP_SELF_STARVED_W"], "?");
        assert_eq!(k["SP_WINDOW_HOURS"], "24");
    }
}
