//! `/units`: every systemd user unit, with its executions folded from the `unit-run-dbus` rows in the metrics store.
//! The unit list comes from systemd itself, so a unit that never ran still has a row.

use crate::stuck::{dur, esc, shell};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

const DAY: i64 = 86_400;
const WEEK: i64 = 7 * DAY;
const STRIP_MAX: usize = 120;
const STALE_INTERVALS: i64 = 3;
const MIN_RUNS_FOR_INTERVAL: usize = 3;
const TRANSIENT_WINDOW: i64 = 2 * DAY;
const RESTART_FLAP: u32 = 5;

pub struct Sources {
    pub run: String,
    pub systemctl: String,
    pub metrics_endpoint: Option<String>,
    pub extra_path: String,
    pub budget: Duration,
    pub desired_dir: String,
    pub units_install: String,
}

#[derive(Debug, Clone)]
pub struct Exec {
    pub unit: String,
    pub t: i64,
    pub result: String,
    pub duration_ms: Option<i64>,
    pub exit_status: Option<i64>,
    pub invocation: String,
}

#[derive(Debug, Clone, Default)]
pub struct Live {
    pub active: String,
    pub sub: String,
    pub unit_file: String,
    pub next_at: Option<i64>,
    pub last_trigger: String,
    pub target: String,
    pub description: String,
    pub fragment: String,
    pub restarts: u32,
    pub exec_paths: Vec<String>,
    pub fixed_at: Option<i64>,
    pub active_since: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub unit: String,
    pub t: i64,
    pub result: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Failing,
    Late,
    NeverRan,
    Fixed,
    Running,
    Healthy,
    Disabled,
    OneOff,
}

impl Verdict {
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Failing => "failing",
            Verdict::Late => "late",
            Verdict::NeverRan => "never ran",
            Verdict::Fixed => "fixed, awaiting run",
            Verdict::Running => "running",
            Verdict::Healthy => "healthy",
            Verdict::Disabled => "disabled (config)",
            Verdict::OneOff => "one-off",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Verdict::Failing => "failing",
            Verdict::Late => "late",
            Verdict::NeverRan => "never",
            Verdict::Fixed => "fixed",
            Verdict::Running => "running",
            Verdict::Healthy => "healthy",
            Verdict::Disabled => "disabled",
            Verdict::OneOff => "oneoff",
        }
    }

    fn rank(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub unit: String,
    pub kind: &'static str,
    pub instances: usize,
    pub listed: bool,
    pub active: String,
    pub sub: String,
    pub unit_file: String,
    pub timer: Option<Live>,
    pub strip: Vec<String>,
    pub runs_24h: usize,
    pub ok_24h: usize,
    pub runs_7d: usize,
    pub ok_7d: usize,
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
    pub last_run: Option<i64>,
    pub last_success: Option<i64>,
    pub interval_s: Option<i64>,
    pub stale: bool,
    pub verdict: Verdict,
    pub failing_since: Option<i64>,
    pub failure: Option<Failure>,
    pub restarts: u32,
    pub service: bool,
    pub active_since: Option<i64>,
    pub last: Option<Exec>,
}

pub struct Snapshot {
    pub taken: Instant,
    pub now: i64,
    pub rows: Vec<Row>,
    pub foreign: usize,
    pub errors: Vec<String>,
}

fn is_ok(e: &Exec) -> bool {
    e.result == "success"
}

fn rate(ok: usize, n: usize) -> Value {
    if n == 0 { Value::Null } else { json!(ok as f64 / n as f64) }
}

fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

fn median_gap(times: &[i64]) -> Option<i64> {
    if times.len() < MIN_RUNS_FOR_INTERVAL {
        return None;
    }
    let mut gaps: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0).collect();
    gaps.sort_unstable();
    percentile(&gaps, 0.5)
}

pub fn parse_execs(rows: &[Value]) -> BTreeMap<String, Vec<Exec>> {
    let mut by: BTreeMap<String, Vec<Exec>> = BTreeMap::new();
    for r in rows {
        let (Some(unit), Some(t)) = (r["unit"].as_str(), r["t"].as_i64()) else { continue };
        by.entry(unit.to_string()).or_default().push(Exec {
            unit: unit.to_string(),
            t,
            result: r["result"].as_str().unwrap_or("unknown").to_string(),
            duration_ms: r["duration_ms"].as_i64(),
            exit_status: r["exit_status"].as_i64(),
            invocation: r["invocation_id"].as_str().unwrap_or("").to_string(),
        });
    }
    for v in by.values_mut() {
        v.sort_by_key(|e| e.t);
    }
    by
}

/// `systemctl show` output: blocks of `Key=Value` lines separated by a blank line.
pub fn parse_show(text: &str) -> BTreeMap<String, Live> {
    let mut out = BTreeMap::new();
    for block in text.split("\n\n") {
        let mut id = String::new();
        let mut l = Live::default();
        for line in block.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "Id" => id = v.to_string(),
                "ActiveState" => l.active = v.to_string(),
                "SubState" => l.sub = v.to_string(),
                "UnitFileState" => l.unit_file = v.to_string(),
                "LastTriggerUSec" => l.last_trigger = v.to_string(),
                "Unit" => l.target = v.to_string(),
                "Description" => l.description = v.to_string(),
                "FragmentPath" => l.fragment = v.to_string(),
                "ActiveEnterTimestamp" => l.active_since = v.strip_prefix('@').and_then(|n| n.parse().ok()),
                "NRestarts" => l.restarts = v.parse().unwrap_or(0),
                "ExecStart" => l.exec_paths = exec_paths(v),
                _ => {}
            }
        }
        if !id.is_empty() {
            out.insert(id, l);
        }
    }
    out
}

/// Absolute paths in the `argv[]=` of an `ExecStart={ path=… ; argv[]=… ; … }` value.
pub fn exec_paths(v: &str) -> Vec<String> {
    let Some((_, argv)) = v.split_once("argv[]=") else { return Vec::new() };
    argv.split(" ;").next().unwrap_or("").split_whitespace().filter(|t| t.starts_with('/')).map(str::to_string).collect()
}

