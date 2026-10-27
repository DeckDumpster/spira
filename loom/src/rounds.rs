//! `/rounds` and `/rounds/spira/<batch>`: every round, and for a VM round its members, passes and
//! the suite table parsed from the pass log under `<run>/vm-results/`. Batches come from
//! `spira-lc list --batches`; the log is read-only and cached by size and mtime.

use crate::shell::shell;
use crate::stuck::{dur, esc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

pub const WINDOW_S: i64 = 86_400;
const TAIL_BYTES: u64 = 48 * 1024;
const TAIL_LINES: usize = 120;
const TERMINAL: [&str; 4] = ["LANDED", "SETTLED", "ABANDONED", "DISCARDED"];

#[derive(Debug, Clone, PartialEq)]
pub struct Suite {
    pub name: String,
    pub verdict: String,
    pub duration_s: Option<i64>,
    pub first_fail: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PassLog {
    pub suites: Vec<Suite>,
    pub verdict: String,
    pub ran: Option<i64>,
    pub red: Option<i64>,
    pub skipped: Option<i64>,
    pub unit: String,
    pub unit_failed: Vec<String>,
    pub bare: String,
    pub instance_type: String,
    pub setup_s: Option<i64>,
    pub suites_s: Option<i64>,
}

fn secs(token: &str) -> Option<i64> {
    token.trim().strip_suffix('s')?.parse().ok()
}

fn kv(line: &str, key: &str) -> Option<i64> {
    line.split_whitespace().find_map(|w| w.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}

/// One suite line of the live listing: `  test-x.sh   ok   98s`, `  test-x.sh RED     rc=1 after 0s`,
/// `  test-x.sh   SKIPPED`.
fn suite_line(line: &str) -> Option<Suite> {
    if !line.starts_with(' ') {
        return None;
    }
    let mut w = line.split_whitespace();
    let name = w.next().filter(|n| n.starts_with("test-") && n.ends_with(".sh"))?;
    let verdict = w.next().filter(|v| v.chars().all(|c| c.is_ascii_alphabetic()))?.to_ascii_lowercase();
    let rest: Vec<&str> = w.collect();
    let duration_s = match rest.iter().position(|t| *t == "after") {
        Some(i) => rest.get(i + 1).and_then(|t| secs(t)),
        None => rest.first().and_then(|t| secs(t)),
    };
    Some(Suite { name: name.to_string(), verdict, duration_s, first_fail: String::new() })
}

/// One row of the final markdown table: `| suite | 0s (rc=1) | red | first FAIL line |`.
fn table_row(line: &str) -> Option<(String, Option<i64>, String, String)> {
    let cells: Vec<&str> = line.strip_prefix('|')?.trim_end().strip_suffix('|')?.split('|').map(str::trim).collect();
    if cells.len() < 4 || !cells[0].starts_with("test-") {
        return None;
    }
    let duration = cells[1].split_whitespace().next().and_then(secs);
    Some((cells[0].to_string(), duration, cells[2].to_ascii_lowercase(), cells[3..].join("|").trim().to_string()))
}

pub fn parse_pass_log(text: &str) -> PassLog {
    let mut p = PassLog::default();
    let mut order: Vec<String> = Vec::new();
    let mut by: HashMap<String, Suite> = HashMap::new();
    let mut in_unit_fail = false;
    for line in text.lines() {
        if let Some(s) = suite_line(line) {
            if !by.contains_key(&s.name) {
                order.push(s.name.clone());
            }
            by.insert(s.name.clone(), s);
        } else if let Some((name, d, verdict, first)) = table_row(line) {
            if !by.contains_key(&name) {
                order.push(name.clone());
            }
            let e = by.entry(name.clone()).or_insert(Suite { name, verdict: verdict.clone(), duration_s: d, first_fail: String::new() });
            e.verdict = verdict;
            e.duration_s = d.or(e.duration_s);
            e.first_fail = first;
        } else if let Some(rest) = line.strip_prefix("VERDICT ") {
            p.verdict = rest.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
            p.ran = kv(rest, "ran");
            p.red = kv(rest, "red");
            p.skipped = kv(rest, "skipped");
        } else if let Some(rest) = line.strip_prefix("round-vm: UNIT-TESTS: ") {
            p.unit = rest.trim().to_string();
            in_unit_fail = rest.starts_with("FAIL");
        } else if let Some(rest) = line.strip_prefix("round-vm: BARE-SUITES: ") {
            p.bare = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("round-vm: EC2 instance type ") {
            p.instance_type = rest.split_whitespace().next().unwrap_or("").to_string();
        } else if let Some(rest) = line.strip_prefix("round-vm: setup ") {
            let mut parts = rest.split(',');
            p.setup_s = parts.next().and_then(|t| secs(t.trim()));
            p.suites_s = parts.next().and_then(|t| secs(t.trim().strip_prefix("suites ")?));
        } else if in_unit_fail && line.starts_with("test ") && line.ends_with("FAILED") && p.unit_failed.len() < 10 {
            p.unit_failed.push(line.trim_end_matches(" ... FAILED").trim_start_matches("test ").to_string());
        }
    }
    p.suites = order.into_iter().filter_map(|n| by.remove(&n)).collect();
    p.suites.sort_by_key(|s| !is_red(&s.verdict));
    p
}

fn is_red(verdict: &str) -> bool {
    !matches!(verdict, "ok" | "skipped" | "skip" | "pass")
}

type Cached = (u64, SystemTime, Arc<PassLog>);

static PARSED: Mutex<Option<HashMap<PathBuf, Cached>>> = Mutex::new(None);

/// Parsing a 500 KB log costs more than a stat: reparse only when the size or mtime moved.
pub fn read_pass_log(path: &Path) -> Option<Arc<PassLog>> {
    let meta = std::fs::metadata(path).ok()?;
    let key = (meta.len(), meta.modified().ok()?);
    let mut held = PARSED.lock().unwrap_or_else(|e| e.into_inner());
    let map = held.get_or_insert_with(HashMap::new);
    if let Some((len, mtime, log)) = map.get(path) {
        if (*len, *mtime) == key {
            return Some(log.clone());
        }
    }
    let text = String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned();
    let log = Arc::new(parse_pass_log(&text));
    map.insert(path.to_path_buf(), (key.0, key.1, log.clone()));
    Some(log)
}

pub fn results_dir(run: &str) -> PathBuf {
    Path::new(run).join("vm-results")
}

fn squash(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

/// The pass logs of a round, oldest first: named by a pass head, by the batch id, or
/// `cert-<batch id without dashes>`, or a re-certification `cert-auto-<batch id>[-suffix]`.
pub fn pass_logs(run: &str, batch: &str, heads: &[String], own: &str) -> Vec<PathBuf> {
    let squashed = squash(batch);
    let mut found: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(results_dir(run))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("log") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_string();
            let recert = format!("cert-auto-{batch}");
            let hit = heads.iter().any(|h| *h == stem)
                || stem == batch
                || stem == squashed
                || stem.strip_prefix("cert-").is_some_and(|r| r == squashed || r == batch)
                || stem == recert
                || stem.strip_prefix(&recert).is_some_and(|r| r.starts_with('-'));
            hit.then(|| (e.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH), path))
        })
        .collect();
    let own = Path::new(own);
    if !own.as_os_str().is_empty() && own.extension().is_some() && !found.iter().any(|(_, p)| p == own) {
        if let Ok(modified) = std::fs::metadata(own).and_then(|m| m.modified()) {
            found.push((modified, own.to_path_buf()));
        }
    }
    found.sort();
    found.into_iter().map(|(_, p)| p).collect()
}

pub fn stem(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string()
}

/// The last lines of a log without reading all of it.
pub fn tail(path: &Path) -> String {
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_BYTES);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.take(TAIL_BYTES).read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    let skip = if start > 0 { 1 } else { 0 };
    let from = lines.len().saturating_sub(TAIL_LINES).max(skip.min(lines.len()));
    lines[from..].join("\n")
}

pub fn is_terminal(batch: &Value) -> bool {
    TERMINAL.contains(&batch["state"].as_str().unwrap_or(""))
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.parse().ok())
}

