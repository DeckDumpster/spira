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
                ]
                .join(",")
                .pipe(|rows| format!("[{rows}]")),
                ["list", "--ids", _] => r#"[{"bead_id":"sp-w","state":"WORKING","holds":[]}]"#.to_string(),
                ["plan", "watermark"] => r#"{"watermark":7}"#.to_string(),
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
        let st = Store { bdq: "/nonexistent/bdq".into(), lc: LcClient::over(svc.clone(), true), lc_bin: "/nonexistent/spira-lc".into(), timeout: Duration::from_secs(1) };
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
    fn nothing_is_published_while_the_control_store_is_not_in_use() {
        let (seen, entries) = judged(false, true);
        assert!(published(&seen).is_none());
        assert_eq!(entries.len(), 1);
    }

    fn store_over(t: Arc<dyn Transport>) -> Store {
        Store { bdq: "/nonexistent/bdq".into(), lc: LcClient::over(t, true), lc_bin: "/nonexistent/spira-lc".into(), timeout: Duration::from_secs(1) }
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
        let st = Store { bdq: "/nonexistent/bdq".into(), lc: LcClient::over(svc.clone(), true), lc_bin: "/nonexistent/spira-lc".into(), timeout: Duration::from_secs(1) };
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: "/nonexistent/run".into(), queue_dir: "/nonexistent/queue".into(), ..Config::default() };
        let env = Env { stdin: &mut stdin, config, lookup: &none };
        let a = Args::parse(&["--changed".into(), "sp-w".into()]).unwrap();
        assert_eq!(republish_dependents(&a, &env, &st), Ok(2));
        let p = published(&svc.0.lock().unwrap()).expect("no publish-readiness call");
        let mut ids: Vec<&str> = p["entries"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, ["sp-r", "sp-s"], "sp-c is neither waiting on sp-w nor under it, and a related edge holds nothing back");
    }
}
