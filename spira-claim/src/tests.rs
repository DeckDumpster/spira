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
    let pinned = testkit::env(&[("SPIRA_HOME", Some("/some/pinned/tree"))]);
    assert_eq!(super::rune_home().as_deref(), Ok(std::path::Path::new("/some/pinned/tree")));
    drop(pinned);
    let rel = testkit::env(&[("SPIRA_HOME", None), ("SPIRA_RELEASE", Some("/r"))]);
    assert_eq!(super::rune_home().as_deref(), Ok(std::path::Path::new("/r/spira")));
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

// ---- ready-count shares one computation per ready_ttl_s (sp-su318h) ----------------------

// ---- readiness is published for every live state ----------------------------------------

mod published {
    use super::*;
    use spira_config::lc_call::{LcClient, Reply, Transport};
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
        let st = Store { bdq: "/nonexistent/bdq".into(), lc: LcClient::over(svc.clone(), true), lc_bin: "/nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None };
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store, run: "/nonexistent/run".into(), queue_dir: "/nonexistent/queue".into(), ..Config::default() };
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
            let config = Config { control_store: true, run: "/nonexistent/run".into(), queue_dir: "/nonexistent/queue".into(), ..Config::default() };
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
        Store { bdq: "/nonexistent/bdq".into(), lc: LcClient::over(t, true), lc_bin: "/nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None }
    }

    fn judge_over(t: Arc<dyn Transport>) {
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: "/nonexistent/run".into(), queue_dir: "/nonexistent/queue".into(), ..Config::default() };
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
        let st = Store { bdq: "/nonexistent/bdq".into(), lc: LcClient::over(svc.clone(), true), lc_bin: "/nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None };
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: "/nonexistent/run".into(), queue_dir: "/nonexistent/queue".into(), ..Config::default() };
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
        let st = Store { bdq: "/nonexistent/bdq".into(), lc: LcClient::over(svc.clone(), true), lc_bin: "/nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None };
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: "/nonexistent/run".into(), queue_dir: "/nonexistent/queue".into(), ..Config::default() };
        let env = Env { stdin: &mut stdin, config, lookup: &none };
        let a = Args::parse(&["--changed".into(), "sp-n".into()]).unwrap();
        assert_eq!(republish_dependents(&a, &env, &st), Ok(1));
        let p = published(&svc.0.lock().unwrap()).expect("no publish-readiness call");
        let ids: Vec<&str> = p["entries"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["sp-n"]);
    }
}

// ---- a reader answers from the readiness the dispatcher published -------------------------

mod published_read {
    use super::*;
    use serde_json::{json, Value};
    use spira_config::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn snooze() -> String {
        spira_config::lc_state::snooze_reason(ready::now_epoch() + 3_600)
    }

    fn lifecycle() -> Vec<Value> {
        let row = |id: &str,
                   state: &str,
                   holds: Value,
                   reason: Option<String>,
                   tip: Option<&str>| {
            json!({ "bead_id": id, "state": state, "holds": holds, "reason": reason, "tip": tip })
        };
        vec![
            row("sp-free", "READY", json!([]), None, None),
            row("sp-held", "READY", json!(["manual"]), None, None),
            row("sp-snoozed", "READY", json!(["wait"]), Some(snooze()), None),
            row("sp-blocked", "READY", json!([]), None, None),
            row("sp-stacked", "READY", json!([]), None, None),
            row("sp-epic", "READY", json!([]), None, None),
            row("sp-rework", "REWORK", json!([]), None, None),
            row("sp-working", "WORKING", json!([]), None, None),
            row(
                "sp-certified",
                "CERTIFIED",
                json!([]),
                None,
                Some("0123456789abcdef0123456789abcdef01234567"),
            ),
        ]
    }

    fn item(id: &str, kind: &str, blocked_on: Option<&str>) -> Value {
        let deps: Vec<Value> = blocked_on
            .map(|b| json!({ "issue_id": id, "depends_on_id": b, "type": "blocks" }))
            .into_iter()
            .collect();
        json!({ "id": id, "title": id, "priority": 2, "issue_type": kind, "labels": ["repo:spira"], "dependencies": deps })
    }

    fn items() -> Vec<Value> {
        vec![
            item("sp-free", "task", None),
            item("sp-held", "task", None),
            item("sp-snoozed", "task", None),
            item("sp-blocked", "task", Some("sp-working")),
            item("sp-stacked", "task", Some("sp-certified")),
            item("sp-epic", "epic", None),
            item("sp-rework", "task", None),
            item("sp-working", "task", None),
            item("sp-certified", "task", None),
        ]
    }

