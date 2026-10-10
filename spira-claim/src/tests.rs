//! CLI-level tests: the contract at the interface (exit codes, stdout shapes), with every
//! store read replaced by a file (`--events`, `--ready`, `--epics`, `--lifecycle`,
//! `--blocker-records`) or by a fake `bd` script driven through [`Store`] directly.

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

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

/// This crate's own `spira/` tree — `conf.d` lives here. Every subprocess test's
/// `SPIRA_HOME` must point somewhere with a `conf.d` beside it (triage guide: "the tree's
/// spira dir") — a custom chamber fixture ([`chamber_home`]) gets one by symlink.
const REAL_SPIRA_HOME: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../spira");

/// A complete, throwaway config file (every registered key declared;
/// `spira_config::process::fixture_toml`) with `declare`'s overrides — kept alive as long
/// as the caller holds the returned `Tmp`.
fn toml_fixture(declare: &[(&str, &str)]) -> Tmp {
    let dir = testkit::TempDir::new("spira-claim-toml");
    let p = spira_config::process::fixture_toml(&dir, declare);
    Tmp { path: p.to_string_lossy().into_owned(), _dir: dir }
}

/// `dispatch` now reads every registered key through `spira_config::process::cfg`, which
/// caches per PROCESS (per Ryan 2026-10-05: one source of config) — so a unit test cannot
/// vary it in-process. This execs the real `spira-claim` binary instead: a fresh process
/// per call, with a fresh throwaway config file (`toml_fixture`, no overrides) and
/// `SPIRA_HOME` pointed at this crate's own tree.
fn run(args: &[&str], stdin: &str) -> Outcome {
    run_cfg(args, stdin, &[], &[])
}

/// [`run`], plus `toml_declare` (registered-key overrides for the subprocess's own
/// throwaway config file — `SPIRA_BD`, labels, `SPIRA_CLAIM_RETRIES`, … everything
/// `spira/conf.d` registers; raw env no longer reaches any of these) and `extra_env`
/// (anything else the subprocess should see: `SPIRA_HOME` for a fayth chamber, `PATH`, the
/// few still-unregistered knobs). `SPIRA_HOME` defaults to [`REAL_SPIRA_HOME`] unless
/// `extra_env` overrides it. One `testkit::env` call for everything: its lock is not
/// reentrant, so a caller must never also be holding it.
fn run_cfg(args: &[&str], stdin: &str, toml_declare: &[(&str, &str)], extra_env: &[(&str, Option<&str>)]) -> Outcome {
    let toml = toml_fixture(toml_declare);
    let mut edits: Vec<(&str, Option<&str>)> = vec![("SPIRA_HOME", Some(REAL_SPIRA_HOME)), ("SPIRA_TOML", Some(toml.as_str()))];
    edits.extend_from_slice(extra_env);
    let _env = testkit::env(&edits);
    exec_spira_claim(args, stdin)
}

/// The compiled `spira-claim` binary, fed `args`/`stdin`, its exit code/stdout/stderr
/// folded into an [`Outcome`] the same shape `dispatch` used to return directly.
fn exec_spira_claim(args: &[&str], stdin: &str) -> Outcome {
    use std::io::Write;
    // The binary cargo built beside this test executable (tests/bin_is_built.rs makes it
    // build): <target>/<profile>/deps/<this-test> -> <target>/<profile>/spira-claim.
    let bin = std::env::current_exe().unwrap().parent().and_then(|d| d.parent()).unwrap().join("spira-claim");
    let mut child = std::process::Command::new(&bin)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn spira-claim");
    child.stdin.take().expect("its stdin").write_all(stdin.as_bytes()).expect("write stdin");
    let out = child.wait_with_output().expect("wait for spira-claim");
    Outcome {
        code: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).into_owned(),
        err: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
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
fn bd_mode_is_unchanged_and_the_label_is_not_the_poison() {
    // The poison is the lifecycle hold; bd mode does not refilter (bd ready already
    // applied the predicate's exclusions).
    let ready = serde_json::json!([{"id":"P","priority":0,"labels":["spira-poison"]}]).to_string();
    assert_eq!(run(&["select", "--fayth", "t", "--count"], &ready).out, "1\n");
}

#[test]
fn machine_mode_unreachable_machine_is_cannot_tell() {
    let (ready, _, recs) = machine_fixture();
    // Unreachable by construction: no spira-lc on PATH. Inheriting the caller's PATH and
    // SPIRA_LC_* reached the production store from the gate and passed only while it was slow.
    let no_lc = testkit::TempDir::new("spira-claim-no-lc");
    let o = run_cfg(
        &["select", "--fayth", "t", "--blockers", "machine", "--blocker-records", &recs],
        &ready,
        &[],
        &[("PATH", Some(no_lc.path().to_str().unwrap()))],
    );
    assert_eq!((o.code, o.out.as_str()), (CANNOT_TELL, ""));
    assert!(o.err.contains("the machine must answer"), "{}", o.err);
}

// ---- stack: the aeon's own claim-time proposal ----------------------------------------

fn b_stacked_on_a() -> String {
    serde_json::json!([{"id":"B","labels":["repo:spira"],"dependencies":[{"issue_id":"B","depends_on_id":"A","type":"blocks"}]}]).to_string()
}

#[test]
fn cli_stack_reports_the_certified_prerequisites_tip() {
    let bd = tmp_exe(&format!("#!/bin/sh\necho '{}'\n", b_stacked_on_a()));
    let lc = tmp(&serde_json::json!([
        {"bead_id":"B","state":"READY","holds":"[]"},
        {"bead_id":"A","state":"CERTIFIED","holds":"[]","stack_depth":"0","tip":"abc123"},
    ]).to_string());
    let recs = tmp(&serde_json::json!([{"id":"A","status":"open","issue_type":"task","labels":["repo:spira"]}]).to_string());
    let o = run_cfg(&["stack", "B", "--lifecycle", &lc, "--blocker-records", &recs], "", &[("SPIRA_BD", bd.as_str())], &[]);
    assert_eq!(o.code, 0, "{}", o.err);
    let v: serde_json::Value = serde_json::from_str(&o.out).unwrap();
    assert_eq!(v["claimable"], true);
    assert_eq!(v["stack"]["A"], "abc123");
    assert_eq!(v["stack_depth"], 1);
}

#[test]
fn cli_stack_past_the_ceiling_is_refused_but_still_names_the_attempted_depth() {
    let bd = tmp_exe(&format!("#!/bin/sh\necho '{}'\n", b_stacked_on_a()));
    let lc = tmp(&serde_json::json!([
        {"bead_id":"B","state":"READY","holds":"[]"},
        {"bead_id":"A","state":"CERTIFIED","holds":"[]","stack_depth":"4","tip":"abc123"},
    ]).to_string());
    let recs = tmp(&serde_json::json!([{"id":"A","status":"open","issue_type":"task","labels":["repo:spira"]}]).to_string());
    let o = run_cfg(&["stack", "B", "--lifecycle", &lc, "--blocker-records", &recs], "", &[("SPIRA_BD", bd.as_str())], &[]);
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
    _lc: Tmp,
}

impl std::ops::Deref for FakeStore {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.store
    }
}

