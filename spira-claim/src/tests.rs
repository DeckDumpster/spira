//! CLI-level tests: the contract at the interface (exit codes, stdout shapes), with every
//! store read replaced by a file (`--events`, `--ready`, `--epics`, `--lifecycle`,
//! `--blocker-records`) or by a fake `bd` script driven through [`Store`] directly.

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// The lifecycle switch as `select` sees it in this test's thread (main.rs `lifecycle_on`).
    pub static ENFORCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn enforce(on: bool) {
    ENFORCE.with(|c| c.set(on));
}

/// A file's path, and the scratch dir that holds it: the file is removed when this drops,
/// so hold it for as long as anything reads the path (sp-qgfdi).
struct Tmp {
    _dir: testkit::TempDir,
    path: String,
}

impl std::ops::Deref for Tmp {
    type Target = String;
    fn deref(&self) -> &String {
        &self.path
    }
}

impl std::fmt::Display for Tmp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.path)
    }
}

impl AsRef<std::path::Path> for Tmp {
    fn as_ref(&self) -> &std::path::Path {
        std::path::Path::new(&self.path)
    }
}

fn tmp(content: &str) -> Tmp {
    let dir = testkit::TempDir::new("spira-claim-test");
    let p: PathBuf = dir.join(format!("f{}", N.fetch_add(1, Ordering::SeqCst)));
    std::fs::write(&p, content).unwrap();
    Tmp { path: p.to_string_lossy().into_owned(), _dir: dir }
}

/// An executable in the same scratch directory, written without an ETXTBSY race
/// (testkit::write_exe — DESIGN.md there).
fn tmp_exe(content: &str) -> Tmp {
    let dir = testkit::TempDir::new("spira-claim-test");
    let p: PathBuf = dir.join(format!("x{}", N.fetch_add(1, Ordering::SeqCst)));
    testkit::write_exe(&p, content);
    Tmp { path: p.to_string_lossy().into_owned(), _dir: dir }
}

fn run(args: &[&str], stdin: &str) -> Outcome {
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    dispatch(&a, &mut stdin.as_bytes())
}

fn ev(id: &str, t: &str, v: &str, at: &str) -> serde_json::Value {
    serde_json::json!({"issue_id": id, "event_type": t, "new_value": v, "created_at": at})
}

fn events_file(evs: &[serde_json::Value]) -> Tmp {
    tmp(&serde_json::to_string(evs).unwrap())
}

/// n failed claims (claimed, then claimed again …) for `id`, one minute apart from `h`:00.
fn claims(id: &str, n: usize, hour: u32) -> Vec<serde_json::Value> {
    (0..n).map(|i| ev(id, "claimed", "", &format!("2026-09-28T{hour:02}:{i:02}:00Z"))).collect()
}

#[test]
fn attempts_null_safe_with_no_events() {
    let f = events_file(&[]);
    let o = run(&["attempts", "sp-a", "--events", &f], "");
    assert_eq!((o.code, o.out.as_str()), (0, "0\n"));
}

#[test]
fn attempts_null_safe_right_after_clear() {
    let mut e = claims("sp-a", 3, 10);
    e.push(ev("sp-a", "poison.cleared", "operator", "2026-09-28T11:00:00Z"));
    let o = run(&["attempts", "sp-a", "--events", &events_file(&e)], "");
    assert_eq!((o.code, o.out.as_str()), (0, "0\n"));
}

#[test]
fn attempts_json_ledger_names_each_attempt() {
    let o = run(&["attempts", "sp-a", "--events", &events_file(&claims("sp-a", 2, 10)), "--json"], "");
    assert_eq!(o.code, 0);
    let v: serde_json::Value = serde_json::from_str(&o.out).unwrap();
    assert_eq!(v["attempts"], 2);
    assert_eq!(v["attempt_log"].as_array().unwrap().len(), 2);
    assert_eq!(v["attempt_log"][1]["outcome"], "charged");
}

#[test]
fn events_read_failure_is_cannot_tell_with_empty_stdout() {
    let o = run(&["attempts", "sp-a", "--events", "/nonexistent/events.json"], "");
    assert_eq!(o.code, CANNOT_TELL);
    assert!(o.out.is_empty());
    let o = run(&["attempts", "sp-a", "--events", &tmp("Error: bd schema mismatch")], "");
    assert_eq!(o.code, CANNOT_TELL);
    assert!(o.out.is_empty());
    let o = run(&["attempts", "sp-a", "--events", &tmp("")], "");
    assert_eq!(o.code, CANNOT_TELL, "empty output is not zero rows");
}

#[test]
fn requeues_verb_rebase_vs_red() {
    // sp-j1q6o's test: three rebase returns then submitted — below any cap; three reds — 3.
    let mut e = Vec::new();
    for (i, cause) in ["rebase-conflict", "eject", "round-122-conflict-returned"].iter().enumerate() {
        e.push(ev("sp-r", "reopened", "", &format!("2026-09-28T1{i}:00:00Z")));
        e.push(ev("sp-r", "reopen", "work-close-converted", &format!("2026-09-28T1{i}:00:01Z")));
        e.push(ev("sp-r", "reopen", cause, &format!("2026-09-28T1{i}:30:00Z")));
    }
    let o = run(&["requeues", "sp-r", "--events", &events_file(&e)], "");
    assert_eq!(o.out, "0\n");
    let mut e = Vec::new();
    for (i, cause) in ["gate-red", "cert-gate-red", "batch-eject"].iter().enumerate() {
        e.push(ev("sp-r", "reopened", "", &format!("2026-09-28T1{i}:00:00Z")));
        e.push(ev("sp-r", "reopen", cause, &format!("2026-09-28T1{i}:00:01Z")));
    }
    let o = run(&["requeues", "sp-r", "--events", &events_file(&e)], "");
    assert_eq!(o.out, "3\n");
}