/// The newest pass's verdict, else the batch state.
pub fn verdict(batch: &Value) -> String {
    match batch["last_pass"]["verdict"].as_str() {
        Some(v) => v.to_string(),
        None if is_terminal(batch) => text(&batch["state"]).to_ascii_lowercase(),
        None if batch["last_pass"].is_null() => text(&batch["state"]).to_ascii_lowercase(),
        None => "running".to_string(),
    }
}

pub fn is_live(batch: &Value) -> bool {
    !is_terminal(batch) && !batch["last_pass"].is_null() && batch["last_pass"]["verdict"].is_null()
}

fn elapsed(batch: &Value, now: i64) -> i64 {
    let opened = int(&batch["opened_at"]).unwrap_or(now);
    let end = if is_terminal(batch) { int(&batch["last_at"]).unwrap_or(now) } else { now };
    end - opened
}

fn where_runs(batch: &Value) -> String {
    match (int(&batch["pr"]).unwrap_or(0), text(&batch["run"])) {
        (pr, _) if pr > 0 => format!("forge PR #{pr}"),
        (_, run) if !run.is_empty() => run.to_string(),
        _ => "local".to_string(),
    }
}

fn badge(verdict: &str) -> String {
    let class = if matches!(verdict, "red" | "abandoned" | "incomplete" | "fault" | "preempted") { " red" } else if matches!(verdict, "green" | "landed" | "settled" | "ok") { " ok" } else { "" };
    format!("<span class=\"badge{class}\">{}</span>", esc(verdict))
}