fn names_spira(s: &str) -> bool {
    let s = s.to_lowercase();
    s.contains("spira") || s.contains("concierge")
}

fn os_owned(name: &str, l: &Live) -> bool {
    !names_spira(name) && !names_spira(&l.description) && ["/usr/", "/lib/"].iter().any(|p| l.fragment.starts_with(p))
}

fn off(file: &str) -> bool {
    matches!(file, "masked" | "masked-runtime" | "disabled")
}

pub struct Rule {
    kind: &'static str,
    glob: String,
    label: String,
}

pub const DEFAULT_FAMILIES: &str = include_str!("../families.conf");

pub fn parse_families(text: &str) -> Vec<Rule> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let kind = match w.next()? {
                "session" => "session",
                "system" => "system",
                "named" => "named",
                _ => return None,
            };
            let glob = w.next()?.to_string();
            let label = w.next().map_or_else(|| glob.clone(), str::to_string);
            Some(Rule { kind, glob, label })
        })
        .collect()
}

fn glob_match(g: &[u8], s: &[u8]) -> bool {
    match g.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| glob_match(rest, &s[i..])),
        Some((b'#', rest)) => s.first().is_some_and(u8::is_ascii_digit) && glob_match(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && glob_match(rest, &s[1..]),
    }
}

/// Whether the declared unit set puts this group in the page's scope.
fn declared_scope(d: &BTreeSet<String>, label: &str, members: &[String]) -> bool {
    d.contains(label)
        || members.iter().any(|m| {
            let timer = m.strip_suffix(".service").map(|b| format!("{b}.timer"));
            d.contains(m) || timer.is_some_and(|t| d.contains(&t))
        })
}

/// The family a unit name belongs to: a matching rule, else a template instance
/// (`name@x.service` → `name@.service`), else the unit itself.
pub fn family_of(rules: &[Rule], name: &str) -> (String, &'static str) {
    if let Some(r) = rules.iter().find(|r| glob_match(r.glob.as_bytes(), name.as_bytes())) {
        return (r.label.clone(), r.kind);
    }
    if let Some((base, rest)) = name.split_once('@') {
        if let Some((_, ext)) = rest.rsplit_once('.') {
            return (format!("{base}@.{ext}"), "template");
        }
    }
    (name.to_string(), "plain")
}

pub fn parse_unit_files(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|n| (n.ends_with(".service") || n.ends_with(".timer")) && !n.contains("@."))
        .map(str::to_string)
        .collect()
}