#[test]
fn rebase_returns_raise_no_ask_red_returns_do() {
    // End to end through poison-decide with REQUEUE_AT=3.
    let mut rebase = Vec::new();
    let mut red = Vec::new();
    for i in 0..3 {
        rebase.push(ev("sp-q", "reopen", "rebase-conflict", &format!("2026-09-28T1{i}:00:00Z")));
        red.push(ev("sp-q", "reopen", "gate-red", &format!("2026-09-28T1{i}:00:00Z")));
    }
    let o = run(&["poison-decide", "sp-q", "--requeue-at", "3", "--events", &events_file(&rebase)], "");
    assert_eq!(o.out, "none\n");
    let o = run(&["poison-decide", "sp-q", "--requeue-at", "3", "--events", &events_file(&red)], "");
    assert_eq!(o.out, "requeue-mail\n");
}

#[test]
fn poison_decide_threshold_and_clear() {
    let at = events_file(&claims("sp-p", 3, 10));
    assert_eq!(run(&["poison-decide", "sp-p", "--events", &at], "").out, "poison ask\n");
    assert_eq!(run(&["poison-decide", "sp-p", "--events", &at, "--asked", "0:0:1:0"], "").out, "poison\n");
    assert_eq!(run(&["poison-decide", "sp-p", "--events", &at, "--poisoned", "1", "--asked", "1:1:1"], "").out, "none\n");
    let below = events_file(&claims("sp-p", 2, 10));
    assert_eq!(run(&["poison-decide", "sp-p", "--events", &below], "").out, "none\n");
    assert_eq!(run(&["poison-decide", "sp-p", "--events", &below, "--poisoned", "1", "--asked", "1:1:1"], "").out, "clear\n");
    let o = run(&["poison-decide", "sp-p", "--events", &at, "--json"], "");
    let v: serde_json::Value = serde_json::from_str(&o.out).unwrap();
    assert_eq!(v["attempts"], 3);
    assert_eq!(v["decision"], serde_json::json!(["poison", "ask"]));
    assert_eq!(run(&["poison-decide", "sp-p", "--events", &at, "--poisoned", "yes"], "").code, USAGE);
}

#[test]
fn poison_decide_fetch_failure_decides_nothing() {
    let o = run(&["poison-decide", "sp-p", "--events", "/nonexistent"], "");
    assert_eq!((o.code, o.out.as_str()), (CANNOT_TELL, ""));
}

#[test]
fn decide_matches_check4_decide_call_shapes() {
    // sentinel main loop
    assert_eq!(run(&["decide", "3", "0", "0", "plan,repo:spira", "0:0:0:0", "0"], "").out, "poison ask\n");
    // closed-bead supplement: check4_decide 0 "$_requeues" 0 "$_labels" "$_rq_asked:1:1"
    assert_eq!(run(&["decide", "0", "5", "0", "plan", "0:1:1"], "").out, "requeue-mail\n");
    // stale-poison-clear scan: check4_decide "$n" 0 0 "" "1:1:1" 1
    assert_eq!(run(&["decide", "1", "0", "0", "", "1:1:1", "1"], "").out, "clear\n");
    // unpoison.sh: empty stamp
    assert_eq!(run(&["decide", "0", "0", "0", "", "", "0"], "").out, "none\n");
    // thresholds as flags, before `--`
    assert_eq!(run(&["decide", "--poison-at", "0", "--", "1", "0", "0"], "").out, "poison ask\n");
    assert_eq!(run(&["decide", "x", "0", "0"], "").code, USAGE);
    assert_eq!(run(&["decide", "1"], "").code, USAGE);
}

#[test]
fn counts_every_id_in_input_order_with_zeros() {
    let mut e = claims("sp-b", 3, 10);
    e.push(ev("sp-a", "reclaimed", "", "2026-09-28T10:00:00Z"));
    e.push(ev("sp-a", "reopened", "", "2026-09-28T10:10:00Z"));
    let o = run(&["counts", "--events", &events_file(&e)], "sp-b\tplan,repo:spira\nsp-a\tplan\n\nsp-none\nsp-b\tdup\n");
    assert_eq!(o.code, 0);
    assert_eq!(o.out, "sp-b\t3\t0\t0\nsp-a\t0\t1\t1\nsp-none\t0\t0\t0\n");
    assert_eq!(run(&["counts", "--events", &events_file(&e)], "").out, "");
    assert_eq!(run(&["counts", "--events", "/nonexistent"], "sp-a\n").code, CANNOT_TELL);
}

#[test]
fn usage_errors() {
    assert_eq!(run(&[], "").code, USAGE);
    assert_eq!(run(&["frobnicate"], "").code, USAGE);
    assert_eq!(run(&["attempts"], "").code, USAGE);
    assert_eq!(run(&["attempts", "a", "b"], "").code, USAGE);
    assert_eq!(run(&["attempts", "a", "--bogus", "1"], "").code, USAGE);
    assert_eq!(run(&["select"], "[]").code, USAGE, "--fayth is required");
    assert_eq!(run(&["select", "--fayth", "t", "--count", "--json"], "[]").code, USAGE);
    assert_eq!(run(&["select", "--fayth", "t", "--blockers", "maybe"], "[]").code, USAGE);
}

// ---- select --------------------------------------------------------------------------

