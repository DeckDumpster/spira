//! `deadline-check` and `deadline lower`: the store's request deadline comes down only when no
//! call in the window took longer than the new value to answer. Local verbs; no store call.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{CANNOT_TELL, REFUSED};

const ANSWER_US: &str = "spira.answer_us";
const CUT_MS: &str = "spira.deadline_cut_ms";
const DEADLINE_PATH: &str = "spira.lc_request_deadline_ms";

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
    cut_ms: Option<u64>,
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
            let attr = |key: &str| span["attributes"].as_array().and_then(|a| a.iter().find(|kv| kv["key"] == key)).and_then(|kv| int(&kv["value"]["intValue"]));
            out.push(Measured { verb: span["name"].as_str().unwrap_or_default().to_string(), answer_us: attr(ANSWER_US), cut_ms: attr(CUT_MS) });
        }
    }
    out
}

/// Judges the files' spans: `Err` (exit 3) naming each verb with a call over `ms` or cut by the request cutoff, and each file with a covered span lacking `spira.answer_us`.
/// A cut call's answer time is the cutoff, not the call's, so it is unmeasured whatever it says.
fn judge(files: &[(String, String)], ms: u64, scope: &Scope, since_ns: u64, hours: u64) -> Result<String, String> {
    #[derive(Default)]
    struct Tally {
        max: u64,
        over: u64,
        total: u64,
        cut: u64,
        cut_ms: u64,
    }
    let mut by_verb: BTreeMap<String, Tally> = BTreeMap::new();
    let mut unmeasured = Vec::new();
    for (name, text) in files {
        let covered: Vec<Measured> = measured(text, since_ns).into_iter().filter(|s| scope.covers(&s.verb)).collect();
        let missing = covered.iter().filter(|s| s.answer_us.is_none() && s.cut_ms.is_none()).count();
        if missing > 0 {
            unmeasured.push(format!("{name}: {missing} span(s) without {ANSWER_US}"));
        }
        for s in &covered {
            let t = by_verb.entry(s.verb.clone()).or_default();
            t.total += 1;
            if let Some(cut) = s.cut_ms {
                t.cut += 1;
                t.cut_ms = t.cut_ms.max(cut);
            } else if let Some(us) = s.answer_us {
                t.max = t.max.max(us.div_ceil(1000));
                t.over += u64::from(us > ms * 1000);
            }
        }
    }
    let mut refusals: Vec<String> = by_verb.iter().filter(|(_, t)| t.over > 0).map(|(verb, t)| format!("{verb} max={}ms over={}/{}", t.max, t.over, t.total)).collect();
    refusals.extend(by_verb.iter().filter(|(_, t)| t.cut > 0).map(|(verb, t)| format!("{verb} cut={}/{} at {}ms: answer time unknown", t.cut, t.total, t.cut_ms)));
    refusals.extend(unmeasured);
    if !refusals.is_empty() {
        return Err(refusals.join("\n"));
    }
    let calls: u64 = by_verb.values().map(|t| t.total).sum();
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
    /// `None` under `--configured`, which takes every value from the config.
    ms: Option<u64>,
    verb: Option<String>,
    hours: u64,
    file: Option<String>,
    configured: bool,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let (mut ms, mut verb, mut hours, mut file, mut configured) = (None, None, 24, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--ms" => ms = Some(value("--ms")?.parse::<u64>().map_err(|_| "--ms is a whole number of milliseconds".to_string())?),
            "--hours" => hours = value("--hours")?.parse::<u64>().map_err(|_| "--hours is a whole number".to_string())?,
            "--verb" => verb = Some(value("--verb")?),
            "--file" => file = Some(value("--file")?),
            "--configured" => configured = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if configured {
        if ms.is_some() || verb.is_some() || file.is_some() {
            return Err("--configured checks the configured values and excludes --ms, --verb and --file".to_string());
        }
        return Ok(Parsed { ms: None, verb, hours, file, configured });
    }
    let ms = ms.ok_or("--ms is required")?;
    if ms == 0 || ms > 3000 {
        return Err("--ms is 1..=3000".to_string());
    }
    Ok(Parsed { ms: Some(ms), verb, hours, file, configured })
}

fn default_scope() -> Scope {
    let mut excluded: BTreeSet<String> = ["work", "watch"].iter().map(|v| v.to_string()).collect();
    excluded.extend(crate::composed::SWEEPS.iter().map(|v| v.to_string()));
    Scope::Default { excluded }
}

fn usage(why: &str) -> (i32, String) {
    (CANNOT_TELL, format!("spira-lc deadline: {why}\nusage: deadline-check (--ms N [--verb V] | --configured) [--hours H]\n       deadline lower --ms N --file F"))
}

fn window(p: &Parsed, dir: &Path, now: SystemTime) -> (u64, Vec<(String, String)>) {
    let since = now.checked_sub(Duration::from_secs(p.hours * 3600)).unwrap_or(UNIX_EPOCH);
    let since_ns = since.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    (since_ns, window_files(dir, &spira_otel::utc_date(since)))
}

fn run_check(p: &Parsed, ms: u64, dir: &Path, now: SystemTime) -> (i32, String) {
    let scope = match &p.verb {
        Some(v) => Scope::Verb(v.clone()),
        None => default_scope(),
    };
    let (since_ns, files) = window(p, dir, now);
    match judge(&files, ms, &scope, since_ns, p.hours) {
        Ok(line) => (0, line),
        Err(lines) => (REFUSED, lines),
    }
}

/// The one configured default over every verb that gets it.
fn run_configured(p: &Parsed, d: &crate::serve::Deadlines, dir: &Path, now: SystemTime) -> (i32, String) {
    let (since_ns, files) = window(p, dir, now);
    match judge(&files, d.default.as_millis() as u64, &default_scope(), since_ns, p.hours) {
        Ok(line) => (0, line),
        Err(lines) => (REFUSED, lines),
    }
}

/// `args` after `deadline-check`.
pub(crate) fn deadline_check(args: &[String], dir: &Path, now: SystemTime) -> (i32, String) {
    deadline_check_from(args, dir, now, spira_config::process::cfg)
}

fn deadline_check_from(args: &[String], dir: &Path, now: SystemTime, read: impl Fn(&str) -> Result<String, String>) -> (i32, String) {
    match parse(args) {
        Ok(p) if p.configured => match crate::serve::deadlines_from(read) {
            Ok(d) => run_configured(&p, &d, dir, now),
            Err(e) => (CANNOT_TELL, e),
        },
        Ok(p) => run_check(&p, p.ms.unwrap_or_default(), dir, now),
        Err(e) => usage(&e),
    }
}

/// `args` after `deadline lower`: runs the check, and only on a pass the `spira-config` command through `config`.
pub(crate) fn deadline_lower(args: &[String], dir: &Path, now: SystemTime, config: &mut dyn FnMut(&[String]) -> i32) -> (i32, String) {
    let p = match parse(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let Some(ms) = p.ms else { return usage("--configured is for deadline-check; lowering takes --ms") };
    let Some(file) = &p.file else { return usage("--file is required") };
    if p.verb.is_some() {
        return usage("--verb is for deadline-check; every verb's deadline is the one default");
    }
    let (key, value) = (DEADLINE_PATH, ms.to_string());
    let (code, text) = run_check(&p, ms, dir, now);
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
    state_store::phases::child("spira-config", || std::process::Command::new(program).args(args).status()).map_or(CANNOT_TELL, |s| s.code().unwrap_or(CANNOT_TELL))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(verb: &str, answer_ms: Option<u64>) -> String {
        let attrs = answer_ms.map(|ms| format!(r#"{{"key":"{ANSWER_US}","value":{{"intValue":"{}"}}}}"#, ms * 1000)).unwrap_or_default();
        format!(r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{{"name":"{verb}","startTimeUnixNano":"1700000000000000000","attributes":[{attrs}]}}]}}]}}]}}"#)
    }

    fn cut_span(verb: &str, answer_ms: u64, cut_ms: u64) -> String {
        span(verb, Some(answer_ms)).replace(r#""attributes":["#, &format!(r#""attributes":[{{"key":"{CUT_MS}","value":{{"intValue":"{cut_ms}"}}}},"#))
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

    #[test_id(tlupzmv)]
    #[test]
    fn a_covered_call_over_the_limit_refuses_naming_its_verb_max_and_count() {
        assert_eq!(check(&lists(), 1000, &default_scope_with(&[])).unwrap_err(), "list max=1200ms over=1/3");
        assert!(check(&lists(), 1500, &default_scope_with(&[])).is_ok());
    }

    #[test_id(ti2nwr5)]
    #[test]
    fn a_verb_named_in_scope_is_judged_on_its_own() {
        assert!(check(&lists(), 1000, &Scope::Verb("show".into())).is_ok());
        assert_eq!(check(&lists(), 1000, &Scope::Verb("list".into())).unwrap_err(), "list max=1200ms over=1/3");
    }

    #[test_id(tyib7td)]
    #[test]
    fn a_covered_span_without_a_measured_answer_refuses_and_an_uncovered_one_is_ignored() {
        let t = text(&[("show", Some(5)), ("show", None), ("show", None)]);
        assert_eq!(check(&t, 1000, &default_scope_with(&[])).unwrap_err(), format!("spans: 2 span(s) without {ANSWER_US}"));
        let t = text(&[("show", Some(5)), ("watch", None)]);
        assert!(check(&t, 1000, &default_scope_with(&["watch"])).is_ok());
    }

    #[test_id(tq2hn64)]
    #[test]
    fn two_second_watch_spans_pass_the_default_scope_check_at_one_second() {
        let t = text(&[("watch", Some(2000)), ("watch", Some(2001)), ("show", Some(5))]);
        let Scope::Default { excluded } = default_scope() else { panic!() };
        assert!(check(&t, 1000, &Scope::Default { excluded }).is_ok());
    }

    #[test_id(tsd3qwl)]
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

    #[test_id(t97rr6a)]
    #[test]
    fn lower_with_a_failing_check_runs_no_spira_config_command() {
        let dir = day_dir(&lists());
        let mut calls = Vec::new();
        let (code, out) = deadline_lower(&args(&["--ms", "1000", "--file", "f.toml"]), dir.path(), now(), &mut |a| {
            calls.push(a.to_vec());
            0
        });
        assert_eq!((code, out.as_str()), (3, "list max=1200ms over=1/3"));
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test_id(t0eyuu3)]
    #[test]
    fn lower_with_a_passing_check_sets_the_key_through_spira_config() {
        let dir = day_dir(&lists());
        let mut calls = Vec::new();
        let (code, _) = deadline_lower(&args(&["--ms", "1500", "--file", "f.toml"]), dir.path(), now(), &mut |a| {
            calls.push(a.to_vec());
            0
        });
        assert_eq!(code, 0);
        assert_eq!(calls, vec![args(&["set", "spira.lc_request_deadline_ms", "1500", "f.toml"])]);
    }

    #[test_id(tqaez1w)]
    #[test]
    fn lower_for_a_verb_is_refused_because_no_verb_has_its_own_deadline() {
        let dir = day_dir(&lists());
        let mut calls = Vec::new();
        let (code, _) = deadline_lower(&args(&["--ms", "1500", "--verb", "list", "--file", "f.toml"]), dir.path(), now(), &mut |a| {
            calls.push(a.to_vec());
            0
        });
        assert_eq!(code, CANNOT_TELL);
        assert!(calls.is_empty());
    }

    #[test_id(tfhw6qg)]
    #[test]
    fn the_default_scope_leaves_out_work_sweeps_and_declared_verbs() {
        let Scope::Default { excluded } = default_scope() else { panic!() };
        for v in ["work", "watch", "reconcile-content"] {
            assert!(excluded.contains(v), "{v}");
        }
        assert!(!excluded.contains("show"));
    }

    #[test_id(tc7pxau)]
    #[test]
    fn a_cut_call_is_unmeasured_even_when_its_elapsed_time_passes() {
        let t = [cut_span("watch", 1000, 1000), cut_span("watch", 1000, 1000)].join("\n");
        assert_eq!(check(&t, 1000, &Scope::Verb("watch".into())).unwrap_err(), "watch cut=2/2 at 1000ms: answer time unknown");
        let mixed = format!("{}\n{}", text(&[("watch", Some(5))]), cut_span("watch", 1000, 1000));
        assert_eq!(check(&mixed, 1000, &Scope::Verb("watch".into())).unwrap_err(), "watch cut=1/2 at 1000ms: answer time unknown");
    }

    #[test_id(tbjcd97)]
    #[test]
    fn lower_over_a_cut_call_runs_no_spira_config_command() {
        let dir = day_dir(&cut_span("show", 1000, 1000));
        let mut calls = Vec::new();
        let (code, out) = deadline_lower(&args(&["--ms", "1000", "--file", "f.toml"]), dir.path(), now(), &mut |a| {
            calls.push(a.to_vec());
            0
        });
        assert_eq!((code, out.as_str()), (3, "show cut=1/1 at 1000ms: answer time unknown"));
        assert!(calls.is_empty());
    }

    #[test_id(txrgpk8)]
    #[test]
    fn configured_excludes_ms_and_verb_and_is_not_a_lowering() {
        let dir = day_dir("");
        for bad in [&["--configured", "--ms", "900"][..], &["--configured", "--verb", "list"][..]] {
            assert_eq!(deadline_check(&args(bad), dir.path(), now()).0, CANNOT_TELL);
        }
        let mut none = |_: &[String]| 0;
        assert_eq!(deadline_lower(&args(&["--configured", "--file", "f"]), dir.path(), now(), &mut none).0, CANNOT_TELL);
    }

    fn configured(body: &str) -> (i32, String) {
        let dir = day_dir(body);
        let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_LC_REQUEST_DEADLINE_MS", "1000")]);
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
        let read = spira_config::process::fresh(&["SPIRA_LC_REQUEST_DEADLINE_MS".to_string()]).unwrap();
        deadline_check_from(&args(&["--configured"]), dir.path(), now(), |k| read.get(k).cloned().ok_or_else(|| format!("{k} missing")))
    }

    #[test_id(tc725xe)]
    #[test]
    fn configured_judges_every_verb_at_the_one_default() {
        let (code, out) = configured(&text(&[("list", Some(900)), ("show", Some(20))]));
        assert_eq!(code, 0, "{out}");
        let (code, out) = configured(&text(&[("list", Some(2000)), ("show", Some(20))]));
        assert_eq!((code, out.as_str()), (3, "list max=2000ms over=1/1"));
    }
}