pub fn build(now: i64, rules: &[Rule], declared: Option<&BTreeSet<String>>, names: &[String], live: &BTreeMap<String, Live>, execs: &BTreeMap<String, Vec<Exec>>) -> (Vec<Row>, usize) {
    let listed: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let mut timer_of: BTreeMap<String, Live> = BTreeMap::new();
    let mut standalone = BTreeSet::new();
    for n in names.iter().filter(|n| n.ends_with(".timer")) {
        let l = live.get(n).cloned().unwrap_or_default();
        let target = if l.target.is_empty() { format!("{}.service", n.trim_end_matches(".timer")) } else { l.target.clone() };
        if listed.contains(target.as_str()) {
            timer_of.insert(target, l);
        } else {
            standalone.insert(n.clone());
        }
    }
    let mut units: BTreeSet<String> = names.iter().filter(|n| n.ends_with(".service")).cloned().collect();
    units.extend(standalone.iter().cloned());
    units.extend(execs.keys().cloned());
    let mut groups: BTreeMap<String, (&'static str, Vec<String>)> = BTreeMap::new();
    for unit in units {
        let (label, kind) = if standalone.contains(&unit) { (unit.clone(), "plain") } else { family_of(rules, &unit) };
        let kind = if kind == "plain" && live.get(&unit).is_some_and(|l| os_owned(&unit, l)) { "system" } else { kind };
        groups.entry(label).or_insert((kind, Vec::new())).1.push(unit);
    }
    let mut foreign = 0;
    let mut rows: Vec<Row> = groups
        .into_iter()
        .filter(|(label, (kind, members))| {
            let Some(d) = declared else { return true };
            let keep = matches!(*kind, "session" | "named") || declared_scope(d, label, members);
            if !keep {
                foreign += 1;
            }
            keep
        })
        .map(|(unit, (kind, members))| {
            let kind = if kind == "named" { "plain" } else { kind };
            let mut ex: Vec<Exec> = members.iter().flat_map(|m| execs.get(m).into_iter().flatten().cloned()).collect();
            ex.sort_by_key(|e| e.t);
            let grouped = kind != "plain";
            let first = members[0].clone();
            let is_listed = members.iter().any(|m| listed.contains(m.as_str()));
            let ex7: Vec<&Exec> = ex.iter().filter(|e| e.t >= now - WEEK).collect();
            let ex24: Vec<&Exec> = ex7.iter().copied().filter(|e| e.t >= now - DAY).collect();
            let mut durs: Vec<i64> = ex7.iter().filter_map(|e| e.duration_ms).collect();
            durs.sort_unstable();
            let times: Vec<i64> = ex7.iter().map(|e| e.t).collect();
            let last_success = ex7.iter().rev().find(|e| is_ok(e)).map(|e| e.t);
            let is_timer = !grouped && standalone.contains(&unit);
            let timer = if grouped { None } else if is_timer { live.get(&unit).cloned() } else { timer_of.get(&unit).cloned() };
            let interval_s = median_gap(&times);
            let timer_active = timer.as_ref().is_some_and(|t| t.active == "active");
            let stale = timer_active && matches!((interval_s, last_success), (Some(i), Some(s)) if now - s > STALE_INTERVALS * i);
            let failure = ex7.iter().rev().find(|e| !is_ok(e)).map(|e| Failure { unit: e.unit.clone(), t: e.t, result: e.result.clone() });
            let own = if grouped {
                let lives: Vec<&Live> = members.iter().filter_map(|m| live.get(m)).collect();
                lives.iter().find(|l| l.active == "failed").or_else(|| lives.iter().find(|l| l.active == "active")).or(lives.first()).copied()
            } else if is_timer {
                timer.as_ref()
            } else {
                live.get(&first)
            };
            let failed_now = own.is_some_and(|l| l.active == "failed");
            let latest_bad = ex7.last().is_some_and(|e| !is_ok(e));
            let running_now = own.is_some_and(|l| l.active == "active");
            let disabled = !grouped && !timer_active && !running_now && (off(own.map_or("", |l| l.unit_file.as_str())) || off(timer.as_ref().map_or("", |t| t.unit_file.as_str())));
            let service = !grouped && !is_timer && timer.is_none();
            let long_running = service && own.is_some_and(|l| l.active == "active" && l.sub == "running");
            let activating = own.is_some_and(|l| l.active == "activating");
            let stale = stale && !activating;
            let restarts = own.map_or(0, |l| l.restarts);
            let failed_at = ex7.last().filter(|e| !is_ok(e)).map(|e| e.t);
            let fixed = !grouped && matches!((own.and_then(|l| l.fixed_at), failed_at), (Some(f), Some(t)) if f > t);
            let verdict = if kind == "session" || (service && !is_listed && (failed_now || latest_bad)) {
                Verdict::OneOff
            } else if disabled {
                Verdict::Disabled
            } else if long_running {
                if restarts >= RESTART_FLAP { Verdict::Failing } else { Verdict::Healthy }
            } else if failed_now || latest_bad {
                if fixed { Verdict::Fixed } else { Verdict::Failing }
            } else if activating {
                Verdict::Running
            } else if stale {
                Verdict::Late
            } else if ex7.is_empty() {
                Verdict::NeverRan
            } else {
                Verdict::Healthy
            };
            let failing_since = if verdict == Verdict::Failing {
                let streak = ex7.iter().rev().take_while(|e| !is_ok(e)).last().map(|e| e.t);
                streak.or(failure.as_ref().map(|f| f.t))
            } else {
                None
            };
            let active_since = own.and_then(|l| l.active_since);
            let (active, sub, unit_file) = own.map(|l| (l.active.clone(), l.sub.clone(), l.unit_file.clone())).unwrap_or_default();
            Row {
                listed: is_listed,
                kind,
                instances: members.len(),
                active: if active.is_empty() && !is_listed { "not-listed".into() } else { active },
                sub,
                unit_file,
                timer,
                strip: ex24.iter().rev().take(STRIP_MAX).rev().map(|e| e.result.clone()).collect(),
                runs_24h: ex24.len(),
                ok_24h: ex24.iter().filter(|e| is_ok(e)).count(),
                runs_7d: ex7.len(),
                ok_7d: ex7.iter().filter(|e| is_ok(e)).count(),
                p50_ms: percentile(&durs, 0.5),
                p95_ms: percentile(&durs, 0.95),
                last_run: ex7.last().map(|e| e.t),
                last_success,
                interval_s,
                stale,
                verdict,
                failing_since,
                failure,
                restarts,
                service,
                active_since,
                last: ex7.last().map(|e| (*e).clone()),
                unit,
            }
        })
        .collect();
    rows.retain(|r| r.listed || r.kind == "session" || r.last_run.is_some_and(|t| t >= now - TRANSIENT_WINDOW));
    rows.sort_by(|a, b| (a.kind == "system", a.verdict.rank(), &a.unit).cmp(&(b.kind == "system", b.verdict.rank(), &b.unit)));
    (rows, foreign)
}

pub fn row_json(r: &Row) -> Value {
    json!({
        "unit": r.unit,
        "kind": r.kind,
        "instances": r.instances,
        "listed": r.listed,
        "state": { "active": r.active, "sub": r.sub, "unit_file": r.unit_file },
        "timer": r.timer.as_ref().map(|t| json!({ "active": t.active, "sub": t.sub, "unit_file": t.unit_file, "next": t.next_at, "last_trigger": t.last_trigger })),
        "strip": r.strip,
        "runs_24h": r.runs_24h,
        "success_rate_24h": rate(r.ok_24h, r.runs_24h),
        "runs_7d": r.runs_7d,
        "success_rate_7d": rate(r.ok_7d, r.runs_7d),
        "p50_ms": r.p50_ms,
        "p95_ms": r.p95_ms,
        "last_run": r.last_run,
        "last_success": r.last_success,
        "interval_s": r.interval_s,
        "stale": r.stale,
        "verdict": r.verdict.word(),
        "service": r.service,
        "timing": r.last.as_ref().map(|e| json!({ "result": e.result, "exit_status": e.exit_status, "duration_ms": e.duration_ms })),
        "failing_since": r.failing_since,
        "last_failure": r.failure.as_ref().map(|f| json!({ "at": f.t, "unit": f.unit, "result": f.result })),
    })
}

pub fn snapshot_json(s: &Snapshot) -> Value {
    json!({ "now": s.now, "foreign_families": s.foreign, "errors": s.errors, "units": s.rows.iter().map(row_json).collect::<Vec<_>>() })
}

fn pct(ok: usize, n: usize) -> String {
    if n == 0 { "–".into() } else { format!("{:.0}%", ok as f64 * 100.0 / n as f64) }
}

fn ms(v: Option<i64>) -> String {
    v.map_or("–".into(), |m| if m < 1000 { format!("{m}ms") } else { dur((m + 500) / 1000) })
}

fn ago(now: i64, t: Option<i64>) -> String {
    t.map_or("never".into(), |t| format!("{} ago", dur(now - t)))
}

pub fn plain(s: i64) -> String {
    let s = s.max(0);
    match s {
        0..=59 => format!("{s} sec"),
        60..=5399 => format!("{} min", (s + 30) / 60),
        5400..=172_799 => format!("{} h", (s + 1800) / 3600),
        _ => format!("{} d", (s + 43_200) / 86_400),
    }
}

fn service_timing(now: i64, r: &Row) -> String {
    if r.active == "active" {
        let up = r.active_since.map_or(String::new(), |t| format!(" for {}", plain(now - t)));
        return format!("up{up}");
    }
    match &r.last {
        None => "no runs in 7 d".into(),
        Some(e) => {
            let how = match (e.result.as_str(), e.exit_status) {
                ("success", _) => "exited cleanly".to_string(),
                ("exit-code", Some(c)) => format!("stopped (exit {c})"),
                (res, _) => format!("stopped ({res})"),
            };
            let after = e.duration_ms.map_or(String::new(), |d| format!(" after {}", plain(d / 1000)));
            format!("{how}{after}, {} ago", plain(now - e.t))
        }
    }
}

/// One sentence of schedule facts: cadence, last success, next run.
pub fn timing(now: i64, r: &Row) -> String {
    let mut parts = Vec::new();
    if r.service {
        return service_timing(now, r);
    }
    if r.verdict == Verdict::OneOff {
        parts.push(format!("{} runs in 7 d", r.runs_7d));
        parts.push(format!("{} failed", r.runs_7d - r.ok_7d));
        if let Some(t) = r.last_run {
            parts.push(format!("last {} ago", plain(now - t)));
        }
        return parts.join(" · ");
    }
    if r.verdict == Verdict::Running {
        parts.push(r.active_since.map_or("running".into(), |t| format!("running for {}", plain(now - t))));
    }
    if let Some(i) = r.interval_s.filter(|_| r.timer.is_some()) {
        parts.push(format!("every {}", plain(i)));
    }
    parts.push(match r.last_success {
        Some(t) if r.stale => format!("last ok {} ago — overdue", plain(now - t)),
        Some(t) => format!("last ok {} ago", plain(now - t)),
        None if r.runs_7d == 0 => "no runs in 7 d".into(),
        None => "no success in 7 d".into(),
    });
    if let Some(t) = &r.timer {
        parts.push(match (t.active == "active", t.next_at) {
            (false, _) => "timer not active".into(),
            (true, Some(n)) if n > now => format!("next in {}", plain(n - now)),
            (true, Some(n)) => format!("next was due {} ago", plain(now - n)),
            (true, None) => "next run pending".into(),
        });
    }
    parts.join(" · ")
}

const CSS: &str = "<style>.units-page{padding:.4rem .8rem}.unit{padding:.55rem 0;border-bottom:1px solid #1d232b}.unit .top{display:flex;gap:.6rem;align-items:baseline;flex-wrap:wrap}\
.unit .u{font-weight:600;overflow-wrap:anywhere}.unit .tm,.unit .why{margin-top:.15rem}.unit .why{color:#ff6b6d}\
.vd{display:inline-block;min-width:5.2rem;text-align:center;padding:0 .5rem;border-radius:3px;font-size:.85em;font-weight:600;color:#0b0e12}\
.v-failing .vd{background:#ff6b6d}.v-late .vd{background:#e0a030}.v-never .vd{background:#8a96a5}.v-healthy .vd{background:#3aa57a}.v-oneoff .vd,.v-disabled .vd,.v-fixed .vd{background:#4a5563;color:#d6dde6}\
.tick{display:inline-block;width:4px;height:14px;margin-right:1px;background:#3aa57a}.tick.bad{background:#ff4d4f}\
.strip{white-space:nowrap;overflow:hidden;max-width:100%;margin-top:.25rem}\
.unit details{margin-top:.25rem}.unit summary{color:#7d8896;cursor:pointer}.unit dl{display:grid;grid-template-columns:max-content 1fr;gap:.1rem .8rem;margin:.3rem 0}\
.unit dt{color:#7d8896}.unit dd{margin:0;overflow-wrap:anywhere}.legend{margin:.3rem 0}header h1{overflow-wrap:anywhere}\
.st-failed{color:#ff6b6d}.st-active{color:#3aa57a}</style>";

pub fn render_page(s: &Snapshot) -> String {
    let shown: Vec<&Row> = s.rows.iter().filter(|r| r.kind != "system").collect();
    let system = s.rows.len() - shown.len();
    let count = |v: Verdict| shown.iter().filter(|r| r.verdict == v).count();
    let (failing, late, never, healthy, oneoff) = (count(Verdict::Failing), count(Verdict::Late), count(Verdict::NeverRan), count(Verdict::Healthy), count(Verdict::OneOff));
    let (fixed, disabled) = (count(Verdict::Fixed), count(Verdict::Disabled));
    let tone = |n: usize| if n > 0 { "red" } else { "dim" };
    let mut b = format!(
        "{CSS}<header><h1>Units <span class=dim>— <span class=\"{}\">{failing} failing</span> · <span class=\"{}\">{late} late</span> · <span class=dim>{never} never ran</span> · <span class=dim>{healthy} healthy</span> · <a href=/stuck>stuck</a></span></h1></header><div class=units-page>",
        tone(failing),
        tone(late),
    );
    for e in &s.errors {
        b.push_str(&format!("<p class=red>{}</p>", esc(e)));
    }
    b.push_str("<p class=\"dim legend\"><b>running</b>: a pass in progress, never late · <b>failing</b>: its last run failed or systemd marks it failed · <b>late</b>: its timer should have run it by now · <b>never ran</b>: no run in 7 d · <b>healthy</b>: running to schedule, or a service that is up and not restarting; a service without a timer is judged on being active, with its uptime or exit status. A failure older than a fix to the unit's script reads <b>fixed, awaiting run</b>; a unit disabled or masked is not judged. Open <i>details</i> for systemd's own state and the 24 h / 7 d success rates.</p>");
    let group = |v: Verdict| shown.iter().filter(move |r| r.verdict == v);
    let running = count(Verdict::Running);
    for r in shown.iter().filter(|r| matches!(r.verdict, Verdict::Failing | Verdict::Late | Verdict::NeverRan)) {
        b.push_str(&row_html(s.now, r));
    }
    if failing + late + never == 0 {
        b.push_str("<p class=dim>nothing needs attention</p>");
    }
    if running > 0 {
        b.push_str(&format!("<details open><summary class=dim>{running} running</summary>"));
        group(Verdict::Running).for_each(|r| b.push_str(&row_html(s.now, r)));
        b.push_str("</details>");
    }
    if healthy > 0 {
        b.push_str(&format!("<details><summary class=dim>{healthy} healthy</summary>"));
        group(Verdict::Healthy).for_each(|r| b.push_str(&row_html(s.now, r)));
        b.push_str("</details>");
    }
    for (n, v, label) in [(fixed, Verdict::Fixed, "fixed, awaiting their next run"), (disabled, Verdict::Disabled, "disabled by config or masked")] {
        if n > 0 {
            b.push_str(&format!("<details><summary class=dim>{n} {label}</summary>"));
            group(v).for_each(|r| b.push_str(&row_html(s.now, r)));
            b.push_str("</details>");
        }
    }
    if oneoff > 0 {
        b.push_str(&format!("<details><summary class=dim>{oneoff} one-off unit families (per-run sessions; not judged)</summary>"));
        group(Verdict::OneOff).for_each(|r| b.push_str(&row_html(s.now, r)));
        b.push_str("</details>");
    }
    if s.foreign > 0 {
        b.push_str(&format!("<p class=dim>{} non-Spira unit families (not in the declared unit set; not judged)</p>", s.foreign));
    }
    if system > 0 {
        b.push_str(&format!("<details><summary class=dim>{system} OS-owned unit families</summary>"));
        for r in s.rows.iter().filter(|r| r.kind == "system") {
            b.push_str(&row_html(s.now, r));
        }
        b.push_str("</details>");
    }
    b.push_str("</div>");
    shell("units", "Units", 60, &b)
}

fn row_html(now: i64, r: &Row) -> String {
    let strip: String = r.strip.iter().map(|x| format!("<i class=\"tick{}\" title=\"{}\"></i>", if x == "success" { "" } else { " bad" }, esc(x))).collect();
    let name = if r.kind == "plain" { esc(&r.unit) } else { format!("{} <span class=dim>({} · {} instances)</span>", esc(&r.unit), r.kind, r.instances) };
    let why = match (r.verdict, &r.failure) {
        (Verdict::Failing, f) => {
            let since = r.failing_since.map_or("failing now".to_string(), |t| format!("failing since {} ago", plain(now - t)));
            let err = f.as_ref().map_or(String::new(), |f| format!(" · last error: {}", esc(&f.result)));
            let err = if r.restarts >= RESTART_FLAP { format!("{err} · restarted {} times since it started", r.restarts) } else { err };
            format!("<div class=why>{since}{err}</div>")
        }
        _ => String::new(),
    };
    let last_failure = r.failure.as_ref().map_or("none in 7 d".to_string(), |f| {
        let inst = if r.kind == "plain" { String::new() } else { format!(" · {}", esc(&f.unit)) };
        format!("{} · {}{inst}", ago(now, Some(f.t)), esc(&f.result))
    });
    let timer = r.timer.as_ref().map_or(String::new(), |t| {
        format!("<dt>timer</dt><dd>{} {} · last trigger {}</dd>", esc(&t.active), esc(&t.sub), esc(&t.last_trigger))
    });
    format!(
        "<div class=\"unit v-{cls}\"><div class=top><span class=vd>{word}</span><span class=u title=\"{full}\">{name}</span></div>\
<div class=\"tm dim\">{tm}</div>{why}<div class=strip>{strip}</div>\
<details><summary>details</summary><dl><dt>systemd</dt><dd><span class=\"st-{a}\">{a}</span> {sub} · {uf}</dd>{timer}\
<dt>24 h</dt><dd>{r24} runs · {p24}</dd><dt>7 d</dt><dd>{r7} runs · {p7}</dd><dt>p50 / p95 (7 d)</dt><dd>{p50} / {p95}</dd><dt>last failure</dt><dd>{last_failure}</dd></dl></details></div>",
        cls = r.verdict.class(),
        word = r.verdict.word(),
        full = esc(&r.unit),
        tm = esc(&timing(now, r)),
        a = esc(&r.active),
        sub = esc(&r.sub),
        uf = esc(&r.unit_file),
        r24 = r.runs_24h,
        p24 = pct(r.ok_24h, r.runs_24h),
        r7 = r.runs_7d,
        p7 = pct(r.ok_7d, r.runs_7d),
        p50 = ms(r.p50_ms),
        p95 = ms(r.p95_ms),
    )
}

async fn run_cmd(program: &str, args: &[String], path: &str, budget: Duration) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.args(args).env("PATH", path).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let out = tokio::time::timeout(budget, cmd.output())
        .await
        .map_err(|_| format!("{program} over its {} ms budget", budget.as_millis()))?
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{program} exited {}: {}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).trim().chars().take(200).collect::<String>()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn strs(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

async fn unit_list(src: &Sources, path: &str) -> Result<(Vec<String>, BTreeMap<String, Live>), String> {
    let files = run_cmd(&src.systemctl, &strs(&["--user", "list-unit-files", "--type=service,timer", "--no-legend", "--no-pager", "--plain"]), path, src.budget).await?;
    let names = parse_unit_files(&files);
    if names.is_empty() {
        return Ok((names, BTreeMap::new()));
    }
    let mut args = strs(&["--user", "show", "--no-pager", "--timestamp=unix", "-p", "Id,ActiveState,ActiveEnterTimestamp,SubState,UnitFileState,LastTriggerUSec,Unit,Description,FragmentPath,NRestarts,ExecStart"]);
    args.extend(names.iter().cloned());
    let shown = run_cmd(&src.systemctl, &args, path, src.budget).await?;
    let mut live = parse_show(&shown);
    for l in live.values_mut() {
        l.fixed_at = l.exec_paths.iter().chain(std::iter::once(&l.fragment)).filter_map(|p| std::fs::metadata(p).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).max();
    }
    Ok((names, live))
}

async fn timer_nexts(src: &Sources, path: &str) -> Result<BTreeMap<String, i64>, String> {
    let text = run_cmd(&src.systemctl, &strs(&["--user", "list-timers", "--all", "--output=json", "--no-pager"]), path, src.budget).await?;
    parse_timer_nexts(&text)
}

/// `list-timers --output=json`: `next` is microseconds since the epoch, 0 or absent when no elapse is scheduled.
pub fn parse_timer_nexts(text: &str) -> Result<BTreeMap<String, i64>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(BTreeMap::new());
    }
    let v: Value = serde_json::from_str(text).map_err(|e| format!("list-timers: unparsable json: {e}"))?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| Some((t["unit"].as_str()?.to_string(), t["next"].as_i64().filter(|n| *n > 0)? / 1_000_000)))
        .collect())
}