fn progress_text(batch: &Value) -> String {
    let p = &batch["progress"];
    match (int(&p["done"]), int(&p["total"])) {
        (Some(d), Some(t)) if t > 0 => format!(" {d}/{t}"),
        _ => String::new(),
    }
}

fn member_ids(batch: &Value) -> Vec<&str> {
    batch["members"].as_array().into_iter().flatten().filter_map(|m| m["bead_id"].as_str()).collect()
}

const CSS: &str = "<style>.badge.ok{background:#1f3a2c;color:#6fd3a0}main{padding:.5rem .8rem}.card{border:1px solid #2a323d;border-radius:6px;padding:.6rem;margin:.5rem 0;background:#131820}\
.kv{display:grid;grid-template-columns:max-content 1fr;gap:.2rem .8rem}.kv dt{color:#7d8896}.kv dd{margin:0;word-break:break-all}\
details summary{cursor:pointer}pre#log{background:#0b0e12;padding:.5rem;overflow:auto;max-height:24rem;font-size:.75rem}\
.stale{background:#3a2d12;color:#e6b94d;padding:.3rem .8rem}.scroll{overflow-x:auto}\
@media(max-width:640px){table.cards thead{display:none}table.cards tr{display:block;border:1px solid #2a323d;border-radius:6px;margin:.4rem 0;padding:.3rem}table.cards td{display:block;border:0;padding:.1rem .3rem}table.cards td::before{content:attr(data-h) \": \";color:#7d8896}}</style>";

fn stale_banner(stale: Option<&str>) -> String {
    stale.map(|e| format!("<div class=stale>showing the last good read — {}</div>", esc(e))).unwrap_or_default()
}

