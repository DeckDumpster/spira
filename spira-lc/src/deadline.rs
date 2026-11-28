//! `deadline-check` and `deadline lower`: the store's request deadline comes down only when no
//! call in the window took longer than the new value to answer. Local verbs; no store call.

use std::collections::BTreeSet;

use spira_otel::vlogs::{Logs, LogsError};

use crate::{CANNOT_TELL, REFUSED};

const ANSWER_US: &str = "spira.answer_us";
const CUT_MS: &str = "spira.deadline_cut_ms";
const DEADLINE_PATH: &str = "spira.lc_request_deadline_ms";
/// `rpc.system` keeps out spira-lc's other log records, which share its service name.
const CALLS: &str = "service.name:spira-lc rpc.system:spira-lc";
const BY_VERB: &str = "rpc.method";

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

/// One verb's calls in the window. A cut call's answer time is the cutoff, not the call's, so it
/// counts as cut and toward neither `max_us` nor `over`.
struct VerbStats {
    verb: String,
    total: u64,
    max_us: u64,
    over: u64,
    cut: u64,
    cut_ms: u64,
    unmeasured: u64,
}

fn measure(logs: &Logs, ms: u64, hours: u64) -> Result<Vec<VerbStats>, LogsError> {
    let measured = format!("!{CUT_MS}:*");
    let max_us = format!("max({ANSWER_US}) if ({measured})");
    let over = format!("count() if ({measured} {ANSWER_US}:>{})", ms * 1000);
    let cut = format!("count() if ({CUT_MS}:*)");
    let cut_ms = format!("max({CUT_MS})");
    let unmeasured = format!("count() if ({measured} !{ANSWER_US}:*)");
    let stats = [("count()", "total"), (&max_us, "max_us"), (&over, "over"), (&cut, "cut"), (&cut_ms, "cut_ms"), (&unmeasured, "unmeasured")];
    let mut verbs = Vec::new();
    for group in logs.stats_by(&format!("{CALLS} _time:{hours}h"), &[BY_VERB], &stats)? {
        let (over, cut) = (group.require("over")?, group.require("cut")?);
        verbs.push(VerbStats {
            verb: group.keys.get(BY_VERB).cloned().unwrap_or_default(),
            total: group.require("total")?,
            max_us: if over > 0 { group.require("max_us")? } else { group.get("max_us").unwrap_or(0) },
            over,
            cut,
            cut_ms: if cut > 0 { group.require("cut_ms")? } else { 0 },
            unmeasured: group.require("unmeasured")?,
        });
    }
    verbs.sort_by(|a, b| a.verb.cmp(&b.verb));
    Ok(verbs)
}

/// `Err` (exit 3) naming each covered verb with a call over `ms`, cut by the request cutoff, or lacking `spira.answer_us`.
fn judge(verbs: &[VerbStats], ms: u64, scope: &Scope, hours: u64) -> Result<String, String> {
    let covered: Vec<&VerbStats> = verbs.iter().filter(|v| scope.covers(&v.verb)).collect();
    let mut refusals: Vec<String> = covered.iter().filter(|v| v.over > 0).map(|v| format!("{} max={}ms over={}/{}", v.verb, v.max_us.div_ceil(1000), v.over, v.total)).collect();
    refusals.extend(covered.iter().filter(|v| v.cut > 0).map(|v| format!("{} cut={}/{} at {}ms: answer time unknown", v.verb, v.cut, v.total, v.cut_ms)));
    refusals.extend(covered.iter().filter(|v| v.unmeasured > 0).map(|v| format!("{}: {} span(s) without {ANSWER_US}", v.verb, v.unmeasured)));
    if !refusals.is_empty() {
        return Err(refusals.join("\n"));
    }
    let calls: u64 = covered.iter().map(|v| v.total).sum();
    Ok(format!("no call to {} took over {ms} ms in {hours} h ({calls} calls)", scope.label()))
}

