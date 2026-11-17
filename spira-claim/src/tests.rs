//! CLI-level tests: the contract at the interface (exit codes, stdout shapes), with every
//! store read replaced by a file (`--events`, `--ready`, `--epics`, `--lifecycle`,
//! `--blocker-records`) or by a fake `bdq` script driven through [`Store`] directly.

use super::*;

// ---- select --------------------------------------------------------------------------

// ---- select --blockers machine: the stacked-dependents parity fixture ------------------

// ---- the lifecycle switch (DESIGN.md §6a) ------------------------------------------------

// ---- stack: the dwarf's own claim-time proposal ----------------------------------------

// ---- the store, through a fake bdq ----------------------------------------------------

// ---- ready / claim CLI (wave 4.25, sp-obhv6) -------------------------------------------
//
// `rune-ready`/`rune-exclude`/`bulk-ready-by-rune`/`claim-retry` read real process
// environment (`SPIRA_HOME`, `SPIRA_TOML`, ...) — unlike the rest of this
// file's tests, which thread every input through a file or a hand-built `Store`. These
// edit the environment only through `testkit::env`, which serializes them against every
// other test in this binary and restores on drop; `run` takes the same lock to read.

/// `rune_home` is the declared home: `SPIRA_HOME` wins, else `$SPIRA_RELEASE/spira`, and
/// neither set is a refusal naming both — never a search from this executable.
#[test]
fn rune_home_is_spira_home_then_the_declared_release_and_refuses_naming_both() {
    let pinned = testkit::env(&[("SPIRA_HOME", Some("some/pinned/tree"))]);
    assert_eq!(super::rune_home().as_deref(), Ok(std::path::Path::new("some/pinned/tree")));
    drop(pinned);
    let rel = testkit::env(&[("SPIRA_HOME", None), ("SPIRA_RELEASE", Some("r"))]);
    assert_eq!(super::rune_home().as_deref(), Ok(std::path::Path::new("r/spira")));
    drop(rel);
    let _env = testkit::env(&[("SPIRA_HOME", None), ("SPIRA_RELEASE", None)]);
    let err = super::rune_home().unwrap_err();
    assert!(err.contains("SPIRA_HOME") && err.contains("SPIRA_RELEASE"), "{err}");
}

// ---- rune-ready / bulk-ready-by-rune: the machine's ready set --------------------------------
// ---- the lifecycle row is the claim (sp-860zj) -------------------------------------------
//
// bd's status and assignee are content nobody reads for claimability: the candidate set is
// the lifecycle machine's READY/REWORK rows. bd below answers its own `list --status open
// --no-assignee` with W alone (open, unassigned) and holds P in_progress under a dead
// dwarf's name; the machine says the opposite.
// ---- ready-count --json: the ready set a display reads (sp-7g5q6) ------------------------
//
// The cockpit's NEXT rows asked bd ready itself; under the machine that set is bd's status
// and assignee, which nobody claims by any more. `ready-count --json` is the same set the
// count is, so a display and the caller cannot disagree.

// ---- one resolution path: the label keys come from the config file, never the caller's env ----

#[test]
fn no_call_site_reads_the_unfiltered_store() {
    let src = include_str!("store.rs");
    assert_eq!(src.matches("once(\"list\")").count(), 1, "one spira-lc list site");
    assert!(src.contains("once(\"list\").chain(filter.iter().copied())"));
}

/// An in-process caller's environment answers every read on the dispatch path; this
/// process's own variables are never consulted.
#[test]
fn dispatch_in_reads_the_callers_environment_not_the_process() {
    let mut stdin = std::io::empty();
    let lookup = |k: &str| match k {
        "SPIRA_POISON_AT" => Some("7".to_string()),
        "SPIRA_DWARF" => Some("dwarf-x".to_string()),
        "BEADS_ACTOR" => Some("  ".to_string()),
        _ => None,
    };
    let env = Env { stdin: &mut stdin, config: Config::default(), lookup: &lookup };
    let a = Args::parse(&["--bead".into(), "sp-a".into(), "--cause".into(), "c".into()]).unwrap();
    assert_eq!(unpoison_opts(&a, &env).unwrap().poison_at, 7);
    assert!(own_dwarf(&env));
    assert_eq!(env_nonempty(&env, "BEADS_ACTOR"), None, "blank is unset");
    let none = |_: &str| None;
    let env = Env { stdin: &mut stdin, config: Config::default(), lookup: &none };
    assert!(!own_dwarf(&env));
    assert_eq!(unpoison_opts(&a, &env).unwrap().poison_at, Thresholds::default().poison_at);
}

