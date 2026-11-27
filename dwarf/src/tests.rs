#![allow(clippy::type_complexity)]
//! Test support shared by dwarf's unit tests (the whole-run tests that lived here went with
//! sp-ao9dn2: each drove real git or other programs).

use std::collections::BTreeMap;

/// Every key spira-config's own "every key declared" test fixture
/// (`spira/testlib/complete.toml`, the same document
/// `spira_config::process::fixture_toml` writes from) declares, renamed `SPIRA_*`/
/// `COCKPIT_*` — the same rename `spira_config::resolve`'s own (private) `toml_key_map`
/// applies when it builds a toml-backed `Resolved`, replicated here (not called) because
/// it is not `pub`. No file I/O, no `$SPIRA_TOML`/`$SPIRA_HOME` env, and nowhere near
/// `spira_config::process::cfg`'s own cached resolution — this builds `Conf`'s backing map
/// directly, the same shape `merge_resolved_config` would produce in production.
///
/// `pub(crate)`: `escape::tests::conf_at` builds its own `Conf` the same way and needs the
/// same base — a hand-built `Conf` whose registered keys are simply absent now under-
/// resolves them to 0/"" (`Conf::i`/`Conf::s` carry no Rust-side default any more, per Ryan
/// 2026-10-05: one source of config), which is a silent behaviour change for whichever one
/// of this file's many fixtures does not also move onto this same base.
pub(crate) fn complete_vars() -> BTreeMap<String, String> {
    let doc = spira_config::validate(&spira_config::process::complete_fixture_text()).expect("the complete fixture validates");
    spira_config::spira_string_map(&doc)
        .into_iter()
        .map(|(k, v)| if k.starts_with("COCKPIT_") { (k, v) } else { (format!("SPIRA_{k}"), v) })
        .chain(std::iter::once((
            "SPIRA_TOML".to_string(),
            spira_config::resolve::locate_home_for_process().unwrap().join("testlib/complete.toml").display().to_string(),
        )))
        .collect()
}

/// `Run::verdict` end to end over fakes: which directory the dirty guard reads, and whether a
/// dirty tree reaches `bead_reopen`/`requeue`. No process, no repository.
pub(crate) mod verdict_run {
    use crate::conf::{Conf, Rune};
    use std::collections::BTreeMap;
    use crate::ledger::Ledger;
    use crate::ports::{Bd, Env, Exec, Git, Seam};
    use crate::run::{Deps, Mode, Run, State};
    use crate::seam::Snapshot;
    use crate::session::{Launcher, SessionSpec, Stop};
    use crate::util::{Out, Sink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    struct NoBd;
    impl Bd for NoBd {
        fn bd(&self, _: &[String]) -> Out {
            Out::ok("[]")
        }
    }
    struct Seams(Mutex<Vec<(String, Vec<String>)>>);
    impl Seam for Seams {
        fn call(&self, f: &str, a: &[String]) -> Out {
            self.0.lock().unwrap().push((f.into(), a.to_vec()));
            Out::ok("")
        }
    }
    struct Lc;
    impl Exec for Lc {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            if prog == "spira-lc" && args.first().map(String::as_str) == Some("show") {
                return Out::ok(format!(r#"{{"bead":{{"bead_id":"{}","state":"SUBMITTED"}}}}"#, args[1]));
            }
            Out::ok("")
        }
    }
    struct Dirty(Vec<&'static str>);
    impl Git for Dirty {
        fn git(&self, dir: &Path, args: &[&str]) -> Out {
            let dirty = self.0.iter().any(|d| Path::new(d) == dir);
            match args[0] {
                "log" => Out::ok("sp-x — the work\n"),
                "status" if dirty => Out::ok(" M f\n"),
                "diff" if dirty && args.contains(&"--name-only") => Out::ok("f\n"),
                _ => Out { code: 1, stdout: String::new(), stderr: String::new() },
            }
        }
    }
    struct NoLaunch;
    impl Launcher for NoLaunch {
        fn run(&self, _: &SessionSpec, _: &Stop) -> i32 {
            0
        }
    }
    #[derive(Default)]
    struct Mute(Mutex<Vec<String>>);
    impl Sink for Mute {
        fn out(&self, line: &str) {
            self.0.lock().unwrap().push(line.to_string());
        }
        fn err(&self, _: &str) {}
    }

    /// Runs `verdict()` for a SUBMITTED restricted bead whose own worktree is `/w/own` and
    /// whose shared checkout is `/shared`; returns the bead_reopen calls and the requeue cause.
    pub(crate) fn run(dirty_dirs: Vec<&'static str>) -> (Vec<Vec<String>>, Option<String>) {
        let seam = Seams(Default::default());
        let cause = with_run(&NoBd, &seam, Dirty(dirty_dirs), "", |r| {
            r.verdict();
            r.s.requeue_cause.clone()
        });
        let reopens = seam.0.lock().unwrap().iter().filter(|(f, _)| f == "bead_reopen").map(|(_, a)| a.clone()).collect();
        (reopens, cause)
    }

    fn with_run<T>(bd: &dyn Bd, seam: &Seams, git: Dirty, claimed_raw: &str, f: impl FnOnce(&mut Run) -> T) -> T {
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        vars.insert("SPIRA_ALLOW_PROD_DIRTY".into(), String::new());
        with_run_over(vars, bd, git, &Lc, &|| 0, seam, claimed_raw, f)
    }

    #[allow(clippy::too_many_arguments)]
    fn with_run_over<T>(vars: BTreeMap<String, String>, bd: &dyn Bd, git: Dirty, exec: &dyn Exec, clock: &(dyn Fn() -> i64 + Sync), seam: &Seams, claimed_raw: &str, f: impl FnOnce(&mut Run) -> T) -> T {
        with_run_sink(vars, bd, git, exec, clock, seam, &Mute::default(), claimed_raw, f)
    }

    #[allow(clippy::too_many_arguments)]
    fn with_run_sink<T>(vars: BTreeMap<String, String>, bd: &dyn Bd, git: Dirty, exec: &dyn Exec, clock: &(dyn Fn() -> i64 + Sync), seam: &dyn Seam, sink: &dyn Sink, claimed_raw: &str, f: impl FnOnce(&mut Run) -> T) -> T {
        with_run_launching(vars, bd, git, exec, clock, seam, sink, &NoLaunch, claimed_raw, f)
    }

    #[allow(clippy::too_many_arguments)]
    fn with_run_launching<T>(vars: BTreeMap<String, String>, bd: &dyn Bd, git: impl Git, exec: &dyn Exec, clock: &(dyn Fn() -> i64 + Sync), seam: &dyn Seam, sink: &dyn Sink, launcher: &dyn Launcher, claimed_raw: &str, f: impl FnOnce(&mut Run) -> T) -> T {
        let original = vars.clone();
        let conf = Conf::new(&Snapshot { vars, ..Default::default() }, Path::new("h/spira"));
        let rune = Rune::from_vars("builder", &Default::default());
        let env = Env::new(original, Default::default());
        let stop = Arc::new(Stop::default());
        let mut r = Run {
            d: Deps { bd, seam, git: &git, exec, launcher, sink, env: &env, clock, sleep: &|_| {} },
            ledger: Ledger { path: PathBuf::from("nonexistent/ledger"), dry: true },
            conf,
            rune,
            snap: Snapshot::default(),
            mode: Mode::DryRun,
            pid: 1,
            own_unit: String::new(),
            t0: 0,
            claim_bin: String::new(),
            stop,
            hb_shutdown: Arc::new(AtomicBool::new(false)),
            hb_done: Arc::new(AtomicBool::new(false)),
            rune_file: PathBuf::new(),
            s: State { bead: "sp-x".into(), branch: "b".into(), repo: PathBuf::from("repo"), work: Some(PathBuf::from("w/own")), lc_model_restricted: true, claimed_raw: claimed_raw.into(), ..Default::default() },
        };
        f(&mut r)
    }

    /// A store whose claim applies: `spira-claim stack` answers `proposal`, the claimed row
    /// carries `holds`, and `spira-claim rejections` answers `rejections`.
    #[derive(Default)]
    struct Claimable {
        proposal: &'static str,
        holds: &'static [&'static str],
        rejections: &'static str,
        holder: Mutex<String>,
        /// The row's state once it leaves WORKING, and when its ask hold was last raised.
        left: Mutex<Option<&'static str>>,
        ask_since: Mutex<i64>,
        calls: Mutex<Vec<Vec<String>>>,
    }
    impl Claimable {
        fn record(&self, prog: &str, args: &[String]) {
            self.calls.lock().unwrap().push(std::iter::once(prog.to_string()).chain(args.iter().cloned()).collect());
        }
        fn called(&self, head: &[&str]) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().iter().filter(|c| c.len() >= head.len() && c.iter().zip(head).all(|(a, b)| a == b)).cloned().collect()
        }
    }
    impl Exec for Claimable {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            self.record(prog, args);
            match (prog, args.first().map(String::as_str)) {
                ("spira-claim", Some("stack")) => Out::ok(self.proposal),
                ("spira-claim", Some("rejections")) => Out::ok(self.rejections),
                ("spira-lc", Some("list")) => Out::ok("[]"),
                ("spira-lc", Some("show")) => Out::ok(serde_json::json!({"bead": {"bead_id": "sp-x", "state": self.left.lock().unwrap().unwrap_or("WORKING"), "holder": *self.holder.lock().unwrap(), "holds": self.holds, "ask_since": self.ask_since.lock().unwrap().to_string()}}).to_string()),
                _ => Out::ok(""),
            }
        }
    }
    impl Seam for Claimable {
        fn call(&self, f: &str, a: &[String]) -> Out {
            self.record(f, a);
            if f == "lc_claim_bead" {
                *self.holder.lock().unwrap() = a[1].clone();
            }
            Out::ok("")
        }
    }

