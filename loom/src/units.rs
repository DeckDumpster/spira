//! `/units`: every systemd user unit, with its executions folded from `run/tsd/unit-run.jsonl`.
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
const LOG_LINES: usize = 5;
const LOG_LOOKUPS: usize = 20;
const STALE_INTERVALS: i64 = 3;
const MIN_RUNS_FOR_INTERVAL: usize = 3;

pub struct Sources {
    pub run: String,
    pub systemctl: String,
    pub duckdb: String,
    pub journalctl: String,
    pub extra_path: String,
    pub budget: Duration,
}

#[derive(Debug, Clone)]
pub struct Exec {
    pub unit: String,
    pub t: i64,
    pub result: String,
    pub duration_ms: Option<i64>,
    pub invocation: String,
}

#[derive(Debug, Clone, Default)]
pub struct Live {
    pub active: String,
    pub sub: String,
    pub unit_file: String,
    pub next: String,
    pub last_trigger: String,
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub unit: String,
    pub t: i64,
    pub result: String,
    pub log: Vec<String>,
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
    pub failure: Option<Failure>,
}

pub struct Snapshot {
    pub taken: Instant,
    pub now: i64,
    pub rows: Vec<Row>,
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
                "NextElapseUSecRealtime" => l.next = v.to_string(),
                "LastTriggerUSec" => l.last_trigger = v.to_string(),
                "Unit" => l.target = v.to_string(),
                _ => {}
            }
        }
        if !id.is_empty() {
            out.insert(id, l);
        }
    }
    out
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

pub fn build(now: i64, rules: &[Rule], names: &[String], live: &BTreeMap<String, Live>, execs: &BTreeMap<String, Vec<Exec>>, logs: &BTreeMap<String, Vec<String>>) -> Vec<Row> {
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
        groups.entry(label).or_insert((kind, Vec::new())).1.push(unit);
    }
    let mut rows: Vec<Row> = groups
        .into_iter()
        .map(|(unit, (kind, members))| {
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
            let stale = timer_active
                && match (interval_s, last_success) {
                    (_, None) => true,
                    (Some(i), Some(s)) => now - s > STALE_INTERVALS * i,
                    (None, Some(_)) => false,
                };
            let failure = ex7.iter().rev().find(|e| !is_ok(e)).map(|e| Failure { unit: e.unit.clone(), t: e.t, result: e.result.clone(), log: logs.get(&e.invocation).cloned().unwrap_or_default() });
            let own = if grouped {
                let lives: Vec<&Live> = members.iter().filter_map(|m| live.get(m)).collect();
                lives.iter().find(|l| l.active == "failed").or_else(|| lives.iter().find(|l| l.active == "active")).or(lives.first()).copied()
            } else if is_timer {
                timer.as_ref()
            } else {
                live.get(&first)
            };
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
                failure,
                unit,
            }
        })
        .collect();
    rows.sort_by(|a, b| (a.kind == "system", !a.stale, a.active != "failed", &a.unit).cmp(&(b.kind == "system", !b.stale, b.active != "failed", &b.unit)));
    rows
}

pub fn row_json(r: &Row) -> Value {
    json!({
        "unit": r.unit,
        "kind": r.kind,
        "instances": r.instances,
        "listed": r.listed,
        "state": { "active": r.active, "sub": r.sub, "unit_file": r.unit_file },
        "timer": r.timer.as_ref().map(|t| json!({ "active": t.active, "sub": t.sub, "unit_file": t.unit_file, "next": t.next, "last_trigger": t.last_trigger })),
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
        "last_failure": r.failure.as_ref().map(|f| json!({ "at": f.t, "unit": f.unit, "result": f.result, "log": f.log })),
    })
}