/// The week of `unit-run-dbus` executions, kept in memory and topped up from the store in
/// bounded pieces, so no request exports the window.
#[derive(Default)]
pub struct History {
    rows: BTreeMap<(i64, String, String), Value>,
    cursor: i64,
    pub synced: bool,
    pub error: Option<String>,
}

impl History {
    pub fn rows(&self) -> Vec<Value> {
        self.rows.values().cloned().collect()
    }

    /// What the page says about the history itself, if it is not whole.
    pub fn problem(&self) -> Option<String> {
        match (&self.error, self.synced) {
            (Some(e), _) => Some(e.clone()),
            (None, false) => Some("history still loading from the metrics store".into()),
            (None, true) => None,
        }
    }
}

const SYNC_PIECE: i64 = DAY;
const SYNC_OVERLAP: i64 = 600;

/// Reads the next pieces of the window into `hist`, committing each piece as it lands. A failure
/// is recorded, never cached past the next sync, and leaves the rows already read.
pub async fn sync_history(endpoint: Option<String>, hist: &tokio::sync::Mutex<History>, now: i64) {
    let from = { hist.lock().await.cursor };
    let mut from = if from == 0 { now - WEEK } else { from - SYNC_OVERLAP }.max(now - WEEK).max(0);
    loop {
        let to = (from + SYNC_PIECE).min(now);
        let ep = endpoint.clone();
        let piece = tokio::task::spawn_blocking(move || tsd::store::Reader::new(ep.as_deref()).and_then(|r| r.rows_between("unit-run-dbus", from as u64, to as u64)).map_err(|e| e.to_string()))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
        let mut h = hist.lock().await;
        match piece {
            Err(e) => {
                h.error = Some(format!("unit-run: {e}"));
                return;
            }
            Ok(rows) => {
                for r in rows {
                    let (Some(unit), Some(t)) = (r["unit"].as_str(), tsd::query::ts_of(&r)) else { continue };
                    let t = t as i64;
                    let inv = r["invocation_id"].as_str().unwrap_or("").to_string();
                    h.rows.insert((t, unit.to_string(), inv.clone()), json!({"unit": unit, "t": t, "result": r["result"], "duration_ms": r["duration_ms"], "invocation_id": inv}));
                }
                h.cursor = to;
                if to >= now {
                    let cutoff = now - WEEK;
                    h.rows.retain(|k, _| k.0 >= cutoff);
                    h.synced = true;
                    h.error = h.rows.is_empty().then(|| format!("unit-run: metrics-store-no-series: no unit-run-dbus row since {}; refusing to report an empty window as all-clear", tsd::iso_utc(cutoff.max(0) as u64)));
                    return;
                }
                from = to;
            }
        }
    }
}

