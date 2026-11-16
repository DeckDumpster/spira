//! The classifier through `heimdall classify`'s own entry (`classify_stdin`): a healthy world
//! with one thing changed per case, so a firing key is attributable to that one thing.

use crate::classify::classify_stdin;
use crate::judge::iso_to_epoch;
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000;

fn obs(log: &str, over: Value) -> Value {
    let mut o = json!({
        "now": NOW, "heimdall_first": 0,
        "sentinel_log": log, "sentinel_log_readable": true, "sentinel_log_error": "",
        "sentinel_log_mtime": NOW - 30, "sentinel_log_path": "run/sentinel.log", "sentinel_timer": "active",
        "db_reachable": true, "db_error": "", "db_path": "db", "fallback_path": "run/a.json",
        "mirror": {"configured": false}, "strands": {},
        "restart_alerts": [],
        "thresholds": {"pass_stale": 600, "starve_passes": 5, "mirror_stale": 90000, "ghost_stale": 1800,
                       "restart_threshold": 5, "restart_window": 3600},
    });
    for (k, v) in over.as_object().unwrap() {
        o[k] = v.clone();
    }
    o
}

fn firing(o: &Value) -> Vec<(String, String)> {
    let out = classify_stdin(&o.to_string()).unwrap();
    let mut got: Vec<(String, String)> = out
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            (v["key"].as_str().unwrap().to_string(), v["evidence"].as_str().unwrap().to_string())
        })
        .collect();
    got.sort();
    got
}

fn keys(o: &Value) -> Vec<String> {
    firing(o).into_iter().map(|(k, _)| k).collect()
}

fn evidence(o: &Value, key: &str) -> String {
    firing(o).into_iter().find(|(k, _)| k == key).map(|(_, e)| e).unwrap_or_default()
}