pub fn snapshot_json(s: &Snapshot) -> Value {
    json!({ "now": s.now, "errors": s.errors, "units": s.rows.iter().map(row_json).collect::<Vec<_>>() })
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

const CSS: &str = "<style>.tick{display:inline-block;width:4px;height:14px;margin-right:1px;background:#3aa57a}\
.tick.bad{background:#ff4d4f}.strip{white-space:nowrap;overflow:hidden;max-width:30rem}td.u{font-weight:600}\
tr.stale td{background:#1e1416}pre{margin:.2rem 0;white-space:pre-wrap;color:#9aa5b1}.st-failed{color:#ff6b6d}.st-active{color:#3aa57a}\
header h1{overflow-wrap:anywhere}\
@media(max-width:639px){body{overflow-x:hidden}table.units,table.units tbody{display:block}table.units tr.head{display:none}\
table.units tr{display:flex;flex-wrap:wrap;gap:.1rem .8rem;padding:.5rem .8rem;border-bottom:1px solid #1d232b}\
table.units td{display:block;padding:0;border:0;min-width:0}table.units td.u{flex:0 0 100%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
table.units td.st{flex:0 0 100%}table.units td.sr{flex:0 0 100%;order:5}table.units td.sr .strip{max-width:100%;display:flex;justify-content:flex-end}table.units .strip i{flex:none}\
table.units td.fl{flex:0 0 100%;order:6}table.units td.m::before{content:attr(data-l) \" \";color:#7d8896}}</style>";

pub fn render_page(s: &Snapshot) -> String {
    let shown: Vec<&Row> = s.rows.iter().filter(|r| r.kind != "system").collect();
    let system = s.rows.len() - shown.len();
    let stale = shown.iter().filter(|r| r.stale).count();
    let failed = shown.iter().filter(|r| r.active == "failed").count();
    let mut b = format!(
        "{CSS}<header><h1>Units <span class=dim>— {} unit families · <span class=\"{}\">{stale} stale</span> · <span class=\"{}\">{failed} failed</span> · <a href=/stuck>stuck</a></span></h1></header>",
        shown.len(),
        if stale > 0 { "red" } else { "dim" },
        if failed > 0 { "red" } else { "dim" },
    );
    for e in &s.errors {
        b.push_str(&format!("<p class=red style=\"padding:.4rem .8rem\">{}</p>", esc(e)));
    }
    b.push_str(TABLE_HEAD);
    for r in &shown {
        b.push_str(&row_html(s.now, r));
    }
    b.push_str("</table>");
    if system > 0 {
        b.push_str(&format!("<details><summary class=dim>{system} OS-owned unit families</summary>{TABLE_HEAD}"));
        for r in s.rows.iter().filter(|r| r.kind == "system") {
            b.push_str(&row_html(s.now, r));
        }
        b.push_str("</table></details>");
    }
    shell("Units", 60, &b)
}

const TABLE_HEAD: &str = "<table class=units><tr class=head><td>unit</td><td>state</td><td>24 h</td><td>24 h</td><td>7 d</td><td>p50 / p95 (7 d)</td><td>last failure</td></tr>";

fn row_html(now: i64, r: &Row) -> String {
    {
        let mut state = format!("<span class=\"st-{a}\">{a}</span> {sub}", a = esc(&r.active), sub = esc(&r.sub));
        if let Some(t) = &r.timer {
            state.push_str(&format!("<br><span class=dim>timer {} · next {} · last {}</span>", esc(&t.active), esc(&t.next), esc(&t.last_trigger)));
        }
        if r.stale {
            state.push_str(&format!("<br><span class=\"badge red\">stale — last success {}</span>", ago(now, r.last_success)));
        }
        let strip: String = r.strip.iter().map(|x| format!("<i class=\"tick{}\" title=\"{}\"></i>", if x == "success" { "" } else { " bad" }, esc(x))).collect();
        let fail = r.failure.as_ref().map_or("<span class=dim>none in 7 d</span>".to_string(), |f| {
            let inst = if r.kind == "plain" { String::new() } else { format!(" · {}", esc(&f.unit)) };
            format!("{} · {}{inst}<pre>{}</pre>", ago(now, Some(f.t)), esc(&f.result), esc(&f.log.join("\n")))
        });
        let name = if r.kind == "plain" { esc(&r.unit) } else { format!("{} <span class=dim>({} · {} instances)</span>", esc(&r.unit), r.kind, r.instances) };
        format!(
            "<tr class=\"{}\"><td class=u title=\"{}\">{}</td><td class=st>{state}</td><td class=sr><div class=strip>{strip}</div></td><td class=m data-l=\"24 h\">{} runs · {}</td><td class=m data-l=\"7 d\">{} runs · {}</td><td class=m data-l=\"p50/p95\">{} / {}</td><td class=fl>{fail}</td></tr>",
            if r.stale { "stale" } else { "" },
            esc(&r.unit),
            name,
            r.runs_24h,
            pct(r.ok_24h, r.runs_24h),
            r.runs_7d,
            pct(r.ok_7d, r.runs_7d),
            ms(r.p50_ms),
            ms(r.p95_ms),
        )
    }
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
    let mut args = strs(&["--user", "show", "--no-pager", "-p", "Id,ActiveState,SubState,UnitFileState,NextElapseUSecRealtime,LastTriggerUSec,Unit"]);
    args.extend(names.iter().cloned());
    let shown = run_cmd(&src.systemctl, &args, path, src.budget).await?;
    Ok((names, parse_show(&shown)))
}

async fn executions(src: &Sources, path: &str, now: i64) -> Result<Vec<Value>, String> {
    let file = tsd::family_path(std::path::Path::new(&src.run), "unit-run");
    if !file.exists() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT unit, t, result, duration_ms, invocation_id FROM \
         (SELECT unit, CAST(epoch(CAST(ts AS TIMESTAMPTZ)) AS BIGINT) AS t, result, duration_ms, invocation_id FROM read_ndjson('{}', columns={{unit:'VARCHAR',ts:'VARCHAR',result:'VARCHAR',duration_ms:'BIGINT',invocation_id:'VARCHAR'}})) \
         WHERE t >= {} ORDER BY t;",
        file.to_string_lossy().replace('\'', "''"),
        now - WEEK
    );
    let text = run_cmd(&src.duckdb, &["-json".to_string(), "-c".to_string(), sql], path, src.budget).await?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(text).map_err(|e| format!("{}: unparsable json: {e}", src.duckdb))
}

async fn invocation_logs(src: &Sources, path: &str, invocations: Vec<String>) -> BTreeMap<String, Vec<String>> {
    let mut set = tokio::task::JoinSet::new();
    for inv in invocations {
        let (jc, path, budget) = (src.journalctl.clone(), path.to_string(), src.budget);
        set.spawn(async move {
            let args = vec!["--user".to_string(), format!("_SYSTEMD_INVOCATION_ID={inv}"), "-n".into(), LOG_LINES.to_string(), "-o".into(), "cat".into(), "--no-pager".into()];
            let lines = run_cmd(&jc, &args, &path, budget).await.map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default();
            (inv, lines)
        });
    }
    let mut out = BTreeMap::new();
    while let Some(Ok((inv, lines))) = set.join_next().await {
        out.insert(inv, lines);
    }
    out
}

pub async fn take(src: &Sources, now: i64) -> Snapshot {
    let path = {
        let inherited = std::env::var("PATH").unwrap_or_default();
        if src.extra_path.is_empty() { inherited } else { format!("{}:{inherited}", src.extra_path) }
    };
    let (list, rows) = tokio::join!(unit_list(src, &path), executions(src, &path, now));
    let mut errors = Vec::new();
    let (names, live) = list.unwrap_or_else(|e| {
        errors.push(format!("unit list: {e}"));
        (Vec::new(), BTreeMap::new())
    });
    let rows = rows.unwrap_or_else(|e| {
        errors.push(format!("unit-run: {e}"));
        Vec::new()
    });
    let execs = parse_execs(&rows);
    let mut recent: Vec<(&str, i64)> = execs
        .values()
        .filter_map(|v| v.iter().rev().find(|e| e.result != "success" && !e.invocation.is_empty()).map(|e| (e.invocation.as_str(), e.t)))
        .collect();
    recent.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    let wanted: Vec<String> = recent.into_iter().take(LOG_LOOKUPS).map(|(i, _)| i.to_string()).collect();
    let logs = invocation_logs(src, &path, wanted).await;
    Snapshot { taken: Instant::now(), now, rows: build(now, &parse_families(DEFAULT_FAMILIES), &names, &live, &execs, &logs), errors }
}
