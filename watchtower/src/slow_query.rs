//! `--slow-query-check` — one incident per pass for every slow shape found in the slow-query log that
//! `spira-lc` appends to (`<epoch>\t<verb>\t<caller>\t<millis>\t<shape>`; the older four-field line has no caller). Only lines past the saved
//! offset are read, so new slow lines are reported on the pass that sees them; the incident
//! reference is fixed, so one slow store is one incident however many statements it slows.

use crate::incident::{self, Finding};
use crate::log::log;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub struct Shape {
    pub shape: String,
    pub verbs: Vec<String>,
    pub callers: Vec<String>,
    pub millis: Vec<u64>,
    pub slowest: (String, String),
}

impl Shape {
    pub fn count(&self) -> usize {
        self.millis.len()
    }
    pub fn max(&self) -> u64 {
        self.millis.iter().copied().max().unwrap_or(0)
    }
    pub fn p50(&self) -> u64 {
        let mut m = self.millis.clone();
        m.sort_unstable();
        m.get(m.len() / 2).copied().unwrap_or(0)
    }
}

pub fn group(text: &str) -> Vec<Shape> {
    let mut by: BTreeMap<String, Shape> = BTreeMap::new();
    for l in text.lines() {
        let f: Vec<&str> = l.splitn(5, '\t').collect();
        let (verb, caller, ms, shape) = match f[..] {
            [_, verb, caller, ms, shape] if ms.parse::<u64>().is_ok() => (verb, caller, ms, shape),
            [_, verb, ms, ..] => (verb, "-", ms, l.splitn(4, '\t').nth(3).unwrap_or("")),
            _ => continue,
        };
        let Ok(ms) = ms.parse::<u64>() else { continue };
        let s = by.entry(shape.to_string()).or_insert_with(|| Shape { shape: shape.to_string(), verbs: vec![], callers: vec![], millis: vec![], slowest: (String::new(), String::new()) });
        if !s.verbs.iter().any(|v| v == verb) {
            s.verbs.push(verb.to_string());
        }
        if !s.callers.iter().any(|c| c == caller) {
            s.callers.push(caller.to_string());
        }
        if s.millis.iter().all(|m| *m < ms) {
            s.slowest = (verb.to_string(), caller.to_string());
        }
        s.millis.push(ms);
    }
    by.into_values().collect()
}

fn is_host_bound(shape: &str) -> bool {
    let u = shape.trim().to_ascii_uppercase();
    u == "CONNECT" || u.starts_with("START TRANSACTION") || u.starts_with("BEGIN")
}

fn is_wal(shape: &str) -> bool {
    shape == "WAL-RESET-ENDED" || shape == "WAL-OVER-LINE"
}

