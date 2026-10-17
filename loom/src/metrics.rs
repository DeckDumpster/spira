//! `/metrics/rounds`: the raw round and CI rows from `run/tsd`, filtered, with per-group
//! count, median and p90. Files are parsed once per size+mtime; a request only filters and folds.

use crate::shell::shell;
use crate::stuck::{dur, esc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

const SHOWN: usize = 300;

type Rows = Arc<Vec<Value>>;

#[derive(Default)]
pub struct Cache {
    files: Mutex<BTreeMap<String, ((u64, Option<SystemTime>), Rows)>>,
}

impl Cache {
    pub fn read(&self, run: &str, family: &str) -> Rows {
        let path = tsd::family_path(Path::new(run), family);
        let Ok(meta) = std::fs::metadata(&path) else { return Rows::default() };
        let stamp = (meta.len(), meta.modified().ok());
        let mut held = self.files.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, rows)) = held.get(family).filter(|(s, _)| *s == stamp) {
            return rows.clone();
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut rows: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).filter(|v: &Value| v.is_object()).collect();
        rows.sort_by_key(|v| std::cmp::Reverse(ts(v)));
        let rows = Arc::new(rows);
        held.insert(family.to_string(), (stamp, rows.clone()));
        rows
    }
}

fn ts(v: &Value) -> i64 {
    v["ts"].as_str().and_then(tsd::parse_ts).map_or(0, |t| t as i64)
}

fn num(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Filter {
    pub repo: String,
    pub itype: String,
    pub place: String,
    pub window_s: Option<i64>,
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'%'));
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const WINDOWS: [(&str, Option<i64>); 5] = [("24h", Some(86_400)), ("3d", Some(3 * 86_400)), ("7d", Some(7 * 86_400)), ("30d", Some(30 * 86_400)), ("all", None)];

impl Filter {
    pub fn parse(query: &str) -> Filter {
        let mut f = Filter { window_s: Some(7 * 86_400), ..Filter::default() };
        for kv in query.split('&') {
            let Some((k, v)) = kv.split_once('=') else { continue };
            let v = decode(v);
            match k {
                "repo" => f.repo = v,
                "type" => f.itype = v,
                "where" => f.place = v,
                "window" => {
                    if let Some((_, w)) = WINDOWS.iter().find(|(n, _)| *n == v) {
                        f.window_s = *w;
                    }
                }
                _ => {}
            }
        }
        f
    }

    fn window_name(&self) -> &'static str {
        WINDOWS.iter().find(|(_, w)| *w == self.window_s).map_or("all", |(n, _)| n)
    }
}

pub struct Phase {
    pub wait: Option<i64>,
    pub boot: Option<i64>,
    pub build: Option<i64>,
    pub suites: Option<i64>,
}

impl Phase {
    fn of(v: &Value) -> Phase {
        Phase { wait: num(&v["capacity_wait_s"]), boot: num(&v["boot_s"]), build: num(&v["build_s"]), suites: num(&v["suites_s"]) }
    }
    fn total(&self) -> Option<i64> {
        let parts = [self.wait, self.boot, self.build, self.suites];
        parts.iter().any(Option::is_some).then(|| parts.iter().flatten().sum())
    }
}

fn type_label(v: &Value) -> String {
    let t = text(v, "instance_type");
    if t.is_empty() { "(none)".into() } else { t.into() }
}

fn keep_cert(v: &Value, f: &Filter, now: i64) -> bool {
    (f.repo.is_empty() || text(v, "repo") == f.repo)
        && (f.itype.is_empty() || type_label(v) == f.itype)
        && (f.place.is_empty() || text(v, "where") == f.place)
        && f.window_s.is_none_or(|w| ts(v) >= now - w)
}

fn keep_repo(v: &Value, f: &Filter, now: i64) -> bool {
    (f.repo.is_empty() || text(v, "repo") == f.repo) && f.window_s.is_none_or(|w| ts(v) >= now - w)
}

pub fn median(sorted: &[i64]) -> Option<i64> {
    let n = sorted.len();
    (n > 0).then(|| if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2] + 1) / 2 })
}