fn ready_json() -> String {
    serde_json::json!([
        {"id":"sp-u1","priority":0,"created_at":"2026-09-01T00:00:00Z","labels":["repo:spira","branch:spira/sp-u1"]},
        {"id":"sp-k1","priority":2,"parent":"sp-E","created_at":"2026-09-02T00:00:00Z","labels":["repo:spira"]},
        {"id":"sp-k2","priority":1,"parent":"sp-E","created_at":"2026-09-03T00:00:00Z","labels":["repo:spira"]},
        {"id":"sp-k3","priority":1,"parent":"sp-E","created_at":"2026-09-01T00:00:00Z","labels":["repo:spira"]},
        {"id":"sp-epic","priority":0,"issue_type":"epic","labels":[]}
    ])
    .to_string()
}

#[test]
fn select_ranks_epic_first_from_stdin() {
    let lk = tmp(r#"{"prio":{"sp-E":0},"started":["sp-E"]}"#);
    let o = run(&["select", "--fayth", "t", "--epics", &lk], &ready_json());
    assert_eq!(o.code, 0, "{}", o.err);
    let ids: Vec<&str> = o.out.lines().map(|l| l.split('\t').nth(5).unwrap()).collect();
    // epic P0 started: k3 (P1, older), k2 (P1), k1 (P2); then the unaffiliated P0 (unstarted).
    assert_eq!(ids, ["sp-k3", "sp-k2", "sp-k1", "sp-u1"]);
    assert_eq!(o.out.lines().next().unwrap(), "0\t0\t1\t1\t2026-09-01T00:00:00Z\tsp-k3\tsp-E");
}

#[test]
fn select_resumable_breaks_ties_within_tier() {
    let lk = tmp(r#"{"prio":{"sp-E":0},"started":["sp-E"]}"#);
    let rs = tmp("sp-k2\n");
    let o = run(&["select", "--fayth", "t", "--epics", &lk, "--resumable", &rs], &ready_json());
    assert_eq!(o.out.lines().next().unwrap().split('\t').nth(5), Some("sp-k2"));
    let rs = tmp("sp-k2,sp-k3");
    let o = run(&["select", "--fayth", "t", "--epics", &lk, "--resumable", &rs], &ready_json());
    assert_eq!(o.out.lines().next().unwrap().split('\t').nth(5), Some("sp-k3"));
}

#[test]
fn select_top_tier_and_count() {
    let lk = tmp(r#"{"prio":{"sp-E":0},"started":["sp-E"]}"#);
    let o = run(&["select", "--fayth", "t", "--epics", &lk, "--top-tier"], &ready_json());
    assert_eq!(o.out, "sp-k2||spira|0|0|1\nsp-k3||spira|0|0|1\n");
    let o = run(&["select", "--fayth", "t", "--count"], &ready_json());
    assert_eq!(o.out, "4\n", "the epic row is never counted");
    let o = run(&["select", "--fayth", "t", "--epics", &lk, "--json"], &ready_json());
    let v: serde_json::Value = serde_json::from_str(&o.out).unwrap();
    assert_eq!(v[0]["id"], "sp-k3");
}

#[test]
fn select_empty_ready_set_is_empty_not_error() {
    for input in ["", "[]", "null"] {
        let o = run(&["select", "--fayth", "t", "--epics", &tmp("{}")], input);
        assert_eq!((o.code, o.out.as_str()), (0, ""), "input {input:?}");
    }
    // no parents at all: no lookup is needed, so none is fetched
    let o = run(&["select", "--fayth", "t"], r#"[{"id":"a","priority":1}]"#);
    assert_eq!(o.code, 0, "{}", o.err);
}

#[test]
fn select_malformed_ready_set_is_cannot_tell() {
    let o = run(&["select", "--fayth", "t", "--epics", &tmp("{}")], "not json");
    assert_eq!((o.code, o.out.as_str()), (CANNOT_TELL, ""));
    let o = run(&["select", "--fayth", "t", "--epics", &tmp("{broken")], "[]");
    assert_eq!(o.code, CANNOT_TELL);
}

#[test]
fn select_reads_a_ready_set_over_128k_from_stdin_and_file() {
    let mut rows = Vec::new();
    for i in 0..2500 {
        rows.push(serde_json::json!({
            "id": format!("sp-{i:05}"), "priority": i % 5, "parent": format!("sp-E{}", i % 9),
            "created_at": "2026-09-01T00:00:00Z", "labels": ["repo:spira", "plan"],
            "description": "d".repeat(150),
        }));
    }
    let big = serde_json::to_string(&rows).unwrap();
    assert!(big.len() > 128 * 1024, "{}", big.len());
    let lk = tmp(r#"{"prio":{},"started":[]}"#);
    let o = run(&["select", "--fayth", "t", "--epics", &lk], &big);
    assert_eq!((o.code, o.out.lines().count()), (0, 2500));
    let f = tmp(&big);
    let o = run(&["select", "--fayth", "t", "--epics", &lk, "--ready", &f, "--count"], "");
    assert_eq!(o.out, "2500\n");
}

// ---- select --blockers machine: the stacked-dependents parity fixture ------------------

/// The fixture graph from sp-f0qhr's acceptance: A certified → B stacked on it; C blocked
/// on a merely-submitted D; F blocked on an open epic G; H stacked on a depth-4 certified I;
/// J with a poison hold; K with no lifecycle row.
fn machine_fixture() -> (String, Tmp, Tmp) {
    let blocks = |id: &str, on: &str| serde_json::json!({"issue_id": id, "depends_on_id": on, "type": "blocks"});
    let ready = serde_json::json!([
        {"id":"B","priority":1,"labels":["repo:spira"],"dependencies":[blocks("B","A")]},
        {"id":"C","priority":1,"labels":["repo:spira"],"dependencies":[blocks("C","D")]},
        {"id":"F","priority":1,"labels":["repo:spira"],"dependencies":[blocks("F","G")]},
        {"id":"H","priority":1,"labels":["repo:spira"],"dependencies":[blocks("H","I")]},
        {"id":"J","priority":0,"labels":["repo:spira"]},
        {"id":"K","priority":0,"labels":["repo:spira"]},
        {"id":"L","priority":2,"labels":["repo:spira"]}
    ])
    .to_string();
    let lc = serde_json::json!([
        {"bead_id":"A","state":"CERTIFIED","holds":"[]","stack_depth":"0"},
        {"bead_id":"B","state":"READY","holds":"[\"wait\"]"},
        {"bead_id":"C","state":"READY","holds":"[]"},
        {"bead_id":"D","state":"SUBMITTED","holds":"[]"},
        {"bead_id":"F","state":"READY","holds":"[]"},
        {"bead_id":"H","state":"READY","holds":"[]"},
        {"bead_id":"I","state":"CERTIFIED","holds":"[]","stack_depth":"4"},
        {"bead_id":"J","state":"READY","holds":"[\"poison\"]"},
        {"bead_id":"L","state":"REWORK","holds":"[]"}
    ])
    .to_string();
    let recs = serde_json::json!([
        {"id":"A","status":"open","issue_type":"task","labels":["repo:spira"]},
        {"id":"D","status":"open","issue_type":"task","labels":["repo:spira"]},
        {"id":"G","status":"open","issue_type":"epic","labels":["repo:spira"]},
        {"id":"I","status":"open","issue_type":"task","labels":["repo:spira"]}
    ])
    .to_string();
    (ready, tmp(&lc), tmp(&recs))
}

#[test]
fn machine_mode_count_and_claim_agree_on_the_fixture() {
    enforce(true);
    let (ready, lc, recs) = machine_fixture();
    let base = ["select", "--fayth", "t", "--blockers", "machine", "--lifecycle", &lc, "--blocker-records", &recs];
    let mut ranked_args = base.to_vec();
    let lk = tmp("{}");
    ranked_args.extend(["--epics", lk.as_str()]);
    let ranked = run(&ranked_args, &ready);
    assert_eq!(ranked.code, 0, "{}", ranked.err);
    let claim_ids: Vec<&str> = ranked.out.lines().map(|l| l.split('\t').nth(5).unwrap()).collect();
    // B (stacked on certified A) and L (REWORK) only.
    assert_eq!(claim_ids, ["B", "L"]);
    let mut count_args = base.to_vec();
    count_args.push("--count");
    assert_eq!(run(&count_args, &ready).out, format!("{}\n", claim_ids.len()));
}

#[test]
fn machine_mode_stack_max_depth_zero_is_todays_rule() {
    enforce(true);
    let (ready, lc, recs) = machine_fixture();
    let o = run(
        &["select", "--fayth", "t", "--blockers", "machine", "--lifecycle", &lc, "--blocker-records", &recs,
          "--stack-max-depth", "0", "--epics", &tmp("{}")],
        &ready,
    );
    let ids: Vec<&str> = o.out.lines().map(|l| l.split('\t').nth(5).unwrap()).collect();
    assert_eq!(ids, ["L"]);
    // A ceiling above 4 is clamped to 4, never honoured.
    let o = run(
        &["select", "--fayth", "t", "--blockers", "machine", "--lifecycle", &lc, "--blocker-records", &recs,
          "--stack-max-depth", "9", "--count"],
        &ready,
    );
    assert_eq!(o.out, "2\n", "H at depth 5 stays held even when 9 is asked for");
}

#[test]
fn machine_mode_bad_snapshot_is_cannot_tell() {
    enforce(true);
    let (ready, _, recs) = machine_fixture();
    let o = run(&["select", "--fayth", "t", "--blockers", "machine", "--lifecycle", &tmp(""), "--blocker-records", &recs], &ready);
    assert_eq!((o.code, o.out.as_str()), (CANNOT_TELL, ""));
}

#[test]
fn bd_mode_does_not_refilter() {
    // bd ready already applied its blocker rule; nothing is dropped here but epics.
    let (ready, _, _) = machine_fixture();
    let o = run(&["select", "--fayth", "t", "--count"], &ready);
    assert_eq!(o.out, "7\n");
}

// ---- the lifecycle switch (DESIGN.md §6a) ------------------------------------------------

#[test]
fn off_machine_mode_is_the_legacy_rule_and_never_reads_the_lifecycle() {
    enforce(false);
    let (ready, _, recs) = machine_fixture();
    // No --lifecycle: were the snapshot read, the store's spira-lc would be run and fail
    // ("cannot tell"). A garbage --lifecycle file: were it read, parsing would fail. Both
    // answer, so neither source was consulted.
    let garbage = tmp("not json");
    let epics = tmp("{}");
    for extra in [vec![], vec!["--lifecycle".to_string(), garbage.to_string()]] {
        let mut args: Vec<String> =
            ["select", "--fayth", "t", "--blockers", "machine", "--blocker-records", &recs, "--epics", &epics]
                .iter()
                .map(|s| s.to_string())
                .collect();
        args.extend(extra);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = run(&args, &ready);
        assert_eq!(o.code, 0, "{}", o.err);
        let ids: Vec<&str> = o.out.lines().map(|l| l.split('\t').nth(5).unwrap()).collect();
        // Every blocker (A, D, G, I) is open in bd, so B, C, F and H wait; J, K and L have
        // none. No stacking without the machine: A's CERTIFIED row does not release B.
        assert_eq!(ids, ["J", "K", "L"]);
    }
}

#[test]
fn off_spira_poison_label_is_not_claimable_in_either_mode() {
    enforce(false);
    let ready = serde_json::json!([
        {"id":"P","priority":0,"labels":["repo:spira","spira-poison"]},
        {"id":"Q","priority":1,"labels":["repo:spira"]}
    ])
    .to_string();
    assert_eq!(run(&["select", "--fayth", "t", "--count"], &ready).out, "1\n");
    let o = run(&["select", "--fayth", "t", "--blockers", "machine", "--blocker-records", &tmp("[]"), "--count"], &ready);
    assert_eq!((o.code, o.out.as_str()), (0, "1\n"), "{}", o.err);
}

#[test]
fn on_bd_mode_is_unchanged_and_the_label_is_not_the_poison() {
    // On, the poison is the lifecycle hold; bd mode does not refilter (bd ready already
    // applied the predicate's exclusions), exactly as before the switch.
    enforce(true);
    let ready = serde_json::json!([{"id":"P","priority":0,"labels":["spira-poison"]}]).to_string();
    assert_eq!(run(&["select", "--fayth", "t", "--count"], &ready).out, "1\n");
}

#[test]
fn on_machine_mode_unreachable_machine_is_cannot_tell() {
    enforce(true);
    let (ready, _, recs) = machine_fixture();
    let o = run(&["select", "--fayth", "t", "--blockers", "machine", "--blocker-records", &recs], &ready);
    assert_eq!((o.code, o.out.as_str()), (CANNOT_TELL, ""));
    assert!(o.err.contains("lifecycle_enforce is on"), "{}", o.err);
}

// ---- stack: the aeon's own claim-time proposal ----------------------------------------

fn b_stacked_on_a() -> String {
    serde_json::json!([{"id":"B","labels":["repo:spira"],"dependencies":[{"issue_id":"B","depends_on_id":"A","type":"blocks"}]}]).to_string()
}

#[test]
fn cli_stack_reports_the_certified_prerequisites_tip() {
    // Holds the crate-wide ENV_LOCK (sp-obhv6 added it; this test set SPIRA_BD with no
    // isolation before then) — `cmd_stack` reads `$SPIRA_BD` same as every other store
    // caller, so this races against any other test that also sets it.
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bd = tmp_exe(&format!("#!/bin/sh\necho '{}'\n", b_stacked_on_a()));
    let saved = save_env(&["SPIRA_BD"]);
    std::env::set_var("SPIRA_BD", &*bd);
    let lc = tmp(&serde_json::json!([
        {"bead_id":"B","state":"READY","holds":"[]"},
        {"bead_id":"A","state":"CERTIFIED","holds":"[]","stack_depth":"0","tip":"abc123"},
    ]).to_string());
    let recs = tmp(&serde_json::json!([{"id":"A","status":"open","issue_type":"task","labels":["repo:spira"]}]).to_string());
    let o = run(&["stack", "B", "--lifecycle", &lc, "--blocker-records", &recs], "");
    restore_env(saved);
    assert_eq!(o.code, 0, "{}", o.err);
    let v: serde_json::Value = serde_json::from_str(&o.out).unwrap();
    assert_eq!(v["claimable"], true);
    assert_eq!(v["stack"]["A"], "abc123");
    assert_eq!(v["stack_depth"], 1);
}

#[test]
fn cli_stack_past_the_ceiling_is_refused_but_still_names_the_attempted_depth() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bd = tmp_exe(&format!("#!/bin/sh\necho '{}'\n", b_stacked_on_a()));
    let saved = save_env(&["SPIRA_BD"]);
    std::env::set_var("SPIRA_BD", &*bd);
    let lc = tmp(&serde_json::json!([
        {"bead_id":"B","state":"READY","holds":"[]"},
        {"bead_id":"A","state":"CERTIFIED","holds":"[]","stack_depth":"4","tip":"abc123"},
    ]).to_string());
    let recs = tmp(&serde_json::json!([{"id":"A","status":"open","issue_type":"task","labels":["repo:spira"]}]).to_string());
    let o = run(&["stack", "B", "--lifecycle", &lc, "--blocker-records", &recs], "");
    restore_env(saved);
    assert_eq!(o.code, 3, "{}", o.err);
    let v: serde_json::Value = serde_json::from_str(&o.out).unwrap();
    assert_eq!(v["claimable"], false);
    assert_eq!(v["reason"], "TooDeep");
    assert_eq!(v["stack_depth"], 5);
}

#[test]
fn cli_deadlocked_treats_apply_as_a_bare_flag_not_a_value_consumer() {
    // `--apply` takes no value (usage: "deadlocked [--apply] --merge-status F
    // [--actor NAME]") but was missing from BOOL_FLAGS: the generic parser then read
    // `--apply`'s own value as the NEXT token, which is always `--actor` in the one argv
    // shape every real caller sends (`groomer deadlocked --apply --actor groomer
    // --merge-status F --db D`, unchanged from `groomer.sh`'s) — so `groomer` itself fell
    // out as a stray positional and every `--apply` call failed usage, unconditionally.
    let empty = tmp("[]");
    let o = run(&["deadlocked", "--apply", "--actor", "groomer", "--merge-status", &empty, "--db", "/nonexistent-spira-claim-test-db"], "");
    assert!(!o.err.contains("positional"), "--apply must not consume --actor's value: {}", o.err);
    assert_ne!(o.code, 1, "{}", o.err);
}

// ---- the store, through a fake bd -----------------------------------------------------

/// A store over a fake bd; the fake lives as long as this does.
struct FakeStore {
    store: Store,
    _bd: Tmp,
}

impl std::ops::Deref for FakeStore {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.store
    }
}

fn fake_bd(script: &str) -> FakeStore {
    let p = tmp_exe(&format!("#!/bin/sh\n{script}\n"));
    FakeStore {
        store: Store { bd: p.to_string(), db: Some("/fake/db".into()), lc: "/nonexistent".into(), timeout: std::time::Duration::from_secs(10) },
        _bd: p,
    }
}

#[test]
fn store_events_parses_and_fails_closed() {
    let ok = fake_bd(r#"echo '[{"issue_id":"a","event_type":"claimed","new_value":"","created_at":"2026-09-28T10:00:00Z"}]'"#);
    assert_eq!(ok.events(&["a".into()]).unwrap().len(), 1);
    let fail = fake_bd("echo 'dolt: connection refused' >&2; exit 1");
    let e = fail.events(&["a".into()]).unwrap_err();
    assert!(e.contains("connection refused"), "{e}");
    let empty = fake_bd("exit 0");
    assert!(empty.events(&["a".into()]).is_err(), "empty stdout is not zero rows");
    let garbage = fake_bd("echo 'Warning: schema migration needed'");
    assert!(garbage.events(&["a".into()]).is_err());
    assert!(fail.lifecycle_snapshot().is_err());
}

#[test]
fn store_passes_db_and_chunks_ids() {
    // The fake records its argv; 450 ids must arrive as three queries, each with -C.
    let log = tmp("");
    let st = fake_bd(&format!("printf '%s\\n' \"$*\" >> {log}; echo '[]'"));
    let ids: Vec<String> = (0..450).map(|i| format!("sp-{i}")).collect();
    assert!(st.events(&ids).unwrap().is_empty());
    let calls = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|l| l.starts_with("-C /fake/db sql --json select")));
}

#[test]
fn store_lookup_fetch() {
    // bd list --id → epic priorities; bd children → started.
    let st = fake_bd(
        r#"case "$3" in
  list) echo '[{"id":"sp-E","priority":0},{"id":"sp-F","priority":2}]' ;;
  children) if [ "$4" = sp-E ]; then echo '[{"id":"k","status":"open","labels":["spira-submitted"]}]'; else echo '[{"id":"k2","status":"open","labels":[]}]'; fi ;;
esac"#,
    );
    let rows = rank::parse_ready(r#"[{"id":"a","parent":"sp-E"},{"id":"b","parent":"sp-F"}]"#).unwrap();
    let epics = st.list_by_ids(&rank::parents(&rows)).unwrap();
    assert_eq!(epics.len(), 2);
    assert!(rank::epic_started(&st.children("sp-E").unwrap(), "spira-submitted"));
    assert!(!rank::epic_started(&st.children("sp-F").unwrap(), "spira-submitted"));
}

#[test]
fn unpoison_usage_errors() {
    // Named arguments only; every refusal here happens before any store is read.
    assert_eq!(run(&["unpoison", "--cause", "x"], "").code, USAGE, "--bead is required");
    assert_eq!(run(&["unpoison", "--bead", "sp-a"], "").code, USAGE, "--cause is required");
    assert_eq!(run(&["unpoison", "--bead", "sp-a", "--cause", "  "], "").code, USAGE, "blank cause");
    assert_eq!(run(&["unpoison", "sp-a", "--cause", "x"], "").code, USAGE, "positional bead");
    assert_eq!(run(&["unpoison", "--bead", "sp a", "--cause", "x"], "").code, USAGE, "bad id");
    assert_eq!(run(&["unpoison", "--bead", "sp-a", "--cause", "x", "--credit", "Bad Slug"], "").code, USAGE);
    assert_eq!(run(&["unpoison", "--bead", "sp-a", "--cause", "x", "--frob", "1"], "").code, USAGE);
    let o = run(&["unpoison", "--bead", "sp-a"], "");
    assert!(o.err.contains("--cause is required"), "{}", o.err);
    assert!(o.out.is_empty());
}

#[test]
fn lifecycle_enforce_resolution_matches_aeon() {
    let on = Config { lifecycle_enforce: Some(true), ..Config::default() };
    let unset = Config::default();
    assert!(lifecycle_enforce(None, &on));
    assert!(!lifecycle_enforce(Some("0"), &on), "the environment wins");
    assert!(!lifecycle_enforce(Some(""), &on), "set-but-empty is off, as aeon");
    assert!(lifecycle_enforce(Some("1"), &unset));
    assert!(lifecycle_enforce(Some("true"), &unset));
    assert!(!lifecycle_enforce(Some("yes"), &unset));
    assert!(!lifecycle_enforce(None, &unset), "default off");
}

// ---- ready / claim CLI (wave 4.25, sp-obhv6) -------------------------------------------
//
// `fayth-ready`/`fayth-exclude`/`bulk-ready-by-fayth`/`claim-retry` read real process
// environment (`SPIRA_HOME`, `SPIRA_BD`, `SPIRA_TOML`, ...) — unlike the rest of this
// file's tests, which thread every input through a file or a hand-built `Store`. These
// serialize against each other (and against any other env-mutating test in this crate)
// through one crate-wide lock, same shape as spira-config's own `ENV_LOCK`.

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn save_env(keys: &[&str]) -> Vec<(String, Option<String>)> {
    keys.iter().map(|k| (k.to_string(), std::env::var(k).ok())).collect()
}

fn restore_env(saved: Vec<(String, Option<String>)>) {
    for (k, v) in saved {
        match v {
            Some(val) => std::env::set_var(&k, val),
            None => std::env::remove_var(&k),
        }
    }
}

fn sh(body: &str) -> Tmp {
    tmp_exe(&format!("#!/bin/sh\n{body}\n"))
}

/// `<home>/chamber/<name>.fayth` per entry, `(name, FAYTH_LABELS, FAYTH_EXCLUDE_LABELS)`.
fn chamber_home(fayths: &[(&str, &str, &str)]) -> Tmp {
    let dir = testkit::TempDir::new("spira-claim-chamber");
    let chamber: PathBuf = dir.join("chamber");
    std::fs::create_dir_all(&chamber).unwrap();
    for (name, labels, exclude) in fayths {
        std::fs::write(chamber.join(format!("{name}.fayth")), format!("FAYTH_LABELS=\"{labels}\"\nFAYTH_EXCLUDE_LABELS=\"{exclude}\"\n")).unwrap();
    }
    Tmp { path: dir.to_string_lossy().into_owned(), _dir: dir }
}

#[test]
fn ready_args_cli_prints_one_token_a_line_in_order() {
    let o = run(&["ready-args", "--scope-label", "plan", "--no-loop-label", "no-loop"], "");
    assert_eq!(o.out, "ready\n--limit\n0\n--exclude-type\nepic,event\n-u\n--label\nplan\n--exclude-label\nno-loop\n");
    let raw = run(&["ready-args", "--raw", "--scope-label", "plan", "--no-loop-label", "no-loop"], "");
    assert_eq!(raw.out, "ready\n--limit\n0\n--exclude-type\nepic,event\n-u\n--exclude-label\nno-loop\n", "--raw never carries the scope label");
}

#[test]
fn ready_count_cli_failed_query_prints_zero_and_fails_closed() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bd = sh("echo 'dolt: connection refused' >&2; exit 1");
    let saved = save_env(&["SPIRA_BD", "SPIRA_DB"]);
    std::env::set_var("SPIRA_BD", &*bd);
    std::env::remove_var("SPIRA_DB");
    let o = run(&["ready-count", "plan", "spira-poison", "--no-loop-label", "no-loop"], "");
    restore_env(saved);
    assert_eq!((o.code, o.out.as_str()), (1, "0"), "a failed query is not a clean zero (sp-3ntca)");
    assert!(o.err.contains("connection refused"), "{}", o.err);
}