fn fake_bd(script: &str) -> FakeStore {
    fake_bd_lc(script, "echo '[]'")
}

fn fake_bd_lc(script: &str, lc_script: &str) -> FakeStore {
    let p = tmp_exe(&format!("#!/bin/sh\n{script}\n"));
    let lc = tmp_exe(&format!("#!/bin/sh\n{lc_script}\n"));
    FakeStore {
        store: Store { bd: p.to_string(), db: Some("/fake/db".into()), lc: lc.to_string(), timeout: std::time::Duration::from_secs(10) },
        _bd: p,
        _lc: lc,
    }
}

#[test]
fn store_events_are_bd_history_plus_lifecycle_facts() {
    let st = fake_bd_lc(
        r#"echo '[{"issue_id":"a","event_type":"claimed","new_value":"","created_at":"2026-09-28T10:00:00Z"}]'"#,
        r#"echo '[{"issue_id":"a","event_type":"requeued","new_value":"gate-red","actor":"harness","created_at":"2026-09-28T11:00:00Z"}]'"#,
    );
    let rows = st.events(&["a".into()]).unwrap();
    assert_eq!(rows.iter().map(|r| r.event_type.as_str()).collect::<Vec<_>>(), ["claimed", "requeued"]);
}

#[test]
fn a_fact_read_that_cannot_tell_is_not_zero_facts() {
    let st = fake_bd_lc("echo '[]'", "echo 'cannot tell: db down'; exit 2");
    let e = st.events(&["a".into()]).unwrap_err();
    assert!(e.contains("spira-lc facts"), "{e}");
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
    assert!(fake_bd_lc("exit 0", "exit 1").lifecycle_in_states(&["READY"]).is_err());
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

/// `epics`: bd list --id gives the epic priorities, bd children the children, and each
/// child's progress is its lifecycle row (sp-mve9i) — sp-E's child is SUBMITTED, so sp-E is
/// started, though bd says the child is open; sp-F's is READY, though bd says closed.
fn epics_with(lc: Option<&str>) -> Outcome {
    let bd = sh(r#"case "$*" in
  *children*sp-E*) echo '[{"id":"k","status":"open","labels":[]}]' ;;
  *children*sp-F*) echo '[{"id":"k2","status":"closed","labels":[]}]' ;;
  *list*) echo '[{"id":"sp-E","priority":0},{"id":"sp-F","priority":2}]' ;;
esac"#);
    let no_lc = testkit::TempDir::new("spira-claim-epics-no-lc");
    let path = match lc {
        Some(rows) => fake_lc_path(rows),
        None => Tmp { path: format!("{}:/usr/bin:/bin", no_lc.to_string_lossy()), _dir: testkit::TempDir::new("spira-claim-epics-path") },
    };
    run_cfg(
        &["epics"],
        r#"[{"id":"a","parent":"sp-E"},{"id":"b","parent":"sp-F"}]"#,
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", "")],
        &[("SPIRA_LC_BIN", None), ("PATH", Some(path.as_str()))],
    )
}

#[test]
fn epics_reads_each_childs_progress_from_its_lifecycle_row() {
    let lc = r#"[{"bead_id":"k","state":"SUBMITTED","holds":"[]"},{"bead_id":"k2","state":"READY","holds":"[]"}]"#;
    let o = epics_with(Some(lc));
    assert_eq!(o.code, 0, "{}", o.err);
    let l: EpicLookup = serde_json::from_str(o.out.trim()).unwrap();
    assert_eq!(l.prio.get("sp-E"), Some(&0));
    assert_eq!(l.started, vec!["sp-E".to_string()]);
}