pub fn run(run_dir: &Path, log_path: &Path, db: &str, home_repo: &str, incident_sh: &str, threshold_ms: u64) {
    let Ok(bytes) = std::fs::read(log_path) else {
        log("watchtower: slow-query-check — no slow-query log");
        return;
    };
    let stamp = run_dir.join("slow-queries.offset");
    let mut off: usize = std::fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    if off > bytes.len() {
        off = 0;
    }
    let end = bytes[off..].iter().rposition(|b| *b == b'\n').map(|i| off + i + 1).unwrap_or(off);
    let text = String::from_utf8_lossy(&bytes[off..end]).into_owned();
    let shapes = group(&text);
    if shapes.is_empty() {
        log("watchtower: slow-query-check — nothing new");
        return;
    }
    if !incident::is_usable(incident_sh) {
        log(&format!("watchtower: slow-query-check skipped — {incident_sh} not readable"));
        return;
    }
    let contended = shapes.iter().any(|s| is_host_bound(&s.shape));
    let slowest = shapes.iter().max_by_key(|s| s.max()).expect("shapes is non-empty");
    let worst = slowest.max();
    let hits: usize = shapes.iter().map(Shape::count).sum();
    let mut body = String::from("The lifecycle store is responding slowly. One incident covers every statement seen slow; the shapes are listed as evidence.\n\n");
    let wal: Vec<&Shape> = shapes.iter().filter(|s| is_wal(&s.shape)).collect();
    if !wal.is_empty() {
        body.push_str(&format!(
            "Verdict: WAL pressure. The control store's WAL stood past its line, or a restart past it cut a read at its limit; the verbs open were: {}. Find what holds a read open across checkpoints, not a slow query.\n\n",
            wal.iter().flat_map(|s| s.verbs.iter().cloned()).collect::<Vec<_>>().join(", ")
        ));
    } else if contended {
        body.push_str("Verdict: host contention. A connection handshake or transaction start has no plan and no index, and it was slow in the same window, so the latency is the host (process churn, CPU or memory pressure), not any one query. Do not tune indexes; find what is loading the box.\n\n");
    } else {
        body.push_str("Verdict: query-attributable. No connect or transaction-start was slow in this window; find each verb's query and fix its plan or its index.\n\n");
    }
    for s in &shapes {
        body.push_str(&format!("shape: {}\nverbs: {}\ncallers: {}\ncount: {}\np50: {}ms\nmax: {}ms\n\n", s.shape, s.verbs.join(", "), s.callers.join(", "), s.count(), s.p50(), s.max()));
    }
    let title = if wal.is_empty() { format!("SLOW STORE: {} by {} {worst}ms over {threshold_ms}ms; {hits} call(s) over in window", slowest.slowest.0, slowest.slowest.1) } else { format!("WAL PRESSURE: {} open; {} shape(s) in window", wal.iter().flat_map(|s| s.verbs.iter().cloned()).collect::<Vec<_>>().join("/"), wal.len()) };
    let f = Finding::new(db, home_repo, &title, &body)
        .priority(1)
        .reference("incident:slow-query".to_string())
        .cause("slow-query");
    incident::alarm(incident_sh, &f);
    log(&format!("watchtower: slow-query-check filed one incident for {} shape(s) ({hits} hit(s), max {worst}ms, contended: {contended})", shapes.len()));
    let _ = std::fs::write(&stamp, end.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_incident(d: &testkit::TempDir) -> (String, std::path::PathBuf) {
        let inc = d.join("inc.sh");
        let calls = d.join("calls");
        std::fs::write(&inc, format!("#!/usr/bin/env bash\necho \"$SPIRA_INCIDENT_REF|$SPIRA_INCIDENT_PRIORITY|$2\" >> {}\ncat >> {}\n", calls.display(), calls.display())).unwrap();
        (inc.to_string_lossy().into_owned(), calls)
    }

    #[test_id(t0r9l4t)]
    #[test]
    fn an_empty_log_files_nothing() {
        let d = testkit::TempDir::new("wt-slow-none");
        let (inc, calls) = fake_incident(&d);
        let log_path = d.join("slow-queries.log");
        std::fs::write(&log_path, "").unwrap();
        run(&d, &log_path, "db", "spira", &inc, 1000);
        assert!(!calls.exists());
    }

    #[test_id(t4sy048)]
    #[test]
    fn the_title_names_the_slowest_verb_its_duration_the_threshold_and_the_count() {
        let d = testkit::TempDir::new("wt-slow-title");
        let (inc, calls) = fake_incident(&d);
        let log_path = d.join("slow-queries.log");
        let toml = spira_config::process::fixture_toml(&d, &[]);
        let home = spira_config::process::spira_home();
        let release = spira_config::process::fixture_release(&d, true);
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap())), ("SPIRA_HOME", home.to_str()), ("SPIRA_RELEASE", release.to_str())]);
        std::fs::write(&log_path, "1\tlist\tc\t1300\tSELECT a\n2\tshow\td\t1100\tSELECT b\n").unwrap();
        run(&d, &log_path, "db", "spira", &inc, 1000);
        let got = std::fs::read_to_string(&calls).unwrap();
        let first = got.lines().next().unwrap();
        for want in ["incident:slow-query", "SLOW STORE: list by c 1300ms over 1000ms; 2 call(s) over in window", "callers: c\n", "callers: d\n"] {
            assert!(got.contains(want), "{want} missing from {first}");
        }
    }

    #[test_id(tvtz80k)]
    #[test]
    fn a_wal_shape_files_an_incident_naming_the_open_verb_and_wal_pressure() {
        let d = testkit::TempDir::new("wt-slow-wal");
        let (inc, calls) = fake_incident(&d);
        let log_path = d.join("slow-queries.log");
        let toml = spira_config::process::fixture_toml(&d, &[]);
        let home = spira_config::process::spira_home();
        let release = spira_config::process::fixture_release(&d, true);
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap())), ("SPIRA_HOME", home.to_str()), ("SPIRA_RELEASE", release.to_str())]);
        std::fs::write(&log_path, "1\tlist-probe\tc\t40\tWAL-RESET-ENDED\n2\tclaim\tc\t3\tWAL-OVER-LINE\n").unwrap();
        run(&d, &log_path, "db", "spira", &inc, 1000);
        let got = std::fs::read_to_string(&calls).unwrap();
        for want in ["incident:slow-query", "WAL PRESSURE: claim/list-probe", "Verdict: WAL pressure", "shape: WAL-RESET-ENDED", "shape: WAL-OVER-LINE"] {
            assert!(got.contains(want), "{want} missing from {got}");
        }
        assert!(!got.contains("query-attributable"), "{got}");
    }

    #[test_id(tg3e7xv)]
    #[test]
    fn distinct_shapes_group_separately() {
        let g = group("1\ta\t1100\tX\n2\tb\t1200\tY\n3\ta\t1300\tX\n");
        assert_eq!(g.len(), 2);
    }

    #[test_id(tlrsqxm)]
    #[test]
    fn a_shape_keeps_every_caller_and_the_slowest_calls_verb_and_caller() {
        let g = group("1\tlist\ta\t1100\tX\n2\tshow\tb\t1900\tX\n3\tlist\ta\t1300\tX\n4\told\t1200\tX\n");
        assert_eq!(g[0].callers, ["a", "b", "-"]);
        assert_eq!(g[0].slowest, ("show".to_string(), "b".to_string()));
    }

    #[test_id(tsolnez)]
    #[test]
    fn a_line_with_a_caller_groups_with_the_same_shape_without_one() {
        let g = group("1\ta\t1100\tX\n2\ta\tspira-lc\t1200\tX\n3\tb\tclaim\t1300\tREQUEST\n");
        assert_eq!(g.len(), 2);
        let x = g.iter().find(|s| s.shape == "X").unwrap();
        assert_eq!((x.count(), x.max()), (2, 1200));
    }
}