#[test]
fn ready_count_cli_real_count() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bd = sh("echo '[1,2,3]'");
    let saved = save_env(&["SPIRA_BD", "SPIRA_DB"]);
    std::env::set_var("SPIRA_BD", &*bd);
    std::env::remove_var("SPIRA_DB");
    let o = run(&["ready-count", "plan", ""], "");
    restore_env(saved);
    assert_eq!((o.code, o.out.as_str(), o.err.as_str()), (0, "3", ""));
}

#[test]
fn claim_retry_cli_succeeds_first_try_with_no_argv_parsing() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bd = sh("echo '[{\"id\":\"sp-a\"}]'");
    let saved = save_env(&["SPIRA_BD", "SPIRA_DB", "SPIRA_TOML"]);
    std::env::set_var("SPIRA_BD", &*bd);
    std::env::remove_var("SPIRA_DB");
    std::env::remove_var("SPIRA_TOML");
    // "--label" and "--claim" here are BD's flags, not spira-claim's — dispatch must pass
    // them through untouched rather than parsing them as its own.
    let o = run(&["claim-retry", "ready", "--limit", "0", "--claim", "--label", "plan"], "");
    restore_env(saved);
    assert_eq!(o.code, 0);
    assert!(o.out.contains("sp-a"), "{}", o.out);
}