pub fn p90(sorted: &[i64]) -> Option<i64> {
    let n = sorted.len();
    (n > 0).then(|| sorted[((n * 9).div_ceil(10)).max(1) - 1])
}

fn stat(mut xs: Vec<i64>) -> String {
    xs.sort_unstable();
    match (median(&xs), p90(&xs)) {
        (Some(m), Some(p)) => format!("{} / {}", dur(m), dur(p)),
        _ => "–".into(),
    }
}

fn cell(x: Option<i64>) -> String {
    x.map_or("–".into(), dur)
}

fn category(v: &Value) -> &'static str {
    match text(v, "cause") {
        "green" => "green",
        "red-attributed" => "red-attributed",
        _ => "infra/incomplete",
    }
}

fn counts<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let mut m: BTreeMap<&str, usize> = BTreeMap::new();
    for i in items {
        *m.entry(if i.is_empty() { "–" } else { i }).or_default() += 1;
    }
    m.iter().map(|(k, n)| format!("{} {n}", esc(k))).collect::<Vec<_>>().join(", ")
}

fn when(t: i64) -> String {
    tsd::iso_utc(t.max(0) as u64).replacen('T', " ", 1).trim_end_matches('Z').to_string()
}

fn options(label: &str, name: &str, chosen: &str, values: &[String]) -> String {
    let mut s = format!("<label>{label} <select name={name}><option value=\"\">all</option>");
    for v in values {
        s.push_str(&format!("<option value=\"{0}\"{1}>{0}</option>", esc(v), if v == chosen { " selected" } else { "" }));
    }
    s.push_str("</select></label> ");
    s
}

fn distinct(rows: &[Value], f: impl Fn(&Value) -> String) -> Vec<String> {
    let mut v: Vec<String> = rows.iter().map(f).filter(|s| !s.is_empty()).collect();
    v.sort();
    v.dedup();
    v
}

fn form(f: &Filter, repos: &[String], types: &[String], places: &[String]) -> String {
    let mut s = String::from("<form method=get class=card>");
    s.push_str(&options("repo", "repo", &f.repo, repos));
    s.push_str(&options("type", "type", &f.itype, types));
    s.push_str(&options("where", "where", &f.place, places));
    s.push_str("<label>window <select name=window>");
    for (n, _) in WINDOWS {
        s.push_str(&format!("<option{}>{n}</option>", if n == f.window_name() { " selected" } else { "" }));
    }
    s.push_str("</select></label> <button>filter</button></form>");
    s
}

fn th(cols: &[&str]) -> String {
    format!("<tr>{}</tr>", cols.iter().map(|c| format!("<td class=dim>{c}</td>")).collect::<String>())
}