#[test]
fn epics_with_no_machine_is_cannot_tell() {
    let o = epics_with(None);
    assert_eq!((o.code, o.out.as_str()), (CANNOT_TELL, ""));
    assert!(o.err.contains("the machine must answer"), "{}", o.err);
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

// ---- ready / claim CLI (wave 4.25, sp-obhv6) -------------------------------------------
//
// `fayth-ready`/`fayth-exclude`/`bulk-ready-by-fayth`/`claim-retry` read real process
// environment (`SPIRA_HOME`, `SPIRA_BD`, `SPIRA_TOML`, ...) — unlike the rest of this
// file's tests, which thread every input through a file or a hand-built `Store`. These
// edit the environment only through `testkit::env`, which serializes them against every
// other test in this binary and restores on drop; `run` takes the same lock to read.

fn sh(body: &str) -> Tmp {
    tmp_exe(&format!("#!/bin/sh\n{body}\n"))
}

/// `<home>/chamber/<name>.fayth` per entry, `(name, FAYTH_LABELS, FAYTH_EXCLUDE_LABELS)`.
/// A test that points `SPIRA_HOME` here (to get this chamber) also hands
/// `spira_config::process::cfg`'s own home-location the SAME directory — so it needs a
/// `conf.d` too, or config resolution refuses ("no config registry at …"). Symlinked to
/// the real tree's, never copied: this fixture is not the one being tested.
fn chamber_home(fayths: &[(&str, &str, &str)]) -> Tmp {
    let dir = testkit::TempDir::new("spira-claim-chamber");
    let chamber: PathBuf = dir.join("chamber");
    std::fs::create_dir_all(&chamber).unwrap();
    for (name, labels, exclude) in fayths {
        std::fs::write(chamber.join(format!("{name}.fayth")), format!("FAYTH_LABELS=\"{labels}\"\nFAYTH_EXCLUDE_LABELS=\"{exclude}\"\n")).unwrap();
    }
    std::os::unix::fs::symlink(std::path::Path::new(REAL_SPIRA_HOME).join("conf.d"), dir.join("conf.d")).unwrap();
    Tmp { path: dir.to_string_lossy().into_owned(), _dir: dir }
}

/// `SPIRA_CHAMBER` for a [`chamber_home`] fixture, to `toml_declare` — `spira.chamber`
/// (the registered key `spira_config::chamber::chamber_dir` actually reads) otherwise
/// comes from the baseline fixture's own declared value, never the chamber this test
/// just built (Concierge, 2026-10-05: "spira.chamber comes from the complete fixture …
/// so 'no fayth in the chamber'").
fn chamber_path(home: &Tmp) -> String {
    format!("{}/chamber", home.as_str())
}

#[test]
fn ready_args_cli_prints_one_token_a_line_in_order() {
    // literal-ok: fixture values below — "no-loop" is SPIRA_NO_LOOP_LABEL's own default
    let o = run(&["ready-args", "--scope-label", "plan", "--noloop-label", "no-loop"], ""); // literal-ok
    assert_eq!(o.out, "ready\n--limit\n0\n--exclude-type\nepic,event\n-u\n--label\nplan\n--exclude-label\nno-loop\n"); // literal-ok
    let raw = run(&["ready-args", "--raw", "--scope-label", "plan", "--noloop-label", "no-loop"], ""); // literal-ok
    assert_eq!(raw.out, "ready\n--limit\n0\n--exclude-type\nepic,event\n-u\n--exclude-label\nno-loop\n", "--raw never carries the scope label"); // literal-ok
}

#[test]
fn ready_count_cli_failed_query_prints_zero_and_fails_closed() {
    let bd = sh("echo 'dolt: connection refused' >&2; exit 1");
    let path = fake_lc_path(&lc_ready(&["a"]));
    let o = run_cfg(
        &["ready-count", "plan", "spira-poison", "--noloop-label", "no-loop"], // literal-ok: fixture value
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", "")],
        &[("PATH", Some(path.as_str()))],
    );
    assert_eq!((o.code, o.out.as_str()), (1, "0"), "a failed query is not a clean zero (sp-3ntca)");
    assert!(o.err.contains("connection refused"), "{}", o.err);
}

#[test]
fn ready_count_cli_real_count() {
    let bd = sh(&format!("echo '{}'", plan_rows(&["a", "b", "c"])));
    let path = fake_lc_path(&lc_ready(&["a", "b", "c"]));
    // `machine_claimable` applies `scope_label` BEFORE the <labels> argument does — the
    // baseline fixture's own "spira" would filter out every "plan"-only bead here; this
    // test means unscoped readiness, so it declares no scope restriction.
    let o = run_cfg(
        &["ready-count", "plan", ""],
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", ""), ("SPIRA_SCOPE_LABEL", "")],
        &[("PATH", Some(path.as_str()))],
    );
    assert_eq!((o.code, o.out.as_str(), o.err.as_str()), (0, "3", ""));
}

#[test]
fn claim_retry_cli_succeeds_first_try_with_no_argv_parsing() {
    let bd = sh("echo '[{\"id\":\"sp-a\"}]'");
    // "--label" and "--claim" here are BD's flags, not spira-claim's — dispatch must pass
    // them through untouched rather than parsing them as its own.
    let o = run_cfg(
        &["claim-retry", "ready", "--limit", "0", "--claim", "--label", "plan"],
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", "")],
        &[],
    );
    assert_eq!(o.code, 0);
    assert!(o.out.contains("sp-a"), "{}", o.out);
}

#[test]
fn claim_retry_cli_retries_past_a_transient_failure() {
    let log = tmp("");
    let bd = sh(&format!(
        "n=$(wc -l < {log}); printf 'x\\n' >> {log}; if [ \"$n\" -lt 1 ]; then echo boom >&2; exit 1; fi; echo '[]'"
    ));
    let o = run_cfg(
        &["claim-retry", "ready", "--limit", "0"],
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", ""), ("SPIRA_CLAIM_RETRIES", "3"), ("SPIRA_CLAIM_RETRY_DELAY_S", "0")],
        &[],
    );
    assert_eq!((o.code, o.out.as_str()), (0, "[]\n"), "the contention case: the second attempt finds the real (empty) result");
}

#[test]
fn claim_retry_cli_exhausts_and_reports_one_stderr_line_with_empty_stdout() {
    let bd = sh("echo 'dolt: connection refused' >&2; exit 1");
    let o = run_cfg(
        &["claim-retry", "ready"],
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", ""), ("SPIRA_CLAIM_RETRIES", "2"), ("SPIRA_CLAIM_RETRY_DELAY_S", "0")],
        &[],
    );
    assert_eq!((o.code, o.out.as_str()), (1, ""));
    assert!(o.err.contains("query failed after 2 attempt(s)"), "{}", o.err);
    assert!(o.err.contains("connection refused"), "{}", o.err);
}

#[test]
fn fayth_exclude_cli_own_then_shared_then_every_other_fayth() {
    let home = chamber_home(&[("builder", "spira,plan", ""), ("ops", "spira,ops-trigger", "")]);
    let chamber = chamber_path(&home);
    // The three shared labels are the baseline fixture's own declared values
    // (spira-queue-waiting/spira-submitted/spira-open-children) — no override needed.
    let o = run_cfg(
        &["fayth-exclude", "builder", "qa-proposed"],
        "",
        &[("SPIRA_FAYTHS", "[]"), ("SPIRA_CHAMBER", &chamber)],
        &[("SPIRA_HOME", Some(home.as_str()))],
    );
    assert_eq!(o.out, "qa-proposed,spira-queue-waiting,spira-open-children,fayth:ops");
    assert_eq!(o.code, 0);
}

#[test]
fn fayth_exclude_cli_refuses_rather_than_default_when_config_does_not_resolve() {
    // THE REGRESSION `fayth_exclude_cli_defaults_to_empty_when_nothing_is_configured` USED
    // TO GUARD (found by test-unclaimable.sh going red): lib.sh's `ready_shared_exclude`/
    // `READY_ARGS`/`ready_raw_args` never hardcoded a label default themselves — only
    // conf.sh's derivation did, and a suite that sourced lib.sh alone (most of them) never
    // ran conf.sh at all, so "nothing configured" had to mean "no exclusions", never a
    // silent, uninvited one. Per Ryan 2026-10-05 (one source of config), "nothing
    // configured" is no longer expressible as a quiet empty default — the config file not
    // resolving at all is now a refusal that names the problem, not a default of any kind.
    // Concierge decision (2026-10-05, sp-hh599): a config-load failure is an ERROR (rc 1,
    // `Outcome::error`), never `CANNOT_TELL` (2) — callers read that 2 as "no fayth /
    // nothing there", not "something is broken".
    let home = chamber_home(&[("builder", "spira,plan", "")]);
    let o = run_cfg(
        &["fayth-exclude", "builder", ""],
        "",
        &[],
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_TOML", Some("/no/such/spira-toml-for-this-test"))],
    );
    assert_eq!(o.code, 1, "{}", o.err);
    assert!(o.err.contains("config"), "{}", o.err);
}

/// A declared-empty `SPIRA_NO_LOOP_LABEL` (still a resolved value, just "") still means "no
/// exclusion" — the behaviour the old defaults-to-empty test also checked, now reached by
/// an explicit declaration rather than by nothing being configured at all.
#[test]
fn ready_args_cli_raw_carries_no_exclude_label_when_no_loop_label_is_declared_empty() {
    let o = run_cfg(&["ready-args", "--raw"], "", &[("SPIRA_NO_LOOP_LABEL", "")], &[]);
    assert_eq!(o.out, "ready\n--limit\n0\n--exclude-type\nepic,event\n-u\n");
}

#[test]
fn shared_exclude_cli_the_two_labels_ready_shared_exclude_carried() {
    // test-dispatch-open-children.sh calls `ready_shared_exclude` directly, not through
    // `fayth_exclude` — this verb exists only for that caller. The baseline fixture's own
    // declared values (spira-queue-waiting/spira-submitted/spira-open-children) are what
    // conf.sh's derivation would hold once it ran; no override needed.
    let o = run(&["shared-exclude"], "");
    assert_eq!((o.code, o.out.as_str()), (0, "spira-queue-waiting,spira-open-children"));
}

/// All three shared labels declared empty (a legitimate, resolved "no restriction" — not
/// "nothing configured", which is a refusal now; see `fayth_exclude_cli_refuses_rather_
/// than_default_when_config_does_not_resolve`) still carries no exclusions.
#[test]
fn shared_exclude_cli_empty_when_every_label_is_declared_empty() {
    let o = run_cfg(
        &["shared-exclude"],
        "",
        &[("SPIRA_QUEUE_WAIT_LABEL", ""), ("SPIRA_SUBMITTED_LABEL", ""), ("SPIRA_OPEN_CHILDREN_LABEL", "")],
        &[],
    );
    assert_eq!((o.code, o.out.as_str()), (0, ""));
}

#[test]
fn fayth_ready_cli_no_fayth_file_is_rc2_stdout_zero() {
    let home = chamber_home(&[]);
    let chamber = chamber_path(&home);
    let o = run_cfg(
        &["fayth-ready", "nosuchpersona"],
        "",
        &[("SPIRA_CHAMBER", &chamber)],
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_READY_CACHE", None)],
    );
    assert_eq!((o.code, o.out.as_str()), (2, "0"));
    assert!(o.err.contains("no fayth in the chamber"), "{}", o.err);
}

#[test]
fn fayth_ready_cli_query_failure_is_rc1_not_rc2_the_sp_3ntca_defect() {
    let home = chamber_home(&[("probe", "plan", "")]);
    let chamber = chamber_path(&home);
    let bd = sh("echo 'Error: the database is locked by another dolt process' >&2; exit 1");
    let path = fake_lc_path(&lc_ready(&["a"]));
    let o = run_cfg(
        &["fayth-ready", "probe"],
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", ""), ("SPIRA_FAYTHS", "[]"), ("SPIRA_CHAMBER", &chamber)],
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_READY_CACHE", None), ("PATH", Some(path.as_str()))],
    );
    assert_eq!((o.code, o.out.as_str()), (1, "0"), "the fayth file is right there — this is not the no-fayth code");
    assert!(o.err.contains("locked by another dolt process"), "{}", o.err);
    assert!(!o.err.contains("no fayth"), "{}", o.err);
}

/// sp-xsnid: under a bare environment (no host config document, no `conf.d` registry —
/// exactly `fayth_label_overlay`'s own "resolution failure" fallback, which is what a bare
/// `spira-sentinel.service` leaves `fayth_predicate` holding), a `FAYTH_LABELS` that
/// references a config variable BARE (no `${VAR:+...}` guard) must refuse rather than
/// hand back the empty string `bd --label "" ...` would read as "match everything" — the
/// exact symptom this bead names: `ops`/`spike`/`builder` all reading `234`, the WHOLE
/// ready queue. rc 3, never rc 2 ("no fayth") and never rc 0 with a widened count.
#[test]
fn fayth_ready_cli_a_bare_reference_that_resolves_empty_refuses_rc3_never_widens() {
    let home = chamber_home(&[("ops", "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_INCIDENT_LABEL", "spira-poison,$SPIRA_ASK_LABEL")]);
    let chamber = chamber_path(&home);
    let bd = sh("echo 'bd must not be called — a widened query must never reach the store' >&2; exit 1");
    // The references under test resolve EMPTY — not undeclared: declared empty, which
    // `fayth_label_overlay`'s own `resolve_for_process` accepts (per Ryan 2026-10-05, a
    // key's declared "" is itself an answer), so this config file still resolves overall
    // (`store::load_config` needs ask_label/scope_label too) and the refusal comes from
    // `fayth_predicate`'s bare-reference check alone.
    let o = run_cfg(
        &["fayth-ready", "ops"],
        "",
        &[
            ("SPIRA_BD", bd.as_str()),
            ("SPIRA_DB", ""),
            ("SPIRA_FAYTHS", "[]"),
            ("SPIRA_CHAMBER", &chamber),
            ("SPIRA_INCIDENT_LABEL", ""),
            ("SPIRA_ASK_LABEL", ""),
            ("SPIRA_SCOPE_LABEL", ""),
        ],
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_READY_CACHE", None)],
    );
    assert_eq!((o.code, o.out.as_str()), (3, "0"), "stderr: {}", o.err);
    assert!(o.err.contains("SPIRA_INCIDENT_LABEL"), "the refusal must name the exact reference: {}", o.err);
    assert!(!o.err.contains("no fayth in the chamber"), "{}", o.err);
}

/// The guarded `SPIRA_SCOPE_LABEL` reference (every real `FAYTH_LABELS`'s own
/// `${SPIRA_SCOPE_LABEL:+...}` opening) must never itself trigger a refusal — "no scope
/// restriction configured" is this harness's own common case. A literal, declared-empty
/// `FAYTH_LABELS=""` (no `$` at all — `concierge.fayth`'s own shape) must not refuse
/// either; it is a deliberate "no restriction", not a config gap.
#[test]
fn fayth_ready_cli_a_guarded_or_declared_literal_empty_never_refuses() {
    let home = chamber_home(&[("concierge", "", "")]);
    let chamber = chamber_path(&home);
    let bd = sh("echo '[]'");
    let path = fake_lc_path("[]");
    let o = run_cfg(
        &["fayth-ready", "concierge"],
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", ""), ("SPIRA_FAYTHS", "[]"), ("SPIRA_CHAMBER", &chamber)],
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_READY_CACHE", None), ("PATH", Some(path.as_str()))],
    );
    assert_eq!((o.code, o.out.as_str()), (0, "0"), "a declared-empty literal predicate must never refuse: {}", o.err);
}

/// sp-hh599: under `spira-sentinel.service`'s own environment (`SPIRA_RELEASE`/`PATH`,
/// never `SPIRA_HOME` — wave 4.25, sp-obhv6, stopped `conf.sh` exporting it), this used to
/// refuse outright with rc 2 — the SAME rc `fayth_ready_cli_no_fayth_file_is_rc2_stdout_zero`
/// above uses for "no such fayth file", so `sentinel::summon::fayth_ready`'s `2 =>
/// ReadyAnswer::NoFayth` mapping could not tell "SPIRA_HOME is not set" apart from "this
/// chamber genuinely has no builder.fayth", and skipped every fayth every pass forever,
/// silently (this bead's own repro).
///
/// Per Ryan 2026-10-05, `dispatch` now loads `Config` (`store::load_config`, every
/// registered key) BEFORE any verb runs, for every verb but `decide`. With `SPIRA_HOME`
/// unset, that load fails first (`spira_config::process::cfg`'s own home-location) —
/// before `cmd_fayth_ready` ever reaches its own `fayth_home()` check — but the
/// Concierge's sp-hh599 decision keeps this bead's own contract intact at the dispatch
/// boundary: a config-load failure is `Outcome::error` (rc 1), never `CANNOT_TELL` (rc 2,
/// which every caller reads as "no fayth / nothing there"). So rc 1 ("could not
/// evaluate") and rc 2 ("no fayth in the chamber") stay the two distinct buckets this bead
/// named, exactly as before — just reached one layer higher up, at config load rather than
/// at `fayth_home()` itself.
#[test]
fn fayth_ready_cli_unresolvable_home_is_rc1_not_rc2_the_sp_hh599_defect() {
    let o = run_cfg(&["fayth-ready", "builder"], "", &[], &[("SPIRA_HOME", None)]);
    assert_eq!((o.code, o.out.as_str()), (1, ""), "stderr: {}", o.err);
    assert!(!o.err.contains("no fayth in the chamber"), "stderr: {}", o.err);
}

/// The same self-resolution, proven positively rather than by its failure mode: with
/// `SPIRA_HOME` unset but THIS PROCESS physically sitting inside a release layout
/// (`<release>/bin/<exe>` beside `<release>/spira/conf.sh`, exactly `own_release_root`'s own
/// contract — sp-kgzql, reused here rather than invented a fourth way), `fayth_home` must
/// still answer `<release>/spira`, matching this bead's own manual repro
/// (`SPIRA_HOME=<rel>/spira` recovers `fayth_ready`). Exercises the pure function directly
/// (`super::fayth_home`) since faking this process's own `current_exe()` end-to-end would
/// need a real exec; `own_release_root`'s own unit tests (spira-config) already cover the
/// ascending search itself.
#[test]
fn fayth_home_pure_function_prefers_the_env_override_and_falls_back_to_the_release_root() {
    // The override still wins outright — a fixture, or a lib.sh shim that already
    // resolved it, must be able to pin a different tree on purpose.
    let pinned = testkit::env(&[("SPIRA_HOME", Some("/some/pinned/tree"))]);
    assert_eq!(super::fayth_home().as_deref(), Ok(std::path::Path::new("/some/pinned/tree")));
    drop(pinned);
    let _env = testkit::env(&[("SPIRA_HOME", None)]);
    let err = super::fayth_home().unwrap_err();
    assert!(err.contains("cannot resolve SPIRA_HOME"), "{err}");
    assert!(!err.contains("is not set"), "{err}");
}

#[test]
fn fayth_ready_cli_real_count_including_zero() {
    let home = chamber_home(&[("probe", "plan", "")]);
    let chamber = chamber_path(&home);
    let bd_zero = sh("echo '[]'");
    let path_zero = fake_lc_path("[]");
    // `machine_claimable` applies `scope_label` before the fayth's own predicate does —
    // "probe"'s FAYTH_LABELS is bare "plan", so the baseline fixture's "spira" scope
    // would filter out every bead below; this test means unscoped readiness.
    let o = run_cfg(
        &["fayth-ready", "probe"],
        "",
        &[("SPIRA_BD", bd_zero.as_str()), ("SPIRA_DB", ""), ("SPIRA_FAYTHS", "[]"), ("SPIRA_CHAMBER", &chamber), ("SPIRA_SCOPE_LABEL", "")],
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_READY_CACHE", None), ("PATH", Some(path_zero.as_str()))],
    );
    assert_eq!((o.code, o.out.as_str(), o.err.as_str()), (0, "0", ""));

    let seven = ["s1", "s2", "s3", "s4", "s5", "s6", "s7"];
    let bd_seven = sh(&format!("echo '{}'", plan_rows(&seven)));
    let path = fake_lc_path(&lc_ready(&seven));
    let o2 = run_cfg(
        &["fayth-ready", "probe"],
        "",
        &[("SPIRA_BD", bd_seven.as_str()), ("SPIRA_DB", ""), ("SPIRA_FAYTHS", "[]"), ("SPIRA_CHAMBER", &chamber), ("SPIRA_SCOPE_LABEL", "")],
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_READY_CACHE", None), ("PATH", Some(path.as_str()))],
    );
    assert_eq!((o2.code, o2.out.as_str()), (0, "7"), "{}", o2.err);
}

#[test]
fn fayth_ready_cli_cache_fast_path_skips_the_query_entirely() {
    let home = chamber_home(&[("probe", "plan", "")]);
    let chamber = chamber_path(&home);
    let cache = tmp("probe 9\nother 1\n");
    let bd = sh("echo 'bd must not be called' >&2; exit 1");
    let o = run_cfg(
        &["fayth-ready", "probe"],
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_FAYTHS", "[]"), ("SPIRA_CHAMBER", &chamber)],
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_READY_CACHE", Some(cache.as_str()))],
    );
    assert_eq!((o.code, o.out.as_str(), o.err.as_str()), (0, "9", ""));
}

#[test]
fn bulk_ready_by_fayth_cli_buckets_one_fetch_by_the_chamber_roster() {
    let home = chamber_home(&[("builder", "spira,plan", ""), ("ops", "spira,ops-trigger", "")]);
    let chamber = chamber_path(&home);
    let bd = sh(
        r#"echo '[{"id":"a","labels":["spira","plan"]},{"id":"b","labels":["spira","ops-trigger"]},{"id":"c","labels":["spira","plan","spira-submitted"]}]'"#,
    );
    let path = fake_lc_path(&lc_ready(&["a", "b", "c"]));
    // SPIRA_SUBMITTED_LABEL is the baseline fixture's own declared value already.
    let o = run_cfg(
        &["bulk-ready-by-fayth"],
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", ""), ("SPIRA_FAYTHS", "[]"), ("SPIRA_CHAMBER", &chamber)],
        &[("SPIRA_HOME", Some(home.as_str())), ("PATH", Some(path.as_str()))],
    );
    assert_eq!(o.out, "builder 2\nops 1\n", "c is READY in the lifecycle: a stale spira-submitted label must not hide it");
}

#[test]
fn bulk_ready_by_fayth_cli_excludes_exactly_what_fayth_ready_excludes() {
    // sp-85p8t: e (open children) and f (addressed to another fayth) are never claimable by
    // a builder, so neither count may include them — the bulk count once did, and the
    // sentinel summoned a builder for them every pass.
    let home = chamber_home(&[("builder", "spira,plan", ""), ("ops", "spira,ops-trigger", "")]);
    let chamber = chamber_path(&home);
    let bd = sh(
        r#"echo '[{"id":"a","labels":["spira","plan"]},{"id":"e","labels":["spira","plan","spira-open-children"]},{"id":"f","labels":["spira","plan","fayth:ops"]}]'"#,
    );
    let path = fake_lc_path(&lc_ready(&["a", "e", "f"]));
    // SPIRA_SUBMITTED_LABEL/SPIRA_OPEN_CHILDREN_LABEL are the baseline fixture's own
    // declared values already.
    let o = run_cfg(
        &["bulk-ready-by-fayth"],
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", ""), ("SPIRA_FAYTHS", "[]"), ("SPIRA_CHAMBER", &chamber)],
        &[("SPIRA_HOME", Some(home.as_str())), ("PATH", Some(path.as_str())), ("SPIRA_READY_CACHE", None)],
    );
    assert_eq!(o.out, "builder 1\nops 0\n", "{}", o.err);
}

// ---- fayth-ready / bulk-ready-by-fayth: the machine's ready set --------------------------------

/// `spira-lc list` rows: each id READY with no holds.
fn lc_ready(ids: &[&str]) -> String {
    let rows: Vec<String> = ids.iter().map(|id| format!(r#"{{"bead_id":"{id}","state":"READY","holds":"[]"}}"#)).collect();
    format!("[{}]", rows.join(","))
}

/// bd's content of each id: an open task labelled `plan`.
fn plan_rows(ids: &[&str]) -> String {
    let rows: Vec<String> =
        ids.iter().map(|id| format!(r#"{{"id":"{id}","status":"open","issue_type":"task","labels":["plan"]}}"#)).collect();
    format!("[{}]", rows.join(","))
}

/// A PATH directory holding a `spira-lc` that prints `lc`.
fn fake_lc_path(lc: &str) -> Tmp {
    let dir = testkit::TempDir::new("spira-claim-lc");
    testkit::write_exe(dir.join("spira-lc"), &format!("#!/bin/sh\ncat <<'EOF'\n{lc}\nEOF\n"));
    // The system directories, never the process PATH: another test may hold a narrowed PATH
    // (an unreachable-machine fixture) while this one is built, outside the env lock.
    let path = format!("{}:/usr/bin:/bin", dir.to_string_lossy());
    Tmp { path, _dir: dir }
}

fn enforced_count(ready: &str, lc: &str, recs: &str, verb: &[&str]) -> Outcome {
    let home = chamber_home(&[("probe", "plan", "")]);
    let chamber = chamber_path(&home);
    let bd = sh(&format!("case \"$*\" in *--id*) echo '{recs}';; *) echo '{ready}';; esac"));
    let path = fake_lc_path(lc);
    // `machine_claimable` applies `scope_label` before any fayth predicate or <labels>
    // argument does — every fixture bead here carries only "plan", never "spira", so the
    // baseline fixture's own scope would filter them all out; these tests mean unscoped
    // readiness.
    run_cfg(
        verb,
        "",
        &[("SPIRA_BD", bd.as_str()), ("SPIRA_DB", ""), ("SPIRA_FAYTHS", "[]"), ("SPIRA_CHAMBER", &chamber), ("SPIRA_SCOPE_LABEL", "")],
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_READY_CACHE", None), ("PATH", Some(path.as_str()))],
    )
}

const ENFORCE_READY: &str = r#"[{"id":"S","priority":1,"labels":["plan"]},
    {"id":"D","priority":1,"labels":["plan"],"dependencies":[{"issue_id":"D","depends_on_id":"S","type":"blocks"}]},
    {"id":"R","priority":1,"labels":["plan"]}]"#;
/// bd's content of each bead, by id: the candidates and their blockers both come from here.
const ENFORCE_RECS: &str = r#"[{"id":"S","status":"open","issue_type":"task","labels":["plan"]},
    {"id":"D","priority":1,"labels":["plan"],"dependencies":[{"issue_id":"D","depends_on_id":"S","type":"blocks"}]},
    {"id":"R","priority":1,"labels":["plan"]}]"#;

#[test]
fn enforced_counts_exclude_submitted_and_blocked_beads_and_keep_one_ready() {
    let lc = r#"[{"bead_id":"S","state":"SUBMITTED","holds":"[]"},{"bead_id":"D","state":"READY","holds":"[]"},{"bead_id":"R","state":"READY","holds":"[]"}]"#;
    let one = enforced_count(ENFORCE_READY, lc, ENFORCE_RECS, &["fayth-ready", "probe"]);
    assert_eq!((one.code, one.out.as_str()), (0, "1"), "{}", one.err);
    let bulk = enforced_count(ENFORCE_READY, lc, ENFORCE_RECS, &["bulk-ready-by-fayth"]);
    assert_eq!((bulk.code, bulk.out.as_str()), (0, "probe 1\n"), "{}", bulk.err);

    let all_held = r#"[{"bead_id":"S","state":"SUBMITTED","holds":"[]"},{"bead_id":"D","state":"READY","holds":"[]"},{"bead_id":"R","state":"SUBMITTED","holds":"[]"}]"#;
    let zero = enforced_count(ENFORCE_READY, all_held, ENFORCE_RECS, &["fayth-ready", "probe"]);
    assert_eq!((zero.code, zero.out.as_str()), (0, "0"), "{}", zero.err);
    let bulk0 = enforced_count(ENFORCE_READY, all_held, ENFORCE_RECS, &["bulk-ready-by-fayth"]);
    assert_eq!((bulk0.code, bulk0.out.as_str()), (0, "probe 0\n"), "{}", bulk0.err);
}

#[test]
fn enforced_counts_refuse_when_the_lifecycle_machine_cannot_answer() {
    let o = enforced_count(ENFORCE_READY, "not json", ENFORCE_RECS, &["fayth-ready", "probe"]);
    assert_eq!((o.code, o.out.as_str()), (1, "0"), "{}", o.err);
    let b = enforced_count(ENFORCE_READY, "not json", ENFORCE_RECS, &["bulk-ready-by-fayth"]);
    assert_ne!(b.code, 0);
    assert_eq!(b.out, "");
}

// ---- the lifecycle row is the claim (sp-860zj) -------------------------------------------
//
// bd's status and assignee are content nobody reads for claimability: the candidate set is
// the lifecycle machine's READY/REWORK rows. bd below answers its own `list --status open
// --no-assignee` with W alone (open, unassigned) and holds P in_progress under a dead
// aeon's name; the machine says the opposite.

const BD_OPEN_UNASSIGNED: &str = r#"[{"id":"W","status":"open","priority":1,"labels":["plan"]}]"#;
const BD_BOTH: &str = r#"[{"id":"W","status":"open","priority":1,"issue_type":"task","labels":["plan"]},
    {"id":"P","status":"in_progress","assignee":"aeon-gone","priority":1,"issue_type":"task","labels":["plan"]}]"#;
const LC_W_HELD_P_READY: &str =
    r#"[{"bead_id":"W","state":"WORKING","holder":"aeon-live","holds":"[]"},{"bead_id":"P","state":"READY","holds":"[]"}]"#;

fn ids_of(json: &str) -> Vec<String> {
    rank::parse_ready(json).unwrap().into_iter().map(|r| r.id).collect()
}

#[test]
fn a_bead_bd_shows_in_progress_is_ready_when_its_lifecycle_row_is_ready() {
    let one = enforced_count(BD_OPEN_UNASSIGNED, LC_W_HELD_P_READY, BD_BOTH, &["fayth-ready", "probe"]);
    assert_eq!((one.code, one.out.as_str()), (0, "1"), "{}", one.err);
    let set = enforced_count(BD_OPEN_UNASSIGNED, LC_W_HELD_P_READY, BD_BOTH, &["fayth-ready", "probe", "--json"]);
    assert_eq!(set.code, 0, "{}", set.err);
    assert_eq!(ids_of(&set.out), vec!["P".to_string()], "the aeon's ready set is the count's own rows");
    let bulk = enforced_count(BD_OPEN_UNASSIGNED, LC_W_HELD_P_READY, BD_BOTH, &["bulk-ready-by-fayth"]);
    assert_eq!(bulk.out, "probe 1\n", "{}", bulk.err);
    let n = enforced_count(BD_OPEN_UNASSIGNED, LC_W_HELD_P_READY, BD_BOTH, &["ready-count", "plan", ""]);
    assert_eq!((n.code, n.out.as_str()), (0, "1"), "{}", n.err);
}

#[test]
fn a_bead_bd_shows_open_and_unassigned_is_not_ready_while_its_row_is_working() {
    let lc = r#"[{"bead_id":"W","state":"WORKING","holder":"aeon-live","holds":"[]"},{"bead_id":"P","state":"SUBMITTED","holds":"[]"}]"#;
    for verb in [&["fayth-ready", "probe"][..], &["ready-count", "plan", ""][..]] {
        let o = enforced_count(BD_OPEN_UNASSIGNED, lc, BD_BOTH, verb);
        assert_eq!((o.code, o.out.as_str()), (0, "0"), "{verb:?}: {}", o.err);
    }
    let set = enforced_count(BD_OPEN_UNASSIGNED, lc, BD_BOTH, &["fayth-ready", "probe", "--json"]);
    assert_eq!((set.code, ids_of(&set.out)), (0, Vec::<String>::new()), "{}", set.err);
}

#[test]
fn a_held_ready_row_is_not_ready_but_a_wait_hold_is_judged_through_its_blockers() {
    let lc = r#"[{"bead_id":"W","state":"READY","holds":"[\"poison\"]"},{"bead_id":"P","state":"REWORK","holds":"[\"wait\"]"}]"#;
    let set = enforced_count(BD_OPEN_UNASSIGNED, lc, BD_BOTH, &["fayth-ready", "probe", "--json"]);
    assert_eq!(ids_of(&set.out), vec!["P".to_string()], "{}", set.err);
}

// ---- ready-count --json: the ready set a display reads (sp-7g5q6) ------------------------
//
// The cockpit's NEXT rows asked bd ready itself; under the machine that set is bd's status
// and assignee, which nobody claims by any more. `ready-count --json` is the same set the
// count is, so a display and the summoner cannot disagree.

#[test]
fn ready_count_json_is_the_machine_set_with_titles() {
    let bd_both = r#"[{"id":"W","title":"held","status":"open","priority":1,"issue_type":"task","labels":["plan"]},
    {"id":"P","title":"take me","status":"in_progress","assignee":"aeon-gone","priority":1,"issue_type":"task","labels":["plan"]}]"#;
    let set = enforced_count(BD_OPEN_UNASSIGNED, LC_W_HELD_P_READY, bd_both, &["ready-count", "plan", "spira-poison", "--json"]);
    assert_eq!(set.code, 0, "{}", set.err);
    let rows = rank::parse_ready(&set.out).unwrap();
    assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["P"]);
    assert_eq!(rows[0].title.as_deref(), Some("take me"));
    let none = enforced_count(BD_OPEN_UNASSIGNED, LC_W_HELD_P_READY, bd_both, &["ready-count", "plan", "plan", "--json"]);
    assert_eq!((none.code, ids_of(&none.out)), (0, Vec::<String>::new()), "{}", none.err);
    let refused = enforced_count(BD_OPEN_UNASSIGNED, "not json", bd_both, &["ready-count", "plan", "", "--json"]);
    assert_eq!((refused.code, refused.out.as_str()), (1, ""), "a machine that cannot answer is no empty queue");
}