pub fn render_list(now: i64, batches: &[Value], stale: Option<&str>) -> String {
    let mut rows = String::new();
    for b in batches {
        let id = text(&b["batch_id"]);
        let members = member_ids(b);
        let list: String = members.iter().map(|m| format!("<a href=\"/beads/{0}\">{0}</a> ", esc(m))).collect();
        let vm = int(&b["pr"]).unwrap_or(0) == 0;
        let id_cell = if vm { format!("<a href=\"/rounds/spira/{0}\">{0}</a>", esc(id)) } else { esc(id) };
        let phase = {
            let ph = text(&b["phase"]);
            let state = text(&b["state"]);
            format!("{}{}{}", esc(state), if ph.is_empty() { String::new() } else { format!(" · {}", esc(ph)) }, esc(&progress_text(b)))
        };
        rows.push_str(&format!(
            "<tr><td data-h=repo>{}</td><td data-h=round class=id>{}</td><td data-h=members><details><summary>{}</summary>{}</details></td><td data-h=phase>{}</td><td data-h=elapsed>{}</td><td data-h=verdict>{}</td><td data-h=\"runs on\">{}</td></tr>",
            esc(text(&b["repo"])),
            id_cell,
            members.len(),
            list,
            phase,
            dur(elapsed(b, now)),
            badge(&verdict(b)),
            esc(&where_runs(b))
        ));
    }
    if rows.is_empty() {
        rows = "<tr><td colspan=7 class=dim>no round opened or open in the last 24 h</td></tr>".into();
    }
    let body = format!(
        "{CSS}<header><h1>Rounds <span class=dim>open and last 24 h</span></h1></header>{}<main><table class=cards><thead><tr><th>repo<th>round<th>members<th>phase<th>elapsed<th>verdict<th>runs on</tr></thead>{rows}</table></main>",
        stale_banner(stale)
    );
    shell("rounds", "Rounds", 15, &body)
}

fn suite_table(log: &PassLog) -> String {
    let mut out = String::from("<div class=scroll><table class=cards><thead><tr><th>suite<th>verdict<th>time<th>first FAIL</tr></thead>");
    for s in &log.suites {
        let class = if is_red(&s.verdict) { " class=refused" } else { "" };
        out.push_str(&format!(
            "<tr{class}><td data-h=suite>{}</td><td data-h=verdict>{}</td><td data-h=time>{}</td><td data-h=\"first FAIL\">{}</td></tr>",
            esc(&s.name),
            esc(&s.verdict),
            s.duration_s.map(dur).unwrap_or_default(),
            esc(&s.first_fail)
        ));
    }
    out.push_str("</table></div>");
    out
}

fn unit_summary(log: &PassLog) -> String {
    if log.unit.is_empty() && log.bare.is_empty() {
        return String::new();
    }
    let mut s = format!("<p>unit tests: {} · bare-host suites: {}</p>", esc(&log.unit), esc(&log.bare));
    if !log.unit_failed.is_empty() {
        s.push_str("<ul class=red>");
        for f in &log.unit_failed {
            s.push_str(&format!("<li>{}</li>", esc(f)));
        }
        s.push_str("</ul>");
    }
    s
}

pub struct Detail<'a> {
    pub now: i64,
    pub batch: &'a Value,
    pub logs: Vec<(String, Option<Arc<PassLog>>)>,
    pub selected: Option<String>,
    pub tail: String,
}

fn member_outcome(batch: &Value, m: &Value) -> String {
    let id = text(&m["bead_id"]);
    if let Some(e) = batch["ejected"].as_array().into_iter().flatten().rev().find(|e| text(&e["bead_id"]) == id) {
        return format!("ejected: {}", text(&e["reason"]));
    }
    match m["outcome"].as_str().filter(|o| !o.is_empty()) {
        Some(o) => o.to_ascii_lowercase(),
        None if matches!(text(&batch["state"]), "LANDED" | "SETTLED") => "landed".to_string(),
        None => "in this round".to_string(),
    }
}