#[test]
fn express_is_a_bool_flag_and_never_swallows_what_follows() {
    let raw: Vec<String> = ["labels", "", "--express"].iter().map(|s| s.to_string()).collect();
    let a = Args::parse(&raw).expect("--express takes no value");
    assert!(a.has("--express"));
    assert_eq!(a.pos, vec!["labels".to_string(), String::new()]);
}

// ---- readiness is published for every live state ----------------------------------------

mod published {
    use super::*;
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const LIVE: &str = "OPEN,READY,WORKING,SUBMITTED,CERTIFIED,IN_DELIVERY,REWORK";

    struct Service(Mutex<Vec<Vec<String>>>);

    impl Transport for Service {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            let mut seen = argv.to_vec();
            seen.extend(stdin.map(|b| format!("stdin={b}")));
            self.0.lock().unwrap().push(seen);
            let out = match argv.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                ["list", "--state", "READY,REWORK", ..] => r#"[{"bead_id":"sp-r","state":"READY","holds":[]}]"#.to_string(),
                ["list", "--state", live, ..] if *live == LIVE => [
                    r#"{"bead_id":"sp-r","state":"READY","holds":[]}"#,
                    r#"{"bead_id":"sp-c","state":"CERTIFIED","holds":[],"tip":"abc"}"#,
                    r#"{"bead_id":"sp-s","state":"SUBMITTED","holds":[]}"#,
                    r#"{"bead_id":"sp-n","state":"READY","holds":[]}"#,
                    r#"{"bead_id":"sp-w","state":"WORKING","holds":[]}"#,
                ]
                .join(",")
                .pipe(|rows| format!("[{rows}]")),
                ["list", "--ids", _] => r#"[{"bead_id":"sp-w","state":"WORKING","holds":[]}]"#.to_string(),
                ["plan", "watermark"] => r#"{"watermark":7}"#.to_string(),
                ["plan", "edges", "--ids", "sp-n", "--dependents"] => r#"{"watermark":7,"edges":[]}"#.to_string(),
                ["plan", "edges", "--ids", "sp-w", "--dependents"] => r#"{"watermark":7,"edges":[
                    {"issue_id":"sp-r","depends_on_id":"sp-w","type":"blocks"},
                    {"issue_id":"sp-s","depends_on_id":"sp-w","type":"parent-child"},
                    {"issue_id":"sp-x","depends_on_id":"sp-w","type":"related"}]}"#
                    .to_string(),
                ["plan", "items", "--ids", ids] if ids.contains("sp-r") => [
                    r#"{"id":"sp-r","dependencies":[{"issue_id":"sp-r","depends_on_id":"sp-w","type":"blocks"}]}"#,
                    r#"{"id":"sp-c"}"#,
                    r#"{"id":"sp-s","dependencies":[{"issue_id":"sp-s","depends_on_id":"sp-w","type":"blocks"}]}"#,
                ]
                .join(",")
                .pipe(|rows| format!(r#"{{"watermark":7,"items":[{rows}]}}"#)),
                ["plan", "items", ..] => r#"{"watermark":7,"items":[]}"#.to_string(),
                _ => String::new(),
            };
            Ok(Reply { code: 0, out, err: String::new() })
        }
    }

    struct Refusing(Service);

    impl Transport for Refusing {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, d: Duration) -> Result<Reply, String> {
            let r = self.0.exchange(argv, stdin, d)?;
            Ok(if argv[0] == "publish-readiness" { Reply { code: 3, out: String::new(), err: "refused: no payload".into() } } else { r })
        }
    }

    trait Pipe: Sized {
        fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
            f(self)
        }
    }
    impl Pipe for String {}

    fn judged(control_store: bool, publish: bool) -> (Vec<Vec<String>>, Vec<verdicts::Entry>) {
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let st = Store { bdq: "nonexistent/bdq".into(), lc: LcClient::over(svc.clone(), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None };
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store, run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
        let env = Env { stdin: &mut stdin, config, lookup: &none };
        let a = Args::parse(&[]).unwrap();
        let (_, entries) = judge_rows(&a, &env, &st, None, publish).unwrap();
        let seen = svc.0.lock().unwrap().clone();
        (seen, entries)
    }

    fn published(seen: &[Vec<String>]) -> Option<serde_json::Value> {
        let call = seen.iter().find(|c| c[0] == "publish-readiness")?;
        serde_json::from_str(call[1].strip_prefix("stdin=")?).ok()
    }

    #[test]
    fn judge_rows_publishes_readiness_for_certified_and_submitted_records_with_the_gates_it_read() {
        let (seen, _) = judged(true, true);
        let p = published(&seen).expect("no publish-readiness call");
        assert_eq!(p["planning_watermark"], "7");
        let entry = |id: &str| p["entries"].as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no entry for {id}: {p}")).clone();
        let working = serde_json::to_value(spira_config::edges::gate(lifecycle::bead::BeadState::Working, None)).unwrap();
        assert_eq!((entry("sp-s")["claimable"].clone(), entry("sp-s")["blocked_by"].clone(), entry("sp-s")["gates"]["sp-w"].clone()), (false.into(), serde_json::json!(["sp-w"]), working.clone()));
        assert_eq!((entry("sp-c")["claimable"].clone(), entry("sp-c")["blocked_by"].clone()), (false.into(), serde_json::json!([])));
        assert_eq!((entry("sp-r")["claimable"].clone(), entry("sp-r")["blocked_by"].clone(), entry("sp-r")["gates"]["sp-w"].clone()), (false.into(), serde_json::json!(["sp-w"]), working));
    }

    #[test]
    fn the_planning_watermark_is_read_before_the_rows_it_stamps() {
        let (seen, _) = judged(true, true);
        let at = |what: &[&str]| seen.iter().position(|c| c.iter().map(String::as_str).take(what.len()).eq(what.iter().copied())).unwrap();
        assert!(at(&["plan", "watermark"]) < at(&["list", "--state", LIVE]));
    }

    #[test]
    fn a_reading_pass_publishes_nothing_and_reads_no_live_set() {
        let (seen, entries) = judged(true, false);
        assert!(published(&seen).is_none());
        assert!(seen.iter().all(|c| c[0] != "publish-readiness" && c[..] != ["plan", "watermark"]), "{seen:?}");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn machine_claimable_is_a_reading_pass_and_machine_judged_publishes_only_when_told_to() {
        let run = |publish: bool| {
            let svc = Arc::new(Service(Mutex::new(Vec::new())));
            let mut stdin = std::io::empty();
            let none = |_: &str| None;
            let config = Config { control_store: true, run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
            let env = Env { stdin: &mut stdin, config, lookup: &none };
            machine_judged(&Args::parse(&[]).unwrap(), &env, &store_over(svc.clone()), publish).unwrap();
            let seen = svc.0.lock().unwrap().clone();
            seen
        };
        assert!(published(&run(false)).is_none());
        assert!(published(&run(true)).is_some());
    }

    #[test]
    fn nothing_is_published_while_the_control_store_is_not_in_use() {
        let (seen, entries) = judged(false, true);
        assert!(published(&seen).is_none());
        assert_eq!(entries.len(), 1);
    }

    fn store_over(t: Arc<dyn Transport>) -> Store {
        Store { bdq: "nonexistent/bdq".into(), lc: LcClient::over(t, true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None }
    }

    fn judge_over(t: Arc<dyn Transport>) {
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
        let env = Env { stdin: &mut stdin, config, lookup: &none };
        judge_rows(&Args::parse(&[]).unwrap(), &env, &store_over(t), None, true).unwrap();
    }

    #[test]
    fn a_refused_readiness_publish_raises_the_alert_that_pages() {
        let svc = Arc::new(Refusing(Service(Mutex::new(Vec::new()))));
        judge_over(svc.clone());
        let seen = svc.0 .0.lock().unwrap().clone();
        let raised = seen.iter().find(|c| c[0] == "condition" && c[1] == "set").unwrap_or_else(|| panic!("no alert raised: {seen:?}"));
        assert_eq!(raised[2], store::READINESS_ALERT);
        assert!(raised.windows(2).any(|w| w == ["--status", "True"]) && raised.windows(2).any(|w| w == ["--owner", "heimdall"]), "{raised:?}");
        assert!(raised.iter().any(|a| a.contains("refused: no payload")), "{raised:?}");
        assert!(!seen.iter().any(|c| c[0] == "condition" && c[1] == "clear"));
    }

    #[test]
    fn an_accepted_readiness_publish_clears_the_alert() {
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        judge_over(svc.clone());
        let seen = svc.0.lock().unwrap().clone();
        assert!(seen.iter().any(|c| c[0] == "condition" && c[1] == "clear" && c[2] == store::READINESS_ALERT), "{seen:?}");
        assert!(!seen.iter().any(|c| c[0] == "condition" && c[1] == "set"));
    }

    #[test]
    fn republish_dependents_judges_and_publishes_only_the_records_that_wait_on_or_descend_from_the_changed() {
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let st = Store { bdq: "nonexistent/bdq".into(), lc: LcClient::over(svc.clone(), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None };
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
        let env = Env { stdin: &mut stdin, config, lookup: &none };
        let a = Args::parse(&["--changed".into(), "sp-w".into()]).unwrap();
        assert_eq!(republish_dependents(&a, &env, &st), Ok(3));
        let p = published(&svc.0.lock().unwrap()).expect("no publish-readiness call");
        let mut ids: Vec<&str> = p["entries"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, ["sp-r", "sp-s", "sp-w"], "sp-c is neither waiting on sp-w nor under it, and a related edge holds nothing back; the changed record is judged with them");
    }

    #[test]
    fn republish_dependents_judges_the_changed_record_itself_when_nothing_waits_on_it() {
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let st = Store { bdq: "nonexistent/bdq".into(), lc: LcClient::over(svc.clone(), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None };
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
        let env = Env { stdin: &mut stdin, config, lookup: &none };
        let a = Args::parse(&["--changed".into(), "sp-n".into()]).unwrap();
        assert_eq!(republish_dependents(&a, &env, &st), Ok(1));
        let p = published(&svc.0.lock().unwrap()).expect("no publish-readiness call");
        let ids: Vec<&str> = p["entries"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["sp-n"]);
    }
}

// ---- a reader answers from the claim view, asked when it reads --------------------------

mod claim_view_read {
    use super::*;
    use serde_json::{json, Value};
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn row(id: &str, state: &str) -> Value {
        json!({ "bead_id": id, "state": state, "holds": [], "tip": null })
    }

    fn item(id: &str) -> Value {
        json!({ "id": id, "title": id, "priority": 2, "issue_type": "task", "labels": ["repo:spira"], "dependencies": [] })
    }

    /// A store whose claim view says `sp-a` is claimable for `builder` and `sp-b` is blocked, and
    /// whose stored readiness says both are claimable. `view_code` is the exit of the view read.
    struct Double {
        seen: Mutex<Vec<Vec<String>>>,
        view_code: i32,
    }

    impl Transport for Double {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            self.seen.lock().unwrap().push(argv.to_vec());
            let words: Vec<&str> = argv.iter().map(String::as_str).collect();
            let (code, out, err) = match words.as_slice() {
                ["claimable"] if self.view_code != 0 => (self.view_code, String::new(), "the control store is unreadable".to_string()),
                ["claimable"] => (
                    0,
                    json!({ "rev": 1, "cannot_tell": 0, "beads": [
                        { "bead_id": "sp-a", "verdict": "claimable", "blocker": null, "runes": ["builder"], "row": row("sp-a", "READY") },
                        { "bead_id": "sp-b", "verdict": "blocked", "blocker": "sp-a", "runes": [], "row": row("sp-b", "READY") },
                    ] })
                    .to_string(),
                    String::new(),
                ),
                ["ready-set"] => (
                    0,
                    json!({ "planning_watermark": "7", "judged_at": verdicts::stamp(ready::now_epoch()), "unjudged": 0, "lifecycle": [row("sp-a", "READY"), row("sp-b", "READY")], "items": [item("sp-a"), item("sp-b")] }).to_string(),
                    String::new(),
                ),
                ["list", "--express", ..] => (0, "[]".into(), String::new()),
                ["list", "--state", ..] => (0, json!([row("sp-a", "READY"), row("sp-b", "READY")]).to_string(), String::new()),
                ["plan", "items", "--ids", ids] => {
                    let wanted: Vec<&str> = ids.split(',').collect();
                    let items: Vec<Value> = [item("sp-a"), item("sp-b")].into_iter().filter(|i| wanted.contains(&i["id"].as_str().unwrap())).collect();
                    (0, json!({ "watermark": 7, "items": items }).to_string(), String::new())
                }
                ["plan", "watermark"] => (0, r#"{"watermark":7}"#.into(), String::new()),
                _ => (0, String::new(), String::new()),
            };
            Ok(Reply { code, out, err })
        }
    }

    fn setup(view_code: i32) -> (Arc<Double>, Store) {
        let svc = Arc::new(Double { seen: Mutex::new(Vec::new()), view_code });
        let st = Store { bdq: "nonexistent/bdq".into(), lc: LcClient::over(svc.clone(), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None };
        (svc, st)
    }

    fn env_over<R>(f: impl FnOnce(&mut Env) -> R) -> R {
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, stack_max_depth: 4, run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
        f(&mut Env { stdin: &mut stdin, config, lookup: &none })
    }

    fn ids(rows: &[rank::ReadyRow]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    fn seen(svc: &Double) -> Vec<String> {
        svc.seen.lock().unwrap().iter().map(|a| a.join(" ")).collect()
    }

    #[test]
    fn rune_ready_lists_the_beads_the_view_says_the_rune_would_take() {
        let (svc, st) = setup(0);
        let view = env_over(|env| claimable_over(&Args::parse(&[]).unwrap(), env, &st).unwrap());
        assert_eq!(ids(&taken_by(&view, "builder").into_iter().cloned().collect::<Vec<_>>()), ["sp-a"]);
        assert!(taken_by(&view, "ops").is_empty(), "a rune the view does not name takes nothing");
        assert!(!seen(&svc).iter().any(|c| c == "ready-set"), "{:?}", seen(&svc));
    }

    #[test]
    fn select_machine_keeps_only_the_beads_the_view_lists_as_claimable() {
        let (svc, st) = setup(0);
        let a = Args::parse(&["--rune".into(), "builder".into(), "--blockers".into(), "machine".into()]).unwrap();
        let mut rows = st.items_by_ids(&["sp-a".to_string(), "sp-b".to_string()]).unwrap();
        svc.seen.lock().unwrap().clear();
        let lc = env_over(|env| machine_filter(&a, env, &st, &mut rows).unwrap_or_else(|o| panic!("{o:?}")));
        assert_eq!(ids(&rows), ["sp-a"]);
        assert!(lc.contains_key("sp-a"));
        assert!(!seen(&svc).iter().any(|c| c == "ready-set"), "{:?}", seen(&svc));
    }

    #[test]
    fn bulk_counts_come_from_the_runes_the_view_names() {
        let (_, st) = setup(0);
        let view = env_over(|env| viewed(&Args::parse(&[]).unwrap(), env, &st).unwrap());
        let parts = vec![ready::RunePart { name: "builder".into(), inc: vec![], exc: vec![] }, ready::RunePart { name: "ops".into(), inc: vec![], exc: vec![] }];
        assert_eq!(ready::bucket_taken(&view.rows, &view.runes, &parts), [("builder".to_string(), 1), ("ops".to_string(), 0)]);
    }

    #[test]
    fn ready_count_counts_the_view_claimable_rows() {
        let (_, st) = setup(0);
        let n = env_over(|env| machine_claimable_shared(&Args::parse(&[]).unwrap(), env, &st).unwrap().len());
        assert_eq!(n, 1);
    }

    #[test]
    fn urgent_lists_the_views_rework_beads_and_reads_the_view_once() {
        let (svc, st) = setup(0);
        let out = env_over(|env| urgent_over(&Args::parse(&[]).unwrap(), env, &st).unwrap());
        assert_eq!(out, "", "sp-a is READY at priority 2, so nothing outranks the lanes");
        let calls = seen(&svc);
        assert_eq!(calls.iter().filter(|c| *c == "claimable").count(), 1, "{calls:?}");
        assert!(!calls.iter().any(|c| c == "ready-set"), "{calls:?}");
    }

    #[test]
    fn ready_count_express_counts_the_views_claimable_rows_that_are_express() {
        let (_, st) = setup(0);
        let n = env_over(|env| ready_count_over(&Args::parse(&["".into(), "--express".into()]).unwrap(), env, &st));
        assert_eq!((n.code, n.out.as_str()), (0, "0"), "{n:?}");
        let n = env_over(|env| ready_count_over(&Args::parse(&["".into()]).unwrap(), env, &st));
        assert_eq!((n.code, n.out.as_str()), (0, "1"), "{n:?}");
    }

    #[test]
    fn an_unreadable_view_is_the_reads_error_and_nothing_judges_in_its_place() {
        let (svc, st) = setup(2);
        let err = env_over(|env| claimable_over(&Args::parse(&[]).unwrap(), env, &st).err().expect("the view is unreadable"));
        assert!(err.contains("claimable") && err.contains("unreadable"), "{err}");
        let calls = seen(&svc);
        assert_eq!(calls, ["claimable"], "no content read, no lifecycle read, no judging: {calls:?}");
    }
}

#[test]
fn the_verbs_a_caller_waits_on_push_no_run_metric_when_they_succeed() {
    let hot = |v: &str, code| discards_run_metric(&[v.to_string(), "builder".into()], code);
    assert!(hot("rune-ready", 0) && hot("ready-count", 0) && hot("urgent", 0));
    assert!(!hot("rune-ready", 1), "a failing read still reports its failure");
    assert!(!hot("bulk-ready-by-rune", 0) && !hot("unpoison", 0) && !discards_run_metric(&[], 0));
}

fn fake_proc(t: &testkit::TempDir, pids: &[(&str, std::path::PathBuf)]) -> std::path::PathBuf {
    let proc = t.join("proc");
    for (pid, cwd) in pids {
        std::fs::create_dir_all(proc.join(pid)).unwrap();
        std::os::unix::fs::symlink(cwd, proc.join(pid).join("cwd")).unwrap();
    }
    std::fs::create_dir_all(proc.join("self")).unwrap();
    proc
}

fn ready(id: &str) -> rank::ReadyRow {
    rank::ReadyRow { id: id.into(), ..Default::default() }
}

#[test]
fn a_bead_whose_worktree_a_live_process_holds_is_dropped_and_returns_once_it_exits() {
    let t = testkit::TempDir::new("claim-occupied");
    let run = t.join("run");
    for d in ["sp-a/sub", "sp-b"] {
        std::fs::create_dir_all(run.join("worktree").join(d)).unwrap();
    }
    let proc = fake_proc(&t, &[("100", run.join("worktree/sp-a/sub"))]);
    let mut rows = vec![ready("sp-a"), ready("sp-b")];
    assert_eq!(retain_unoccupied(&mut rows, &proc, &run), vec!["sp-a".to_string()]);
    assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["sp-b"]);
    std::fs::remove_dir_all(proc.join("100")).unwrap();
    let mut rows = vec![ready("sp-a"), ready("sp-b")];
    assert!(retain_unoccupied(&mut rows, &proc, &run).is_empty());
    assert_eq!(rows.len(), 2);
}

#[test]
fn a_worktree_sharing_only_a_name_prefix_does_not_occupy_the_bead() {
    let t = testkit::TempDir::new("claim-occupied-prefix");
    let run = t.join("run");
    std::fs::create_dir_all(run.join("worktree/sp-a2")).unwrap();
    let proc = fake_proc(&t, &[("100", run.join("worktree/sp-a2"))]);
    let mut rows = vec![ready("sp-a")];
    assert!(retain_unoccupied(&mut rows, &proc, &run).is_empty());
    assert_eq!(rows.len(), 1);
}

fn stopped_repo_rows(t: &testkit::TempDir, owner: &str) -> (std::path::PathBuf, Vec<rank::ReadyRow>) {
    let run = t.join("run");
    spira_config::scoped_stop::pause_repo(&run, "shop", &["claims".to_string()], owner, "broken", "free text", 1).unwrap();
    let mut fix = ready("sp-fix");
    fix.labels = vec!["repo:shop".into(), spira_config::scoped_stop::CONTRACT_FIX_LABEL.into()];
    let mut other = ready("sp-other");
    other.labels = vec!["repo:shop".into()];
    (run, vec![fix, other])
}

#[test]
fn the_bead_that_fixes_a_repo_contract_stop_survives_it_and_its_siblings_do_not() {
    let t = testkit::TempDir::new("claim-contract-fix");
    let (run, mut rows) = stopped_repo_rows(&t, spira_config::scoped_stop::CONTRACT_OWNER);
    assert_eq!(retain_unstopped(&mut rows, &run), vec!["sp-other".to_string()]);
    assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["sp-fix"]);
}

#[test]
fn the_contract_fix_label_does_not_pass_a_stop_another_owner_holds() {
    let t = testkit::TempDir::new("claim-contract-fix-other-owner");
    let (run, mut rows) = stopped_repo_rows(&t, "operator");
    assert_eq!(retain_unstopped(&mut rows, &run).len(), 2);
    assert!(rows.is_empty());
}