// ---- one resolution path: the label keys come from the config file, never the caller's env ----

const OC_LABEL: &str = "oc-fixture";

fn label_ready_set(extra_toml: &[(&str, &str)], exported: Option<&str>) -> Outcome {
    let rows = format!(
        r#"[{{"id":"A","priority":1,"issue_type":"task","labels":["plan","{OC_LABEL}"]}},{{"id":"B","priority":1,"issue_type":"task","labels":["plan"]}}]"#
    );
    let lc = r#"[{"bead_id":"A","state":"READY","holds":"[]"},{"bead_id":"B","state":"READY","holds":"[]"}]"#;
    let home = chamber_home(&[("probe", "plan", "")]);
    let chamber = chamber_path(&home);
    let bd = sh(&format!("echo '{rows}'"));
    let path = fake_lc_path(lc);
    let mut declare = vec![
        ("SPIRA_BD", bd.as_str()),
        ("SPIRA_DB", ""),
        ("SPIRA_FAYTHS", "[]"),
        ("SPIRA_CHAMBER", chamber.as_str()),
        ("SPIRA_SCOPE_LABEL", ""),
        ("SPIRA_OPEN_CHILDREN_LABEL", OC_LABEL),
    ];
    declare.extend_from_slice(extra_toml);
    run_cfg(
        &["fayth-ready", "probe", "--json"],
        "",
        &declare,
        &[("SPIRA_HOME", Some(home.as_str())), ("SPIRA_READY_CACHE", None), ("PATH", Some(path.as_str())), ("SPIRA_OPEN_CHILDREN_LABEL", exported)],
    )
}