pub fn render_detail(d: &Detail, stale: Option<&str>) -> String {
    let b = d.batch;
    let id = text(&b["batch_id"]);
    let selected = d.selected.as_ref().and_then(|s| d.logs.iter().find(|(n, _)| n == s));
    let log = selected.and_then(|(_, l)| l.as_deref());
    let live = is_live(b);
    let instance = match (text(&b["run"]), log.map(|l| l.instance_type.as_str()).unwrap_or("")) {
        ("", _) => "—".to_string(),
        (r, "") => r.to_string(),
        (r, t) => format!("{r} ({t})"),
    };
    let head = |v: &Value| text(v).chars().take(10).collect::<String>();
    let mut out = format!(
        "{CSS}<header><h1><a href=\"/rounds\">Rounds</a> / {} {}</h1></header>{}<main><div class=card><dl class=kv>\
         <dt>batch<dd>{}<dt>repo<dd>{}<dt>base → head<dd>{} → {}<dt>phase<dd>{}{}{}<dt>started<dd>{}<dt>elapsed<dd>{}<dt>verdict<dd>{}<dt>runs on<dd>{}</dl></div>",
        esc(id),
        badge(&verdict(b)),
        stale_banner(stale),
        esc(id),
        esc(text(&b["repo"])),
        esc(&head(&b["base"])),
        esc(&head(&b["head"])),
        esc(text(&b["state"])),
        if text(&b["phase"]).is_empty() { String::new() } else { format!(" · {}", esc(text(&b["phase"]))) },
        esc(&progress_text(b)),
        int(&b["opened_at"]).map(|t| dur(d.now - t) + " ago").unwrap_or_default(),
        dur(elapsed(b, d.now)),
        badge(&verdict(b)),
        esc(&instance)
    );

    out.push_str("<h2>Members</h2><div class=scroll><table class=cards><thead><tr><th>bead<th>title<th>tip<th>outcome</tr></thead>");
    for m in b["members"].as_array().into_iter().flatten() {
        let mid = text(&m["bead_id"]);
        let outcome = member_outcome(b, m);
        let class = if outcome.starts_with("ejected") { " class=refused" } else { "" };
        out.push_str(&format!(
            "<tr{class}><td data-h=bead><a href=\"/beads/{0}\">{0}</a></td><td data-h=title>{1}</td><td data-h=tip>{2}</td><td data-h=outcome>{3}</td></tr>",
            esc(mid),
            esc(text(&m["title"])),
            esc(&head(&m["tip"])),
            esc(&outcome)
        ));
    }
    out.push_str("</table></div><h2>Certification passes</h2><div class=scroll><table class=cards><thead><tr><th>pass<th>verdict<th>wall<th>build<th>suites<th>ejects<th>red suites</tr></thead>");
    for p in b["passes"].as_array().into_iter().flatten() {
        let (started, ended) = (int(&p["started_at"]), int(&p["ended_at"]));
        let wall = match (started, ended) {
            (Some(s), Some(e)) => dur(e - s),
            (Some(s), None) => format!("{} so far", dur(d.now - s)),
            _ => String::new(),
        };
        let red: Vec<&str> = p["red_suites"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        out.push_str(&format!(
            "<tr><td data-h=pass>{}</td><td data-h=verdict>{}</td><td data-h=wall>{}</td><td data-h=build>{}</td><td data-h=suites>{}</td><td data-h=ejects>{}</td><td data-h=\"red suites\">{}</td></tr>",
            int(&p["n"]).unwrap_or(0),
            badge(p["verdict"].as_str().unwrap_or("running")),
            wall,
            int(&p["build_s"]).map(dur).unwrap_or_default(),
            int(&p["suites_s"]).map(dur).unwrap_or_default(),
            int(&p["ejects"]).unwrap_or(0),
            esc(&red.join(", "))
        ));
    }
    out.push_str("</table></div>");

    out.push_str("<h2>Pass logs</h2>");
    if d.logs.is_empty() {
        out.push_str("<p class=dim>no pass log under vm-results for this round</p>");
    }
    for (name, l) in &d.logs {
        let (v, wall) = l.as_deref().map(|l| (l.verdict.as_str(), l.setup_s.zip(l.suites_s).map(|(a, b)| dur(a + b)).unwrap_or_default())).unwrap_or(("", String::new()));
        let mark = if Some(name) == d.selected.as_ref() { " ◀" } else { "" };
        out.push_str(&format!("<div><a href=\"/rounds/spira/{}?log={}\">{}</a> {} {}{}</div>", esc(id), esc(name), esc(name), badge(if v.is_empty() { "running" } else { v }), esc(&wall), mark));
    }
    if let Some(l) = log {
        out.push_str(&format!("<h2>Suites <span class=dim>{} ran · {} red · {} skipped</span></h2>", l.ran.unwrap_or(l.suites.len() as i64), l.red.unwrap_or(0), l.skipped.unwrap_or(0)));
        out.push_str(&unit_summary(l));
        out.push_str(&suite_table(l));
    }
    if let Some(name) = &d.selected {
        let refresh = if live {
            format!("<script>setInterval(async()=>{{const r=await fetch('/rounds/spira/{0}/log?log={1}');if(r.ok){{const e=document.getElementById('log');e.textContent=await r.text();e.scrollTop=e.scrollHeight}}}},3000)</script>", esc(id), esc(name))
        } else {
            String::new()
        };
        out.push_str(&format!("<h2>Log{}</h2><pre id=log>{}</pre>{refresh}", if live { " <span class=dim>live</span>" } else { "" }, esc(&d.tail)));
    }
    out.push_str("</main>");
    shell("rounds", &format!("Round {id}"), if live { 3600 } else { 60 }, &out)
}

pub fn detail_json(d: &Detail) -> Value {
    let log_json = |l: &PassLog| {
        json!({
            "verdict": l.verdict, "ran": l.ran, "red": l.red, "skipped": l.skipped, "unit": l.unit, "unit_failed": l.unit_failed,
            "bare": l.bare, "instance_type": l.instance_type, "setup_s": l.setup_s, "suites_s": l.suites_s,
            "suites": l.suites.iter().map(|s| json!({"suite": s.name, "verdict": s.verdict, "duration_s": s.duration_s, "first_fail": s.first_fail})).collect::<Vec<_>>(),
        })
    };
    json!({
        "batch": d.batch,
        "verdict": verdict(d.batch),
        "live": is_live(d.batch),
        "members": d.batch["members"].as_array().into_iter().flatten().map(|m| json!({"bead_id": m["bead_id"], "title": m["title"], "tip": m["tip"], "outcome": member_outcome(d.batch, m)})).collect::<Vec<_>>(),
        "logs": d.logs.iter().map(|(n, l)| json!({"name": n, "parsed": l.as_deref().map(log_json)})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rounds_own_recorded_log_is_a_pass_log_even_when_vm_results_has_none() {
        let dir = testkit::TempDir::new("loom-own-log");
        let own = dir.join("elsewhere").join("r-own.log");
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, "round-vm: x\n").unwrap();
        let run = dir.to_string_lossy().into_owned();
        assert!(pass_logs(&run, "r-own", &[], "").is_empty(), "positive control: no recorded log, none found");
        assert_eq!(pass_logs(&run, "r-own", &[], &own.to_string_lossy()), vec![own.clone()]);
        assert!(pass_logs(&run, "r-own", &[], &dir.join("gone.log").to_string_lossy()).is_empty());
    }

    const RED: &str = "round-vm: EC2 instance type c7i.8xlarge for ec2-x\n\
\x20\x20test-ops-read-model.sh           ok      22s\n\
\x20\x20test-acceptance-ci-scratch-queue.sh RED     rc=1 after 0s\n\
\x20\x20\x20\x20not ok 10 - publish-settle has nothing to publish\n\
\x20\x20test-closed-strand.sh            ok      9s\n\
\x20\x20test-dwarf-launch-grammar.sh      SKIPPED\n\
| Suite | Duration | Verdict | First FAIL line |\n\
|-------|----------|---------|-----------------|\n\
| test-acceptance-ci-scratch-queue.sh | 0s (rc=1) | red | not ok 10 - publish-settle has nothing | to publish |\n\
VERDICT RED ran=470 red=1 skipped=9\n\
round-vm: BARE-SUITES: PASS\n\
round-vm: UNIT-TESTS: PASS (211 test binaries)\n\
round-vm: setup 42s, suites 158s\n";

    #[test]
    fn a_red_suite_sorts_first_with_its_first_fail_line_and_a_green_one_has_none() {
        let p = parse_pass_log(RED);
        assert_eq!(p.suites[0].name, "test-acceptance-ci-scratch-queue.sh");
        assert_eq!(p.suites[0].verdict, "red");
        assert_eq!(p.suites[0].duration_s, Some(0));
        assert!(p.suites[0].first_fail.starts_with("not ok 10"), "{:?}", p.suites[0]);
        assert_eq!(p.suites.len(), 4);
        let ok = p.suites.iter().find(|s| s.name == "test-ops-read-model.sh").unwrap();
        assert_eq!((ok.verdict.as_str(), ok.duration_s, ok.first_fail.as_str()), ("ok", Some(22), ""));
        assert_eq!(p.suites.iter().find(|s| s.name == "test-dwarf-launch-grammar.sh").unwrap().verdict, "skipped");
        assert_eq!((p.verdict.as_str(), p.ran, p.red, p.skipped), ("red", Some(470), Some(1), Some(9)));
        assert_eq!((p.setup_s, p.suites_s, p.instance_type.as_str()), (Some(42), Some(158), "c7i.8xlarge"));
    }

    fn fixture(name: &str) -> PassLog {
        parse_pass_log(&std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).expect("a fixture"))
    }

    #[test]
    fn the_parser_reads_three_real_pass_logs_from_2026_10_15() {
        let red = fixture("pass-red-2026-10-15.log");
        assert_eq!((red.verdict.as_str(), red.ran, red.red, red.skipped), ("red", Some(470), Some(1), Some(9)));
        assert_eq!(red.suites.len(), 469);
        assert_eq!(red.suites[0].name, "test-acceptance-ci-scratch-queue.sh");
        assert!(red.suites[0].first_fail.starts_with("not ok 10 - publish-settle has nothing to publish"), "{:?}", red.suites[0]);
        assert_eq!(red.suites.iter().filter(|s| is_red(&s.verdict) && s.verdict != "skipped").count(), 1);
        assert_eq!(red.unit, "PASS (211 test binaries)");

        let recert = fixture("pass-recert-2026-10-15.log");
        assert_eq!((recert.verdict.as_str(), recert.red, recert.setup_s, recert.suites_s), ("green", None, Some(45), Some(155)));
        assert!(recert.suites.iter().all(|s| s.verdict == "ok" || s.verdict == "skipped"));

        let green = fixture("pass-green-2026-10-15.log");
        assert_eq!((green.verdict.as_str(), green.ran, green.setup_s, green.suites_s), ("green", Some(470), Some(62), Some(152)));
        let slow = green.suites.iter().find(|s| s.name == "test-queue-land-local.sh").expect("a suite from the log");
        assert_eq!((slow.verdict.as_str(), slow.duration_s), ("ok", Some(98)));
    }

    #[test]
    fn a_failed_unit_run_names_the_failing_tests() {
        let p = parse_pass_log("VERDICT GREEN ran=3 skipped=0\nround-vm: UNIT-TESTS: FAIL (unit run rc=101):\ntest a::b ... FAILED\ntest a::c ... ok\n");
        assert_eq!(p.verdict, "green");
        assert!(p.unit.starts_with("FAIL"));
        assert_eq!(p.unit_failed, vec!["a::b".to_string()]);
    }
}