#[test]
fn claim_retry_cli_retries_past_a_transient_failure() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let log = tmp("");
    let toml = tmp("[spira]\nclaim_retries = \"3\"\nclaim_retry_delay_s = \"0\"\n");
    let bd = sh(&format!(
        "n=$(wc -l < {log}); printf 'x\\n' >> {log}; if [ \"$n\" -lt 1 ]; then echo boom >&2; exit 1; fi; echo '[]'"
    ));
    let saved = save_env(&["SPIRA_BD", "SPIRA_DB", "SPIRA_TOML"]);
    std::env::set_var("SPIRA_BD", &*bd);
    std::env::remove_var("SPIRA_DB");
    std::env::set_var("SPIRA_TOML", &*toml);
    let o = run(&["claim-retry", "ready", "--limit", "0"], "");
    restore_env(saved);
    assert_eq!((o.code, o.out.as_str()), (0, "[]\n"), "the contention case: the second attempt finds the real (empty) result");
}

#[test]
fn claim_retry_cli_exhausts_and_reports_one_stderr_line_with_empty_stdout() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let toml = tmp("[spira]\nclaim_retries = \"2\"\nclaim_retry_delay_s = \"0\"\n");
    let bd = sh("echo 'dolt: connection refused' >&2; exit 1");
    let saved = save_env(&["SPIRA_BD", "SPIRA_DB", "SPIRA_TOML"]);
    std::env::set_var("SPIRA_BD", &*bd);
    std::env::remove_var("SPIRA_DB");
    std::env::set_var("SPIRA_TOML", &*toml);
    let o = run(&["claim-retry", "ready"], "");
    restore_env(saved);
    assert_eq!((o.code, o.out.as_str()), (1, ""));
    assert!(o.err.contains("query failed after 2 attempt(s)"), "{}", o.err);
    assert!(o.err.contains("connection refused"), "{}", o.err);
}