    fn pick(rows: &[Value], key: &str, ids: &[&str]) -> Vec<Value> {
        let mut out: Vec<Value> = rows
            .iter()
            .filter(|r| ids.contains(&r[key].as_str().unwrap()))
            .cloned()
            .collect();
        out.sort_by(|a, b| a[key].as_str().cmp(&b[key].as_str()));
        out
    }

    /// The store as the dispatcher left it, answering the reads a judging pass makes and, once
    /// `published` holds a pass, the one `ready-set` read.
    struct Store0 {
        seen: Mutex<Vec<Vec<String>>>,
        published: Mutex<Option<Value>>,
    }

    impl Transport for Store0 {
        fn exchange(
            &self,
            argv: &[String],
            stdin: Option<&str>,
            _d: Duration,
        ) -> Result<Reply, String> {
            self.seen.lock().unwrap().push(argv.to_vec());
            let words: Vec<&str> = argv.iter().map(String::as_str).collect();
            let all = lifecycle();
            let out = match words.as_slice() {
                ["publish-readiness"] => {
                    *self.published.lock().unwrap() = serde_json::from_str(stdin.unwrap()).ok();
                    String::new()
                }
                ["ready-set"] => {
                    let p = self
                        .published
                        .lock()
                        .unwrap()
                        .clone()
                        .expect("a dispatcher pass before the read");
                    let claimable: Vec<&str> = p["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|e| e["claimable"] == true)
                        .map(|e| e["id"].as_str().unwrap())
                        .collect();
                    let ready: Vec<Value> = all
                        .iter()
                        .filter(|r| matches!(r["state"].as_str(), Some("READY" | "REWORK")))
                        .filter(|r| claimable.contains(&r["bead_id"].as_str().unwrap()))
                        .cloned()
                        .collect();
                    let ids: Vec<&str> = ready
                        .iter()
                        .map(|r| r["bead_id"].as_str().unwrap())
                        .collect();
                    json!({ "planning_watermark": p["planning_watermark"], "judged_at": p["computed_at"], "unjudged": 0, "lifecycle": ready, "items": pick(&items(), "id", &ids) }).to_string()
                }
                ["list", "--state", states, ..] => {
                    let wanted: Vec<&str> = states.split(',').collect();
                    Value::Array(
                        all.into_iter()
                            .filter(|r| wanted.contains(&r["state"].as_str().unwrap()))
                            .collect(),
                    )
                    .to_string()
                }
                ["list", "--express", ..] => "[]".to_string(),
                ["list", "--ids", ids] => {
                    let wanted: Vec<&str> = ids.split(',').collect();
                    Value::Array(pick(&all, "bead_id", &wanted)).to_string()
                }
                ["plan", "watermark"] => r#"{"watermark":7}"#.to_string(),
                ["plan", "items", "--ids", ids] => {
                    let wanted: Vec<&str> = ids.split(',').collect();
                    json!({ "watermark": 7, "items": pick(&items(), "id", &wanted) }).to_string()
                }
                _ => String::new(),
            };
            Ok(Reply {
                code: 0,
                out,
                err: String::new(),
            })
        }
    }

    fn setup() -> (Arc<Store0>, Store) {
        let svc = Arc::new(Store0 {
            seen: Mutex::new(Vec::new()),
            published: Mutex::new(None),
        });
        let st = Store {
            bdq: "/nonexistent/bdq".into(),
            lc: LcClient::over(svc.clone(), true),
            lc_bin: "/nonexistent/spira-lc".into(),
            timeout: Duration::from_secs(1),
            views: None,
        };
        (svc, st)
    }

    fn env_over<R>(f: impl FnOnce(&mut Env) -> R) -> R {
        env_with(true, f)
    }

    fn env_with<R>(control_store: bool, f: impl FnOnce(&mut Env) -> R) -> R {
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config {
            control_store,
            ready_ttl_s: 60,
            stack_max_depth: 4,
            run: "/nonexistent/run".into(),
            queue_dir: "/nonexistent/queue".into(),
            ..Config::default()
        };
        f(&mut Env {
            stdin: &mut stdin,
            config,
            lookup: &none,
        })
    }