#[test]
fn the_ready_set_is_the_same_whether_or_not_the_caller_exports_the_label_variables() {
    let unexported = label_ready_set(&[], None);
    let exported_empty = label_ready_set(&[], Some(""));
    let exported_other = label_ready_set(&[], Some("something-else"));
    assert_eq!(unexported.code, 0, "{}", unexported.err);
    assert_eq!(ids_of(&unexported.out), vec!["B".to_string()], "the declared exclusion applies");
    assert_eq!(exported_empty.out, unexported.out, "{}", exported_empty.err);
    assert_eq!(exported_other.out, unexported.out, "{}", exported_other.err);
}

#[test]
fn an_absent_registry_key_is_refused_by_name_never_resolved_to_empty() {
    let toml = toml_fixture(&[]);
    let text = std::fs::read_to_string(toml.as_str()).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("open_children_label")).collect();
    assert!(kept.len() < text.lines().count(), "the plant must remove the key, or this test proves nothing");
    std::fs::write(toml.as_str(), kept.join("\n")).unwrap();
    let _env = testkit::env(&[("SPIRA_HOME", Some(REAL_SPIRA_HOME)), ("SPIRA_TOML", Some(toml.as_str()))]);
    let o = exec_spira_claim(&["fayth-ready", "probe"], "");
    assert_ne!(o.code, 0, "{}", o.out);
    assert!(o.err.contains("SPIRA_OPEN_CHILDREN_LABEL"), "the refusal names the key: {}", o.err);
}