#[test]
fn fayth_exclude_cli_own_then_shared_then_every_other_fayth() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = chamber_home(&[("builder", "spira,plan", ""), ("ops", "spira,ops-trigger", "")]);
    let saved = save_env(&["SPIRA_HOME", "SPIRA_FAYTHS", "SPIRA_TOML"]);
    std::env::set_var("SPIRA_HOME", &*home);
    std::env::remove_var("SPIRA_FAYTHS");
    std::env::remove_var("SPIRA_TOML");
    let o = run(&["fayth-exclude", "builder", "qa-proposed"], "");
    restore_env(saved);
    assert_eq!(o.out, "qa-proposed,spira-queue-waiting,spira-submitted,spira-open-children,fayth:ops");
    assert_eq!(o.code, 0);
}

#[test]
fn fayth_ready_cli_no_fayth_file_is_rc2_stdout_zero() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = chamber_home(&[]);
    let saved = save_env(&["SPIRA_HOME", "SPIRA_READY_CACHE"]);
    std::env::set_var("SPIRA_HOME", &*home);
    std::env::remove_var("SPIRA_READY_CACHE");
    let o = run(&["fayth-ready", "nosuchpersona"], "");
    restore_env(saved);
    assert_eq!((o.code, o.out.as_str()), (2, "0"));
    assert!(o.err.contains("no fayth in the chamber"), "{}", o.err);
}

