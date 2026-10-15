// covers: unit-run/**
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use testkit::TempDir;
use unit_run::*;

const T0: u64 = 1_792_000_000_000_000;
const S: u64 = 1_000_000;

fn line(seq: u64, at_us: u64, id: &str, unit: &str, inv: &str, extra: Value) -> String {
    let mut v = json!({
        "__CURSOR": format!("c{seq:04}"),
        "__REALTIME_TIMESTAMP": at_us.to_string(),
        "MESSAGE_ID": id,
        "USER_UNIT": unit,
        "USER_INVOCATION_ID": inv,
        "MESSAGE": "m",
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v.to_string()
}

fn fixture() -> Vec<String> {
    let mut l = Vec::new();
    let mut n = 0;
    let mut add = |at: u64, id: &str, unit: &str, inv: &str, extra: Value| {
        n += 1;
        l.push(line(n, at, id, unit, inv, extra));
    };
    add(T0, ID_STARTING, "ok.service", "inv-ok", json!({}));
    add(T0 + S / 2, ID_STARTING, "bad.service", "inv-bad", json!({}));
    add(T0 + 3 * S / 2, ID_EXIT, "ok.service", "inv-ok", json!({"EXIT_CODE": "exited", "EXIT_STATUS": "0", "COMMAND": "ExecStart"}));
    add(T0 + 3 * S / 2 + 50, ID_SUCCESS, "ok.service", "inv-ok", json!({}));
    add(T0 + 3 * S / 2 + 90, ID_RESOURCES, "ok.service", "inv-ok", json!({"CPU_USAGE_NSEC": "2500000000", "MEMORY_PEAK": "1048576", "IO_METRIC_READ_BYTES": "4096", "IO_METRIC_WRITE_BYTES": "8192"}));
    add(T0 + 2 * S, ID_STARTING, "tick.service", "inv-tick", json!({}));
    add(T0 + 3 * S, ID_EXIT, "bad.service", "inv-bad", json!({"EXIT_CODE": "exited", "EXIT_STATUS": "1", "COMMAND": "ExecStart"}));
    add(T0 + 3 * S + 10, ID_FAILURE, "bad.service", "inv-bad", json!({"UNIT_RESULT": "exit-code"}));
    add(T0 + 3 * S + 40, ID_RESOURCES, "bad.service", "inv-bad", json!({"CPU_USAGE_NSEC": "1000000", "MEMORY_PEAK": "2048"}));
    add(T0 + 4 * S, ID_STARTING, "long.service", "inv-long", json!({}));
    add(T0 + 5 * S, ID_SUCCESS, "tick.service", "inv-tick", json!({}));
    add(T0 + 20 * S, ID_STARTING, "later.service", "inv-later", json!({}));
    add(T0 + 21 * S, ID_SUCCESS, "long.service", "inv-long", json!({}));
    add(T0 + 40 * S, ID_STARTING, "open.service", "inv-open", json!({}));
    l
}

struct Fake {
    lines: Vec<String>,
    stop_after: Option<usize>,
    idle_at_end: bool,
}

impl Source for Fake {
    fn open(&self, after: Option<&str>, _follow: bool) -> Result<Box<dyn Iterator<Item = Item>>, String> {
        let from = after.map_or(0, |c| self.lines.iter().position(|l| l.contains(&format!("\"{c}\""))).map_or(0, |i| i + 1));
        let mut v: Vec<Item> = self.lines[from..].iter().cloned().map(Item::Line).collect();
        if let Some(n) = self.stop_after {
            v.truncate(n.saturating_sub(from));
        } else if self.idle_at_end {
            v.push(Item::Idle);
        }
        Ok(Box::new(v.into_iter()))
    }
}

struct Fixed(u64);
impl Clock for Fixed {
    fn now_epoch(&self) -> u64 {
        self.0
    }
}

struct Systemctl(BTreeMap<&'static str, &'static str>);
impl Proc for Systemctl {
    fn run(&self, argv: &[&str]) -> Result<String, String> {
        Ok(self.0.get(argv[argv.len() - 1]).map(|t| format!("{t}\n")).unwrap_or_default())
    }
}

fn systemctl() -> Systemctl {
    Systemctl(BTreeMap::from([("tick.service", "tick.timer")]))
}

fn sink<'a>(file: &Path, clock: &'a dyn Clock, proc: &'a dyn Proc) -> Sink<'a> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let seen = written_ids(&text);
    Sink { path: file.to_path_buf(), host: "h1".into(), instance: "inst-x".into(), clock, triggers: Triggers::new(proc), seen, scanned: text.len() as u64 }
}

fn rows(file: &Path) -> Vec<Value> {
    std::fs::read_to_string(file).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn by_inv(rows: &[Value]) -> BTreeMap<String, Value> {
    rows.iter().map(|r| (r["invocation_id"].as_str().unwrap().to_string(), r.clone())).collect()
}

fn follow_from(file: &Path, src: &Fake, cp: &RefCell<Folder>) -> usize {
    let (clock, sys) = (Fixed(1_792_100_000), systemctl());
    let mut s = sink(file, &clock, &sys);
    let start = cp.borrow().clone();
    let (f, n) = pass(src, start, true, &mut s, &mut |f| {
        *cp.borrow_mut() = f.clone();
        Ok(())
    })
    .unwrap();
    *cp.borrow_mut() = f;
    n
}

#[test]
fn one_record_per_invocation_with_the_right_result_and_duration() {
    let d = TempDir::new("unit-run-fold");
    let file = d.path().join("unit-run.jsonl");
    let cp = RefCell::new(Folder::default());
    follow_from(&file, &Fake { lines: fixture(), stop_after: None, idle_at_end: true }, &cp);
    let r = rows(&file);
    let m = by_inv(&r);
    assert_eq!(r.len(), 4, "ok, bad, tick, long — never the one still open: {r:?}");
    assert!(!m.contains_key("inv-open") && !m.contains_key("inv-later"));

    let ok = &m["inv-ok"];
    assert_eq!((ok["result"].as_str(), ok["unit"].as_str(), ok["kind"].as_str()), (Some("success"), Some("ok.service"), Some("service")));
    assert_eq!(ok["duration_ms"], 1500);
    assert_eq!(ok["exit_status"], 0);
    assert_eq!(ok["cpu_usec"], 2_500_000);
    assert_eq!(ok["memory_peak"], 1_048_576);
    assert_eq!((ok["host"].as_str(), ok["instance"].as_str(), ok["family"].as_str()), (Some("h1"), Some("inst-x"), Some("unit-run")));
    assert_eq!(ok["ts"].as_str(), Some(tsd::iso_utc(1_792_100_000).as_str()), "the producer's own clock");
    assert_eq!(ok["started_at"].as_str(), Some(tsd::iso_utc(T0 / S).as_str()));

    let bad = &m["inv-bad"];
    assert_eq!((bad["result"].as_str(), bad["exit_status"].as_i64(), bad["exit_code"].as_str()), (Some("exit-code"), Some(1), Some("exited")));
    assert_eq!(bad["duration_ms"], 2500);
    assert!(bad.get("io_read_bytes").is_none(), "no accounting line, no field");

    let tick = &m["inv-tick"];
    assert_eq!((tick["kind"].as_str(), tick["triggered_by"].as_str(), tick["result"].as_str()), (Some("timer-triggered"), Some("tick.timer"), Some("success")));
    assert_eq!(tick["duration_ms"], 3000);
    assert!(tick.get("cpu_usec").is_none());
}

#[test]
fn io_accounting_yields_both_byte_counts() {
    let d = TempDir::new("unit-run-io");
    let file = d.path().join("unit-run.jsonl");
    follow_from(&file, &Fake { lines: fixture(), stop_after: None, idle_at_end: true }, &RefCell::new(Folder::default()));
    let ok = by_inv(&rows(&file))["inv-ok"].clone();
    assert_eq!((ok["io_read_bytes"].as_u64(), ok["io_write_bytes"].as_u64()), (Some(4096), Some(8192)));
}

#[test]
fn a_follower_killed_at_any_line_neither_drops_nor_duplicates() {
    let d = TempDir::new("unit-run-kill");
    let whole = d.path().join("whole.jsonl");
    follow_from(&whole, &Fake { lines: fixture(), stop_after: None, idle_at_end: true }, &RefCell::new(Folder::default()));
    let want = rows(&whole);
    assert_eq!(want.len(), 4);
    for k in 0..=fixture().len() {
        let file = d.path().join(format!("k{k}.jsonl"));
        let cp = RefCell::new(Folder::default());
        follow_from(&file, &Fake { lines: fixture(), stop_after: Some(k), idle_at_end: false }, &cp);
        follow_from(&file, &Fake { lines: fixture(), stop_after: None, idle_at_end: true }, &cp);
        let mut got: Vec<_> = rows(&file).iter().map(|r| r["invocation_id"].as_str().unwrap().to_string()).collect();
        got.sort();
        assert_eq!(got, ["inv-bad", "inv-long", "inv-ok", "inv-tick"], "killed after line {k}");
        let m = by_inv(&rows(&file));
        for w in &want {
            let g = &m[w["invocation_id"].as_str().unwrap()];
            assert_eq!(g["duration_ms"], w["duration_ms"], "killed after line {k}");
            assert_eq!(g["result"], w["result"], "killed after line {k}");
        }
    }
}

#[test]
fn a_stale_checkpoint_replays_without_writing_twice() {
    let d = TempDir::new("unit-run-stale");
    let file = d.path().join("unit-run.jsonl");
    let early = RefCell::new(Folder::default());
    follow_from(&file, &Fake { lines: fixture(), stop_after: Some(3), idle_at_end: false }, &early);
    let full = RefCell::new(Folder::default());
    follow_from(&file, &Fake { lines: fixture(), stop_after: None, idle_at_end: true }, &full);
    let before = std::fs::read_to_string(&file).unwrap();
    assert_eq!(follow_from(&file, &Fake { lines: fixture(), stop_after: None, idle_at_end: true }, &early), 0);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn backfill_run_twice_yields_the_same_file() {
    let d = TempDir::new("unit-run-backfill");
    let file = d.path().join("unit-run.jsonl");
    let (clock, sys) = (Fixed(1_792_100_000), systemctl());
    let src = Fake { lines: fixture(), stop_after: None, idle_at_end: false };
    let first = pass(&src, Folder::default(), false, &mut sink(&file, &clock, &sys), &mut |_| Ok(())).unwrap().1;
    let once = std::fs::read_to_string(&file).unwrap();
    let second = pass(&src, Folder::default(), false, &mut sink(&file, &clock, &sys), &mut |_| Ok(())).unwrap().1;
    assert_eq!((first, second), (4, 0));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), once);
}

#[test]
fn an_execution_whose_start_predates_the_journal_is_written_without_a_duration() {
    let d = TempDir::new("unit-run-headless");
    let file = d.path().join("unit-run.jsonl");
    let lines = vec![line(1, T0, ID_FAILURE, "x.service", "inv-x", json!({"UNIT_RESULT": "timeout"}))];
    let (clock, sys) = (Fixed(1_792_100_000), systemctl());
    pass(&Fake { lines, stop_after: None, idle_at_end: false }, Folder::default(), false, &mut sink(&file, &clock, &sys), &mut |_| Ok(())).unwrap();
    let r = rows(&file);
    assert_eq!((r.len(), r[0]["result"].as_str()), (1, Some("timeout")));
    assert!(r[0].get("started_at").is_none() && r[0].get("duration_ms").is_none());
}

#[test]
fn lines_that_are_not_lifecycle_entries_are_ignored() {
    assert!(parse_entry("not json").is_none());
    assert!(parse_entry(&json!({"MESSAGE_ID": "00000000000000000000000000000000", "__CURSOR": "c", "__REALTIME_TIMESTAMP": "1", "USER_UNIT": "u", "USER_INVOCATION_ID": "i"}).to_string()).is_none());
    assert!(parse_entry(&line(1, T0, ID_STARTING, "u.service", "i", json!({}))).is_some(), "positive control");
    let mut no_inv: Value = serde_json::from_str(&line(1, T0, ID_STARTING, "u.service", "i", json!({}))).unwrap();
    no_inv.as_object_mut().unwrap().remove("USER_INVOCATION_ID");
    assert!(parse_entry(&no_inv.to_string()).is_none());
}

#[test]
fn io_accounting_dropin_is_written_once() {
    let d = TempDir::new("unit-run-acct");
    assert!(ensure_io_accounting(d.path()).unwrap());
    assert_eq!(std::fs::read_to_string(accounting_dropin_path(d.path())).unwrap(), ACCOUNTING_DROPIN);
    assert!(!ensure_io_accounting(d.path()).unwrap());
}

#[test]
fn a_restart_is_two_records() {
    let d = TempDir::new("unit-run-restart");
    let file = d.path().join("unit-run.jsonl");
    let mut lines = Vec::new();
    for (i, inv) in ["r1", "r2"].iter().enumerate() {
        let t = T0 + i as u64 * 10 * S;
        lines.push(line(i as u64 * 3 + 1, t, ID_STARTING, "r.service", inv, json!({})));
        lines.push(line(i as u64 * 3 + 2, t + S, ID_FAILURE, "r.service", inv, json!({"UNIT_RESULT": "signal"})));
        lines.push(line(i as u64 * 3 + 3, t + S + 5, ID_RESOURCES, "r.service", inv, json!({"CPU_USAGE_NSEC": "1000"})));
    }
    let (clock, sys) = (Fixed(1_792_100_000), systemctl());
    pass(&Fake { lines, stop_after: None, idle_at_end: false }, Folder::default(), false, &mut sink(&file, &clock, &sys), &mut |_| Ok(())).unwrap();
    assert_eq!(rows(&file).len(), 2);
}

fn real_journal_fixture() -> Vec<String> {
    let io = json!({"CPU_USAGE_NSEC": "1500000000", "MEMORY_PEAK": "1000", "IO_METRIC_READ_BYTES": "111", "IO_METRIC_WRITE_BYTES": "222"});
    vec![
        line(1, T0, ID_STARTING, "ok.service", "inv-ok", json!({"MESSAGE": "Starting ok.service - x..."})),
        line(2, T0 + 2 * S, ID_STARTED, "ok.service", "inv-ok", json!({"MESSAGE": "Finished ok.service - x.", "JOB_RESULT": "done", "JOB_TYPE": "start"})),
        line(3, T0 + 2 * S + 100, ID_RESOURCES, "ok.service", "inv-ok", io.clone()),
        line(4, T0 + 3 * S, ID_STARTING, "bad.service", "inv-bad", json!({"MESSAGE": "Starting bad.service - x..."})),
        line(5, T0 + 4 * S, ID_EXIT, "bad.service", "inv-bad", json!({"EXIT_CODE": "exited", "EXIT_STATUS": "1", "COMMAND": "ExecStart"})),
        line(6, T0 + 4 * S + 10, ID_FAILURE, "bad.service", "inv-bad", json!({"UNIT_RESULT": "exit-code"})),
        line(7, T0 + 4 * S + 20, ID_STARTED, "bad.service", "inv-bad", json!({"MESSAGE": "Failed to start bad.service - x.", "JOB_RESULT": "failed", "JOB_TYPE": "start"})),
        line(8, T0 + 4 * S + 30, ID_RESOURCES, "bad.service", "inv-bad", io.clone()),
        line(9, T0 + 5 * S, ID_STARTED, "long.service", "inv-long1", json!({"MESSAGE": "Started long.service - x.", "JOB_RESULT": "done", "JOB_TYPE": "start"})),
        line(10, T0 + 15 * S, ID_FAILURE, "long.service", "inv-long1", json!({"UNIT_RESULT": "signal"})),
        line(11, T0 + 15 * S + 10, ID_RESOURCES, "long.service", "inv-long1", io),
        line(12, T0 + 25 * S, ID_STARTED, "long.service", "inv-long2", json!({"MESSAGE": "Started long.service - x.", "JOB_RESULT": "done", "JOB_TYPE": "start"})),
    ]
}

#[test]
fn a_journal_of_a_success_a_failure_and_a_restart_yields_exactly_three_records() {
    let d = TempDir::new("unit-run-real");
    let file = d.path().join("unit-run.jsonl");
    let (clock, sys) = (Fixed(1_792_100_000), systemctl());
    pass(&Fake { lines: real_journal_fixture(), stop_after: None, idle_at_end: false }, Folder::default(), false, &mut sink(&file, &clock, &sys), &mut |_| Ok(())).unwrap();
    let m = by_inv(&rows(&file));
    assert_eq!(m.len(), 3, "{m:?}");
    let ok = &m["inv-ok"];
    assert_eq!((ok["result"].as_str(), ok["duration_ms"].as_u64()), (Some("success"), Some(2000)));
    let bad = &m["inv-bad"];
    assert_eq!((bad["result"].as_str(), bad["exit_status"].as_i64()), (Some("exit-code"), Some(1)));
    let long = &m["inv-long1"];
    assert_eq!((long["result"].as_str(), long["duration_ms"].as_u64()), (Some("signal"), Some(10_000)));
    for r in m.values() {
        assert_eq!((r["io_read_bytes"].as_u64(), r["io_write_bytes"].as_u64()), (Some(111), Some(222)));
    }
}

#[test]
fn io_controller_presence_is_read_from_the_controllers_file() {
    let d = TempDir::new("unit-run-ctl");
    let f = d.path().join("cgroup.controllers");
    std::fs::write(&f, "cpu memory pids\n").unwrap();
    assert!(!io_controller_available(&f));
    std::fs::write(&f, "cpu io memory pids\n").unwrap();
    assert!(io_controller_available(&f));
    assert!(!io_controller_available(&d.path().join("absent")));
}

#[test]
fn two_sinks_on_one_file_write_each_execution_once() {
    let d = TempDir::new("unit-run-twosinks");
    let file = d.path().join("unit-run.jsonl");
    let (clock, sys) = (Fixed(1_792_100_000), systemctl());
    let src = Fake { lines: fixture(), stop_after: None, idle_at_end: false };
    let (mut a, mut b) = (sink(&file, &clock, &sys), sink(&file, &clock, &sys));
    let first = pass(&src, Folder::default(), false, &mut a, &mut |_| Ok(())).unwrap().1;
    let second = pass(&src, Folder::default(), false, &mut b, &mut |_| Ok(())).unwrap().1;
    assert_eq!((first, second), (4, 0));
    assert_eq!(rows(&file).len(), 4);
}

#[test]
fn follow_never_reads_the_journal_from_its_start() {
    let with = journal_args(Some("c0042"), true);
    assert!(with.contains(&"--after-cursor=c0042".to_string()));
    let none = journal_args(None, true);
    let n = none.iter().position(|a| a == "-n").expect("no cursor must bound the read");
    assert_eq!(none[n + 1], "0");
    assert!(!journal_args(None, false).iter().any(|a| a == "-n" || a.starts_with("--after-cursor")), "positive control: backfill is the unbounded read");
}

#[test]
fn follow_does_not_spawn_a_backfill() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs")).unwrap();
    let body = src.split("fn follow()").nth(1).unwrap().split("\nfn ").next().unwrap();
    assert!(!body.contains("backfill("));
}

#[test]
fn unit_file_fences_io() {
    let u = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../systemd/spira-unit-run.service")).unwrap();
    assert!(u.lines().any(|l| l.starts_with("IOReadBandwidthMax=")));
    assert!(u.lines().any(|l| l.starts_with("IOWeight=")));
}