/// The unit set the reconciler declares: the Composite's Units resource, else the installer's enable manifest.
async fn declared_units(src: &Sources, path: &str) -> Result<BTreeSet<String>, String> {
    use spira_desired_state::resource::{parse_resource, KindSpec};
    let composite = spira_desired_state::store::FsStore::new(&src.desired_dir).read_current()?;
    let from_composite: BTreeSet<String> = composite
        .iter()
        .flat_map(|c| c.resources.values())
        .filter_map(|r| parse_resource(&r.raw).ok())
        .filter_map(|r| match r.spec {
            KindSpec::Units(u) => Some(u.units.into_iter().map(|u| u.name)),
            _ => None,
        })
        .flatten()
        .collect();
    if !from_composite.is_empty() {
        return Ok(from_composite);
    }
    let text = run_cmd(&src.units_install, &strs(&["--list-enable"]), path, src.budget).await?;
    let names: BTreeSet<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
    if names.is_empty() {
        return Err("no units declared".into());
    }
    Ok(names)
}

pub async fn take(src: &Sources, now: i64, history: &History) -> Snapshot {
    let path = {
        let inherited = std::env::var("PATH").unwrap_or_default();
        if src.extra_path.is_empty() { inherited } else { format!("{}:{inherited}", src.extra_path) }
    };
    let (list, nexts, declared) = tokio::join!(unit_list(src, &path), timer_nexts(src, &path), declared_units(src, &path));
    let mut errors = Vec::new();
    let declared = declared.map_err(|e| errors.push(format!("declared units: {e}"))).ok();
    let (names, mut live) = list.unwrap_or_else(|e| {
        errors.push(format!("unit list: {e}"));
        (Vec::new(), BTreeMap::new())
    });
    match nexts {
        Ok(n) => n.into_iter().for_each(|(timer, at)| {
            if let Some(l) = live.get_mut(&timer) {
                l.next_at = Some(at);
            }
        }),
        Err(e) => errors.push(format!("timers: {e}")),
    }
    errors.extend(history.problem());
    let execs = parse_execs(&history.rows());
    let (rows, foreign) = build(now, &parse_families(DEFAULT_FAMILIES), declared.as_ref(), &names, &live, &execs);
    Snapshot { taken: Instant::now(), now, rows, foreign, errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn ex(unit: &str, ago: i64, result: &str) -> Exec {
        Exec { unit: unit.into(), t: NOW - ago, result: result.into(), duration_ms: Some(100), exit_status: None, invocation: String::new() }
    }

    fn live(active: &str, sub: &str) -> Live {
        Live { active: active.into(), sub: sub.into(), ..Default::default() }
    }

    fn timer(next_in: Option<i64>) -> Live {
        Live { next_at: next_in.map(|n| NOW + n), target: String::new(), ..live("active", "waiting") }
    }

    fn rows(names: &[&str], lives: &[(&str, Live)], runs: Vec<Exec>) -> Vec<Row> {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let live: BTreeMap<String, Live> = lives.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        scoped(None, &names, &live, runs).0
    }

    fn scoped(declared: Option<&[&str]>, names: &[String], live: &BTreeMap<String, Live>, runs: Vec<Exec>) -> (Vec<Row>, usize) {
        let declared: Option<BTreeSet<String>> = declared.map(|d| d.iter().map(|s| s.to_string()).collect());
        let execs = parse_execs(&runs.iter().map(|e| json!({"unit": e.unit, "t": e.t, "result": e.result, "duration_ms": e.duration_ms, "exit_status": e.exit_status})).collect::<Vec<_>>());
        build(NOW, &parse_families(DEFAULT_FAMILIES), declared.as_ref(), names, live, &execs)
    }

    fn every_5_min(unit: &str, last_ago: i64, n: i64) -> Vec<Exec> {
        (0..n).map(|i| ex(unit, last_ago + i * 300, "success")).collect()
    }

    fn by<'a>(rs: &'a [Row], name: &str) -> &'a Row {
        rs.iter().find(|r| r.unit == name).unwrap_or_else(|| panic!("{name} missing"))
    }

    fn fixture() -> Vec<Row> {
        let mut runs = every_5_min("between.service", 540, 20);
        runs.extend(every_5_min("overdue.service", 6 * 3600, 20));
        runs.extend([ex("broken.service", 1800, "success"), ex("broken.service", 900, "exit-code"), ex("broken.service", 600, "exit-code")]);
        runs.extend(every_5_min("old-timeout.service", 120_000, 1));
        runs.push(ex("old-timeout.service", 119_000, "timeout"));
        runs.extend(every_5_min("old-timeout.service", 60, 1));
        runs.push(ex("concierge-1799990000.service", 3 * DAY, "exit-code"));
        rows(
            &["between.service", "between.timer", "overdue.service", "overdue.timer", "broken.service", "never.service", "never.timer", "old-timeout.service", "concierge-1799990000.service"],
            &[
                ("between.service", live("inactive", "dead")),
                ("between.timer", timer(Some(60))),
                ("overdue.service", live("inactive", "dead")),
                ("overdue.timer", timer(Some(10))),
                ("broken.service", live("failed", "failed")),
                ("never.service", live("inactive", "dead")),
                ("never.timer", timer(Some(30))),
                ("old-timeout.service", live("inactive", "dead")),
                ("concierge-1799990000.service", live("failed", "failed")),
            ],
            runs,
        )
    }

    #[test]
    fn verdicts_and_attention_order_for_each_case() {
        let rs = fixture();
        let v = |n: &str| by(&rs, n).verdict;
        assert_eq!(v("between.service"), Verdict::Healthy, "a timer job between runs is healthy, not 'inactive dead'");
        assert_eq!(v("broken.service"), Verdict::Failing);
        assert_eq!(v("overdue.service"), Verdict::Late);
        assert_eq!(v("never.service"), Verdict::NeverRan);
        assert_eq!(v("old-timeout.service"), Verdict::Healthy, "an old failure in a recovered job is not a current problem");
        assert_eq!(v("concierge-run"), Verdict::OneOff, "a past transient failure is not a failure");
        let order: Vec<&str> = rs.iter().map(|r| r.verdict.word()).collect();
        let mut sorted = order.clone();
        sorted.sort_by_key(|w| ["failing", "late", "never ran", "fixed, awaiting run", "running", "healthy", "disabled (config)", "one-off"].iter().position(|x| x == w));
        assert_eq!(order, sorted, "failing, late, never ran, healthy, one-off");
        assert_eq!(rs[0].unit, "broken.service");
    }

    fn lv(active: &str, sub: &str, f: impl FnOnce(&mut Live)) -> Live {
        let mut l = live(active, sub);
        f(&mut l);
        l
    }

    #[test]
    fn each_noise_category_maps_to_its_verdict() {
        let mut runs = vec![ex("old-script.service", 2 * DAY, "exit-code")];
        runs.push(ex("gone-transient.service", 3 * DAY, "exit-code"));
        runs.push(ex("recent-transient.service", DAY / 2, "exit-code"));
        runs.push(ex("loop.service", 2 * DAY, "exit-code"));
        runs.push(ex("flappy.service", 2 * DAY, "success"));
        let rs = rows(
            &["spira-a.service", "snapd.service", "vendor.service", "old-script.service", "loop.service", "flappy.service", "off.service", "masked.service", "spira-vendor.service"],
            &[
                ("vendor.service", lv("inactive", "dead", |l| l.fragment = "/usr/lib/systemd/user/vendor.service".into())),
                ("spira-vendor.service", lv("inactive", "dead", |l| l.fragment = "/usr/lib/systemd/user/x.service".into())),
                ("old-script.service", lv("failed", "failed", |l| l.fixed_at = Some(NOW - DAY))),
                ("loop.service", lv("active", "running", |_| {})),
                ("flappy.service", lv("active", "running", |l| l.restarts = RESTART_FLAP)),
                ("off.service", lv("inactive", "dead", |l| l.unit_file = "disabled".into())),
                ("masked.service", lv("inactive", "dead", |l| l.unit_file = "masked".into())),
            ],
            runs,
        );
        let v = |n: &str| by(&rs, n).verdict;
        assert_eq!(by(&rs, "vendor.service").kind, "system", "an OS-owned unit is not Spira's");
        assert_eq!(by(&rs, "spira-vendor.service").kind, "plain", "a unit that names itself Spira's stays");
        assert_eq!(by(&rs, "snapd").kind, "system");
        assert_eq!(v("old-script.service"), Verdict::Fixed);
        assert_eq!(v("loop.service"), Verdict::Healthy, "an active service is judged by state, not its old exit");
        assert_eq!(v("flappy.service"), Verdict::Failing, "an active service that keeps restarting is failing");
        assert_eq!(v("off.service"), Verdict::Disabled);
        assert_eq!(v("masked.service"), Verdict::Disabled);
        assert_eq!(v("spira-a.service"), Verdict::NeverRan);
        assert!(rs.iter().all(|r| r.unit != "gone-transient.service"), "a transient that no longer exists ages out");
        assert!(rs.iter().any(|r| r.unit == "recent-transient.service"), "inside the window it still shows");
        let page = render_page(&Snapshot { taken: Instant::now(), now: NOW, rows: rs, foreign: 0, errors: vec![] });
        assert!(page.contains("1 fixed, awaiting their next run") && page.contains("2 disabled by config or masked"), "{page}");
    }

    #[test]
    fn a_failure_newer_than_the_fix_still_fails() {
        let rs = rows(&["x.service"], &[("x.service", lv("failed", "failed", |l| l.fixed_at = Some(NOW - 3 * DAY)))], vec![ex("x.service", DAY, "exit-code")]);
        assert_eq!(by(&rs, "x.service").verdict, Verdict::Failing);
    }

    #[test]
    fn show_parses_description_restarts_and_script_paths() {
        let m = parse_show("Id=a.service\nDescription=Spira thing\nNRestarts=4\nFragmentPath=/h/a.service\nExecStart={ path=/bin/bash ; argv[]=/bin/bash /h/run.sh -x ; ignore_errors=no }\n");
        let l = &m["a.service"];
        assert_eq!((l.description.as_str(), l.restarts, l.fragment.as_str()), ("Spira thing", 4, "/h/a.service"));
        assert_eq!(l.exec_paths, vec!["/bin/bash", "/h/run.sh"]);
    }

    #[test]
    fn failing_rows_say_since_when_and_timing_is_plain() {
        let rs = fixture();
        let broken = by(&rs, "broken.service");
        assert_eq!(broken.failing_since, Some(NOW - 900), "the start of the unbroken failure streak");
        let page = render_page(&Snapshot { taken: Instant::now(), now: NOW, rows: rs.clone(), foreign: 0, errors: vec![] });
        assert!(page.contains("failing since 15 min ago · last error: exit-code"), "{page}");
        assert_eq!(timing(NOW, by(&rs, "between.service")), "every 5 min · last ok 9 min ago · next in 1 min");
        assert!(timing(NOW, by(&rs, "overdue.service")).contains("overdue"));
        assert_eq!(timing(NOW, by(&rs, "never.service")), "no runs in 7 d · next in 30 sec");
        assert!(page.contains("<summary class=dim>"), "healthy rows are collapsed behind a count: {page}");
        assert!(page.contains("2 healthy"), "{page}");
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    fn lives(l: &[(&str, Live)]) -> BTreeMap<String, Live> {
        l.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn a_unit_outside_the_declared_set_is_not_on_the_page() {
        let snap = "app-snap\\x2duserd\\x2dautostart@gnome.service";
        let (rs, foreign) = scoped(
            Some(&["spira-a.service", "spira-a.timer"]),
            &names(&["spira-a.service", "spira-a.timer", snap]),
            &lives(&[("spira-a.service", live("inactive", "dead")), ("spira-a.timer", timer(Some(5))), (snap, live("inactive", "dead"))]),
            vec![],
        );
        assert!(rs.iter().all(|r| !r.unit.contains("snap")), "{:?}", rs.iter().map(|r| &r.unit).collect::<Vec<_>>());
        assert_eq!((foreign, by(&rs, "spira-a.service").verdict), (1, Verdict::NeverRan));
    }

    #[test]
    fn a_service_that_ran_for_days_and_stopped_reads_as_one_long_run() {
        let mut run = ex("concierge-rounds-loop.service", 3600, "exit-code");
        run.duration_ms = Some(3 * DAY * 1000);
        run.exit_status = Some(1);
        let (rs, _) = scoped(Some(&["spira-a.service"]), &names(&["spira-a.service"]), &lives(&[]), vec![run]);
        let r = by(&rs, "concierge-rounds-loop");
        assert_eq!(r.verdict, Verdict::OneOff, "a transient that stopped is not judged by a success rate");
        let t = timing(NOW, r);
        assert!(t.starts_with("stopped (exit 1) after 3 d"), "{t}");
    }

    #[test]
    fn a_pass_in_progress_is_running_not_late() {
        let mut runs = every_5_min("pass.service", 1500, 20);
        runs.reverse();
        let l = lives(&[("pass.service", Live { active_since: Some(NOW - 600), ..live("activating", "start") }), ("pass.timer", timer(Some(30)))]);
        let (rs, _) = scoped(Some(&["pass.service", "pass.timer"]), &names(&["pass.service", "pass.timer"]), &l, runs);
        let r = by(&rs, "pass.service");
        assert_eq!(r.verdict, Verdict::Running);
        assert!(timing(NOW, r).contains("running for 10 min"), "{}", timing(NOW, r));
        let idle = lives(&[("pass.service", live("inactive", "dead")), ("pass.timer", timer(Some(30)))]);
        let (rs, _) = scoped(Some(&["pass.service", "pass.timer"]), &names(&["pass.service", "pass.timer"]), &idle, every_5_min("pass.service", 1500, 20));
        assert_eq!(by(&rs, "pass.service").verdict, Verdict::Late, "idle and overdue is still late");
    }

    #[test]
    fn next_is_read_for_monotonic_and_calendar_timers_alike() {
        let json = r#"[{"next":1800000060000000,"left":1,"unit":"a.timer","activates":"a.service"},{"next":0,"unit":"b.timer"},{"unit":"c.timer"}]"#;
        let m = parse_timer_nexts(json).unwrap();
        assert_eq!(m.get("a.timer"), Some(&1_800_000_060));
        assert!(!m.contains_key("b.timer") && !m.contains_key("c.timer"));
        assert!(parse_timer_nexts("not json").is_err());
    }
}