#[test]
fn fayth_ready_cli_query_failure_is_rc1_not_rc2_the_sp_3ntca_defect() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = chamber_home(&[("probe", "plan", "")]);
    let bd = sh("echo 'Error: the database is locked by another dolt process' >&2; exit 1");
    let saved = save_env(&["SPIRA_HOME", "SPIRA_BD", "SPIRA_DB", "SPIRA_READY_CACHE", "SPIRA_FAYTHS"]);
    std::env::set_var("SPIRA_HOME", &*home);
    std::env::set_var("SPIRA_BD", &*bd);
    std::env::remove_var("SPIRA_DB");
    std::env::remove_var("SPIRA_READY_CACHE");
    std::env::remove_var("SPIRA_FAYTHS");
    let o = run(&["fayth-ready", "probe"], "");
    restore_env(saved);
    assert_eq!((o.code, o.out.as_str()), (1, "0"), "the fayth file is right there — this is not the no-fayth code");
    assert!(o.err.contains("locked by another dolt process"), "{}", o.err);
    assert!(!o.err.contains("no fayth"), "{}", o.err);
}

#[test]
fn fayth_ready_cli_real_count_including_zero() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = chamber_home(&[("probe", "plan", "")]);
    let saved = save_env(&["SPIRA_HOME", "SPIRA_BD", "SPIRA_DB", "SPIRA_READY_CACHE", "SPIRA_FAYTHS"]);
    std::env::set_var("SPIRA_HOME", &*home);
    std::env::remove_var("SPIRA_DB");
    std::env::remove_var("SPIRA_READY_CACHE");
    std::env::remove_var("SPIRA_FAYTHS");
    let bd_zero = sh("echo '[]'");
    std::env::set_var("SPIRA_BD", &*bd_zero);
    let o = run(&["fayth-ready", "probe"], "");
    assert_eq!((o.code, o.out.as_str(), o.err.as_str()), (0, "0", ""));

    let bd_seven = sh("echo '[1,2,3,4,5,6,7]'");
    std::env::set_var("SPIRA_BD", &*bd_seven);
    let o2 = run(&["fayth-ready", "probe"], "");
    restore_env(saved);
    assert_eq!((o2.code, o2.out.as_str()), (0, "7"));
}