    fn ids(rows: &[rank::ReadyRow]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn a_reader_asks_the_store_once_for_the_ready_set() {
        let (svc, st) = setup();
        env_over(|env| {
            let a = Args::parse(&[]).unwrap();
            judge_rows(&a, env, &st, None, true).unwrap();
            svc.seen.lock().unwrap().clear();
            claimable_over(&a, env, &st).unwrap();
        });
        let seen = svc.seen.lock().unwrap().clone();
        assert_eq!(seen, [vec!["ready-set".to_string()]], "{seen:?}");
    }

    #[test]
    fn the_published_read_returns_the_rows_judge_rows_returns_for_the_same_store() {
        let (svc, st) = setup();
        env_over(|env| {
            let a = Args::parse(&[]).unwrap();
            let (judged, entries) = judge_rows(&a, env, &st, None, true).unwrap();
            let kinds: Vec<verdicts::Kind> = entries.iter().map(|e| e.verdict).collect();
            for k in [
                verdicts::Kind::Claimable,
                verdicts::Kind::Held,
                verdicts::Kind::Snoozed,
                verdicts::Kind::OutOfScope,
            ] {
                assert!(
                    kinds.contains(&k),
                    "the fixture must reach {k:?}: {entries:?}"
                );
            }
            assert!(entries
                .iter()
                .any(|e| e.id == "sp-blocked" && e.verdict == verdicts::Kind::Blocked));
            let (read, fresh) = claimable_over(&a, env, &st).unwrap();
            assert_eq!(fresh.expect("a published answer says what it is from").planning_watermark.as_deref(), Some("7"));
            assert_eq!(ids(&read), ids(&judged));
            assert_eq!(read, judged);
            assert!(
                ids(&read).contains(&"sp-stacked") && ids(&read).contains(&"sp-rework"),
                "{:?} {:?}",
                ids(&read),
                entries
            );
            drop(svc);
        });
    }

    #[test]
    fn a_reader_does_not_trust_a_judgement_for_a_bead_whose_hold_has_changed_since() {
        let (svc, st) = setup();
        env_over(|env| {
            let a = Args::parse(&[]).unwrap();
            judge_rows(&a, env, &st, None, true).unwrap();
            let mut p = svc.published.lock().unwrap().take().unwrap();
            for e in p["entries"].as_array_mut().unwrap() {
                if e["id"] == "sp-held" || e["id"] == "sp-snoozed" {
                    e["claimable"] = json!(true);
                }
            }
            *svc.published.lock().unwrap() = Some(p);
            assert!(!ids(&claimable_over(&a, env, &st).unwrap().0)
                .iter()
                .any(|i| *i == "sp-held" || *i == "sp-snoozed"));
        });
    }

    #[test]
    fn a_reader_judges_for_itself_while_any_ready_bead_awaits_its_first_judgement() {
        struct Fresh(Store0);
        impl Transport for Fresh {
            fn exchange(
                &self,
                argv: &[String],
                stdin: Option<&str>,
                d: Duration,
            ) -> Result<Reply, String> {
                if argv[0] == "ready-set" {
                    self.0.seen.lock().unwrap().push(argv.to_vec());
                    return Ok(Reply { code: 0, out: r#"{"planning_watermark":"7","judged_at":"2026-11-11T10:00:05Z","unjudged":1,"lifecycle":[{"bead_id":"sp-free","state":"READY","holds":[]}],"items":[{"id":"sp-free","labels":["repo:spira"]}]}"#.into(), err: String::new() });
                }
                self.0.exchange(argv, stdin, d)
            }
        }
        let svc = Arc::new(Fresh(Store0 {
            seen: Mutex::new(Vec::new()),
            published: Mutex::new(None),
        }));
        let st = Store {
            bdq: "/nonexistent/bdq".into(),
            lc: LcClient::over(svc, true),
            lc_bin: "/nonexistent/spira-lc".into(),
            timeout: Duration::from_secs(1),
            views: None,
        };
        env_over(|env| {
            let (rows, fresh) = claimable_over(&Args::parse(&[]).unwrap(), env, &st).unwrap();
            assert_eq!(fresh, None, "rows judged here name no pass");
            assert_eq!(ids(&rows), ["sp-free", "sp-rework", "sp-stacked"]);
        });
    }
    fn seen_of(svc: &Store0) -> Vec<Vec<String>> {
        svc.seen.lock().unwrap().clone()
    }

    fn only_ready_set() -> Vec<Vec<String>> {
        vec![vec!["ready-set".to_string()]]
    }

    #[test]
    fn urgent_over_a_published_pass_reads_the_ready_set_once() {
        let (svc, st) = setup();
        let out = env_over(|env| {
            let a = Args::parse(&[]).unwrap();
            judge_rows(&a, env, &st, None, true).unwrap();
            svc.seen.lock().unwrap().clear();
            urgent_over(&a, env, &st).unwrap()
        });
        assert_eq!(seen_of(&svc), only_ready_set());
        assert_eq!(out, "sp-rework\t2\t1\t0\n");
    }

    #[test]
    fn ready_count_express_over_a_published_pass_reads_the_ready_set_once() {
        let (svc, st) = setup();
        env_over(|env| {
            judge_rows(&Args::parse(&[]).unwrap(), env, &st, None, true).unwrap();
            svc.seen.lock().unwrap().clear();
            let a = Args::parse(&["".into(), "--express".into()]).unwrap();
            let o = ready_count_over(&a, env, &st);
            assert_eq!((o.code, o.out.as_str()), (0, "0"), "{o:?}");
        });
        assert_eq!(seen_of(&svc), only_ready_set());
    }

    #[test]
    fn select_machine_over_the_published_rows_reads_the_ready_set_once_and_ranks_as_judging_does() {
        let (svc, st) = setup();
        let a = Args::parse(&["--rune".into(), "builder".into(), "--blockers".into(), "machine".into()]).unwrap();
        let (mut fast, fast_rework) = env_over(|env| {
            judge_rows(&a, env, &st, None, true).unwrap();
            let mut rows = claimable_over(&a, env, &st).unwrap().0;
            svc.seen.lock().unwrap().clear();
            let lc = machine_filter(&a, env, &st, &mut rows).unwrap_or_else(|o| panic!("{o:?}"));
            (rows, lc.into_iter().filter(|(_, r)| r.state == lifecycle::bead::BeadState::Rework).map(|(k, _)| k).collect::<BTreeSet<_>>())
        });
        assert_eq!(seen_of(&svc), only_ready_set());
        let (mut slow, slow_rework) = env_with(false, |env| {
            let mut rows = claimable_over(&a, env, &st).unwrap().0;
            let lc = machine_filter(&a, env, &st, &mut rows).unwrap_or_else(|o| panic!("{o:?}"));
            (rows, lc.into_iter().filter(|(_, r)| r.state == lifecycle::bead::BeadState::Rework).map(|(k, _)| k).collect::<BTreeSet<_>>())
        });
        fast.sort_by(|x, y| x.id.cmp(&y.id));
        slow.sort_by(|x, y| x.id.cmp(&y.id));
        assert_eq!(fast, slow);
        assert_eq!(fast_rework, slow_rework);
    }

    fn fixed_ready_set(judged_at: &str) -> (Arc<Store0>, Store) {
        struct Fixed(Arc<Store0>, String);
        impl Transport for Fixed {
            fn exchange(&self, argv: &[String], stdin: Option<&str>, d: Duration) -> Result<Reply, String> {
                if argv[0] == "ready-set" {
                    self.0.seen.lock().unwrap().push(argv.to_vec());
                    let at = &self.1;
                    return Ok(Reply { code: 0, out: format!(r#"{{"planning_watermark":"7","judged_at":"{at}","unjudged":0,"lifecycle":[{{"bead_id":"sp-free","state":"READY","holds":[]}}],"items":[{{"id":"sp-free","labels":["repo:spira"]}}]}}"#), err: String::new() });
                }
                self.0.exchange(argv, stdin, d)
            }
        }
        let inner = Arc::new(Store0 { seen: Mutex::new(Vec::new()), published: Mutex::new(None) });
        let st = Store { bdq: "/nonexistent/bdq".into(), lc: LcClient::over(Arc::new(Fixed(inner.clone(), judged_at.into())), true), lc_bin: "/nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None };
        (inner, st)
    }

    #[test]
    fn a_published_answer_older_than_the_accepted_age_is_judged_here() {
        let (_, old) = fixed_ready_set("2020-01-01T00:00:00Z");
        let (_, new) = fixed_ready_set(&verdicts::stamp(ready::now_epoch()));
        env_over(|env| {
            let a = Args::parse(&[]).unwrap();
            assert_eq!(claimable_over(&a, env, &old).unwrap().1, None, "a stale verdict is not accepted");
            assert!(claimable_over(&a, env, &new).unwrap().1.is_some(), "a fresh verdict is");
        });
    }

    #[test]
    fn accepts_a_stamp_up_to_the_age_and_refuses_beyond_it_or_without_one() {
        let now = 1_800_000_000;
        let at = |age: i64| Some(verdicts::stamp(now - age));
        assert!(accepts(at(60).as_deref(), now, 60));
        assert!(!accepts(at(61).as_deref(), now, 60));
        assert!(!accepts(None, now, 60));
        assert!(!accepts(Some("not a stamp"), now, 60));
        assert!(!accepts(at(0).as_deref(), now, 0), "0 means always judge");
    }
}

#[test]
fn the_verbs_a_caller_waits_on_push_no_run_metric_when_they_succeed() {
    let hot = |v: &str, code| discards_run_metric(&[v.to_string(), "builder".into()], code);
    assert!(hot("rune-ready", 0) && hot("ready-count", 0) && hot("urgent", 0));
    assert!(!hot("rune-ready", 1), "a failing read still reports its failure");
    assert!(!hot("bulk-ready-by-rune", 0) && !hot("unpoison", 0) && !discards_run_metric(&[], 0));
}
