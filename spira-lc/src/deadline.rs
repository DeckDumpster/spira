//! `deadline-check` and `deadline lower`: the store's request deadline comes down only when no
//! call in the window took longer than the new value to answer. Local verbs; no store call.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{CANNOT_TELL, REFUSED};

const ANSWER_US: &str = "spira.answer_us";
const DEADLINE_PATH: &str = "spira.lc_request_deadline_ms";
const SLOW_PATH: &str = "spira.lc_slow_verb_deadlines";

#[derive(Debug, PartialEq)]
pub(crate) enum Scope {
    /// Every verb that gets the configured default: none of the listed verbs.
    Default { excluded: BTreeSet<String> },
    Verb(String),
}

impl Scope {
    fn covers(&self, verb: &str) -> bool {
        match self {
            Scope::Default { excluded } => !excluded.contains(verb),
            Scope::Verb(v) => v == verb,
        }
    }

    fn label(&self) -> String {
        match self {
            Scope::Default { .. } => "a default-deadline verb".to_string(),
            Scope::Verb(v) => format!("`{v}`"),
        }
    }
}

struct Measured {
    verb: String,
    answer_us: Option<u64>,
}

fn int(v: &serde_json::Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

/// The spans in one file's text that started at or after `since_ns`. Lines that are not spans are skipped.
fn measured(text: &str, since_ns: u64) -> Vec<Measured> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(resources) = doc["resourceSpans"].as_array() else { continue };
        for span in resources.iter().flat_map(|r| r["scopeSpans"].as_array().into_iter().flatten()).flat_map(|s| s["spans"].as_array().into_iter().flatten()) {
            let Some(start_ns) = int(&span["startTimeUnixNano"]) else { continue };
            if start_ns < since_ns {
                continue;
            }
            let answer_us = span["attributes"].as_array().and_then(|a| a.iter().find(|kv| kv["key"] == ANSWER_US)).and_then(|kv| int(&kv["value"]["intValue"]));
            out.push(Measured { verb: span["name"].as_str().unwrap_or_default().to_string(), answer_us });
        }
    }
    out
}

/// Judges the files' spans: `Err` (exit 3) naming each offending verb, or a covered span with no `spira.answer_us`.
fn judge(files: &[(String, String)], ms: u64, scope: &Scope, since_ns: u64, hours: u64) -> Result<String, String> {
    let mut by_verb: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    let mut unmeasured = Vec::new();
    for (name, text) in files {
        let spans = measured(text, since_ns);
        let missing = spans.iter().filter(|s| s.answer_us.is_none() && scope.covers(&s.verb)).count();
        if missing > 0 {
            unmeasured.push(format!("{name}: {missing} span(s) without {ANSWER_US}"));
        }
        for s in spans.iter().filter(|s| scope.covers(&s.verb)) {
            let Some(us) = s.answer_us else { continue };
            let e = by_verb.entry(s.verb.clone()).or_default();
            e.0 = e.0.max(us.div_ceil(1000));
            e.1 += u64::from(us > ms * 1000);
            e.2 += 1;
        }
    }
    let mut refusals: Vec<String> = by_verb.iter().filter(|(_, v)| v.1 > 0).map(|(verb, (max, over, total))| format!("{verb} max={max}ms over={over}/{total}")).collect();
    refusals.extend(unmeasured);
    if !refusals.is_empty() {
        return Err(refusals.join("\n"));
    }
    let calls: u64 = by_verb.values().map(|v| v.2).sum();
    Ok(format!("no call to {} took over {ms} ms in {hours} h ({calls} calls)", scope.label()))
}

#[cfg(test)]
fn check(text: &str, ms: u64, scope: &Scope) -> Result<String, String> {
    judge(&[("spans".to_string(), text.to_string())], ms, scope, 0, 24)
}

/// The span files of the window: `spira-lc.<date>.jsonl` dated from the window's first day on.
fn window_files(dir: &Path, since_day: &str) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.strip_prefix("spira-lc.").and_then(|r| r.strip_suffix(".jsonl")).is_some_and(|d| d >= since_day))
        .collect();
    names.sort();
    names.into_iter().filter_map(|n| std::fs::read_to_string(dir.join(&n)).ok().map(|t| (n, t))).collect()
}