    const UNSTACKED: &str = r#"{"claimable":true,"stack":{},"stack_depth":0,"stack_max_depth":4}"#;

    /// `Run::claim()` of sp-x as `dwarf direct`; returns the log lines and the ledger. The run
    /// ends at the repository lookup, past every gate that could turn the claim away.
    fn directed_claim(store: &Claimable) -> (Vec<String>, String) {
        directed_claim_then(store, |_| {})
    }

    fn directed_claim_then(store: &Claimable, then: impl FnOnce(&mut Run)) -> (Vec<String>, String) {
        let dir = testkit::TempDir::new("dwarf-directed-claim");
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_RUN".into(), dir.path().display().to_string());
        vars.insert("SPIRA_REPO".into(), "shared".into());
        let ledger = dir.path().join("dwarf-ledger.log");
        let bd = CountingBd(Default::default());
        let sink = Mute::default();
        with_run_sink(vars, &bd, Dirty(vec![]), store, &|| 0, store, &sink, "", |r| {
            r.mode = Mode::Directed(crate::direct::Direct { bead: "sp-x".into(), by: "concierge".into(), reason: "loop fix".into(), replaced: None });
            r.claim_bin = "spira-claim".into();
            r.ledger = Ledger { path: ledger.clone(), dry: false };
            let _ = r.claim();
            then(r);
        });
        let lines = sink.0.lock().unwrap().clone();
        (lines, std::fs::read_to_string(&ledger).unwrap_or_default())
    }

    fn assert_claim_kept(store: &Claimable, ledger: &str) {
        assert!(store.called(&["release_own_claim"]).is_empty() && store.called(&["spira-lc", "unclaim"]).is_empty(), "{:?}", store.calls.lock().unwrap());
        assert!(ledger.contains("unmapped-repo"), "positive control: the claim reached the repository lookup\n{ledger}");
    }

