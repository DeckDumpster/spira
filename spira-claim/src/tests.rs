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
// `rune-ready`/`rune-exclude`/`bulk-ready-by-rune` read real process
// environment (`SPIRA_HOME`, `SPIRA_TOML`, ...) — unlike the rest of this
// file's tests, which thread every input through a file or a hand-built `Store`. These
// edit the environment only through `testkit::env`, which serializes them against every
// other test in this binary and restores on drop; `run` takes the same lock to read.

/// `rune_home` is the declared home: `SPIRA_HOME` wins, else `$SPIRA_RELEASE/spira`, and
/// neither set is a refusal naming both — never a search from this executable.
#[test_id(tmj3p2h)]
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
mod full_repositories {
    use super::*;
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use serde_json::{json, Value};
    use spira_config::build_dir::{lease_for, Lease};
    use std::sync::Arc;
    use std::time::Duration;

    /// One READY builder bead in each of `repo:spira` and `repo:other`.
    struct TwoRepos;

    fn row(id: &str) -> Value {
        json!({ "bead_id": id, "state": "READY", "holds": [], "tip": null })
    }

    fn item(id: &str, repo: &str) -> Value {
        json!({ "id": id, "title": id, "priority": 2, "issue_type": "task", "labels": [format!("repo:{repo}")], "dependencies": [] })
    }

    impl Transport for TwoRepos {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            let items = [item("sp-full", "spira"), item("sp-open", "other")];
            let out = match argv.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                ["claimable"] => json!({ "rev": 1, "cannot_tell": 0, "beads": [
                    { "bead_id": "sp-full", "verdict": "claimable", "blocker": null, "runes": ["builder"], "row": row("sp-full"), "content": items[0] },
                    { "bead_id": "sp-open", "verdict": "claimable", "blocker": null, "runes": ["builder"], "row": row("sp-open"), "content": items[1] },
                ] }),
                ["list", "--express", ..] => json!([]),
                ["list", "--state", ..] => json!([row("sp-full"), row("sp-open")]),
                ["plan", "items", "--ids", ..] => json!({ "watermark": 1, "items": items }),
                _ => json!([]),
            };
            Ok(Reply { code: 0, out: out.to_string(), err: String::new(), read_ended: false })
        }
    }

    fn open() -> Result<Store, String> {
        Ok(Store { lc: LcClient::over(Arc::new(TwoRepos), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None, readiness_alert: Default::default() })
    }

    /// A home whose one rune builds, and a ceiling of 2 build folders per repository.
    struct Fixture {
        dir: testkit::TempDir,
        _env: testkit::EnvGuard,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = testkit::TempDir::new("claim-full-repo");
            let runes = dir.path().join("runes");
            // Locked before anything resolves: the runes directory comes from the environment.
            let _env = testkit::env(&[("SPIRA_HOME", Some(dir.path().to_str().unwrap())), ("SPIRA_RUNES_DIR", Some(runes.to_str().unwrap()))]);
            std::fs::create_dir_all(&runes).unwrap();
            std::fs::write(runes.join("builder.rune"), "RUNE_LABELS=plan\nRUNE_BUILDS=1\n").unwrap();
            Fixture { dir, _env }
        }

        fn base(&self) -> std::path::PathBuf {
            self.dir.path().join("builds")
        }

        /// spira's folders held to the ceiling by one builder and one gate.
        fn fill_spira(&self) -> [Lease; 2] {
            ["sp-b", "gate sp-g.1"].map(|who| lease_for(&[self.base()], who, "spira", 2).unwrap())
        }

        fn with_env<R>(&self, base: &str, f: impl FnOnce(&mut Env) -> R) -> R {
            let mut stdin = std::io::empty();
            let none = |_: &str| None;
            let config = Config { runes: "builder".into(), run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), build_base: base.into(), build_ceiling: 2, ..Config::default() };
            f(&mut Env { stdin: &mut stdin, config, lookup: &none })
        }

        fn counted(&self) -> String {
            self.with_env(&self.base().display().to_string(), |env| ready_by_rune_over(&Args::parse(&[]).unwrap(), env, &open).unwrap().lines)
        }

        fn listed(&self) -> Vec<String> {
            let a = Args::parse(&["builder".into(), "--json".into()]).unwrap();
            let o = self.with_env(&self.base().display().to_string(), |env| rune_ready_over(&a, env, self.dir.path(), "builder", &open));
            assert_eq!(o.code, 0, "{o:?}");
            let rows: Vec<Value> = serde_json::from_str(o.out.trim()).unwrap();
            rows.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
        }
    }

    #[test_id(tf2ky8b)]
    #[test]
    fn a_repository_whose_build_folders_are_all_leased_has_no_builder_work_until_one_is_released() {
        let fx = Fixture::new();
        let held = fx.fill_spira();
        assert_eq!(fx.counted(), "builder 1\n");
        assert_eq!(fx.listed(), ["sp-open"]);
        drop(held);
        assert_eq!(fx.counted(), "builder 2\n");
        assert_eq!(fx.listed(), ["sp-full", "sp-open"]);
    }

    #[test_id(tlo58b9)]
    #[test]
    fn the_exclusions_of_a_building_rune_name_a_full_repository() {
        let fx = Fixture::new();
        let _held = fx.fill_spira();
        let exclude = fx.with_env(&fx.base().display().to_string(), |env| rune_exclude_str(env, fx.dir.path(), "builder", "own")).unwrap();
        assert!(exclude.split(',').any(|x| x == "repo:spira") && !exclude.contains("repo:other"), "{exclude}");
    }

    #[test_id(t9dtzlh)]
    #[test]
    fn a_building_rune_whose_build_folders_cannot_be_counted_is_neither_counted_nor_listed() {
        let fx = Fixture::new();
        let not_a_dir = fx.dir.path().join("file");
        std::fs::write(&not_a_dir, "").unwrap();
        for base in ["", not_a_dir.to_str().unwrap()] {
            let counted = fx.with_env(base, |env| ready_by_rune_over(&Args::parse(&[]).unwrap(), env, &open).unwrap());
            assert_eq!(counted.lines, "", "{base:?}");
            assert!(counted.warnings.iter().any(|w| w.contains("builder") && w.contains("build folders cannot be counted")), "{:?}", counted.warnings);
            let a = Args::parse(&["builder".into()]).unwrap();
            let o = fx.with_env(base, |env| rune_ready_over(&a, env, fx.dir.path(), "builder", &open));
            assert_eq!((o.code, o.out.as_str()), (1, "0"), "{o:?}");
            assert!(o.err.contains("build folders cannot be counted"), "{o:?}");
        }
    }
}

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

