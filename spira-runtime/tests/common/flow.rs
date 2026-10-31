#![allow(dead_code)]
//! One world for the flow pass: the same `run/tsd` series, lifecycle rows and metrics store built
//! twice, once for the binary and once for the handler, so neither can write into the other's.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use spira_runtime::watchers::reconciler_flow::ReconcilerFlow;
use spira_runtime::world::World;

pub const SCOPE: &str = "acme";
pub const INTERVAL_SECS: u64 = 1800;
pub const GRACE_SECS: u64 = 900;
pub const WINDOW_HOURS: f64 = 3.0;
pub const BASELINE_HOURS: f64 = 12.0;
pub const REWORK_HOURS: f64 = 4.0;
pub const SENTINEL_SECS: u64 = 120;
pub const UNOBSERVABLE_GRACE_SECS: u64 = 2700;
pub const LC_SEQ: u64 = 41;

pub fn handler() -> ReconcilerFlow {
    ReconcilerFlow {
        interval_ms: INTERVAL_SECS * 1000,
        window_hours: WINDOW_HOURS,
        baseline_hours: BASELINE_HOURS,
        grace_secs: GRACE_SECS,
        unobservable_grace_secs: UNOBSERVABLE_GRACE_SECS,
        rework_window_hours: REWORK_HOURS,
        sentinel_timer_secs: SENTINEL_SECS,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Quiet,
    BacklogRising,
    StalledStage,
    DwellRegression,
    HighFlipRate,
}

#[derive(Clone)]
pub struct LcRow {
    pub id: String,
    pub state: &'static str,
    pub repo: &'static str,
}

/// The lifecycle rows each scenario holds: the scope's, another repository's, and the epic that has none.
pub fn lc_rows(s: Scenario) -> Vec<LcRow> {
    let row = |i: usize, state, repo| LcRow { id: format!("sp-{i}"), state, repo };
    let live = if s == Scenario::BacklogRising { 30 } else { 8 };
    let mut rows: Vec<LcRow> = (0..live).map(|i| row(i, ["READY", "WORKING", "SUBMITTED", "REWORK"][i % 4], SCOPE)).collect();
    rows.extend((100..105).map(|i| row(i, "LANDED", SCOPE)));
    rows.extend((200..203).map(|i| row(i, "WORKING", "elsewhere")));
    rows
}

fn stage_row(ts: u64, machine: &str, key: &str, from: &str, to: &str) -> Value {
    json!({"ts": tsd::iso_utc(ts), "host": "h", "family": "bead-stage", "machine": machine, "key": key, "from_state": from, "to_state": to, "applied": true})
}

/// Every `bead-stage` row the scenario holds, numbered in time order as the exporter numbers them.
fn walk(rows: &mut Vec<(u64, Value)>, key: String, from_ts: u64, steps: &[(&str, u64)]) {
    let (mut from, mut t) = ("NONE", from_ts);
    for (to, took) in steps {
        rows.push((t, stage_row(t, "bead", &key, from, to)));
        from = to;
        t += took;
    }
    rows.push((t, stage_row(t, "bead", &key, from, "LANDED")));
}

/// Every `bead-stage` row the scenario holds, numbered in time order as the exporter numbers them.
pub fn stage_rows(s: Scenario, now: u64) -> Vec<Value> {
    let mut rows: Vec<(u64, Value)> = vec![];
    let steady = [("READY", 100), ("WORKING", 200), ("SUBMITTED", 120), ("CERTIFIED", 60), ("IN_DELIVERY", 90)];
    let lasts = steady.iter().map(|(_, d)| d).sum::<u64>();
    for i in 0..17u64 {
        let started = now - 11 * 3600 + i * 2000;
        if s == Scenario::StalledStage && started + lasts > now - 3 * 3600 - 1800 {
            break;
        }
        walk(&mut rows, format!("sp-done-{i}"), started, &steady);
    }
    if s == Scenario::StalledStage {
        for i in 0..3u64 {
            let t = now - 3600 + i * 60;
            rows.push((t, stage_row(t, "bead", &format!("sp-wait-{i}"), "NONE", "READY")));
            rows.push((t + 30, stage_row(t + 30, "bead", &format!("sp-wait-{i}"), "READY", "SUBMITTED")));
        }
    } else {
        walk(&mut rows, "sp-fresh".into(), now - 1200 - lasts, &steady);
    }
    if s == Scenario::DwellRegression {
        let slow = [("READY", 100), ("WORKING", 12_000), ("SUBMITTED", 120), ("CERTIFIED", 60), ("IN_DELIVERY", 90)];
        for i in 0..2u64 {
            walk(&mut rows, format!("sp-slow-{i}"), now - 14_000 + i * 100, &slow);
        }
    }
    let batch_rows = |rows: &mut Vec<(u64, Value)>, key: String, at: u64, path: &[&str]| {
        let mut from = "NONE";
        for (i, to) in path.iter().enumerate() {
            let t = at + 120 * i as u64;
            rows.push((t, stage_row(t, "batch", &key, from, to)));
            from = to;
        }
    };
    for i in 0..6u64 {
        batch_rows(&mut rows, format!("batch-ok-{i}"), now - 10 * 3600 + i * 1800, &["OPEN", "CI_RUNNING", "GREEN", "LANDED"]);
    }
    if s == Scenario::HighFlipRate {
        for i in 0..4u64 {
            batch_rows(&mut rows, format!("batch-red-{i}"), now - 2000 + i * 300, &["OPEN", "CI_RUNNING", "ATTRIBUTING"]);
        }
    }
    rows.retain(|(t, _)| *t < now - 30);
    rows.sort_by_key(|(t, _)| *t);
    rows.into_iter().enumerate().map(|(i, (_, mut v))| {
        v["seq"] = json!(i + 1);
        v
    })
    .collect()
}

pub struct Flow {
    pub dir: testkit::TempDir,
    pub store: spira_otel::fixture::FakeStore,
    pub now: u64,
    pub scenario: Scenario,
}

impl Flow {
    pub fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn run(&self) -> PathBuf {
        self.p("run")
    }

    pub fn build(name: &str, scenario: Scenario, now: u64) -> Flow {
        let f = Flow { dir: testkit::TempDir::new(&format!("flow-{name}")), store: spira_otel::fixture::FakeStore::start(), now, scenario };
        for d in ["run/tsd", "bin", "desired", "mails"] {
            fs::create_dir_all(f.p(d)).unwrap();
        }
        for r in stage_rows(scenario, now) {
            tsd::store::push_row(Some(f.store.addr()), "bead-stage", &r.to_string()).unwrap();
        }
        f.sentinel_pass("p1", 30, now - 600);
        let family = |name: &str| tsd::family_path(&f.run(), name);
        for hours in 1..=11u64 {
            let line = tsd::build_row(&tsd::iso_utc(now - hours * 3600), "h", "backlog", &[tsd::parse_field("count=8").unwrap()]).unwrap();
            tsd::append_line(&family("backlog"), &line).unwrap();
        }
        for (i, at) in [now - 1800, now - 900].iter().enumerate() {
            let fields = ["live=3", "ceiling=3", "ready=0", "capacity_paused=0"].map(|x| tsd::parse_field(x).unwrap());
            let line = tsd::build_row(&tsd::iso_utc(*at), &format!("h{i}"), "slots", &fields).unwrap();
            tsd::append_line(&family("slots"), &line).unwrap();
        }
        fs::write(f.p("run/tsd/.bead-stage-checkpoint-lifecycle.json"), format!("{{\"last_seq\":{LC_SEQ}}}")).unwrap();
        f
    }

    /// A sentinel pass that took `secs`, finished at `at`.
    pub fn sentinel_pass(&self, pass: &str, secs: u64, at: u64) {
        let row = json!({"ts": tsd::iso_utc(at), "host": "h", "family": "sentinel-phase", "pass": pass, "check": "CHECK1", "secs": secs});
        tsd::store::push_row(Some(self.store.addr()), "sentinel-phase", &row.to_string()).unwrap();
    }

    /// A fake spira-lc socket serving this world's rows through the filters the real `list` has.
    pub fn serve_lc(&self, seq: Arc<AtomicU64>) -> PathBuf {
        let sock = self.p("lc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let rows = lc_rows(self.scenario);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let argv: Vec<String> = serde_json::from_str(&line).unwrap();
                let flag = |name: &str| argv.iter().position(|a| a == name).and_then(|i| argv.get(i + 1)).cloned();
                let (code, body) = match argv[0].as_str() {
                    "list" => {
                        let live = argv.iter().any(|a| a == "--live");
                        let repo = flag("--repo");
                        let picked: Vec<Value> = rows
                            .iter()
                            .filter(|r| !live || !matches!(r.state, "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE"))
                            .filter(|r| repo.as_deref().is_none_or(|x| x == r.repo))
                            .map(|r| json!({"bead_id": r.id, "state": r.state, "repo": r.repo}))
                            .collect();
                        (0, Value::Array(picked).to_string())
                    }
                    "event-seq" => (0, json!({"seq": seq.load(Ordering::SeqCst), "keys": []}).to_string()),
                    _ => (2, "no such door".into()),
                };
                let mut w = &stream;
                writeln!(w, "{{\"exit_code\": {code}, \"len\": {}}}", body.len()).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
        });
        sock
    }

    /// A script that counts its runs in `exports.log`; the exporter the world launches when it is behind.
    pub fn counting_exporter(&self) -> PathBuf {
        let path = self.p("bin/export-counted");
        testkit::write_exe(&path, &format!("#!/usr/bin/env bash\necho run >> {}\nexit 0\n", self.p("exports.log").display()));
        path
    }

    pub fn exports(&self) -> usize {
        fs::read_to_string(self.p("exports.log")).map(|t| t.lines().count()).unwrap_or(0)
    }

    pub fn world(&self, sock: &Path, export_bin: &Path) -> World {
        World {
            run_dir: self.run(),
            lc_socket: sock.to_path_buf(),
            metrics_endpoint: Some(self.store.addr().to_string()),
            scope_label: SCOPE.into(),
            desired_dir: self.p("desired"),
            export_bin: export_bin.display().to_string(),
            ..World::default()
        }
    }

    pub fn state_file(&self) -> PathBuf {
        reconciler_engine::paths::flow_state(&self.run())
    }

    pub fn alerted_file(&self) -> PathBuf {
        reconciler_engine::paths::flow_alerted(&self.run())
    }

    /// The `reconciler-status` rows the store holds, one line each without the time they were taken.
    pub fn statuses(&self) -> Vec<String> {
        let rows = tsd::store::Reader::new(Some(self.store.addr())).unwrap().rows_or_empty("reconciler-status", 0).unwrap();
        let mut lines: Vec<String> = rows
            .iter()
            .map(|r| {
                let text = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                format!("{} {} gap={} since={} {} | {}", text("key"), text("status"), r["is_gap"], r.get("since").map_or(String::new(), |s| s.to_string()), text("desired"), text("observed"))
            })
            .collect();
        lines.sort();
        lines
    }
}