    #[test_id(tf9lqjz)]
    #[test]
    fn a_directed_claim_is_sent_unstacked_past_a_blocked_stack_proposal() {
        let store = Claimable { proposal: r#"{"claimable":false,"reason":"Blocked { on: \"sp-p\" }"}"#, ..Default::default() };
        let (lines, ledger) = directed_claim(&store);
        let sent = store.called(&["lc_claim_bead"]);
        assert_eq!(sent.len(), 1, "{lines:?}");
        assert_eq!((sent[0][1].as_str(), sent[0][4].as_str()), ("sp-x", "{}"));
        assert!(sent[0].last().is_some_and(|d| d.contains("concierge")), "{sent:?}");
        assert_claim_kept(&store, &ledger);
    }

    #[test_id(tbvnrsy)]
    #[test]
    fn a_directed_claim_keeps_a_held_row_and_its_claim_line_names_the_holds() {
        let store = Claimable { proposal: UNSTACKED, holds: &["manual"], ..Default::default() };
        let (lines, ledger) = directed_claim(&store);
        assert_claim_kept(&store, &ledger);
        assert!(!ledger.contains("hold-raced"), "{ledger}");
        assert!(lines.iter().any(|l| l.contains("directed claim of sp-x keeps its manual hold(s)")), "{lines:?}");
    }

    #[test_id(tzrnapi)]
    #[test]
    fn a_directed_claim_past_the_escalation_ceiling_is_kept_and_not_held() {
        let store = Claimable { proposal: UNSTACKED, rejections: r#"{"rejections":9,"stronger_rejections":9,"history":[]}"#, ..Default::default() };
        let (lines, ledger) = directed_claim(&store);
        assert_claim_kept(&store, &ledger);
        assert!(store.called(&["spira-lc", "hold"]).is_empty() && !ledger.contains("escalation-hold"), "{ledger}");
        assert!(lines.iter().any(|l| l.contains("escalation-ceiling") && l.contains("directed")), "{lines:?}");
    }

    /// The log of a directed session on an ask-held row that ends SUBMITTED, its ask hold
    /// last raised at `raised` (100 when claimed).
    fn teardown_of_an_ask_held_directed_run(raised: i64) -> Vec<String> {
        let store = Claimable { proposal: UNSTACKED, holds: &["ask"], ask_since: Mutex::new(100), ..Default::default() };
        directed_claim_then(&store, |r| {
            *store.left.lock().unwrap() = Some("SUBMITTED");
            *store.ask_since.lock().unwrap() = raised;
            r.s.session_started = true;
            r.hb_done.store(true, std::sync::atomic::Ordering::SeqCst);
            r.teardown(0);
        })
        .0
    }

    #[test_id(twobh5b)]
    #[test]
    fn an_ask_hold_the_directed_claim_inherited_is_not_read_as_this_sessions_question() {
        let lines = teardown_of_an_ask_held_directed_run(100);
        assert!(!lines.iter().any(|l| l.contains("operator-wait")), "{lines:?}");
    }

    #[test_id(tu1gb9g)]
    #[test]
    fn a_question_asked_during_a_directed_session_over_an_inherited_ask_is_still_seen() {
        let lines = teardown_of_an_ask_held_directed_run(200);
        assert!(lines.iter().any(|l| l.contains("operator-wait")), "{lines:?}");
    }

    fn ready_set_call(express: Option<&str>) -> Vec<String> {
        let mut vars = super::complete_vars();
        if let Some(v) = express {
            vars.insert("SPIRA_REQUIRE_EXPRESS".into(), v.into());
        }
        let seam = Seams(Default::default());
        with_run_over(vars, &NoBd, Dirty(vec![]), &Lc, &|| 0, &seam, "", |r| {
            let _ = r.ready_set(1, std::time::Duration::ZERO);
        });
        let calls = seam.0.lock().unwrap();
        let (f, a) = calls.iter().find(|(f, _)| f == "_dwarf_ready_set").expect("ready_set called the seam").clone();
        assert_eq!(f, "_dwarf_ready_set");
        a
    }

    #[test_id(tito0p4)]
    #[test]
    fn ready_set_sends_express_only_when_the_requirement_is_set_and_non_empty() {
        assert_eq!(ready_set_call(None), crate::seam::ready_set_argv("builder", false));
        assert_eq!(ready_set_call(Some("")), crate::seam::ready_set_argv("builder", false));
        assert_eq!(ready_set_call(Some("1")), crate::seam::ready_set_argv("builder", true));
    }

    struct Appends(Mutex<Vec<String>>);
    impl Exec for Appends {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            if prog == "inbox-triage" {
                self.0.lock().unwrap().push(args[1..].join(" "));
            }
            Out::ok("")
        }
    }

    /// The inbox lines `report_refused_submit` appends for `bead` at `now`, over an inbox file
    /// already holding `inbox`.
    fn refused_submit(bead: &str, now: i64, inbox: &str, reason: &str) -> Vec<String> {
        let dir = testkit::TempDir::new("dwarf-inbox");
        let log = dir.path().join("inbox.log");
        std::fs::write(&log, inbox).unwrap();
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_CONCIERGE_INBOX".into(), log.display().to_string());
        let exec = Appends(Default::default());
        with_run_over(vars, &NoBd, Dirty(vec![]), &exec, &move || now, &Seams(Default::default()), "", |r| r.report_refused_submit(bead, reason));
        let got = exec.0.lock().unwrap().clone();
        got
    }

    #[test_id(ttzu41x)]
    #[test]
    fn the_end_of_a_session_removes_its_tmpdir_and_nothing_beside_it() {
        let work = testkit::TempDir::new("dwarf-tmpdir");
        let dir = Run::make_session_tmpdir(work.path()).unwrap();
        std::fs::write(dir.join("scratch"), "x").unwrap();
        let sibling = work.path().join(".runtime").join("keep");
        std::fs::write(&sibling, "y").unwrap();
        let w = work.path().to_path_buf();
        with_run(&NoBd, &Seams(Default::default()), Dirty(vec![]), "", |r| {
            r.s.work = Some(w);
            r.remove_tmpdir();
        });
        assert!(!dir.exists());
        assert!(sibling.is_file());
    }

    #[test_id(ttqtqp2)]
    #[test]
    fn a_session_exports_its_worktree_tmpdir_to_what_it_starts() {
        let work = testkit::TempDir::new("dwarf-tmpdir-export");
        let w = work.path().to_path_buf();
        let got = with_run(&NoBd, &Seams(Default::default()), Dirty(vec![]), "", |r| {
            r.export_session_tmpdir(&w);
            r.d.env.child().get("TMPDIR").cloned()
        });
        let want = Run::session_tmpdir(work.path());
        assert_eq!(got.as_deref(), Some(want.display().to_string().as_str()));
        assert!(want.is_dir() && want.starts_with(work.path()));
    }

    struct Capture(Mutex<Vec<BTreeMap<String, String>>>);
    impl Launcher for Capture {
        fn run(&self, spec: &SessionSpec, _: &Stop) -> i32 {
            self.0.lock().unwrap().push(spec.env.clone());
            0
        }
    }

    struct StackAnswer(&'static str);
    impl Exec for StackAnswer {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            if prog == "spira-claim" && args.first().map(String::as_str) == Some("stack") {
                return Out::ok(self.0);
            }
            Out::ok("")
        }
    }

    fn lead_for_proposal(answer: &'static str) -> String {
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        with_run_over(vars, &NoBd, Dirty(vec![]), &StackAnswer(answer), &|| 0, &Seams(Default::default()), "", |r| {
            r.claim_bin = "spira-claim".into();
            r.s.stack = r.stack_proposal("sp-x", "t").stack;
            r.lead_briefs(Path::new("w/own"), "head", &[])
        })
    }

    #[test_id(t3ttyv4)]
    #[test]
    fn a_claim_stacked_on_a_prerequisite_tells_the_builder_in_its_task_text() {
        let lead = lead_for_proposal(r#"{"claimable":true,"stack":{"sp-a":"tipA"},"stack_depth":1,"stack_max_depth":4}"#);
        assert!(lead.contains("## Stacked on unlanded work") && lead.contains("- sp-a at tipA"), "{lead}");
        let plain = lead_for_proposal(r#"{"claimable":true,"stack":{},"stack_depth":0,"stack_max_depth":4}"#);
        assert!(!plain.contains("Stacked on"), "{plain}");
    }

    #[test_id(tav6yvb)]
    #[test]
    fn the_env_the_session_port_receives_carries_the_worktree_tmpdir() {
        let _otel = OTEL.lock().unwrap_or_else(|e| e.into_inner());
        let work = testkit::TempDir::new("dwarf-tmpdir-spec");
        let w = work.path().to_path_buf();
        let cap = Capture(Default::default());
        let mut vars = super::super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        let cwd = std::env::current_dir().unwrap();
        let res = with_run_launching(vars, &NoBd, Dirty(vec![]), &Lc, &|| 0, &Seams(Default::default()), &Mute::default(), &cap, "", |r| {
            r.s.logf = Some(w.join("log"));
            let bin = w.join("model-bin");
            std::fs::create_dir_all(&bin).unwrap();
            let work_bin = bin.join("work");
            testkit::write_exe(&work_bin, "#!/bin/sh\n");
            r.d.env.set(spira_config::release_env::MODEL_BIN_OVERRIDE_ENV, &bin.display().to_string());
            r.session(&w).map_err(|a| format!("{a:?}"))
        });
        std::env::set_current_dir(cwd).unwrap();
        res.unwrap();
        let envs = cap.0.lock().unwrap();
        let want = Run::session_tmpdir(work.path()).display().to_string();
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].get("TMPDIR"), Some(&want));
    }

    fn gate_reservation_after_release(submitted: Option<i64>) -> Option<String> {
        let work = testkit::TempDir::new("dwarf-reserve");
        let base = work.path().join("builds");
        with_run(&NoBd, &Seams(Default::default()), Dirty(vec![]), "", |r| {
            r.s.bead = "sp-res".into();
            r.s.submitted_at = submitted;
            r.s.build_dir = Some(spira_config::build_dir::lease_for(std::slice::from_ref(&base), "sp-res", "spira", 1).unwrap());
            r.release_build_dir();
        });
        std::fs::read_to_string(base.join("spira/1.gate")).ok()
    }

    #[test_id(tplace4)]
    #[test]
    fn a_submitted_run_reserves_its_build_directory_for_its_gate_and_an_unsubmitted_one_does_not() {
        assert_eq!(gate_reservation_after_release(Some(T0)).as_deref(), Some("sp-res"));
        assert_eq!(gate_reservation_after_release(None), None);
    }

    #[test_id(tqumq7u)]
    #[test]
    fn a_reservation_that_cannot_be_written_is_logged_with_its_path_and_bead() {
        let work = testkit::TempDir::new("dwarf-reserve-fail");
        let base = work.path().join("builds");
        std::fs::create_dir_all(base.join("spira/1.gate")).unwrap();
        let sink = Mute::default();
        with_run_sink(crate::tests::complete_vars(), &NoBd, Dirty(vec![]), &Lc, &|| 0, &Seams(Default::default()), &sink, "", |r| {
            r.s.bead = "sp-res".into();
            r.s.submitted_at = Some(T0);
            r.s.build_dir = Some(spira_config::build_dir::lease_for(std::slice::from_ref(&base), "sp-res", "spira", 1).unwrap());
            r.release_build_dir();
        });
        let lines = sink.0.lock().unwrap();
        let gate = base.join("spira/1.gate").display().to_string();
        assert!(lines.iter().any(|l| l.contains("could not reserve") && l.contains(&gate) && l.contains("sp-res")), "{lines:?}");
    }

    #[test_id(ttez6v8)]
    #[test]
    fn a_released_build_directory_logs_one_lease_report_naming_the_bead() {
        let work = testkit::TempDir::new("dwarf-lease-report");
        let base = work.path().join("builds");
        let sink = Mute::default();
        with_run_sink(crate::tests::complete_vars(), &NoBd, Dirty(vec![]), &Lc, &|| 0, &Seams(Default::default()), &sink, "", |r| {
            r.s.bead = "sp-res".into();
            r.s.build_dir = Some(spira_config::build_dir::lease_for(std::slice::from_ref(&base), "sp-res", "spira", 1).unwrap());
            r.release_build_dir();
            r.release_build_dir();
        });
        let lines = sink.0.lock().unwrap();
        let reports: Vec<&String> = lines.iter().filter(|l| l.contains("build folder ")).collect();
        let want = format!("build folder {} repo:spira item:sp-res state:new first_build_secs:none", base.join("spira/1").display());
        assert!(reports.len() == 1 && reports[0].ends_with(&want), "{lines:?}");
    }

    fn session_env_of(builds: bool) -> BTreeMap<String, String> {
        let _otel = OTEL.lock().unwrap_or_else(|e| e.into_inner());
        let work = testkit::TempDir::new("dwarf-cargo-env");
        let w = work.path().to_path_buf();
        let cap = Capture(Default::default());
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        vars.insert("CARGO_INCREMENTAL".into(), "0".into());
        vars.insert("CARGO_BUILD_INCREMENTAL".into(), "false".into());
        let cwd = std::env::current_dir().unwrap();
        let res = with_run_launching(vars, &NoBd, Dirty(vec![]), &Lc, &|| 0, &Seams(Default::default()), &Mute::default(), &cap, "", |r| {
            r.s.logf = Some(w.join("log"));
            r.rune.builds = builds;
            if builds {
                r.s.build_dir = Some(spira_config::build_dir::lease_for(&[w.join("builds")], "sp-x", "spira", 1).unwrap());
            }
            let bin = w.join("model-bin");
            std::fs::create_dir_all(&bin).unwrap();
            testkit::write_exe(bin.join("work"), "#!/bin/sh\n");
            r.d.env.set(spira_config::release_env::MODEL_BIN_OVERRIDE_ENV, &bin.display().to_string());
            r.session(&w).map_err(|a| format!("{a:?}"))
        });
        std::env::set_current_dir(cwd).unwrap();
        res.unwrap();
        let envs = cap.0.lock().unwrap();
        assert_eq!(envs.len(), 1);
        envs[0].clone()
    }

    #[test_id(tjjyb9n)]
    #[test]
    fn a_building_session_compiles_under_the_harness_jobs_and_threads_and_never_one_shot() {
        let env = session_env_of(true);
        assert_eq!(env.get("CARGO_BUILD_JOBS").map(String::as_str), Some("6"));
        assert_eq!(env.get("RUST_TEST_THREADS").map(String::as_str), Some("4"));
        assert!(!env.contains_key("CARGO_INCREMENTAL") && !env.contains_key("CARGO_BUILD_INCREMENTAL"), "{env:?}");
    }

    #[test_id(t96x90m)]
    #[test]
    fn a_session_that_does_not_build_gets_no_jobs_or_threads() {
        let env = session_env_of(false);
        assert!(!env.contains_key("CARGO_BUILD_JOBS") && !env.contains_key("RUST_TEST_THREADS"), "{env:?}");
    }

    struct WorldUp(&'static str);
    impl Exec for WorldUp {
        fn exec(&self, _: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            if args.first().map(String::as_str) == Some("world") && args.get(1).map(String::as_str) == Some("up") {
                let config = Path::new(&args[2]).join("config");
                std::fs::create_dir_all(&config).unwrap();
                std::fs::write(config.join("sim.env"), self.0).unwrap();
            }
            Out::ok("")
        }
    }

    #[test_id(tgr4f1n)]
    #[test]
    fn a_pilgrim_session_is_started_with_the_grafana_url_its_world_names() {
        let _otel = OTEL.lock().unwrap_or_else(|e| e.into_inner());
        let work = testkit::TempDir::new("dwarf-pilgrim-grafana");
        let w = work.path().to_path_buf();
        let release = w.join("release");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        testkit::write_exe(release.join("bin/sim"), "#!/bin/sh\n");
        let _env = testkit::env(&[(spira_config::RELEASE_ENV, Some(release.to_str().unwrap()))]);
        let cap = Capture(Default::default());
        let mut vars = super::super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        let cwd = std::env::current_dir().unwrap();
        let sim_env = "SPIRA_LC_SOCKET=w/lc.sock\nSPIRA_TOML=w/config/sim.toml\nPILGRIM_GRAFANA_URL=http://g:3000\n";
        let res = with_run_launching(vars, &NoBd, Dirty(vec![]), &WorldUp(sim_env), &|| 0, &Seams(Default::default()), &Mute::default(), &cap, "", |r| {
            r.rune = Rune::from_vars("pilgrim", &Default::default());
            r.conf.run = w.join("run");
            r.s.logf = Some(w.join("log"));
            let bin = w.join("model-bin");
            std::fs::create_dir_all(&bin).unwrap();
            testkit::write_exe(bin.join("work"), "#!/bin/sh\n");
            r.d.env.set(spira_config::release_env::MODEL_BIN_OVERRIDE_ENV, &bin.display().to_string());
            r.session(&w).map_err(|a| format!("{a:?}"))
        });
        std::env::set_current_dir(cwd).unwrap();
        res.unwrap();
        let envs = cap.0.lock().unwrap();
        assert_eq!(envs[0].get("PILGRIM_GRAFANA_URL").map(String::as_str), Some("http://g:3000"), "{envs:?}");
    }

    #[derive(Clone, Default)]
    struct Told(std::sync::Arc<Mutex<Vec<String>>>);
    impl crate::session_feed::Outlet for Told {
        fn publish(&self, _: &str, payload: &serde_json::Value) {
            self.0.lock().unwrap().push(payload["state"].as_str().unwrap_or_default().to_string());
        }
    }

    /// What the feed had said by the time the world was taken down.
    struct ToldAtWorldDown(Told, Mutex<Vec<String>>);
    impl Exec for ToldAtWorldDown {
        fn exec(&self, prog: &str, args: &[String], stdin: Option<Vec<u8>>, cwd: Option<&Path>) -> Out {
            if args.get(1).map(String::as_str) == Some("down") {
                *self.1.lock().unwrap() = self.0 .0.lock().unwrap().clone();
            }
            WorldUp("SPIRA_LC_SOCKET=w/lc.sock\nSPIRA_TOML=w/config/sim.toml\n").exec(prog, args, stdin, cwd)
        }
    }

    #[test_id(tjyxad6)]
    #[test]
    fn a_pilgrim_session_is_published_ended_before_its_world_is_taken_down() {
        let _otel = OTEL.lock().unwrap_or_else(|e| e.into_inner());
        let work = testkit::TempDir::new("dwarf-pilgrim-ended");
        let w = work.path().to_path_buf();
        let release = w.join("release");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        testkit::write_exe(release.join("bin/sim"), "#!/bin/sh\n");
        let _env = testkit::env(&[(spira_config::RELEASE_ENV, Some(release.to_str().unwrap()))]);
        let told = Told::default();
        let exec = ToldAtWorldDown(told.clone(), Default::default());
        let cap = Capture(Default::default());
        let mut vars = super::super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        let cwd = std::env::current_dir().unwrap();
        let res = with_run_launching(vars, &NoBd, Dirty(vec![]), &exec, &|| 0, &Seams(Default::default()), &Mute::default(), &cap, "", |r| {
            r.rune = Rune::from_vars("pilgrim", &Default::default());
            r.conf.run = w.join("run");
            r.s.logf = Some(w.join("log"));
            let outlet = told.clone();
            r.s.feed = Some(std::sync::Arc::new(crate::session_feed::SessionFeed::new("sp-x", "pilgrim", 300, |_| Box::new(outlet))));
            let bin = w.join("model-bin");
            std::fs::create_dir_all(&bin).unwrap();
            testkit::write_exe(bin.join("work"), "#!/bin/sh\n");
            r.d.env.set(spira_config::release_env::MODEL_BIN_OVERRIDE_ENV, &bin.display().to_string());
            r.session(&w).map_err(|a| format!("{a:?}"))
        });
        std::env::set_current_dir(cwd).unwrap();
        res.unwrap();
        assert_eq!(*exec.1.lock().unwrap(), ["started", "ended"]);
    }

    #[test_id(tpl1fti)]
    #[test]
    fn the_env_the_session_port_receives_carries_the_forge_fences_and_no_token() {
        let _otel = OTEL.lock().unwrap_or_else(|e| e.into_inner());
        let work = testkit::TempDir::new("dwarf-fences-spec");
        let w = work.path().to_path_buf();
        let cap = Capture(Default::default());
        let mut vars = super::super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        let cwd = std::env::current_dir().unwrap();
        let (res, gh) = with_run_launching(vars, &NoBd, Dirty(vec![]), &Lc, &|| 0, &Seams(Default::default()), &Mute::default(), &cap, "", |r| {
            r.s.logf = Some(w.join("log"));
            let bin = w.join("model-bin");
            std::fs::create_dir_all(&bin).unwrap();
            testkit::write_exe(bin.join("work"), "#!/bin/sh\n");
            r.d.env.set(spira_config::release_env::MODEL_BIN_OVERRIDE_ENV, &bin.display().to_string());
            r.d.env.set("GH_TOKEN", "secret");
            r.d.env.set("GITHUB_TOKEN", "secret");
            (r.session(&w).map_err(|a| format!("{a:?}")), r.run_dir().join("dwarf-empty-gh"))
        });
        std::env::set_current_dir(cwd).unwrap();
        res.unwrap();
        let envs = cap.0.lock().unwrap();
        let env = &envs[0];
        assert_eq!(env.get("GH_CONFIG_DIR").map(String::as_str), Some(gh.to_str().unwrap()));
        assert!(env.get("GIT_SSH_COMMAND").is_some_and(|v| v.contains("no SSH credentials")), "{env:?}");
        assert_eq!(env.get("GIT_ASKPASS").map(String::as_str), Some(system_paths::BIN_FALSE));
        assert_eq!(env.get("GIT_TERMINAL_PROMPT").map(String::as_str), Some("0"));
        assert!(!env.contains_key("GH_TOKEN") && !env.contains_key("GITHUB_TOKEN"));
    }

    const REFUSED: &str = "The work is finished and checked, but work submit refuses: spira-lc content: cannot tell\nmore";
    const T0: i64 = 1_700_000_000;

    #[test_id(tkuu7yj)]
    #[test]
    fn a_no_progress_end_on_a_refused_submit_says_so_in_the_inbox() {
        let lines = refused_submit("sp-a", T0, "", REFUSED);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("SUBMIT REFUSED sp-a: The work is finished and checked, but work submit refuses: spira-lc content: cannot tell"), "{lines:?}");
        assert!(!lines[0].contains("more"), "the first line only: {lines:?}");
    }

    #[test_id(tbdfyme)]
    #[test]
    fn a_no_progress_end_for_any_other_reason_says_nothing() {
        assert!(refused_submit("sp-a", T0, "", "I could not decide how to split this").is_empty());
    }

    #[test_id(tpdv1wr)]
    #[test]
    fn two_beads_refused_inside_ten_minutes_are_one_p0_page() {
        let first = format!("{} [dwarf] SUBMIT REFUSED sp-b: work submit refuses\n", crate::util::iso_utc(T0 - 300));
        let lines = refused_submit("sp-a", T0, &first, REFUSED);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains("P0 SUBMIT BLOCKED") && lines[1].contains("sp-a") && lines[1].contains("sp-b"), "{lines:?}");
        let paged = format!("{first}{} [dwarf] P0 SUBMIT BLOCKED: x\n", crate::util::iso_utc(T0 - 100));
        assert_eq!(refused_submit("sp-c", T0, &paged, REFUSED).len(), 1, "one page for the stall");
    }

