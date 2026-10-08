//! `--slow-query-check` — one incident per pass for every slow shape found in the slow-query log that
//! `spira-lc` appends to (`<epoch>\t<verb>\t<millis>\t<shape>`). Only lines past the saved
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
    pub millis: Vec<u64>,
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
        let f: Vec<&str> = l.splitn(4, '\t').collect();
        let [_, verb, ms, shape] = f[..] else { continue };
        let Ok(ms) = ms.parse::<u64>() else { continue };
        let s = by.entry(shape.to_string()).or_insert_with(|| Shape { shape: shape.to_string(), verbs: vec![], millis: vec![] });
        if !s.verbs.iter().any(|v| v == verb) {
            s.verbs.push(verb.to_string());
        }
        s.millis.push(ms);
    }
    by.into_values().collect()
}

fn is_host_bound(shape: &str) -> bool {
    let u = shape.trim().to_ascii_uppercase();
    u == "CONNECT" || u.starts_with("START TRANSACTION") || u.starts_with("BEGIN")
}

pub fn run(run_dir: &Path, log_path: &Path, db: &str, home_repo: &str, incident_sh: &str) {
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
    let worst = shapes.iter().map(Shape::max).max().unwrap_or(0);
    let hits: usize = shapes.iter().map(Shape::count).sum();
    let mut body = String::from("The lifecycle store is responding slowly. One incident covers every statement seen slow; the shapes are listed as evidence.\n\n");
    if contended {
        body.push_str("Verdict: host contention. A connection handshake or transaction start has no plan and no index, and it was slow in the same window, so the latency is the host (process churn, CPU or memory pressure), not any one query. Do not tune indexes; find what is loading the box.\n\n");
    } else {
        body.push_str("Verdict: query-attributable. No connect or transaction-start was slow in this window; find each verb's query and fix its plan or its index.\n\n");
    }
    for s in &shapes {
        body.push_str(&format!("shape: {}\nverbs: {}\ncount: {}\np50: {}ms\nmax: {}ms\n\n", s.shape, s.verbs.join(", "), s.count(), s.p50(), s.max()));
    }
    let f = Finding::new(db, home_repo, "SLOW STORE: lifecycle queries over threshold", &body)
        .priority(1)
        .reference("incident:slow-query".to_string())
        .cause("slow-query");
    incident::file(incident_sh, &f);
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

    #[test]
    fn repeats_of_one_shape_file_one_incident_and_a_fast_shape_none() {
        let d = testkit::TempDir::new("wt-slow");
        let (inc, calls) = fake_incident(&d);
        let log_path = d.join("slow-queries.log");
        let mut text = String::new();
        for i in 0..5 {
            text.push_str(&format!("{}\tlist\t{}\tSELECT * FROM bead WHERE id = ?\n", 100 + i, 1500 + i * 100));
        }
        std::fs::write(&log_path, &text).unwrap();
        run(&d, &log_path, "db", "spira", &inc);
        run(&d, &log_path, "db", "spira", &inc);
        let got = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(got.matches("incident:slow-query").count(), 1, "{got}");
        assert!(got.contains("|1|file"), "{got}");
        assert!(got.contains("count: 5") && got.contains("p50: 1700ms") && got.contains("max: 1900ms"), "{got}");
    }

    #[test]
    fn an_empty_log_files_nothing() {
        let d = testkit::TempDir::new("wt-slow-none");
        let (inc, calls) = fake_incident(&d);
        let log_path = d.join("slow-queries.log");
        std::fs::write(&log_path, "").unwrap();
        run(&d, &log_path, "db", "spira", &inc);
        assert!(!calls.exists());
    }

    #[test]
    fn distinct_shapes_group_separately() {
        let g = group("1\ta\t1100\tX\n2\tb\t1200\tY\n3\ta\t1300\tX\n");
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn many_shapes_file_one_incident_and_a_slow_connect_says_contention() {
        let d = testkit::TempDir::new("wt-slow-many");
        let (inc, calls) = fake_incident(&d);
        let log_path = d.join("slow-queries.log");
        std::fs::write(&log_path, "1\tlist\t2600\tCONNECT\n2\tshow\t5300\tSELECT a FROM bead\n3\tfact\t2300\tSTART TRANSACTION; INSERT INTO event\n").unwrap();
        run(&d, &log_path, "db", "spira", &inc);
        let got = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(got.matches("incident:slow-query").count(), 1, "{got}");
        assert!(got.contains("host contention") && !got.contains("query-attributable"), "{got}");
        assert!(got.contains("shape: CONNECT") && got.contains("shape: SELECT a FROM bead"), "{got}");
    }

    #[test]
    fn slow_selects_alone_are_query_attributable() {
        let d = testkit::TempDir::new("wt-slow-q");
        let (inc, calls) = fake_incident(&d);
        let log_path = d.join("slow-queries.log");
        std::fs::write(&log_path, "1\tlist\t2600\tSELECT a FROM bead\n").unwrap();
        run(&d, &log_path, "db", "spira", &inc);
        let got = std::fs::read_to_string(&calls).unwrap();
        assert!(got.contains("query-attributable") && !got.contains("host contention"), "{got}");
    }
}