pub fn mail_env(root: &Path) -> mail::env::Env {
    mail::env::Env {
        mail_root: root.join("mail"),
        kinds_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/mail/kinds"),
        index_file: root.join("mail-index"),
        mute: false,
        loom_budget_ms: 1000,
        repeat_window_s: 600,
        tidy_fresh_s: 0,
        id_prefix: "sp".into(),
        ask_label: "ask".into(),
        db: String::new(),
        bd_bin: String::new(),
        bd_conn_retries: 1,
        operator_actor: "operator".into(),
        run_dir: root.join("run"),
        home: root.to_path_buf(),
        mail_from: None,
        lint_considered: None,
        repeat_considered: None,
        operator_considered: None,
        statute_guards: String::new(),
        statute_considered: None,
        allow_blocking: false,
        bead_id: None,
        lock_timeout_ms: 1000,
    }
}

/// The Concierge's delivered mails as `Subject\nbody`, sorted.
pub fn delivered(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(root.join("mail/concierge/new"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let text = fs::read_to_string(e.path()).unwrap();
            let (head, body) = text.split_once("\n\n").unwrap();
            let subject = head.lines().find_map(|l| l.strip_prefix("Subject: ")).unwrap();
            let body = body.strip_prefix("## Note\n").expect("a note's body sits under its heading");
            format!("{subject}\n{}", body.trim_end())
        })
        .collect();
    out.sort();
    out
}