fn stamp(t: i64) -> String {
    let days = t.div_euclid(86400);
    let s = t.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// `passes` passes two minutes apart, the last starting at `end`, in the format the sentinel writes.
fn mklog(passes: i64, ready: i64, dwarves: i64, called: bool, complete: bool, end: i64, extra: &str) -> String {
    let mut out = String::new();
    for i in 0..passes {
        let ts = stamp(end - (passes - 1 - i) * 120);
        out += &format!("{ts} spira: state: open=20 plan_ready={ready} in_progress=0 dwarves={dwarves} runes=[builder ]\n");
        if !extra.is_empty() {
            out += &format!("{ts} spira: {extra}\n");
        }
        if called {
            out += &format!("{ts} spira: CHECK7 builder: {ready} ready, 1 free \u{2014} calling\n");
        }
        if complete {
            out += &format!("{ts} spira: pass complete \u{2014} 0 action(s), 0 progress\n");
        }
    }
    out
}

fn healthy() -> String {
    mklog(10, 5, 1, true, true, NOW - 60, "")
}

fn replay(name: &str) -> (String, i64) {
    let text = std::fs::read_to_string(spira_config::resolve::locate_home_for_process().unwrap().join(format!("testdata/{name}"))).unwrap();
    let last = text
        .lines()
        .filter(|l| l.len() > 20 && l.as_bytes()[10] == b'T' && l.as_bytes()[19] == b'Z')
        .map(|l| iso_to_epoch(&l[..19]))
        .max()
        .unwrap();
    (text, last + 60)
}

// covers: UC-ops-detection-remediation-18
#[test]
fn a_healthy_loop_fires_nothing_and_neither_does_a_replay_of_real_passes() {
    assert_eq!(keys(&obs(&healthy(), json!({}))), Vec::<String>::new());
    let (text, now) = replay("sentinel-healthy.log");
    assert!(text.lines().count() > 90, "the noise floor is unmeasured without its fixture");
    assert_eq!(keys(&obs(&text, json!({"now": now}))), Vec::<String>::new());
    let (text, now) = replay("sentinel-pre-check7.log");
    assert!(!keys(&obs(&text, json!({"now": now}))).contains(&"call-starved".to_string()), "the rule inferred from an absent line");
}

// covers: UC-ops-detection-remediation-18
#[test]
fn passes_that_never_finish_and_a_dead_timer_raise_sentinel_stalled_naming_which() {
    let wedged = obs(&mklog(10, 5, 0, false, false, NOW - 60, ""), json!({"heimdall_first": NOW - 86400}));
    assert_eq!(keys(&wedged), ["sentinel-stalled"]);
    assert!(evidence(&wedged, "sentinel-stalled").contains("wedged mid-flight"));

    let dead = obs(&mklog(10, 5, 0, false, true, NOW - 4000, ""), json!({"sentinel_timer": "inactive", "sentinel_log_mtime": NOW - 4000}));
    assert_eq!(keys(&dead), ["sentinel-stalled"]);
    assert!(evidence(&dead, "sentinel-stalled").contains("is not being run at all"));
}

// covers: UC-ops-detection-remediation-18
#[test]
fn a_tail_with_no_completed_pass_is_measured_from_the_later_of_its_start_and_heimdalls_first_run() {
    let nodone = mklog(10, 5, 0, false, false, NOW - 60, "");
    assert_eq!(keys(&obs(&nodone, json!({"heimdall_first": NOW - 60}))), Vec::<String>::new(), "no evidence yet, and Heimdall only just started");
    assert_eq!(keys(&obs(&nodone, json!({"heimdall_first": NOW - 86400}))), ["sentinel-stalled"]);
    let seen_end = obs(&mklog(10, 5, 0, false, true, NOW - 4000, ""), json!({"heimdall_first": NOW - 60, "sentinel_log_mtime": NOW - 4000}));
    assert_eq!(keys(&seen_end), ["sentinel-stalled"], "a stall whose end Heimdall can see fires however new Heimdall is");
}

// covers: UC-ops-detection-remediation-18
#[test]
fn an_unreadable_log_is_the_alert_not_a_clear() {
    let o = obs("", json!({"sentinel_log_readable": false, "sentinel_log_error": "no such file"}));
    assert_eq!(keys(&o), ["sentinel-unobservable"]);
}

// covers: UC-ops-detection-remediation-18
#[test]
fn halt_or_drain_suppresses_sentinel_stalled_but_only_because_the_same_world_fires_without_it() {
    let dead = mklog(10, 5, 0, false, true, NOW - 4000, "");
    let stopped = json!({"sentinel_timer": "inactive", "sentinel_log_mtime": NOW - 3900});
    assert_eq!(keys(&obs(&dead, stopped.clone())), ["sentinel-stalled"], "positive control");
    let mut halted = stopped.clone();
    halted["world_halted"] = json!(true);
    halted["world_halt_at"] = json!(NOW - 900);
    assert_eq!(keys(&obs(&dead, halted)), Vec::<String>::new());
    assert_eq!(keys(&obs(&dead, json!({"world_draining": true, "world_drain_at": NOW - 300}))), Vec::<String>::new());
}

// covers: UC-ops-detection-remediation-18
#[test]
fn call_starvation_needs_sustained_passes_and_excludes_withholds_and_dwarves_at_cap() {
    let starved = |n, ready, dwarves, extra: &str| obs(&mklog(n, ready, dwarves, false, true, NOW - 60, extra), json!({}));
    assert_eq!(keys(&starved(6, 15, 0, "")), ["call-starved"]);
    assert_eq!(keys(&starved(4, 15, 0, "")), Vec::<String>::new(), "four passes is not sustained");
    let capacity = "CHECK7 builder: the account is out of capacity for another 900s \u{2014} not calling";
    assert_eq!(keys(&starved(6, 15, 0, capacity)), Vec::<String>::new(), "a deliberate withhold");
    assert_eq!(keys(&starved(6, 15, 2, "")), Vec::<String>::new(), "dwarves at the cap is throughput");

    let stale = obs(&mklog(6, 15, 0, false, true, NOW - 4000, ""), json!({"sentinel_log_mtime": NOW - 4000}));
    assert_eq!(keys(&stale), ["sentinel-stalled"], "a stale window is stalled, not also starved");

    let inflight = format!("{}{}", mklog(5, 15, 1, true, true, NOW - 180, ""), mklog(1, 15, 0, false, false, NOW - 20, ""));
    assert_eq!(keys(&obs(&inflight, json!({}))), Vec::<String>::new(), "the pass in flight has not reached its call check");
}

// covers: UC-ops-detection-remediation-18
#[test]
fn database_mirror_ghost_and_restart_alerts_are_distinct_and_do_not_mask_each_other() {
    let h = healthy();
    assert_eq!(keys(&obs(&h, json!({"db_reachable": false, "db_error": "refused"}))), ["db-unreachable"]);

    assert!(keys(&obs(&h, json!({"mirror": {"configured": false}}))).is_empty());
    assert_eq!(keys(&obs(&h, json!({"mirror": {"configured": true, "exists": false, "path": "m", "exporter": "e"}}))), ["mirror-stale"]);
    let mirror = |mtime: i64| json!({"mirror": {"configured": true, "exists": true, "mtime": mtime, "path": "m", "exporter": "e"}});
    assert!(keys(&obs(&h, mirror(NOW - 3600))).is_empty());
    assert_eq!(keys(&obs(&h, mirror(NOW - 200_000))), ["mirror-stale"]);

    let ghost = |first: i64| json!({"strands": {"ghost:sp-a": {"first": first, "acted": 1, "escalated": 1}}});
    assert!(keys(&obs(&h, ghost(NOW - 1000))).is_empty(), "a fresh ghost is the reaper's business");
    assert_eq!(keys(&obs(&h, ghost(NOW - 10_000))), ["lease-unreclaimed"]);
    assert!(keys(&obs(&h, json!({"strands": {"waiting:sp-e": {"first": 1}}}))).is_empty());

    let ra = |unit: &str, n: i64| json!({"unit": unit, "current": n, "baseline": 0, "delta": n, "window": 3600});
    let one = obs(&h, json!({"restart_alerts": [ra("a.service", 255)]}));
    assert_eq!(keys(&one), ["restart-loop:a.service"]);
    let ev = evidence(&one, "restart-loop:a.service");
    assert!(ev.contains("255") && ev.contains("a.service") && ev.contains("incident intake misses it"), "{ev}");
    assert!(keys(&obs(&h, json!({"restart_alerts": [ra("a.service", 3)]}))).is_empty());
    assert_eq!(keys(&obs(&h, json!({"restart_alerts": [ra("a.service", 50), ra("b.service", 20)]}))), ["restart-loop:a.service", "restart-loop:b.service"]);

    let wedged = mklog(10, 5, 0, false, false, NOW - 60, "");
    let both = obs(&wedged, json!({"heimdall_first": NOW - 86400, "db_reachable": false, "db_error": "x"}));
    assert_eq!(keys(&both), ["db-unreachable", "sentinel-stalled"]);
}

// covers: UC-ops-detection-remediation-18
#[test]
fn the_older_ready_spelling_still_parses_and_untimestamped_lines_are_skipped() {
    let old = mklog(6, 15, 0, false, true, NOW - 60, "").replace("plan_ready=", "ready=");
    assert_eq!(keys(&obs(&old, json!({}))), ["call-starved"]);
    let noisy = format!("Auto-merging spira/sentinel.sh\nREAPED sp-x  branch and worktree\n{}", healthy());
    assert_eq!(keys(&obs(&noisy, json!({}))), Vec::<String>::new());
}