#[test]
fn fayth_ready_cli_cache_fast_path_skips_the_query_entirely() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = chamber_home(&[("probe", "plan", "")]);
    let cache = tmp("probe 9\nother 1\n");
    let bd = sh("echo 'bd must not be called' >&2; exit 1");
    let saved = save_env(&["SPIRA_HOME", "SPIRA_BD", "SPIRA_READY_CACHE", "SPIRA_FAYTHS"]);
    std::env::set_var("SPIRA_HOME", &*home);
    std::env::set_var("SPIRA_BD", &*bd);
    std::env::set_var("SPIRA_READY_CACHE", &*cache);
    std::env::remove_var("SPIRA_FAYTHS");
    let o = run(&["fayth-ready", "probe"], "");
    restore_env(saved);
    assert_eq!((o.code, o.out.as_str(), o.err.as_str()), (0, "9", ""));
}

#[test]
fn bulk_ready_by_fayth_cli_buckets_one_fetch_by_the_chamber_roster() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = chamber_home(&[("builder", "spira,plan", ""), ("ops", "spira,ops-trigger", "")]);
    let bd = sh(
        r#"echo '[{"id":"a","labels":["spira","plan"]},{"id":"b","labels":["spira","ops-trigger"]},{"id":"c","labels":["spira","plan","spira-submitted"]}]'"#,
    );
    let saved = save_env(&["SPIRA_HOME", "SPIRA_BD", "SPIRA_DB", "SPIRA_READY_SNAPSHOT", "SPIRA_FAYTHS"]);
    std::env::set_var("SPIRA_HOME", &*home);
    std::env::set_var("SPIRA_BD", &*bd);
    std::env::remove_var("SPIRA_DB");
    std::env::remove_var("SPIRA_READY_SNAPSHOT");
    std::env::remove_var("SPIRA_FAYTHS");
    let o = run(&["bulk-ready-by-fayth"], "");
    restore_env(saved);
    assert_eq!(o.out, "builder 1\nops 1\n", "c is dropped by the shared spira-submitted exclusion");
}

#[test]
fn bulk_ready_by_fayth_cli_reads_the_snapshot_instead_of_calling_bd() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = chamber_home(&[("builder", "spira,plan", "")]);
    let snap = tmp(r#"[{"id":"a","labels":["spira","plan"]}]"#);
    let bd = sh("echo 'bd must not be called' >&2; exit 1");
    let saved = save_env(&["SPIRA_HOME", "SPIRA_BD", "SPIRA_READY_SNAPSHOT", "SPIRA_FAYTHS"]);
    std::env::set_var("SPIRA_HOME", &*home);
    std::env::set_var("SPIRA_BD", &*bd);
    std::env::set_var("SPIRA_READY_SNAPSHOT", &*snap);
    std::env::remove_var("SPIRA_FAYTHS");
    let o = run(&["bulk-ready-by-fayth"], "");
    restore_env(saved);
    assert_eq!((o.code, o.out.as_str()), (0, "builder 1\n"));
}