#[test_id(tgxbzzb)]
#[test]
fn no_call_site_reads_the_unfiltered_store() {
    let src = include_str!("store.rs");
    assert_eq!(src.matches("once(\"list\")").count(), 1, "one spira-lc list site");
    assert!(src.contains("once(\"list\").chain(filter.iter().copied())"));
}

/// An in-process caller's environment answers every read on the dispatch path; this
/// process's own variables are never consulted.
#[test_id(tre8rk0)]
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

#[test_id(tmceg7f)]
#[test]
fn express_is_a_bool_flag_and_never_swallows_what_follows() {
    let raw: Vec<String> = ["labels", "", "--express"].iter().map(|s| s.to_string()).collect();
    let a = Args::parse(&raw).expect("--express takes no value");
    assert!(a.has("--express"));
    assert_eq!(a.pos, vec!["labels".to_string(), String::new()]);
}

/// The argv a dwarf's claim seam sends (`dwarf/src/seam.rs`): `--dropped` last, with nothing after
/// it. Release 88977d0bc1 answered "--dropped needs a value" to every claim.
#[test_id(th73514)]
#[test]
fn dropped_is_a_bool_flag_as_the_claim_seam_sends_it() {
    let raw: Vec<String> = ["builder", "--json", "--dropped"].iter().map(|s| s.to_string()).collect();
    let a = Args::parse(&raw).expect("--dropped takes no value");
    assert!(a.has("--dropped") && a.has("--json"));
    assert_eq!(a.pos, vec!["builder".to_string()]);
    let raw: Vec<String> = ["builder", "--json", "--dropped", "--express"].iter().map(|s| s.to_string()).collect();
    assert!(Args::parse(&raw).expect("--dropped never swallows --express").has("--express"));
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
            Ok(Reply { code: 0, out, err: String::new(), read_ended: false })
        }
    }

    struct Refusing(Service);

    impl Transport for Refusing {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, d: Duration) -> Result<Reply, String> {
            let r = self.0.exchange(argv, stdin, d)?;
            Ok(if argv[0] == "publish-readiness" { Reply { code: 3, out: String::new(), err: "refused: no payload".into(), read_ended: false } } else { r })
        }
    }

    /// Refuses the first publish only.
    struct RefusingOnce(Arc<Service>, std::sync::atomic::AtomicBool);

    impl Transport for RefusingOnce {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, d: Duration) -> Result<Reply, String> {
            let r = self.0.exchange(argv, stdin, d)?;
            let first = argv[0] == "publish-readiness" && !self.1.swap(true, std::sync::atomic::Ordering::SeqCst);
            Ok(if first { Reply { code: 3, out: String::new(), err: "refused: no payload".into(), read_ended: false } } else { r })
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
        let st = Store { lc: LcClient::over(svc.clone(), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None, readiness_alert: Default::default() };
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

    #[test_id(t4zn6zp)]
    #[test]
    fn judge_rows_publishes_readiness_for_certified_and_submitted_records_with_the_gates_it_read() {
        let (seen, _) = judged(true, true);
        let p = published(&seen).expect("no publish-readiness call");
        assert_eq!(p["planning_watermark"], "7");
        let entry = |id: &str| p["entries"].as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no entry for {id}: {p}")).clone();
        let working = serde_json::to_value(spira_config::edges::gate(lifecycle::bead::BeadState::Working, None, false)).unwrap();
        assert_eq!((entry("sp-s")["claimable"].clone(), entry("sp-s")["blocked_by"].clone(), entry("sp-s")["gates"]["sp-w"].clone()), (false.into(), serde_json::json!(["sp-w"]), working.clone()));
        assert_eq!((entry("sp-c")["claimable"].clone(), entry("sp-c")["blocked_by"].clone()), (false.into(), serde_json::json!([])));
        assert_eq!((entry("sp-r")["claimable"].clone(), entry("sp-r")["blocked_by"].clone(), entry("sp-r")["gates"]["sp-w"].clone()), (false.into(), serde_json::json!(["sp-w"]), working));
    }

    #[test_id(tbuj560)]
    #[test]
    fn the_planning_watermark_is_read_before_the_rows_it_stamps() {
        let (seen, _) = judged(true, true);
        let at = |what: &[&str]| seen.iter().position(|c| c.iter().map(String::as_str).take(what.len()).eq(what.iter().copied())).unwrap();
        assert!(at(&["plan", "watermark"]) < at(&["list", "--state", LIVE]));
    }

    #[test_id(ta1ahxx)]
    #[test]
    fn a_reading_pass_publishes_nothing_and_reads_no_live_set() {
        let (seen, entries) = judged(true, false);
        assert!(published(&seen).is_none());
        assert!(seen.iter().all(|c| c[0] != "publish-readiness" && c[..] != ["plan", "watermark"]), "{seen:?}");
        assert_eq!(entries.len(), 1);
    }

    #[test_id(tkx4v4c)]
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

    #[test_id(t0vbr1x)]
    #[test]
    fn nothing_is_published_while_the_control_store_is_not_in_use() {
        let (seen, entries) = judged(false, true);
        assert!(published(&seen).is_none());
        assert_eq!(entries.len(), 1);
    }

    fn store_over(t: Arc<dyn Transport>) -> Store {
        Store { lc: LcClient::over(t, true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None, readiness_alert: Default::default() }
    }

    fn judge_over(t: Arc<dyn Transport>) {
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
        let env = Env { stdin: &mut stdin, config, lookup: &none };
        judge_rows(&Args::parse(&[]).unwrap(), &env, &store_over(t), None, true).unwrap();
    }

    #[test_id(tm9m4uw)]
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

    fn clears(seen: &[Vec<String>]) -> usize {
        seen.iter().filter(|c| c[0] == "condition" && c[1] == "clear" && c[2] == store::READINESS_ALERT).count()
    }

    fn pass_over(st: &Store) {
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
        let env = Env { stdin: &mut stdin, config, lookup: &none };
        judge_rows(&Args::parse(&[]).unwrap(), &env, st, None, true).unwrap();
    }

    #[test_id(t2bvwx6)]
    #[test]
    fn accepted_publishes_with_no_alert_raised_send_no_condition_clear() {
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let st = store_over(svc.clone());
        pass_over(&st);
        pass_over(&st);
        let seen = svc.0.lock().unwrap().clone();
        assert_eq!(seen.iter().filter(|c| c[0] == "publish-readiness").count(), 2);
        assert_eq!(clears(&seen), 0, "{seen:?}");
    }

    #[test_id(tr63xb9)]
    #[test]
    fn a_refused_publish_followed_by_accepted_ones_clears_the_alert_once() {
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let refusing = store_over(Arc::new(RefusingOnce(svc.clone(), std::sync::atomic::AtomicBool::new(false))));
        pass_over(&refusing);
        pass_over(&refusing);
        pass_over(&refusing);
        assert_eq!(clears(&svc.0.lock().unwrap()), 1);
    }

    #[test_id(twopmkj)]
    #[test]
    fn an_alert_one_pass_raised_is_cleared_by_a_later_pass_with_its_own_store() {
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let service: Arc<dyn Transport> = Arc::new(RefusingOnce(svc.clone(), Default::default()));
        let of_process = || Store::of_process(LcClient::over(service.clone(), true), "nonexistent/spira-lc".into(), Duration::from_secs(1));
        pass_over(&of_process());
        pass_over(&of_process());
        pass_over(&of_process());
        assert_eq!(clears(&svc.0.lock().unwrap()), 1);
    }

    #[test_id(ty0tfiu)]
    #[test]
    fn a_clear_the_store_does_not_take_is_sent_again_by_the_next_accepted_publish() {
        struct ClearFails(Arc<Service>, std::sync::atomic::AtomicUsize);
        impl Transport for ClearFails {
            fn exchange(&self, argv: &[String], stdin: Option<&str>, d: Duration) -> Result<Reply, String> {
                let r = self.0.exchange(argv, stdin, d)?;
                let failed = argv[0] == "condition" && argv[1] == "clear" && self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                Ok(if failed { Reply { code: 2, out: String::new(), err: "cannot tell".into(), read_ended: false } } else { r })
            }
        }
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let st = store_over(Arc::new(ClearFails(svc.clone(), Default::default())));
        st.alert_readiness_refused("refused").unwrap();
        pass_over(&st);
        pass_over(&st);
        pass_over(&st);
        assert_eq!(clears(&svc.0.lock().unwrap()), 2);
    }

    /// A watched view of a store whose only condition is the readiness alert, at `status`.
    fn watched_alert(status: &str) -> (Arc<store_view::Projection>, Arc<Service>) {
        struct Held(Arc<Service>, String);
        impl Transport for Held {
            fn exchange(&self, argv: &[String], stdin: Option<&str>, d: Duration) -> Result<Reply, String> {
                self.0.exchange(argv, stdin, d)?;
                let out = match argv[0].as_str() {
                    "rc" => r#"{"ok":{"items":[],"list_rev":3}}"#.to_string(),
                    "condition" => format!(r#"{{"rev":3,"conditions":[{{"key":"{}","owner":"heimdall","status":"{}"}}]}}"#, store::READINESS_ALERT, self.1),
                    _ => "[]".to_string(),
                };
                Ok(Reply { code: 0, out, err: String::new(), read_ended: false })
            }
        }
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let client = LcClient::over(Arc::new(Held(svc.clone(), status.into())), true);
        let view = Arc::new(store_view::Projection::new(client, Duration::from_secs(60), Arc::new(std::time::Instant::now)));
        view.load().unwrap();
        (view, svc)
    }

    #[test_id(tti704w)]
    #[test]
    fn an_alert_the_watched_view_shows_firing_is_cleared_by_a_process_that_did_not_raise_it() {
        let (view, _held) = watched_alert("True");
        let _watching = view.hold_watching();
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let st = Store { views: Some(view.clone()), ..store_over(svc.clone()) };
        st.settle_readiness_alert().unwrap();
        assert_eq!(clears(&svc.0.lock().unwrap()), 1);
    }

    #[test_id(tr3b2as)]
    #[test]
    fn an_alert_the_watched_view_shows_cleared_is_not_cleared_again_and_is_read_once() {
        let (view, held) = watched_alert("False");
        let _watching = view.hold_watching();
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let st = Store { views: Some(view.clone()), ..store_over(svc.clone()) };
        st.settle_readiness_alert().unwrap();
        st.settle_readiness_alert().unwrap();
        assert!(svc.0.lock().unwrap().is_empty(), "{:?}", svc.0.lock().unwrap());
        assert_eq!(held.0.lock().unwrap().iter().filter(|c| c[0] == "condition").count(), 1);
    }

    #[test_id(tyw5x3f)]
    #[test]
    fn republish_dependents_judges_and_publishes_only_the_records_that_wait_on_or_descend_from_the_changed() {
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let st = Store { lc: LcClient::over(svc.clone(), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None, readiness_alert: Default::default() };
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

    #[test_id(toolfu0)]
    #[test]
    fn republish_dependents_judges_the_changed_record_itself_when_nothing_waits_on_it() {
        let svc = Arc::new(Service(Mutex::new(Vec::new())));
        let st = Store { lc: LcClient::over(svc.clone(), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None, readiness_alert: Default::default() };
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
                ["claimable"] if self.view_code == NO_PLAN_ROW => (
                    0,
                    json!({ "rev": 1, "cannot_tell": 0, "content_error": null, "beads": [
                        { "bead_id": "sp-a", "verdict": "claimable", "blocker": null, "runes": ["builder"], "row": row("sp-a", "READY"), "content": null },
                    ] })
                    .to_string(),
                    String::new(),
                ),
                ["claimable"] if self.view_code != 0 => (self.view_code, String::new(), "the control store is unreadable".to_string()),
                ["claimable"] => (
                    0,
                    json!({ "rev": 1, "cannot_tell": 0, "beads": [
                        { "bead_id": "sp-a", "verdict": "claimable", "blocker": null, "runes": ["builder"], "row": row("sp-a", "READY"), "content": item("sp-a") },
                        { "bead_id": "sp-b", "verdict": "blocked", "blocker": "sp-a", "runes": [], "row": row("sp-b", "READY"), "content": item("sp-b") },
                    ] })
                    .to_string(),
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
            Ok(Reply { code, out, err, read_ended: false })
        }
    }

    const NO_PLAN_ROW: i32 = -1;

    fn setup(view_code: i32) -> (Arc<Double>, Store) {
        let svc = Arc::new(Double { seen: Mutex::new(Vec::new()), view_code });
        let st = Store { lc: LcClient::over(svc.clone(), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None, readiness_alert: Default::default() };
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

    #[test_id(tp9uh4t)]
    #[test]
    fn every_reader_of_the_view_makes_one_claimable_call_and_nothing_else() {
        let a = Args::parse(&[]).unwrap();
        type Read = fn(&Args, &mut Env, &Store);
        let readers: [(&str, Read); 4] = [
            ("ready-count", |a, env, st| drop(ready_count_over(a, env, st))),
            ("urgent", |a, env, st| drop(urgent_over(a, env, st).unwrap())),
            ("rune-ready", |a, env, st| drop(claimable_over(a, env, st).unwrap())),
            ("bulk-ready-by-rune", |a, env, st| drop(viewed(a, env, st).unwrap())),
        ];
        for (name, read) in readers {
            let (svc, st) = setup(0);
            env_over(|env| read(&a, env, &st));
            assert_eq!(seen(&svc), ["claimable"], "{name}");
        }
    }

    #[test_id(tovngh8)]
    #[test]
    fn the_verbs_of_one_tick_share_one_claimable_call_and_the_next_tick_asks_again() {
        let (svc, st) = setup(0);
        let a = Args::parse(&[]).unwrap();
        {
            let _tick = TickView::begin();
            env_over(|env| {
                viewed(&a, env, &st).unwrap();
                claimable_over(&a, env, &st).unwrap();
                urgent_over(&a, env, &st).unwrap();
                drop(ready_count_over(&a, env, &st));
            });
            assert_eq!(seen(&svc), ["claimable"]);
        }
        env_over(|env| viewed(&a, env, &st).unwrap());
        assert_eq!(seen(&svc), ["claimable", "claimable"], "a view is not kept past its tick");
    }

    #[test_id(t5iez9x)]
    #[test]
    fn rune_ready_lists_the_beads_the_view_says_the_rune_would_take() {
        let (_, st) = setup(0);
        let view = env_over(|env| claimable_over(&Args::parse(&[]).unwrap(), env, &st).unwrap());
        assert_eq!(ids(&taken_split(&view, "builder").0.into_iter().cloned().collect::<Vec<_>>()), ["sp-a"]);
        assert!(taken_split(&view, "ops").0.is_empty(), "a rune the view does not name takes nothing");
    }

    struct ReworkAndFresh;

    impl Transport for ReworkAndFresh {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            let out = match argv.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                ["claimable"] => json!({ "rev": 1, "cannot_tell": 0, "beads": [
                    { "bead_id": "sp-rw", "verdict": "claimable", "blocker": null, "runes": ["builder"], "row": row("sp-rw", "REWORK"), "content": item("sp-rw") },
                    { "bead_id": "sp-fresh", "verdict": "claimable", "blocker": null, "runes": ["builder"], "row": row("sp-fresh", "READY"), "content": item("sp-fresh") },
                ] })
                .to_string(),
                _ => String::new(),
            };
            Ok(Reply { code: 0, out, err: String::new(), read_ended: false })
        }
    }

    #[test_id(tzmgbyp)]
    #[test]
    fn rune_ready_names_a_rework_bead_a_live_process_occupies_and_the_rule_that_dropped_it() {
        let t = testkit::TempDir::new("claim-dropped-occupied");
        let run = t.join("run");
        let tree = run.join("worktree/sp-rw");
        std::fs::create_dir_all(&tree).unwrap();
        let mut holder = std::process::Command::new("sleep").arg("60").current_dir(&tree).spawn().unwrap();
        let st = Store { lc: LcClient::over(Arc::new(ReworkAndFresh), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None, readiness_alert: Default::default() };
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, run: run.display().to_string(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
        let view = viewed(&Args::parse(&[]).unwrap(), &Env { stdin: &mut stdin, config, lookup: &none }, &st).unwrap();
        let _ = holder.kill();
        let _ = holder.wait();
        let (mine, dropped) = taken_split(&view, "builder");
        let json = dropped_json(&mine, &dropped);
        assert_eq!(json["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect::<Vec<_>>(), ["sp-fresh"]);
        assert_eq!(json["dropped"], json!([{ "id": "sp-rw", "rule": "occupied" }]));
    }

    #[test_id(t8v315w)]
    #[test]
    fn select_machine_keeps_only_the_beads_the_view_lists_as_claimable() {
        let (svc, st) = setup(0);
        let a = Args::parse(&["--rune".into(), "builder".into(), "--blockers".into(), "machine".into()]).unwrap();
        let mut rows = st.items_by_ids(&["sp-a".to_string(), "sp-b".to_string()]).unwrap();
        svc.seen.lock().unwrap().clear();
        let lc = env_over(|env| machine_filter(&a, env, &st, &mut rows).unwrap_or_else(|o| panic!("{o:?}")));
        assert_eq!(ids(&rows), ["sp-a"]);
        assert!(lc.contains_key("sp-a"));
    }

    #[test_id(tlpbsnm)]
    #[test]
    fn bulk_counts_come_from_the_runes_the_view_names() {
        let (_, st) = setup(0);
        let view = env_over(|env| viewed(&Args::parse(&[]).unwrap(), env, &st).unwrap());
        let parts = vec![ready::RunePart { name: "builder".into(), inc: vec![], exc: vec![] }, ready::RunePart { name: "ops".into(), inc: vec![], exc: vec![] }];
        assert_eq!(ready::bucket_taken(&view.rows, &view.runes, &parts), [("builder".to_string(), 1), ("ops".to_string(), 0)]);
    }

    #[test_id(th0xr7g)]
    #[test]
    fn ready_count_counts_the_view_claimable_rows() {
        let (_, st) = setup(0);
        let n = env_over(|env| machine_claimable_shared(&Args::parse(&[]).unwrap(), env, &st).unwrap().len());
        assert_eq!(n, 1);
    }

    #[test_id(tlkia0o)]
    #[test]
    fn urgent_lists_the_views_rework_beads_and_reads_the_view_once() {
        let (svc, st) = setup(0);
        let out = env_over(|env| urgent_over(&Args::parse(&[]).unwrap(), env, &st).unwrap());
        assert_eq!(out, "", "sp-a is READY at priority 2, so nothing outranks the lanes");
        let calls = seen(&svc);
        assert_eq!(calls.iter().filter(|c| *c == "claimable").count(), 1, "{calls:?}");
    }

    #[test_id(t1kb4jp)]
    #[test]
    fn ready_count_express_counts_the_views_claimable_rows_that_are_express() {
        let (_, st) = setup(0);
        let n = env_over(|env| ready_count_over(&Args::parse(&["".into(), "--express".into()]).unwrap(), env, &st));
        assert_eq!((n.code, n.out.as_str()), (0, "0"), "{n:?}");
        let n = env_over(|env| ready_count_over(&Args::parse(&["".into()]).unwrap(), env, &st));
        assert_eq!((n.code, n.out.as_str()), (0, "1"), "{n:?}");
    }

    #[test_id(taq3dyk)]
    #[test]
    fn a_claimable_bead_with_no_plan_row_is_not_ranked_and_does_not_fail_the_read() {
        let (_, st) = setup(NO_PLAN_ROW);
        let v = env_over(|env| viewed(&Args::parse(&[]).unwrap(), env, &st).unwrap());
        assert!(v.rows.is_empty(), "{:?}", ids(&v.rows));
        assert!(v.lifecycle.contains_key("sp-a"));
    }

    #[test_id(thzim7n)]
    #[test]
    fn an_unreadable_view_is_the_reads_error_and_nothing_judges_in_its_place() {
        let (svc, st) = setup(2);
        let err = env_over(|env| claimable_over(&Args::parse(&[]).unwrap(), env, &st).err().expect("the view is unreadable"));
        assert!(err.contains("claimable") && err.contains("unreadable"), "{err}");
        let calls = seen(&svc);
        assert_eq!(calls, ["claimable"], "no content read, no lifecycle read, no judging: {calls:?}");
    }
}

#[test_id(tpv41kc)]
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

#[test_id(t2g7hkw)]
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

#[test_id(tn05sje)]
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

#[test_id(tvekqcw)]
#[test]
fn the_contract_fix_label_passes_no_claims_stop() {
    let t = testkit::TempDir::new("claim-contract-fix-passes-nothing");
    let (run, mut rows) = stopped_repo_rows(&t, "repo-contract");
    assert_eq!(retain_unstopped(&mut rows, &run).len(), 2);
    assert!(rows.is_empty());
}

#[test_id(t0mur1x)]
#[test]
fn claim_retry_is_no_longer_a_verb() {
    let toml = format!("{}/../spira/testlib/complete.toml", std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let _env = testkit::env(&[("SPIRA_TOML", Some(&toml))]);
    let raw = ["claim-retry".to_string(), "list".to_string()];
    let out = crate::cli::dispatch_in(&raw, &mut std::io::empty(), &|_| None);
    assert_eq!(out.code, crate::cli::USAGE);
    assert!(out.err.contains("unknown verb claim-retry"), "{}", out.err);
}

// ---- a Skuld request waits for its blocker to land and never stacks on it ----------------

mod skuld_waits {
    use super::*;
    use serde_json::{json, Value};
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::Arc;
    use std::time::Duration;

    const SKULD: &str = "skuld-x";

    struct Blocker(&'static str);

    fn item(id: &str, labels: &[&str], blocks: Option<&str>) -> Value {
        let deps: Vec<Value> = blocks.iter().map(|b| json!({ "issue_id": id, "depends_on_id": b, "type": "blocks" })).collect();
        json!({ "id": id, "title": id, "priority": 2, "issue_type": "task", "labels": labels, "dependencies": deps })
    }

    fn row(id: &str, state: &str) -> Value {
        json!({ "bead_id": id, "state": state, "holds": [], "tip": "abc", "repo": "spira" })
    }

    impl Transport for Blocker {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            let words: Vec<&str> = argv.iter().map(String::as_str).collect();
            let out = match words.as_slice() {
                ["list", "--state", ..] => json!([row("sp-b", "READY")]).to_string(),
                ["list", "--ids", ids] => json!([row("sp-b", "READY"), row("sp-a", self.0)].into_iter().filter(|r| ids.contains(r["bead_id"].as_str().unwrap())).collect::<Vec<_>>()).to_string(),
                ["plan", "items", "--ids", ids] => {
                    let all = [item("sp-b", &["repo:spira", SKULD], Some("sp-a")), item("sp-a", &["repo:spira"], None)];
                    let items: Vec<Value> = all.into_iter().filter(|i| ids.contains(i["id"].as_str().unwrap())).collect();
                    json!({ "watermark": 7, "items": items }).to_string()
                }
                ["plan", "watermark"] => r#"{"watermark":7}"#.into(),
                _ => String::new(),
            };
            Ok(Reply { code: 0, out, err: String::new(), read_ended: false })
        }
    }

    fn config() -> Config {
        Config { control_store: true, stack_max_depth: 4, skuld_label: SKULD.into(), run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() }
    }

    fn store(blocker_state: &'static str) -> Store {
        Store { lc: LcClient::over(Arc::new(Blocker(blocker_state)), true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None, readiness_alert: Default::default() }
    }

    fn judged(blocker_state: &'static str, config: Config) -> (Vec<String>, Vec<verdicts::Entry>) {
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let env = Env { stdin: &mut stdin, config, lookup: &none };
        let (rows, entries) = judge_rows(&Args::parse(&[]).unwrap(), &env, &store(blocker_state), None, false).unwrap();
        (rows.into_iter().map(|r| r.id).collect(), entries)
    }

    #[test_id(tf3w9kd)]
    #[test]
    fn judge_rows_blocks_a_skuld_bead_on_a_certified_or_in_delivery_blocker_and_claims_it_once_landed() {
        for open in ["CERTIFIED", "IN_DELIVERY"] {
            let (claimable, entries) = judged(open, config());
            assert!(claimable.is_empty(), "{open}: {claimable:?}");
            let e = entries.iter().find(|e| e.id == "sp-b").unwrap_or_else(|| panic!("{open}: no verdict for sp-b"));
            assert_eq!((e.verdict, e.blocker.as_deref()), (verdicts::Kind::Blocked, Some("sp-a")), "{open}");
        }
        assert_eq!(judged("LANDED", config()).0, ["sp-b"]);
    }

    #[test_id(tn7q2xr)]
    #[test]
    fn a_bead_without_the_skuld_label_still_stacks_on_an_in_delivery_blocker() {
        let (claimable, _) = judged("IN_DELIVERY", Config { skuld_label: String::new(), ..config() });
        assert_eq!(claimable, ["sp-b"], "an empty label matches nothing");
    }

    #[test_id(tz5c8hm)]
    #[test]
    fn stack_refuses_a_skuld_bead_on_an_unlanded_blocker_with_exit_3_and_a_blocked_reason() {
        for open in ["CERTIFIED", "IN_DELIVERY"] {
            let mut stdin = std::io::empty();
            let none = |_: &str| None;
            let mut env = Env { stdin: &mut stdin, config: config(), lookup: &none };
            let out = stack_over(&Args::parse(&[]).unwrap(), &mut env, &store(open), "sp-b".into(), 4);
            let v: Value = serde_json::from_str(&out.out).unwrap();
            assert_eq!(out.code, 3, "{open}: {}", out.out);
            assert!(v["reason"].as_str().unwrap().starts_with("Blocked"), "{open}: {v}");
        }
    }
}

mod provisional_blocker {
    use super::*;
    use serde_json::json;
    use spira_store::lc_call::{LcClient, Reply, Transport};
    use std::sync::Arc;
    use std::time::Duration;

    struct Rows {
        blocker_state: &'static str,
        blocker_reason: Option<&'static str>,
    }

    impl Transport for Rows {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            let words: Vec<&str> = argv.iter().map(String::as_str).collect();
            let cand = json!({"bead_id": "sp-c", "state": "READY", "holds": []});
            let blocker = json!({"bead_id": "sp-b", "state": self.blocker_state, "holds": [], "tip": "tipb", "reason": self.blocker_reason});
            let item = |id: &str, deps: serde_json::Value| {
                json!({"id": id, "title": id, "priority": 2, "issue_type": "task", "labels": ["repo:spira"], "dependencies": deps})
            };
            let dep = json!([{"issue_id": "sp-c", "depends_on_id": "sp-b", "type": "blocks"}]);
            let out = match words.as_slice() {
                ["list", "--state", ..] => json!([cand]),
                ["list", "--ids", ids] if ids.contains("sp-c") => json!([cand, blocker]),
                ["list", "--ids", _] => json!([blocker]),
                ["plan", "items", "--ids", ids] => {
                    let mut items = Vec::new();
                    if ids.contains("sp-c") {
                        items.push(item("sp-c", dep));
                    }
                    if ids.contains("sp-b") {
                        items.push(item("sp-b", json!([])));
                    }
                    json!({"watermark": 1, "items": items})
                }
                _ => json!([]),
            };
            Ok(Reply { code: 0, out: out.to_string(), err: String::new(), read_ended: false })
        }
    }

    fn over(blocker_state: &'static str, blocker_reason: Option<&'static str>) -> Store {
        let svc = Arc::new(Rows { blocker_state, blocker_reason });
        Store { lc: LcClient::over(svc, true), lc_bin: "nonexistent/spira-lc".into(), timeout: Duration::from_secs(1), views: None, readiness_alert: Default::default() }
    }

    fn env<R>(f: impl FnOnce(&mut Env) -> R) -> R {
        let mut stdin = std::io::empty();
        let none = |_: &str| None;
        let config = Config { control_store: true, stack_max_depth: 4, run: "nonexistent/run".into(), queue_dir: "nonexistent/queue".into(), ..Config::default() };
        f(&mut Env { stdin: &mut stdin, config, lookup: &none })
    }

    fn verdict_of_c(st: &Store) -> (verdicts::Kind, Option<String>) {
        let a = Args::parse(&[]).unwrap();
        let (_, entries) = env(|e| judge_rows(&a, e, st, None, false)).unwrap();
        let e = entries.into_iter().find(|e| e.id == "sp-c").expect("sp-c judged");
        (e.verdict, e.blocker)
    }

    fn stack_of_c(st: &Store) -> Outcome {
        let a = Args::parse(&["sp-c".to_string()]).unwrap();
        env(|e| stack_over(&a, e, st, "sp-c".to_string(), 4))
    }

    #[test_id(tqwulcc)]
    #[test]
    fn a_blocker_delivered_from_submitted_blocks_the_dependent_through_judge_rows() {
        let st = over("IN_DELIVERY", Some("delivered-from-submitted"));
        assert_eq!(verdict_of_c(&st), (verdicts::Kind::Blocked, Some("sp-b".to_string())));
    }

    #[test_id(t7i9dhb)]
    #[test]
    fn a_blocker_delivered_from_certified_or_landed_leaves_the_dependent_claimable() {
        assert_eq!(verdict_of_c(&over("IN_DELIVERY", None)).0, verdicts::Kind::Claimable);
        assert_eq!(verdict_of_c(&over("LANDED", Some("delivered-from-submitted"))).0, verdicts::Kind::Claimable);
    }

    #[test_id(t4yccvs)]
    #[test]
    fn stack_refuses_a_dependent_of_a_blocker_delivered_from_submitted_and_names_blocked() {
        let o = stack_of_c(&over("IN_DELIVERY", Some("delivered-from-submitted")));
        assert_eq!(o.code, 3);
        let v: serde_json::Value = serde_json::from_str(o.out.trim()).unwrap();
        assert!(v["reason"].as_str().unwrap().starts_with("Blocked"), "{v}");
        let ok = stack_of_c(&over("IN_DELIVERY", None));
        assert_eq!(ok.code, 0, "{}", ok.out);
    }
}