/// The window's verdict as read from VictoriaLogs. A window holding no call at all proves
/// nothing, so it cannot tell.
fn verdict(url: &str, ms: u64, scope: &Scope, hours: u64) -> (i32, String) {
    let verbs = match Logs::new(url).and_then(|logs| measure(&logs, ms, hours)) {
        Ok(verbs) => verbs,
        Err(e) => return (CANNOT_TELL, format!("spira-lc deadline: cannot read VictoriaLogs: {e}")),
    };
    if verbs.iter().all(|v| v.total == 0) {
        return (CANNOT_TELL, format!("spira-lc deadline: no spira-lc call recorded in VictoriaLogs in {hours} h"));
    }
    match judge(&verbs, ms, scope, hours) {
        Ok(line) => (0, line),
        Err(lines) => (REFUSED, lines),
    }
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

fn run_check(p: &Parsed, ms: u64, url: &str) -> (i32, String) {
    let scope = match &p.verb {
        Some(v) => Scope::Verb(v.clone()),
        None => default_scope(),
    };
    verdict(url, ms, &scope, p.hours)
}

/// The one configured default over every verb that gets it.
fn run_configured(p: &Parsed, d: &crate::serve::Deadlines, url: &str) -> (i32, String) {
    verdict(url, d.default.as_millis() as u64, &default_scope(), p.hours)
}

/// `args` after `deadline-check`.
pub(crate) fn deadline_check(args: &[String], url: &str) -> (i32, String) {
    deadline_check_from(args, url, spira_config::process::cfg)
}

fn deadline_check_from(args: &[String], url: &str, read: impl Fn(&str) -> Result<String, String>) -> (i32, String) {
    match parse(args) {
        Ok(p) if p.configured => match crate::serve::deadlines_from(read) {
            Ok(d) => run_configured(&p, &d, url),
            Err(e) => (CANNOT_TELL, e),
        },
        Ok(p) => run_check(&p, p.ms.unwrap_or_default(), url),
        Err(e) => usage(&e),
    }
}

/// `args` after `deadline lower`: runs the check, and only on a pass the `spira-config` command through `config`.
pub(crate) fn deadline_lower(args: &[String], url: &str, config: &mut dyn FnMut(&[String]) -> i32) -> (i32, String) {
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
    let (code, text) = run_check(&p, ms, url);
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
    state_store::phases::child("spira-config", || crate::proc::command(program).map_err(std::io::Error::other)?.args(args).status()).map_or(CANNOT_TELL, |s| s.code().unwrap_or(CANNOT_TELL))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::vlogs::fake::{FakeLogs, Script};

    /// One verb's row as VictoriaLogs answers it, omitting a `max` over no values.
    fn row(verb: &str, total: u64, max_ms: Option<u64>, over: u64, cut: Option<(u64, u64)>, unmeasured: u64) -> String {
        let mut r = serde_json::json!({"rpc.method": verb, "total": total.to_string(), "over": over.to_string(), "cut": cut.map_or(0, |c| c.0).to_string(), "unmeasured": unmeasured.to_string()});
        if let Some(ms) = max_ms {
            r["max_us"] = (ms * 1000).to_string().into();
        }
        if let Some((_, ms)) = cut {
            r["cut_ms"] = ms.to_string().into();
        }
        format!("{r}\n")
    }

    fn store(rows: &[String]) -> FakeLogs {
        answering(Script { status: 200, body: rows.concat(), hang: false })
    }

    fn answering(script: Script) -> FakeLogs {
        let f = FakeLogs::start();
        f.answer("stats by (rpc.method)", script);
        f
    }

    fn default_scope_with(slow: &[&str]) -> Scope {
        Scope::Default { excluded: slow.iter().map(|s| s.to_string()).collect() }
    }

    fn lists(over: u64) -> FakeLogs {
        store(&[row("list", 3, Some(1200), over, None, 0), row("show", 1, Some(20), 0, None, 0)])
    }

    fn check(f: &FakeLogs, ms: u64, scope: &Scope) -> (i32, String) {
        verdict(&f.url(), ms, scope, 24)
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn lower(f: &FakeLogs, a: &[&str]) -> (i32, String, Vec<Vec<String>>) {
        let mut calls = Vec::new();
        let (code, out) = deadline_lower(&args(a), &f.url(), &mut |a| {
            calls.push(a.to_vec());
            0
        });
        (code, out, calls)
    }

    #[test_id(tlupzmv)]
    #[test]
    fn a_covered_call_over_the_limit_refuses_naming_its_verb_max_and_count() {
        assert_eq!(check(&lists(1), 1000, &default_scope_with(&[])), (REFUSED, "list max=1200ms over=1/3".to_string()));
        assert_eq!(check(&lists(0), 1500, &default_scope_with(&[])).0, 0);
    }

    #[test_id(tjg1uk6)]
    #[test]
    fn a_window_with_every_call_under_the_limit_passes_naming_the_call_count() {
        let f = lists(0);
        assert_eq!(check(&f, 1500, &default_scope_with(&[])), (0, "no call to a default-deadline verb took over 1500 ms in 24 h (4 calls)".to_string()));
        let query = &f.queries()[0];
        for part in ["service.name:spira-lc rpc.system:spira-lc _time:24h | stats by (rpc.method) count() total", "count() if (!spira.deadline_cut_ms:* spira.answer_us:>1500000) over", "max(spira.answer_us) if (!spira.deadline_cut_ms:*) max_us"] {
            assert!(query.contains(part), "{part} not in {query}");
        }
    }

    #[test_id(tr00bf7)]
    #[test]
    fn a_window_with_no_recorded_call_cannot_tell_and_is_never_a_pass() {
        let (code, out) = check(&store(&[]), 1000, &default_scope_with(&[]));
        assert_eq!((code, out.as_str()), (CANNOT_TELL, "spira-lc deadline: no spira-lc call recorded in VictoriaLogs in 24 h"));
        let (code, _, calls) = lower(&store(&[]), &["--ms", "1000", "--file", "f.toml"]);
        assert_eq!(code, CANNOT_TELL);
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test_id(t8e1w56)]
    #[test]
    fn a_store_that_cannot_be_read_cannot_tell_and_names_victorialogs() {
        let refusing = answering(Script { status: 500, body: "boom".into(), hang: false });
        let (code, out) = check(&refusing, 1000, &default_scope_with(&[]));
        assert_eq!(code, CANNOT_TELL);
        assert!(out.contains("VictoriaLogs") && out.contains("status 500"), "{out}");
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", gone.local_addr().unwrap());
        drop(gone);
        let (code, out) = verdict(&url, 1000, &default_scope_with(&[]), 24);
        assert_eq!(code, CANNOT_TELL);
        assert!(out.contains("victorialogs-unreachable"), "{out}");
    }

    #[test_id(ti2nwr5)]
    #[test]
    fn a_verb_named_in_scope_is_judged_on_its_own() {
        assert_eq!(check(&lists(1), 1000, &Scope::Verb("show".into())).0, 0);
        assert_eq!(check(&lists(1), 1000, &Scope::Verb("list".into())), (REFUSED, "list max=1200ms over=1/3".to_string()));
    }

    #[test_id(tyib7td)]
    #[test]
    fn a_covered_span_without_a_measured_answer_refuses_and_an_uncovered_one_is_ignored() {
        let f = store(&[row("show", 3, Some(5), 0, None, 2)]);
        assert_eq!(check(&f, 1000, &default_scope_with(&[])), (REFUSED, format!("show: 2 span(s) without {ANSWER_US}")));
        let f = store(&[row("show", 1, Some(5), 0, None, 0), row("watch", 1, None, 0, None, 1)]);
        assert_eq!(check(&f, 1000, &default_scope_with(&["watch"])).0, 0);
    }

    #[test_id(tq2hn64)]
    #[test]
    fn two_second_watch_spans_pass_the_default_scope_check_at_one_second() {
        let f = store(&[row("watch", 2, Some(2001), 2, None, 0), row("show", 1, Some(5), 0, None, 0)]);
        assert_eq!(check(&f, 1000, &default_scope()).0, 0);
    }

    #[test_id(tsd3qwl)]
    #[test]
    fn spans_before_the_window_are_not_counted() {
        let f = lists(0);
        lower(&f, &["--ms", "1500", "--hours", "3", "--file", "f.toml"]);
        assert!(f.queries()[0].contains(" _time:3h |"), "{}", f.queries()[0]);
    }

    #[test_id(t97rr6a)]
    #[test]
    fn lower_with_a_failing_check_runs_no_spira_config_command() {
        let (code, out, calls) = lower(&lists(1), &["--ms", "1000", "--file", "f.toml"]);
        assert_eq!((code, out.as_str()), (3, "list max=1200ms over=1/3"));
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test_id(t0eyuu3)]
    #[test]
    fn lower_with_a_passing_check_sets_the_key_through_spira_config() {
        let (code, _, calls) = lower(&lists(0), &["--ms", "1500", "--file", "f.toml"]);
        assert_eq!(code, 0);
        assert_eq!(calls, vec![args(&["set", "spira.lc_request_deadline_ms", "1500", "f.toml"])]);
    }

    #[test_id(tqaez1w)]
    #[test]
    fn lower_for_a_verb_is_refused_because_no_verb_has_its_own_deadline() {
        let (code, _, calls) = lower(&lists(0), &["--ms", "1500", "--verb", "list", "--file", "f.toml"]);
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
        let f = store(&[row("watch", 2, None, 0, Some((2, 1000)), 0)]);
        assert_eq!(check(&f, 1000, &Scope::Verb("watch".into())), (REFUSED, "watch cut=2/2 at 1000ms: answer time unknown".to_string()));
        let f = store(&[row("watch", 2, Some(5), 0, Some((1, 1000)), 0)]);
        assert_eq!(check(&f, 1000, &Scope::Verb("watch".into())), (REFUSED, "watch cut=1/2 at 1000ms: answer time unknown".to_string()));
    }

    #[test_id(tbjcd97)]
    #[test]
    fn lower_over_a_cut_call_runs_no_spira_config_command() {
        let f = store(&[row("show", 1, None, 0, Some((1, 1000)), 0)]);
        let (code, out, calls) = lower(&f, &["--ms", "1000", "--file", "f.toml"]);
        assert_eq!((code, out.as_str()), (3, "show cut=1/1 at 1000ms: answer time unknown"));
        assert!(calls.is_empty());
    }

    #[test_id(txrgpk8)]
    #[test]
    fn configured_excludes_ms_and_verb_and_is_not_a_lowering() {
        let f = store(&[]);
        for bad in [&["--configured", "--ms", "900"][..], &["--configured", "--verb", "list"][..]] {
            assert_eq!(deadline_check(&args(bad), &f.url()).0, CANNOT_TELL);
        }
        assert_eq!(lower(&f, &["--configured", "--file", "f"]).0, CANNOT_TELL);
        assert!(f.queries().is_empty(), "refused before any read");
    }

    fn configured(f: &FakeLogs) -> (i32, String) {
        let dir = testkit::TempDir::new("lc-deadline");
        let toml = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_LC_REQUEST_DEADLINE_MS", "900")]);
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
        let read = spira_config::process::fresh(&["SPIRA_LC_REQUEST_DEADLINE_MS".to_string()]).unwrap();
        deadline_check_from(&args(&["--configured"]), &f.url(), |k| read.get(k).cloned().ok_or_else(|| format!("{k} missing")))
    }

    #[test_id(tc725xe)]
    #[test]
    fn configured_judges_every_verb_at_the_one_default() {
        let under = store(&[row("list", 1, Some(800), 0, None, 0), row("show", 1, Some(20), 0, None, 0)]);
        let (code, out) = configured(&under);
        assert_eq!(code, 0, "{out}");
        assert!(under.queries()[0].contains("spira.answer_us:>900000"), "judged at the configured value");
        let (code, out) = configured(&store(&[row("list", 1, Some(2000), 1, None, 0), row("show", 1, Some(20), 0, None, 0)]));
        assert_eq!((code, out.as_str()), (3, "list max=2000ms over=1/1"));
    }
}
