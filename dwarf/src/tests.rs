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
            concat!(env!("CARGO_MANIFEST_DIR"), "/../spira/testlib/complete.toml").to_string(),
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
    struct Mute;
    impl Sink for Mute {
        fn out(&self, _: &str) {}
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
        vars.insert("SPIRA_REPO".into(), "/shared".into());
        vars.insert("SPIRA_ALLOW_PROD_DIRTY".into(), String::new());
        with_run_over(vars, bd, git, &Lc, &|| 0, seam, claimed_raw, f)
    }

    #[allow(clippy::too_many_arguments)]
    fn with_run_over<T>(vars: BTreeMap<String, String>, bd: &dyn Bd, git: Dirty, exec: &dyn Exec, clock: &(dyn Fn() -> i64 + Sync), seam: &Seams, claimed_raw: &str, f: impl FnOnce(&mut Run) -> T) -> T {
        let conf = Conf::new(&Snapshot { vars, ..Default::default() }, Path::new("/home/spira"));
        let rune = Rune::from_vars("builder", &Default::default());
        let env = Env::new(Default::default(), Default::default());
        let stop = Arc::new(Stop::default());
        let mut r = Run {
            d: Deps { bd, seam, git: &git, exec, launcher: &NoLaunch, sink: &Mute, env: &env, clock, sleep: &|_| {} },
            ledger: Ledger { path: PathBuf::from("/nonexistent/ledger"), dry: true },
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
            s: State { bead: "sp-x".into(), branch: "b".into(), repo: PathBuf::from("/repo"), work: Some(PathBuf::from("/w/own")), lc_model_restricted: true, claimed_raw: claimed_raw.into(), ..Default::default() },
        };
        f(&mut r)
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

    const REFUSED: &str = "The work is finished and checked, but work submit refuses: spira-lc content: cannot tell\nmore";
    const T0: i64 = 1_700_000_000;

    #[test]
    fn a_no_progress_end_on_a_refused_submit_says_so_in_the_inbox() {
        let lines = refused_submit("sp-a", T0, "", REFUSED);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("SUBMIT REFUSED sp-a: The work is finished and checked, but work submit refuses: spira-lc content: cannot tell"), "{lines:?}");
        assert!(!lines[0].contains("more"), "the first line only: {lines:?}");
    }

    #[test]
    fn a_no_progress_end_for_any_other_reason_says_nothing() {
        assert!(refused_submit("sp-a", T0, "", "I could not decide how to split this").is_empty());
    }

    #[test]
    fn two_beads_refused_inside_ten_minutes_are_one_p0_page() {
        let first = format!("{} [dwarf] SUBMIT REFUSED sp-b: work submit refuses\n", crate::util::iso_utc(T0 - 300));
        let lines = refused_submit("sp-a", T0, &first, REFUSED);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains("P0 SUBMIT BLOCKED") && lines[1].contains("sp-a") && lines[1].contains("sp-b"), "{lines:?}");
        let paged = format!("{first}{} [dwarf] P0 SUBMIT BLOCKED: x\n", crate::util::iso_utc(T0 - 100));
        assert_eq!(refused_submit("sp-c", T0, &paged, REFUSED).len(), 1, "one page for the stall");
    }

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

    #[test]
    fn the_teardown_reads_the_blocks_targets_in_one_list() {
        let seam = Seams(Default::default());
        let blocked = with_run(&AskBd, &seam, Dirty(vec![]), "", |r| {
            r.conf = Conf::new(&Snapshot { vars: { let mut v = super::complete_vars(); v.insert("SPIRA_ASK_LABEL".into(), "fixture-ask".into()); v }, ..Default::default() }, Path::new("/home/spira"));
            r.ask_blocked("sp-x")
        });
        assert!(blocked);
    }

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


    // covers: UC-dwarf-execution-16
    #[test]
    fn own_dirty_hand_on_is_refused_and_requeued_with_the_note() {
        let (reopens, cause) = run(vec!["/w/own"]);
        assert_eq!(reopens.len(), 1);
        assert_eq!(reopens[0][1], crate::verdict::PROD_DIRTY);
        assert!(reopens[0][2].contains("Modified paths:\n   M f") && reopens[0][2].contains("SPIRA_ALLOW_PROD_DIRTY=1"), "{:?}", reopens[0]);
        assert_eq!(cause.as_deref(), Some(crate::verdict::PROD_DIRTY));
    }

    // covers: UC-dwarf-execution-16
    #[test]
    fn dirty_shared_checkout_lets_the_hand_on_stand() {
        let (reopens, cause) = run(vec!["/shared"]);
        assert!(reopens.is_empty(), "sp-nqtrg: {reopens:?}");
        assert_eq!(cause, None);
    }
}
