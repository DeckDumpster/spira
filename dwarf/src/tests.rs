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
        let conf = Conf::new(&Snapshot { vars, ..Default::default() }, Path::new("/home/spira"));
        let rune = Rune::from_vars("builder", &Default::default());
        let env = Env::new(Default::default(), Default::default());
        let stop = Arc::new(Stop::default());
        let clock = || 0i64;
        let mut r = Run {
            d: Deps { bd, seam, git: &git, exec: &Lc, launcher: &NoLaunch, sink: &Mute, env: &env, clock: &clock, sleep: &|_| {} },
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

    struct CountingBd(Mutex<Vec<String>>);
    impl Bd for CountingBd {
        fn bd(&self, a: &[String]) -> Out {
            self.0.lock().unwrap().push(a.join(" "));
            Out::ok(r#"[{"id":"sp-x"}]"#)
        }
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