pub fn render(now: i64, cert: &[Value], batch: &[Value], gate: &[Value], f: &Filter) -> String {
    let repos = distinct(&[cert, batch, gate].concat(), |v| text(v, "repo").to_string());
    let types = distinct(cert, type_label);
    let places = distinct(cert, |v| text(v, "where").to_string());
    let mut out = String::from("<header><h1>Round metrics</h1></header><main>");
    out.push_str("<p class=dim><a href=/rounds>← rounds</a> · times are phase seconds as recorded; median / p90</p>");
    out.push_str(&form(f, &repos, &types, &places));

    let c: Vec<&Value> = cert.iter().filter(|v| keep_cert(v, f, now)).collect();
    out.push_str(&format!("<h2>Cert passes ({} rows)</h2>", c.len()));
    let mut by: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for v in &c {
        by.entry(type_label(v)).or_default().push(v);
    }
    out.push_str("<div style=\"overflow-x:auto\"><table>");
    out.push_str(&th(&["type", "n", "wait", "boot", "build", "suites", "total", "green", "red-attributed", "infra/incomplete", "causes"]));
    for (t, rows) in &by {
        let ph: Vec<Phase> = rows.iter().map(|v| Phase::of(v)).collect();
        let col = |g: fn(&Phase) -> Option<i64>| stat(ph.iter().filter_map(g).collect());
        let cat = |k: &str| rows.iter().filter(|v| category(v) == k).count();
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(t), rows.len(), col(|p| p.wait), col(|p| p.boot), col(|p| p.build), col(|p| p.suites), col(Phase::total),
            cat("green"), cat("red-attributed"), cat("infra/incomplete"), counts(rows.iter().map(|v| text(v, "cause"))),
        ));
    }
    out.push_str("</table></div>");
    out.push_str("<div style=\"overflow-x:auto\"><table>");
    out.push_str(&th(&["time", "round / batch", "repo", "type", "where", "wait", "boot", "build", "suites", "total", "verdict", "cause"]));
    for v in c.iter().take(SHOWN) {
        let p = Phase::of(v);
        let bad = if text(v, "verdict") == "green" { "" } else { " class=red" };
        let id = match (text(v, "batch"), text(v, "pass")) {
            (b, "") => b.to_string(),
            (b, n) => format!("{b} #{n}"),
        };
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td{bad}>{}</td><td{bad}>{}</td></tr>",
            when(ts(v)), esc(&id), esc(text(v, "repo")), esc(&type_label(v)), esc(text(v, "where")),
            cell(p.wait), cell(p.boot), cell(p.build), cell(p.suites), cell(p.total()), esc(text(v, "verdict")), esc(text(v, "cause")),
        ));
    }
    out.push_str("</table></div>");
    if c.len() > SHOWN {
        out.push_str(&format!("<p class=dim>newest {SHOWN} of {} rows shown; aggregates cover all {}</p>", c.len(), c.len()));
    }

    let b: Vec<&Value> = batch.iter().filter(|v| keep_repo(v, f, now)).collect();
    out.push_str(&format!("<h2>Site CI batch rounds ({} rows)</h2>", b.len()));
    out.push_str(&site_table(&b, "duration_ms", 1000, "verdict"));
    let g: Vec<&Value> = gate.iter().filter(|v| keep_repo(v, f, now)).collect();
    out.push_str(&format!("<h2>Gate runs ({} rows)</h2>", g.len()));
    out.push_str(&site_table(&g, "wall_secs", 1, "status"));
    out.push_str("</main>");
    shell("rounds", "Round metrics", 60, &out)
}

fn site_table(rows: &[&Value], field: &str, div: i64, verdict: &str) -> String {
    let mut by: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for v in rows {
        by.entry(text(v, "repo")).or_default().push(v);
    }
    let mut s = String::from("<div style=\"overflow-x:auto\"><table>");
    s.push_str(&th(&["repo", "n", "duration median / p90", verdict]));
    for (repo, rs) in &by {
        let d: Vec<i64> = rs.iter().filter_map(|v| num(&v[field])).map(|x| x / div).collect();
        s.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>", esc(repo), rs.len(), stat(d), counts(rs.iter().map(|v| text(v, verdict)))));
    }
    s.push_str("</table></div><div style=\"overflow-x:auto\"><table>");
    s.push_str(&th(&["time", "repo", verdict, "duration"]));
    for v in rows.iter().take(SHOWN) {
        s.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            when(ts(v)), esc(text(v, "repo")), esc(text(v, verdict)), cell(num(&v[field]).map(|x| x / div)),
        ));
    }
    s.push_str("</table></div>");
    if rows.len() > SHOWN {
        s.push_str(&format!("<p class=dim>newest {SHOWN} of {} rows shown; aggregates cover all</p>", rows.len()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_and_p90_of_known_samples() {
        assert_eq!(median(&[1, 2, 3]), Some(2));
        assert_eq!(median(&[1, 2, 3, 4]), Some(3));
        assert_eq!(p90(&(1..=10).collect::<Vec<_>>()), Some(9));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn filter_parse_decodes_and_keeps_the_default_window() {
        let f = Filter::parse("repo=a%20b&type=c7i.2xlarge&window=all");
        assert_eq!((f.repo.as_str(), f.itype.as_str(), f.window_s), ("a b", "c7i.2xlarge", None));
        assert_eq!(Filter::parse("").window_s, Some(7 * 86_400));
    }
}