struct Parsed {
    ms: u64,
    verb: Option<String>,
    hours: u64,
    file: Option<String>,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let (mut ms, mut verb, mut hours, mut file) = (None, None, 24, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--ms" => ms = Some(value("--ms")?.parse::<u64>().map_err(|_| "--ms is a whole number of milliseconds".to_string())?),
            "--hours" => hours = value("--hours")?.parse::<u64>().map_err(|_| "--hours is a whole number".to_string())?,
            "--verb" => verb = Some(value("--verb")?),
            "--file" => file = Some(value("--file")?),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let ms = ms.ok_or("--ms is required")?;
    if ms == 0 || ms > 3000 {
        return Err("--ms is 1..=3000".to_string());
    }
    Ok(Parsed { ms, verb, hours, file })
}

fn default_scope(slow: &str) -> Result<Scope, String> {
    let mut excluded: BTreeSet<String> = ["work", "unpublished"].iter().map(|v| v.to_string()).collect();
    excluded.extend(crate::composed::SWEEPS.iter().map(|v| v.to_string()));
    excluded.extend(crate::serve::parse_slow_entries(slow)?.into_iter().map(|(v, _, _)| v));
    Ok(Scope::Default { excluded })
}

fn usage(why: &str) -> (i32, String) {
    (CANNOT_TELL, format!("spira-lc deadline: {why}\nusage: deadline-check --ms N [--verb V] [--hours H]\n       deadline lower --ms N --file F [--verb V]"))
}

fn run_check(p: &Parsed, slow: &str, dir: &Path, now: SystemTime) -> (i32, String) {
    let scope = match &p.verb {
        Some(v) => Scope::Verb(v.clone()),
        None => match default_scope(slow) {
            Ok(s) => s,
            Err(e) => return (CANNOT_TELL, e),
        },
    };
    let since = now.checked_sub(Duration::from_secs(p.hours * 3600)).unwrap_or(UNIX_EPOCH);
    let since_ns = since.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let files = window_files(dir, &spira_otel::utc_date(since));
    match judge(&files, p.ms, &scope, since_ns, p.hours) {
        Ok(line) => (0, line),
        Err(lines) => (REFUSED, lines),
    }
}

/// `args` after `deadline-check`.
pub(crate) fn deadline_check(args: &[String], slow: &str, dir: &Path, now: SystemTime) -> (i32, String) {
    match parse(args) {
        Ok(p) => run_check(&p, slow, dir, now),
        Err(e) => usage(&e),
    }
}

/// The entries with `verb`'s deadline set to `ms`; `None` when the list has no such entry.
fn rewrite_entry(slow: &str, verb: &str, ms: u64) -> Result<Option<String>, String> {
    let entries = crate::serve::parse_slow_entries(slow)?;
    if !entries.iter().any(|(v, _, _)| v == verb) {
        return Ok(None);
    }
    let joined = entries.iter().map(|(v, old, bead)| format!("{v}={}@{bead}", if v == verb { ms } else { *old })).collect::<Vec<_>>().join(",");
    Ok(Some(joined))
}

/// `args` after `deadline lower`: runs the check, and only on a pass the `spira-config` command through `config`.
pub(crate) fn deadline_lower(args: &[String], slow: &str, dir: &Path, now: SystemTime, config: &mut dyn FnMut(&[String]) -> i32) -> (i32, String) {
    let p = match parse(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let Some(file) = &p.file else { return usage("--file is required") };
    let (key, value) = match &p.verb {
        None => (DEADLINE_PATH, p.ms.to_string()),
        Some(v) => match rewrite_entry(slow, v, p.ms) {
            Ok(Some(joined)) => (SLOW_PATH, joined),
            Ok(None) => return (CANNOT_TELL, format!("spira-lc deadline: {SLOW_PATH} has no entry for `{v}`")),
            Err(e) => return (CANNOT_TELL, e),
        },
    };
    let (code, text) = run_check(&p, slow, dir, now);
    if code != 0 {
        return (code, text);
    }
    let set: Vec<String> = ["set", key, &value, file].iter().map(|s| s.to_string()).collect();
    match config(&set) {
        0 => (0, format!("{text}\nset {key} = {value} in {file}; lc-serve takes it at its next restart")),
        rc => (CANNOT_TELL, format!("spira-config set {key} exited {rc}")),
    }
}

/// The live `spira-config` runner.
pub(crate) fn spira_config(args: &[String]) -> i32 {
    let Ok(program) = spira_config::release_env::tool("spira-config") else { return CANNOT_TELL };
    // batch-job: `set` waits on the scheduler's own bounded answer; it is an operator's local verb, not a store call.
    std::process::Command::new(program).args(args).status().map_or(CANNOT_TELL, |s| s.code().unwrap_or(CANNOT_TELL))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(verb: &str, answer_ms: Option<u64>) -> String {
        let attrs = answer_ms.map(|ms| format!(r#"{{"key":"{ANSWER_US}","value":{{"intValue":"{}"}}}}"#, ms * 1000)).unwrap_or_default();
        format!(r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{{"name":"{verb}","startTimeUnixNano":"1700000000000000000","attributes":[{attrs}]}}]}}]}}]}}"#)
    }

    fn text(spans: &[(&str, Option<u64>)]) -> String {
        spans.iter().map(|(v, ms)| span(v, *ms)).collect::<Vec<_>>().join("\n")
    }

    fn default_scope_with(slow: &[&str]) -> Scope {
        Scope::Default { excluded: slow.iter().map(|s| s.to_string()).collect() }
    }

    fn lists() -> String {
        text(&[("list", Some(40)), ("list", Some(80)), ("list", Some(1200)), ("show", Some(20))])
    }

    #[test]
    fn a_covered_call_over_the_limit_refuses_naming_its_verb_max_and_count() {
        assert_eq!(check(&lists(), 1000, &default_scope_with(&[])).unwrap_err(), "list max=1200ms over=1/3");
        assert!(check(&lists(), 1500, &default_scope_with(&[])).is_ok());
    }

    #[test]
    fn a_declared_slow_verb_is_out_of_the_default_scope_but_in_scope_by_name() {
        assert!(check(&lists(), 1000, &default_scope_with(&["list"])).is_ok());
        assert_eq!(check(&lists(), 1000, &Scope::Verb("list".into())).unwrap_err(), "list max=1200ms over=1/3");
    }

    #[test]
    fn a_covered_span_without_a_measured_answer_refuses_and_an_uncovered_one_is_ignored() {
        let t = text(&[("show", Some(5)), ("show", None), ("show", None)]);
        assert_eq!(check(&t, 1000, &default_scope_with(&[])).unwrap_err(), format!("spans: 2 span(s) without {ANSWER_US}"));
        let t = text(&[("show", Some(5)), ("watch", None)]);
        assert!(check(&t, 1000, &default_scope_with(&["watch"])).is_ok());
    }

    #[test]
    fn spans_before_the_window_are_not_counted() {
        let t = lists();
        assert!(judge(&[("f".into(), t)], 1000, &default_scope_with(&[]), 1_700_000_000_000_000_001, 24).is_ok());
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn day_dir(body: &str) -> testkit::TempDir {
        let dir = testkit::TempDir::new("lc-deadline");
        std::fs::write(dir.join("spira-lc.2023-11-14.jsonl"), body).unwrap();
        dir
    }

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_100)
    }

    #[test]
    fn lower_with_a_failing_check_runs_no_spira_config_command() {
        let dir = day_dir(&lists());
        let mut calls = Vec::new();
        let (code, out) = deadline_lower(&args(&["--ms", "1000", "--file", "f.toml"]), "", dir.path(), now(), &mut |a| {
            calls.push(a.to_vec());
            0
        });
        assert_eq!((code, out.as_str()), (3, "list max=1200ms over=1/3"));
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test]
    fn lower_with_a_passing_check_sets_the_key_through_spira_config() {
        let dir = day_dir(&lists());
        let mut calls = Vec::new();
        let (code, _) = deadline_lower(&args(&["--ms", "1500", "--file", "f.toml"]), "", dir.path(), now(), &mut |a| {
            calls.push(a.to_vec());
            0
        });
        assert_eq!(code, 0);
        assert_eq!(calls, vec![args(&["set", "spira.lc_request_deadline_ms", "1500", "f.toml"])]);
    }

    #[test]
    fn lower_for_a_verb_rewrites_only_its_entry_and_keeps_its_bead() {
        let dir = day_dir(&lists());
        let mut calls = Vec::new();
        let slow = "list=3000@sp-a,rc=3000@sp-b";
        let (code, _) = deadline_lower(&args(&["--ms", "1500", "--verb", "list", "--file", "f.toml"]), slow, dir.path(), now(), &mut |a| {
            calls.push(a.to_vec());
            0
        });
        assert_eq!(code, 0);
        assert_eq!(calls, vec![args(&["set", "spira.lc_slow_verb_deadlines", "list=1500@sp-a,rc=3000@sp-b", "f.toml"])]);
    }

    #[test]
    fn the_default_scope_leaves_out_work_sweeps_and_declared_verbs() {
        let Scope::Default { excluded } = default_scope("rc=3000@sp-b").unwrap() else { panic!() };
        for v in ["work", "unpublished", "reconcile-content", "rc"] {
            assert!(excluded.contains(v), "{v}");
        }
        assert!(!excluded.contains("show"));
    }
}