#[test]
fn every_lifecycle_read_carries_a_filter() {
    let log = tmp("");
    let st = fake_bd_lc("echo '[]'", &format!("printf '%s\\n' \"$*\" >> {log}; echo '[]'"));
    st.lifecycle_in_states(&["READY", "REWORK"]).unwrap();
    st.lifecycle_of(&["a".into(), "b".into()]).unwrap();
    assert!(st.lifecycle_of(&[]).unwrap().is_empty());
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(calls.lines().collect::<Vec<_>>(), ["list --state READY,REWORK", "list --ids a,b"], "{calls}");
}

#[test]
fn no_call_site_reads_the_unfiltered_store() {
    let src = include_str!("store.rs");
    assert_eq!(src.matches("c.arg(\"list\")").count(), 1, "one spira-lc list site");
    assert!(src.contains("c.arg(\"list\").args(filter)"));
}

#[test]
fn release_claim_writes_only_to_a_working_row() {
    let log = tmp("");
    let lc = format!(
        "printf '%s\\n' \"$*\" >> {log}; case \"$1\" in list) echo \"[{{\\\"bead_id\\\":\\\"sp-x\\\",\\\"state\\\":\\\"$STATE\\\"}}]\";; esac"
    );
    for (state, writes) in [("REWORK", false), ("SUBMITTED", false), ("READY", false), ("WORKING", true)] {
        std::fs::write(&log, "").unwrap();
        let st = fake_bd_lc("echo '[]'", &format!("STATE={state}; {lc}"));
        st.release_claim("sp-x", "t").unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(calls.lines().any(|l| l.starts_with("release ")), writes, "{state}: {calls}");
    }
}

#[test]
fn express_is_a_bool_flag_and_never_swallows_what_follows() {
    let raw: Vec<String> = ["labels", "", "--express"].iter().map(|s| s.to_string()).collect();
    let a = Args::parse(&raw).expect("--express takes no value");
    assert!(a.has("--express"));
    assert_eq!(a.pos, vec!["labels".to_string(), String::new()]);
}