    #[test_id(txjv53k)]
    #[test]
    fn the_same_bead_or_an_old_line_is_not_a_second_bead() {
        let same = format!("{} [dwarf] SUBMIT REFUSED sp-a: work submit refuses\n", crate::util::iso_utc(T0 - 300));
        assert_eq!(refused_submit("sp-a", T0, &same, REFUSED).len(), 1);
        let old = format!("{} [dwarf] SUBMIT REFUSED sp-b: work submit refuses\n", crate::util::iso_utc(T0 - 700));
        assert_eq!(refused_submit("sp-a", T0, &old, REFUSED).len(), 1);
    }

    struct CountingBd(Mutex<Vec<String>>);
    impl Bd for CountingBd {
        fn bd(&self, a: &[String]) -> Out {
            self.0.lock().unwrap().push(a.join(" "));
            Out::ok(r#"[{"id":"sp-x"}]"#)
        }
    }

    struct AskBd;
    impl Bd for AskBd {
        fn bd(&self, a: &[String]) -> Out {
            match a.first().map(String::as_str) {
                Some("show") => Out::ok(r#"[{"id":"sp-x","dependencies":[{"issue_id":"sp-x","depends_on_id":"sp-ask","type":"blocks"}]}]"#),
                Some("list") if a.join(" ") == "list --all --limit 0 --id sp-ask --json" => Out::ok(r#"[{"id":"sp-ask","status":"open","labels":["fixture-ask"],"title":"decide"}]"#),
                _ => Out::ok("[]"),
            }
        }
    }

    #[test_id(t9v2wkr)]
    #[test]
    fn the_teardown_reads_the_blocks_targets_in_one_list() {
        let seam = Seams(Default::default());
        let blocked = with_run(&AskBd, &seam, Dirty(vec![]), "", |r| {
            r.conf = Conf::new(&Snapshot { vars: { let mut v = super::super::complete_vars(); v.insert("SPIRA_ASK_LABEL".into(), "fixture-ask".into()); v }, ..Default::default() }, Path::new("h/spira"));
            r.ask_blocked("sp-x")
        });
        assert!(blocked);
    }

    struct Notes(Mutex<Vec<Vec<String>>>);
    impl Bd for Notes {
        fn bd(&self, a: &[String]) -> Out {
            self.0.lock().unwrap().push(a.to_vec());
            Out::ok("[]")
        }
    }

    #[test_id(tmhbe3v)]
    #[test]
    fn a_session_sent_back_after_submit_leaves_one_note_and_releases_and_charges_nothing() {
        let (seam, bd) = (Seams(Default::default()), Notes(Default::default()));
        let rc = with_run(&bd, &seam, Dirty(vec![]), "", |r| {
            r.stop.mark_sent_back("REWORK suites-failed");
            r.teardown(0)
        });
        assert_eq!(rc, 0);
        let notes: Vec<_> = bd.0.lock().unwrap().iter().filter(|a| a[0] == "note").cloned().collect();
        assert_eq!(notes.len(), 1);
        assert!(notes[0][2].starts_with("Session stopped: this bead was sent back (REWORK suites-failed) after `work submit`"), "{:?}", notes[0]);
        let calls: Vec<String> = seam.0.lock().unwrap().iter().map(|(f, _)| f.clone()).collect();
        for f in ["release_own_claim", "bump_requeue", "thrash_streak_bump", "bump_lapsed"] {
            assert!(!calls.iter().any(|c| c == f), "{f} must not run for a send-back: {calls:?}");
        }
    }

    struct Calls(Mutex<Vec<(String, Vec<String>, String)>>);
    impl Exec for Calls {
        fn exec(&self, prog: &str, args: &[String], stdin: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            self.0.lock().unwrap().push((prog.into(), args.to_vec(), String::from_utf8_lossy(&stdin.unwrap_or_default()).into_owned()));
            if prog == "spira-lc" && args.first().map(String::as_str) == Some("show") {
                return Out::ok(r#"{"bead":{"bead_id":"sp-a","state":"WORKING"}}"#);
            }
            Out::ok("")
        }
    }

    struct FenceRed {
        lc: Mutex<Vec<Vec<String>>>,
        fact_fails: bool,
        fence_rc: i32,
        fence_out: String,
    }
    impl Exec for FenceRed {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            if prog == "spira-lc" {
                if args[0] == "show" {
                    return Out::ok(format!(r#"{{"bead":{{"bead_id":"{}","state":"SUBMITTED"}}}}"#, args[1]));
                }
                self.lc.lock().unwrap().push(args.to_vec());
                if args[0] == "fact" && args[1] == "add" && self.fact_fails {
                    return Out { code: 1, stdout: String::new(), stderr: "store locked\nsecond line".into() };
                }
                return Out::ok("");
            }
            if args.iter().any(|a| a == "spira/build-fence.sh") {
                return Out { code: self.fence_rc, stdout: self.fence_out.clone(), stderr: String::new() };
            }
            Out::ok("")
        }
    }
    struct CleanGit;
    impl Git for CleanGit {
        fn git(&self, _: &Path, args: &[&str]) -> Out {
            match args[0] {
                "log" => Out::ok("sp-a — the work\n"),
                "rev-parse" => Out::ok("abc123\n"),
                _ => Out::ok(""),
            }
        }
    }

    /// The reopen the teardown of a submitted session makes when its fast tier fails with a long output.
    fn fast_tier_red_reopen(fact_fails: bool, fence_rc: i32) -> (String, Vec<Vec<String>>, std::path::PathBuf, String, testkit::TempDir) {
        let dir = testkit::TempDir::new("dwarf-fast-tier-keep");
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("spira")).unwrap();
        std::fs::write(work.join("spira/build-fence.sh"), "").unwrap();
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_RUN".into(), dir.path().join("run").display().to_string());
        vars.insert("SPIRA_SENDBACK_KEEP_DAYS".into(), "11".into());
        let fence_out: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        let (seam, exec) = (Seams(Default::default()), FenceRed { lc: Default::default(), fact_fails, fence_rc, fence_out: fence_out.clone() });
        let notes = Notes(Default::default());
        {
            // The teardown moves the process into `work`, which this test then deletes.
            let _cwd_owner = OTEL.lock().unwrap_or_else(|e| e.into_inner());
            let cwd = std::env::current_dir().unwrap();
            with_run_launching(vars, &notes, CleanGit, &exec, &|| 0, &seam, &Mute::default(), &NoLaunch, "", |r| {
                r.s.bead = "sp-a".into();
                r.s.work = Some(work.clone());
                r.s.repo = work.clone();
                r.hb_done.store(true, std::sync::atomic::Ordering::SeqCst);
                r.teardown(0);
            });
            std::env::set_current_dir(cwd).unwrap();
        }
        let note = notes.0.lock().unwrap().iter().filter(|a| a[0] == "note").map(|a| a[2].clone()).collect::<Vec<_>>().join("\n");
        let reopen = seam.0.lock().unwrap().iter().find(|(f, _)| f == "bead_reopen").map(|(_, a)| a.join("\n")).unwrap_or_default() + &note;
        let lc = exec.lc.lock().unwrap().clone();
        let text = format!("spira/build-fence.sh failed (rc=1):\n{fence_out}");
        let text = if fence_rc == 1 { text } else { format!("spira/build-fence.sh killed by SIGKILL (rc={fence_rc}) before it judged the branch:\n{fence_out}") };
        (reopen, lc, dir.path().join("run/sendback-logs"), text, dir)
    }

    #[test_id(tr6e7iy)]
    #[test]
    fn a_fast_tier_red_keeps_the_whole_output_references_it_and_names_it_in_the_reopen_note() {
        let (reopen, lc, logs, text, _dir) = fast_tier_red_reopen(false, 1);
        let entry = std::fs::read_dir(&logs).unwrap().next().unwrap().unwrap().file_name().into_string().unwrap();
        assert_eq!(std::fs::read(logs.join(&entry).join("fast-tier.out")).unwrap(), text.as_bytes());
        let want: Vec<String> = ["fact", "add", "sp-a", "--kind", "send-back-logs", "--actor", "fast-tier", "--cause", &entry].map(String::from).to_vec();
        assert!(lc.contains(&want), "{lc:?}");
        assert!(reopen.contains(&format!("work logs {entry} fast-tier.out")), "{reopen}");
    }

    #[test_id(ttqshpm)]
    #[test]
    fn a_fast_tier_red_whose_output_cannot_be_referenced_still_reopens_and_says_so() {
        let (reopen, _, _, _, _dir) = fast_tier_red_reopen(true, 1);
        assert!(reopen.contains("fast-tier-red"), "{reopen}");
        assert!(reopen.contains("full output not kept: "), "{reopen}");
    }

    #[test_id(t0gq9ur)]
    #[test]
    fn a_fast_tier_step_killed_before_it_judged_is_kept_and_named_in_the_certification_note() {
        let (notes, _, logs, text, _dir) = fast_tier_red_reopen(false, 137);
        let entry = std::fs::read_dir(&logs).unwrap().next().unwrap().unwrap().file_name().into_string().unwrap();
        assert_eq!(std::fs::read(logs.join(&entry).join("fast-tier.out")).unwrap(), text.as_bytes());
        assert!(notes.contains("certification judges it") && notes.contains(&format!("work logs {entry} fast-tier.out")), "{notes}");
    }

    const RED_WHY: &str = "pilgrim world did not come up (rc 1): sim world up failed: bd init (server) failed: connection refused";

    fn red_start_after(prior: usize) -> (Vec<String>, Vec<Vec<String>>, String) {
        let dir = testkit::TempDir::new("dwarf-harness-red");
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_RUN".into(), dir.path().display().to_string());
        vars.insert("SPIRA_RAPID_RECUR_THRESHOLD".into(), "3".into());
        let ledger = dir.path().join("dwarf-ledger.log");
        let line = "2026-11-18T09:38:46Z done pilgrim sp-a rc=1 status=harness-red wall_s=342 api_s=? turns=? in_tok=? cache_read_tok=? out_tok=? think_tok=? cost_usd=?\n";
        std::fs::write(&ledger, line.repeat(prior)).unwrap();
        let (seam, exec) = (Seams(Default::default()), Calls(Default::default()));
        let notes = Notes(Default::default());
        with_run_over(vars, &notes, Dirty(vec![]), &exec, &|| 0, &seam, "", |r| {
            r.ledger = Ledger { path: ledger.clone(), dry: false };
            r.s.bead = "sp-a".into();
            r.s.harness_red = Some(RED_WHY.into());
            r.teardown(0);
        });
        let calls = exec.0.lock().unwrap();
        let mails = calls.iter().filter(|(p, a, _)| p == "env" && a.contains(&"mail".to_string())).map(|(_, a, body)| format!("{} || {body}", a.join(" "))).collect();
        let holds = calls.iter().filter(|(p, a, _)| p == "spira-lc" && a.first().map(String::as_str) == Some("hold")).map(|(_, a, _)| a.clone()).collect();
        let notes = notes.0.lock().unwrap().iter().filter(|a| a[0] == "note").map(|a| a[2].clone()).collect::<Vec<_>>().join("\n");
        (mails, holds, notes)
    }

    #[test_id(ton58d9)]
    #[test]
    fn a_third_harness_red_start_in_a_row_asks_the_concierge_once_and_holds_the_bead() {
        let (mails, holds, note) = red_start_after(2);
        assert_eq!(mails.len(), 1, "{mails:?}");
        assert!(mails[0].contains("--kind question") && mails[0].contains("--bead sp-a") && mails[0].contains("concierge"), "{mails:?}");
        let subject = mails[0].split("--subject ").nth(1).unwrap();
        assert!(subject.split(" --kind").next().unwrap().contains("bd init (server) failed"), "{mails:?}");
        assert_eq!(holds.len(), 1, "{holds:?}");
        assert_eq!(&holds[0][..3], ["hold", "sp-a", "manual"]);
        assert!(holds[0][3].contains("harness-red streak 3") && holds[0][3].contains("bd init (server) failed"), "{holds:?}");
        assert!(mails[0].contains("--default fix the harness fault named above, then lift the hold"), "{mails:?}");
        let body = mails[0].split(" || ").nth(1).unwrap();
        assert!(body.contains(RED_WHY) && body.contains("3 times in a row"), "{body}");
        assert!(note.contains("held"), "{note}");
    }

    #[test_id(t37vt77)]
    #[test]
    fn a_second_harness_red_start_only_releases() {
        let (mails, holds, note) = red_start_after(1);
        assert!(mails.is_empty() && holds.is_empty(), "{mails:?} {holds:?}");
        assert!(note.contains("this repeats until the harness config is fixed"), "{note}");
    }

    struct Row(&'static str);
    impl Exec for Row {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            if prog == "spira-lc" && args.first().map(String::as_str) == Some("show") {
                return Out::ok(self.0);
            }
            Out::ok("")
        }
    }

    fn at_session_boundary(row: &'static str) -> (bool, Vec<String>) {
        let (seam, sink) = (Seams(Default::default()), Mute::default());
        let proceeds = with_run_sink(super::complete_vars(), &NoBd, Dirty(vec![]), &Row(row), &|| 0, &seam, &sink, "", |r| {
            r.s.holder = "fjalar-1".into();
            r.confirm_claim_held().is_ok()
        });
        let lines = sink.0.lock().unwrap().clone();
        (proceeds, lines)
    }

    #[test_id(tkxr5z0)]
    #[test]
    fn a_run_whose_row_was_released_does_not_start_its_session() {
        let (proceeds, lines) = at_session_boundary(r#"{"bead":{"bead_id":"sp-x","state":"READY","holder":null}}"#);
        assert!(!proceeds);
        assert!(lines.iter().any(|l| l.contains("sp-x is no longer held by fjalar-1 (row reads state=READY holder=null) — not starting a session")), "{lines:?}");
    }

    #[test_id(tjkv03c)]
    #[test]
    fn a_run_whose_row_names_another_holder_does_not_start_its_session() {
        let (proceeds, _) = at_session_boundary(r#"{"bead":{"bead_id":"sp-x","state":"WORKING","holder":"other-2"}}"#);
        assert!(!proceeds);
    }

    #[test_id(tm3o74g)]
    #[test]
    fn a_run_still_holding_its_row_starts_its_session() {
        let (proceeds, lines) = at_session_boundary(r#"{"bead":{"bead_id":"sp-x","state":"WORKING","holder":"fjalar-1"}}"#);
        assert!(proceeds);
        assert!(!lines.iter().any(|l| l.contains("no longer held")), "{lines:?}");
    }

    #[test_id(tcz8jvk)]
    #[test]
    fn a_row_the_store_cannot_read_does_not_stop_a_session() {
        let (proceeds, _) = at_session_boundary("");
        assert!(proceeds, "an unreadable row is not evidence the claim was lost");
    }

    #[test_id(txrr5le)]
    #[test]
    fn the_claimed_bead_is_read_once_however_many_steps_ask_for_it() {
        let (seam, bd) = (Seams(Default::default()), CountingBd(Default::default()));
        with_run(&bd, &seam, Dirty(vec![]), r#"[{"id":"sp-x"}]"#, |r| {
            for _ in 0..3 {
                r.held_raw();
            }
        });
        assert!(bd.0.lock().unwrap().is_empty(), "a held claim-time row is never re-read");
        with_run(&bd, &seam, Dirty(vec![]), "", |r| {
            for _ in 0..3 {
                assert!(r.held_raw().contains("sp-x"));
            }
        });
        assert_eq!(*bd.0.lock().unwrap(), vec!["show sp-x --json".to_string()], "a directed call with no held row reads once and keeps it");
    }


    /// `Run::decline()` in a world whose run directory is `run` and whose registry maps `repos`.
    fn decline(run: &Path, repos: &[&str]) -> (Option<i32>, Vec<String>) {
        let map = spira_config::process::fixture_repo_map(run, repos);
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        vars.insert("SPIRA_ALLOW_PROD_DIRTY".into(), String::new());
        vars.insert("SPIRA_RUN".into(), run.display().to_string());
        vars.insert("SPIRA_REPO_MAP".into(), map.display().to_string());
        vars.insert("SPIRA_HOME_REPO".into(), repos[0].to_string());
        let sink = Mute::default();
        let rc = with_run_sink(vars, &NoBd, Dirty(vec![]), &Lc, &|| 0, &Seams(Default::default()), &sink, "", |r| r.decline());
        let lines = sink.0.lock().unwrap().clone();
        (rc, lines)
    }

    /// `rune_free` always answers 0: any persona asking it for room is refused.
    struct Live(i64);
    impl Seam for Live {
        fn call(&self, f: &str, _: &[String]) -> Out {
            Out::ok(match f {
                "dwarf_count" => self.0.to_string(),
                "rune_free" => "0".into(),
                _ => String::new(),
            })
        }
    }

    /// `decline()` or `sweep()` for `persona` with `live` of its dwarves running; the log lines it wrote.
    fn at_live(persona: &str, rune_vars: &[(&str, &str)], live: i64, sweep: bool) -> (Option<i32>, Vec<String>) {
        let d = testkit::TempDir::new("dwarf-single-flight");
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        vars.insert("SPIRA_RUN".into(), d.path().display().to_string());
        let rv: BTreeMap<String, String> = rune_vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let sink = Mute::default();
        let rc = with_run_sink(vars, &NoBd, Dirty(vec![]), &Lc, &|| 0, &Live(live), &sink, "", |r| {
            r.rune = Rune::from_vars(persona, &rv);
            if sweep { Some(r.sweep(None)) } else { r.decline() }
        });
        let lines = sink.0.lock().unwrap().clone();
        (rc, lines)
    }

    #[test_id(tnzkoi3)]
    #[test]
    fn a_persona_without_single_flight_does_not_decline_for_capacity() {
        let (rc, lines) = at_live("mender", &[], 9, false);
        assert!(!lines.iter().any(|l| l.contains("at capacity")), "{lines:?} {rc:?}");
    }

    #[test_id(te12u6o)]
    #[test]
    fn a_single_flight_persona_with_one_live_declines_at_capacity() {
        let (rc, lines) = at_live("warden", &[("RUNE_SINGLE_FLIGHT", "1")], 1, false);
        assert_eq!(rc, Some(0));
        assert!(lines.iter().any(|l| l.contains("at capacity (1/1)")), "{lines:?}");
        let (_, lines) = at_live("warden", &[("RUNE_SINGLE_FLIGHT", "1")], 0, false);
        assert!(!lines.iter().any(|l| l.contains("at capacity")), "{lines:?}");
    }

    #[test_id(tizj5hw)]
    #[test]
    fn a_second_sweep_of_one_persona_declines_while_one_is_live() {
        let (rc, lines) = at_live("ops", &[], 1, true);
        assert_eq!(rc, Some(0));
        assert!(lines.iter().any(|l| l.contains("at capacity (1/1), not sweeping")), "{lines:?}");
    }

    fn stop_lane(run: &Path, repo: &str, lane: &str) {
        spira_config::scoped_stop::pause_repo(run, repo, &[lane.to_string()], "cutover-mode", "cutover", "world stop", 1).unwrap();
    }

    #[test_id(t5b1c9d)]
    #[test]
    fn a_slain_session_is_held_by_the_halt_stamp_and_not_by_a_dwarves_lane_stop() {
        let d = testkit::TempDir::new("dwarf-checkpoint-stamp");
        stop_lane(d.path(), "fixture", "dwarves");
        let held = || {
            let mut vars = super::super::complete_vars();
            vars.insert("SPIRA_REPO".into(), "shared".into());
            vars.insert("SPIRA_RUN".into(), d.path().display().to_string());
            with_run_over(vars, &NoBd, Dirty(vec![]), &Lc, &|| 0, &Seams(Default::default()), "", |r| {
                r.s.session_id = "sess".into();
                r.checkpoint_across_stop()
            })
        };
        assert!(!held(), "a dwarves-lane stop with no stamp leaves the plain release standing");
        std::fs::write(d.path().join("world.halted"), "").unwrap();
        assert!(held(), "the halt stamp holds a slain session");
    }

    #[test_id(tao9mrp)]
    #[test]
    fn a_dwarf_declines_only_when_the_dwarves_lane_is_stopped_on_every_registered_repo() {
        let d = testkit::TempDir::new("dwarf-dwarves-lane");
        let repos = ["fixture", "other"];
        assert_eq!(decline(d.path(), &repos).0, None, "nothing stopped: the dwarf goes on");
        stop_lane(d.path(), "fixture", "rounds");
        stop_lane(d.path(), "other", "rounds");
        assert_eq!(decline(d.path(), &repos).0, None, "another lane's stop is no decline");
        stop_lane(d.path(), "fixture", "dwarves");
        assert_eq!(decline(d.path(), &repos).0, None, "one repo still takes dwarves");
        stop_lane(d.path(), "other", "dwarves");
        let (rc, lines) = decline(d.path(), &repos);
        assert_eq!(rc, Some(0));
        assert!(lines.iter().any(|l| l.contains("builder: skipped — the dwarves lane of fixture is stopped on purpose by cutover-mode") && l.contains("world stop")), "{lines:?}");
    }

    // covers: UC-dwarf-execution-16
    #[test_id(tmhpotq)]
    #[test]
    fn own_dirty_hand_on_is_refused_and_requeued_with_the_note() {
        let (reopens, cause) = run(vec!["w/own"]);
        assert_eq!(reopens.len(), 1);
        assert_eq!(reopens[0][1], crate::verdict::PROD_DIRTY);
        assert!(reopens[0][2].contains("Modified paths:\n   M f") && reopens[0][2].contains("SPIRA_ALLOW_PROD_DIRTY=1"), "{:?}", reopens[0]);
        assert_eq!(cause.as_deref(), Some(crate::verdict::PROD_DIRTY));
    }

    // covers: UC-dwarf-execution-16
    #[test_id(tax0hgt)]
    #[test]
    fn dirty_shared_checkout_lets_the_hand_on_stand() {
        let (reopens, cause) = run(vec!["shared"]);
        assert!(reopens.is_empty(), "sp-nqtrg: {reopens:?}");
        assert_eq!(cause, None);
    }

    struct BlockedLc(&'static str);
    impl Exec for BlockedLc {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            if prog != "spira-lc" || args.first().map(String::as_str) != Some("show") {
                return Out::ok("");
            }
            match args[1].as_str() {
                "sp-x" => Out::ok(format!(r#"{{"bead":{{"bead_id":"sp-x","state":"WORKING","claim":{{"blocks":["{}"]}}}}}}"#, self.0)),
                "sp-dep" => Out::ok(r#"{"bead":{"bead_id":"sp-dep","state":"LANDED","updated_at":1795123820}}"#),
                _ => Out { code: 2, stdout: String::new(), stderr: "down".into() },
            }
        }
    }
    struct OnBase;
    impl Git for OnBase {
        fn git(&self, _: &Path, args: &[&str]) -> Out {
            match args[0] {
                "log" if args.contains(&"sp-dep") => Out::ok("8ce75d2da5\n"),
                "rev-parse" => Out::ok("head\n"),
                _ => Out { code: 1, stdout: String::new(), stderr: String::new() },
            }
        }
    }

    fn task_brief_over(exec: &dyn Exec) -> String {
        let dir = testkit::TempDir::new("dwarf-deps-brief");
        std::fs::create_dir_all(dir.path().join("runes")).unwrap();
        std::fs::write(dir.path().join("runes/builder.md"), "You are a builder.").unwrap();
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_RUN".into(), dir.path().display().to_string());
        let raw = r#"{"id":"sp-x","title":"t","description":"d","notes":"waits on sp-dep"}"#;
        let (seam, sink) = (Seams(Default::default()), Mute::default());
        let original = vars.clone();
        let conf = Conf::new(&Snapshot { vars, ..Default::default() }, dir.path());
        let rune = Rune::from_vars("builder", &Default::default());
        let env = Env::new(original, Default::default());
        let git = OnBase;
        let mut r = Run {
            d: Deps { bd: &NoBd, seam: &seam, git: &git, exec, launcher: &NoLaunch, sink: &sink, env: &env, clock: &|| 0, sleep: &|_| {} },
            ledger: Ledger { path: PathBuf::from("nonexistent/ledger"), dry: true },
            conf,
            rune,
            snap: Snapshot::default(),
            mode: Mode::DryRun,
            pid: 1,
            own_unit: String::new(),
            t0: 0,
            claim_bin: String::new(),
            stop: Arc::new(Stop::default()),
            hb_shutdown: Arc::new(AtomicBool::new(false)),
            hb_done: Arc::new(AtomicBool::new(false)),
            rune_file: PathBuf::new(),
            s: State { bead: "sp-x".into(), branch: "b".into(), repo: PathBuf::from("repo"), claimed_raw: raw.into(), base_fq: "local/main".into(), ..Default::default() },
        };
        assert!(r.write_static_prompt(Path::new("w"), "", "", "").is_ok(), "the prompt is written");
        std::fs::read_to_string(dir.path().join("sp-x.task.md")).unwrap()
    }

    #[test_id(txcfrj4)]
    #[test]
    fn a_claimed_beads_task_brief_opens_with_the_state_of_its_landed_blocker_before_the_notes() {
        let task = task_brief_over(&BlockedLc("sp-dep"));
        let line = task.find("sp-dep LANDED since 2026-11-19T21:30:20Z, 8ce75d2da5 on the landing ref").unwrap_or_else(|| panic!("{task}"));
        assert!(line < task.find("waits on sp-dep").expect("the notes are in the brief"), "{task}");
    }

    #[test_id(tmprw7d)]
    #[test]
    fn a_blocker_the_store_cannot_read_is_named_in_the_task_brief_not_dropped() {
        let task = task_brief_over(&BlockedLc("sp-gone"));
        assert!(task.contains("- sp-gone state cannot tell (rc=2: down)"), "{task}");
    }

    static OTEL: Mutex<()> = Mutex::new(());

    struct NoCommits;
    impl Git for NoCommits {
        fn git(&self, _: &Path, _: &[&str]) -> Out {
            Out { code: 1, stdout: String::new(), stderr: String::new() }
        }
    }

    struct SubmittedAt(Option<i64>);
    impl Exec for SubmittedAt {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            if prog == "spira-lc" && args.first().map(String::as_str) == Some("show") {
                return match self.0 {
                    Some(at) => Out::ok(format!(r#"{{"bead":{{"bead_id":"{}","state":"SUBMITTED","since":{at}}}}}"#, args[1])),
                    None => Out::ok(format!(r#"{{"bead":{{"bead_id":"{}","state":"WORKING","since":1}}}}"#, args[1])),
                };
            }
            Out::ok("")
        }
    }

    struct ExitsAt<'a>(&'a std::sync::atomic::AtomicI64, i64);
    impl Launcher for ExitsAt<'_> {
        fn run(&self, _: &SessionSpec, _: &Stop) -> i32 {
            self.0.store(self.1, std::sync::atomic::Ordering::SeqCst);
            0
        }
    }

    /// A loopback sink answering pushes until the run's closing push (`finish`'s run
    /// count) or a minute; yields each request's raw bytes. Other tests' events may land here.
    fn sink_until_finish() -> (String, std::thread::JoinHandle<Vec<Vec<u8>>>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        l.set_nonblocking(true).unwrap();
        let h = std::thread::spawn(move || {
            let give_up = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let mut pushes: Vec<Vec<u8>> = Vec::new();
            while std::time::Instant::now() < give_up && !pushes.last().is_some_and(|p| has(p, b"spira_component_runs")) { // clock-safe: caps the wait for the run's closing push
                let Ok((mut c, _)) = l.accept() else {
                    std::thread::yield_now();
                    continue;
                };
                c.set_nonblocking(false).unwrap();
                let (mut buf, mut chunk) = (Vec::new(), [0u8; 4096]);
                loop {
                    let got = c.read(&mut chunk).unwrap();
                    buf.extend_from_slice(&chunk[..got]);
                    let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    let len: usize = head.split("content-length: ").nth(1).unwrap().split("\r\n").next().unwrap().parse().unwrap();
                    if buf.len() >= i + 4 + len {
                        break;
                    }
                }
                c.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
                pushes.push(buf);
            }
            pushes
        });
        (addr, h)
    }

    fn has(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// The pushes a run makes when its session exits at `T0 + 300` and its
    /// teardown ends at `T0 + 354`, the lifecycle row saying SUBMITTED since `submitted_at`.
    fn pushed_after_submit(submitted_at: Option<i64>) -> Vec<Vec<u8>> {
        use std::sync::atomic::{AtomicI64, Ordering};
        let _g = OTEL.lock().unwrap_or_else(|e| e.into_inner());
        let work = testkit::TempDir::new("dwarf-after-submit");
        let w = work.path().to_path_buf();
        let now = AtomicI64::new(T0);
        let launcher = ExitsAt(&now, T0 + 300);
        let (addr, sink) = sink_until_finish();
        spira_otel::run::begin("dwarf", Some(&addr));
        let mut vars = super::complete_vars();
        vars.insert("SPIRA_REPO".into(), "shared".into());
        let cwd = std::env::current_dir().unwrap();
        let clock = || now.load(Ordering::SeqCst);
        with_run_launching(vars, &NoBd, NoCommits, &SubmittedAt(submitted_at), &clock, &Seams(Default::default()), &Mute::default(), &launcher, "", |r| {
            r.s.logf = Some(w.join("log"));
            let bin = w.join("model-bin");
            std::fs::create_dir_all(&bin).unwrap();
            testkit::write_exe(bin.join("work"), "#!/bin/sh\n");
            r.d.env.set(spira_config::release_env::MODEL_BIN_OVERRIDE_ENV, &bin.display().to_string());
            r.session(&w).map_err(|a| format!("{a:?}")).unwrap();
            now.store(T0 + 354, Ordering::SeqCst);
            r.teardown(0);
        });
        std::env::set_current_dir(cwd).unwrap();
        spira_otel::run::finish(0);
        sink.join().unwrap()
    }

    #[test_id(tjylxaq)]
    #[test]
    fn a_submitted_run_pushes_how_long_its_session_and_its_teardown_held_the_slot_after_submit() {
        let pushes = pushed_after_submit(Some(T0));
        for (part, secs) in [("session", 300.0f64), ("teardown", 54.0)] {
            let one = pushes.iter().find(|p| has(p, part.as_bytes())).unwrap_or_else(|| panic!("no push for {part}: {:?}", pushes.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect::<Vec<_>>()));
            for want in ["dwarf_after_submit_seconds", "builder", "submitted"] {
                assert!(has(one, want.as_bytes()), "{want} missing from the {part} push: {}", String::from_utf8_lossy(one));
            }
            assert!(has(one, &secs.to_le_bytes()), "{part} should be {secs}");
        }
    }

    #[test_id(tqy141m)]
    #[test]
    fn a_run_that_never_submitted_pushes_no_after_submit_seconds() {
        let pushes = pushed_after_submit(None);
        assert!(pushes.iter().any(|p| has(p, b"spira_component_runs")), "positive control: the run's own closing push arrived");
        assert!(!pushes.iter().any(|p| has(p, b"dwarf_after_submit_seconds")));
    }
}
