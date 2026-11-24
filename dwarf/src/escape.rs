//! escape — direct-call a dwarf for a named rune, bypassing pool and lane checks
//! (replaces `spira/escape.sh`, sp-zpaq0). `dwarf --escape <rune> [--dry-run]`.
//!
//! WHEN TO USE. The sentinel decides how many dwarves may start by comparing live dwarf
//! counts against pool and lane budgets. A bug in that arithmetic — wrong pool
//! calculation, a lane accidentally counted against the pool — can prevent the bead that
//! REPAIRS the bug from being claimed: the scheduler cannot schedule its own fix. This is
//! the path around that. It skips the pool and lane capacity checks entirely and invokes
//! the dwarf directly.
//!
//! THE CONTROL PLANE MUST NOT DEPEND ON THE DATA PLANE IT CONTROLS. Normal scheduling is
//! the data plane; a broken scheduler is exactly when this — the control plane — must be
//! reachable. Ryan's own case: eight P0s skipped because the bug fixing the ordering
//! logic was itself subject to the ordering it would fix.
//!
//! WHAT THIS DOES NOT BYPASS. The rune must exist (checked by the caller, the same
//! `home/runes/<rune>.rune` test every mode uses). Its partition must have ready
//! work. The account must have API capacity. The bead must not be poisoned. The rune's
//! own RUNE_SINGLE_FLIGHT is enforced by the called dwarf internally. All of that
//! still applies — `world_gate` and `call_argv` are in-process here; `rune_ready` is
//! reached through lib.sh (§5's seam). The capacity check is in-process too (`capacity::check_and_probe`, wave 4.26 — family K's home is this
//! crate, and dwarf is its probe's one owner), so a probe that clears the pause file early
//! happens exactly once no matter how many of this crate's own entry points ask.
//!
//! WHEN NOT TO USE. Normal scheduling is observable, coordinated, and lower-cost. Reserve
//! this for a scheduler that is demonstrably failing to call a rune that has ready work.
//!
//! RETIRED RATHER THAN PORTED AS A SEPARATE SCRIPT. escape.sh's only job was to gate and
//! then invoke the `dwarf` binary; the binary now does both.

use std::path::{Path, PathBuf};

use crate::conf::Conf;
use crate::ports::{Env, Exec, Seam};
use crate::util::{self, Sink};

/// What escape decided to do, independent of how the facts were gathered — the part this
/// module's tests hold to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Gated (halted, draining, or the account is paused). Exit 1, nothing called.
    Gated,
    /// The partition's ready count could not be read. Exit 1 (`die`), nothing called.
    CannotReadPartition,
    /// Nothing ready. Exit 0, nothing called — the positive control that distinguishes
    /// "gated" from "correctly declined".
    NothingReady,
    /// `dwarf` is not on PATH. Exit 1 (`die`), nothing called.
    DwarfMissing,
    /// Called: the `systemd-run` argv that was (or would be) run.
    Called(Vec<String>),
}

/// Find `name` on a `:`-separated PATH, returning its full path — `command -v`.
pub fn find_on_path(path: &str, name: &str, exists: impl Fn(&Path) -> bool) -> Option<String> {
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let p = Path::new(dir).join(name);
        if exists(&p) {
            return Some(p.display().to_string());
        }
    }
    None
}

/// The whole decision, given the already-gathered facts (world_gate's rc, capacity_paused's
/// rc and its stdout seconds-left, rune_ready's rc and its stdout count, and whether `dwarf`
/// resolved on PATH). Pure — no IO, fully covered by unit tests below.
pub struct Facts {
    pub world_gate_ok: bool,
    pub capacity_left: Option<String>, // Some(seconds) when paused
    pub ready: Result<i64, ()>,        // Err on a rune_ready failure (rc != 0)
    pub dwarf_path: Option<String>,
}

pub fn decide(f: &Facts) -> Outcome {
    if !f.world_gate_ok {
        return Outcome::Gated;
    }
    if f.capacity_left.is_some() {
        return Outcome::Gated;
    }
    let ready = match f.ready {
        Ok(r) => r,
        Err(()) => return Outcome::CannotReadPartition,
    };
    if ready == 0 {
        return Outcome::NothingReady;
    }
    let Some(_) = f.dwarf_path.as_ref() else {
        return Outcome::DwarfMissing;
    };
    Outcome::Called(Vec::new()) // argv filled in by build_argv, kept apart from decide()
}

/// `systemd-run --user --collect --quiet --unit=<stem> <call argv...> <dwarf-path> --home <home>
/// <rune> <tail...>`: the one unit call every direct entry point (`--escape`, `direct`) starts a
/// dwarf by.
pub fn unit_call_argv(stem: &str, home: &Path, dwarf_path: &str, call_argv: &[String], rune: &str, tail: &[String]) -> Vec<String> {
    let mut a = vec!["--user".to_string(), "--collect".to_string(), "--quiet".to_string(), format!("--unit={stem}")];
    a.extend(call_argv.iter().cloned());
    a.extend([dwarf_path.to_string(), "--home".to_string(), home.display().to_string(), rune.to_string()]);
    a.extend(tail.iter().cloned());
    a
}

/// The `--escape` call: unit `spira-dwarf-<rune>-escape-<epoch>`, `[--dry-run]` after the rune.
pub fn build_argv(rune: &str, home: &Path, dwarf_path: &str, call_argv: &[String], epoch: i64, dry_run: bool) -> Vec<String> {
    let tail: Vec<String> = dry_run.then(|| "--dry-run".to_string()).into_iter().collect();
    unit_call_argv(&format!("spira-dwarf-{rune}-escape-{epoch}"), home, dwarf_path, call_argv, rune, &tail)
}

/// Why a call is refused by the world's own state, `None` when calls are permitted. Halted and
/// draining hold until the operator lifts them — no clock does — and a stopped `claims` lane
/// on every repo refuses too.
pub fn world_gate<S: AsRef<str>>(run: &Path, repos: &[S], rune: &str, prefix: &str) -> Option<String> {
    if run.join("world.halted").is_file() {
        return Some(format!("{prefix} {rune}: halted — not calling (world.sh start to lift)"));
    }
    if run.join("world.draining").is_file() {
        return Some(format!("{prefix} {rune}: draining — not calling (world.sh resume to lift)"));
    }
    spira_config::scoped_stop::skip_if_stopped(run, repos, rune, "claims").map(|why| format!("{prefix} {why}"))
}

/// The `systemd-run` flags of a called dwarf's unit: its environment, then the properties of
/// `units::dwarf_props` from the rune's `RUNE_TIMEOUT_SECONDS`.
pub fn call_argv(home: &Path, rune: &str, output_append: Option<PathBuf>) -> Result<Vec<String>, String> {
    let timeout = spira_config::runes::rune_get(home, rune, "RUNE_TIMEOUT_SECONDS", "3600");
    let props = runtime::units::UnitProps { output_append, ..runtime::units::dwarf_props(&timeout)? };
    crate::direct::call_flags(&spira_config::release_env::dwarf_unit_env()?, &props)
}

/// The full orchestration: gathers the facts through the seam, decides, and (unless
/// merely deciding) spawns `systemd-run`. Returns the process exit code.
#[allow(clippy::too_many_arguments)]
pub fn run(seam: &dyn Seam, exec: &dyn Exec, env: &Env, sink: &dyn Sink, conf: &Conf, call_bin: &str, call_argv: &[String], home: &Path, rune: &str, dry_run: bool, now: i64) -> i32 {
    let gate = world_gate(&conf.run, &conf.repos.all(), rune, "escape.sh");
    if let Some(line) = &gate {
        sink.out(&util::log_line(now, line));
    }
    let cv = crate::capacity::check_and_probe(conf, exec, now);
    for l in &cv.log {
        sink.out(&util::log_line(now, l));
    }
    if cv.clear_file {
        let _ = std::fs::remove_file(conf.capacity_pause());
    }
    let capacity_left = match cv.state {
        crate::capacity::Paused::Open => None,
        crate::capacity::Paused::Paused(n) => Some(n.to_string()),
        crate::capacity::Paused::Unknown => Some("?".to_string()),
    };
    let fr = seam.call("_dwarf_rune_ready", &[]);
    let path = env.child().get("PATH").cloned().unwrap_or_default();
    let dwarf_path = find_on_path(&path, "dwarf", is_executable);

    let facts = Facts {
        world_gate_ok: gate.is_none(),
        capacity_left,
        ready: if fr.code == 0 { fr.stdout.trim().parse::<i64>().map_err(|_| ()) } else { Err(()) },
        dwarf_path: dwarf_path.clone(),
    };

    match decide(&facts) {
        Outcome::Gated => {
            if facts.world_gate_ok {
                // the world allowed it; the capacity pause is what gated it.
                sink.out(&util::log_line(now, &format!("escape.sh {rune}: account out of capacity for another {}s — not calling", facts.capacity_left.unwrap_or_default())));
            }
            1
        }
        Outcome::CannotReadPartition => {
            sink.err(&util::log_line(now, &format!("FATAL {rune}: cannot read its partition")));
            1
        }
        Outcome::NothingReady => {
            sink.out(&util::log_line(now, &format!("escape.sh {rune}: nothing ready in its partition — nothing to call")));
            0
        }
        Outcome::DwarfMissing => {
            sink.err(&util::log_line(now, &format!("FATAL escape.sh {rune}: dwarf is not on PATH")));
            1
        }
        Outcome::Called(_) => {
            let ready = facts.ready.unwrap_or(0);
            sink.out(&util::log_line(now, &format!("escape.sh {rune}: {ready} ready — calling directly (pool and lane checks bypassed)")));
            let argv = build_argv(rune, home, dwarf_path.as_deref().unwrap_or("dwarf"), call_argv, now, dry_run);
            let o = exec.exec(call_bin, &argv, None, None);
            o.code
        }
    }
}

pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Facts {
        Facts { world_gate_ok: true, capacity_left: None, ready: Ok(3), dwarf_path: Some("rel/bin/dwarf".into()) }
    }

    #[test_id(tt51epy)]
    #[test]
    fn halted_or_draining_gates_before_anything_else() {
        let f = Facts { world_gate_ok: false, ..ok() };
        assert_eq!(decide(&f), Outcome::Gated);
    }

    #[test_id(to4zjft)]
    #[test]
    fn capacity_paused_gates() {
        let f = Facts { capacity_left: Some("120".into()), ..ok() };
        assert_eq!(decide(&f), Outcome::Gated);
    }

    #[test_id(texcj93)]
    #[test]
    fn unreadable_partition_is_fatal_not_a_decline() {
        let f = Facts { ready: Err(()), ..ok() };
        assert_eq!(decide(&f), Outcome::CannotReadPartition);
    }

    #[test_id(t66cg77)]
    #[test]
    fn zero_ready_declines_cleanly_exit_0() {
        let f = Facts { ready: Ok(0), ..ok() };
        assert_eq!(decide(&f), Outcome::NothingReady);
    }

    #[test_id(tm38kbk)]
    #[test]
    fn positive_control_ready_and_ungated_calls() {
        assert!(matches!(decide(&ok()), Outcome::Called(_)));
    }

    #[test_id(tk2cj86)]
    #[test]
    fn dwarf_missing_from_path_is_fatal() {
        let f = Facts { dwarf_path: None, ..ok() };
        assert_eq!(decide(&f), Outcome::DwarfMissing);
    }

    #[test_id(tscq0ve)]
    #[test]
    fn find_on_path_returns_first_hit() {
        let hit = |p: &Path| p == Path::new("b/dwarf");
        assert_eq!(find_on_path("a:b:c", "dwarf", hit), Some("b/dwarf".to_string()));
    }

    #[test_id(t3u4b6z)]
    #[test]
    fn find_on_path_none_when_absent() {
        assert_eq!(find_on_path("a:b", "dwarf", |_| false), None);
    }

    #[test_id(tep3775)]
    #[test]
    fn build_argv_shape_matches_escape_sh() {
        let a = build_argv("stretchy", Path::new("h/spira"), "rel/bin/dwarf", &["--setenv=HOME=/h".to_string()], 1000, false);
        assert_eq!(
            a,
            vec!["--user", "--collect", "--quiet", "--unit=spira-dwarf-stretchy-escape-1000", "--setenv=HOME=/h", "rel/bin/dwarf", "--home", "h/spira", "stretchy"]
        );
    }

    #[test_id(t413f9g)]
    #[test]
    fn build_argv_appends_dry_run_last() {
        let a = build_argv("f", Path::new("h"), "bin/dwarf", &[], 1, true);
        assert_eq!(a.last().unwrap(), "--dry-run");
    }

    // ---- run(): the seam/exec-driven orchestration, with fakes -------------------------

    struct FakeSeam {
        ready: String,
        ready_rc: i32,
    }
    impl Seam for FakeSeam {
        fn call(&self, func: &str, _args: &[String]) -> util::Out {
            match func {
                "_dwarf_rune_ready" => util::Out { code: self.ready_rc, stdout: self.ready.clone(), stderr: String::new() },
                other => panic!("unexpected seam call: {other}"),
            }
        }
    }

    /// A `Conf` whose capacity pause file lives under `run` — real-file-driven now that
    /// family K is in-process (wave 4.26), replacing the old FakeSeam's
    /// `_dwarf_capacity_paused` stub. Built on `crate::tests::complete_vars()` (every
    /// registered key declared, per Ryan 2026-10-05: one source of config), not a
    /// single-key hand-built map — `Conf::i`/`Conf::s` carry no Rust-side default any
    /// more, so a hand-built `Conf` missing SPIRA_CAPACITY_PROBE_WINDOW resolved it to 0
    /// instead of 18000, which made `capacity::paused`'s own `left > probe_window` check
    /// wrongly take the early-probe branch on every pause this far out.
    fn conf_at(run: &Path) -> Conf {
        let mut vars = crate::tests::complete_vars();
        vars.insert("SPIRA_RUN".to_string(), run.display().to_string());
        let snap = crate::seam::Snapshot { vars, ..Default::default() };
        Conf::new(&snap, Path::new("h/spira"))
    }
    struct FakeExec(std::sync::Mutex<Option<(String, Vec<String>)>>);
    impl Exec for FakeExec {
        fn exec(&self, prog: &str, args: &[String], _stdin: Option<Vec<u8>>, _cwd: Option<&Path>) -> util::Out {
            *self.0.lock().unwrap() = Some((prog.to_string(), args.to_vec()));
            util::Out::ok("")
        }
    }

    fn env_with_dwarf_on_path(bin: &Path) -> Env {
        let mut base = std::collections::BTreeMap::new();
        base.insert("PATH".to_string(), bin.display().to_string());
        Env::new(base.clone(), base)
    }

    fn call(dir: &Path, seam: &FakeSeam, exec: &FakeExec, sink: &util::MemSink, argv: &[String]) -> i32 {
        run(seam, exec, &env_with_dwarf_on_path(dir), sink, &conf_at(dir), "systemd-run", argv, Path::new("h/spira"), "stretchy", false, 1000)
    }

    #[test_id(t6o0qs2)]
    #[test]
    fn run_halted_does_not_call_and_exits_1() {
        let dir = testkit::TempDir::new("escape-run");
        std::fs::write(dir.join("world.halted"), "").unwrap();
        let exec = FakeExec(std::sync::Mutex::new(None));
        let sink = util::MemSink::default();
        let rc = call(&dir, &FakeSeam { ready: "4".into(), ready_rc: 0 }, &exec, &sink, &[]);
        assert_eq!(rc, 1);
        assert!(exec.0.lock().unwrap().is_none());
        assert!(sink.all().contains("escape.sh stretchy: halted — not calling"));
    }

    #[test_id(tlgb0sw)]
    #[test]
    fn run_draining_does_not_call_and_exits_1() {
        let dir = testkit::TempDir::new("escape-run");
        std::fs::write(dir.join("world.draining"), "").unwrap();
        let exec = FakeExec(std::sync::Mutex::new(None));
        let sink = util::MemSink::default();
        assert_eq!(call(&dir, &FakeSeam { ready: "4".into(), ready_rc: 0 }, &exec, &sink, &[]), 1);
        assert!(exec.0.lock().unwrap().is_none());
        assert!(sink.all().contains("draining — not calling"));
    }

    #[test_id(tjw517y)]
    #[test]
    fn run_ungated_calls_with_the_unit_properties_it_was_given() {
        let dir = testkit::TempDir::new("escape-run");
        testkit::write_exe(dir.join("dwarf"), "#!/bin/sh\n");
        let exec = FakeExec(std::sync::Mutex::new(None));
        let sink = util::MemSink::default();
        let rc = call(&dir, &FakeSeam { ready: "4".into(), ready_rc: 0 }, &exec, &sink, &["--property=TimeoutStartSec=3600".to_string()]);
        assert_eq!(rc, 0);
        let (_, argv) = exec.0.lock().unwrap().clone().expect("a call was made");
        assert!(argv.contains(&"--property=TimeoutStartSec=3600".to_string()), "{argv:?}");
    }

    #[test_id(tm5qcfh)]
    #[test]
    fn world_gate_refuses_when_the_claims_lane_is_stopped_on_every_repo() {
        let dir = testkit::TempDir::new("escape-gate");
        assert_eq!(world_gate(&dir, &["r1"], "stretchy", "escape.sh"), None);
        spira_config::scoped_stop::pause_repo(&dir, "r1", &["claims".to_string()], "op", "why", "later", 1).unwrap();
        let why = world_gate(&dir, &["r1"], "stretchy", "escape.sh").expect("stopped");
        assert!(why.starts_with("escape.sh stretchy: skipped — the claims lane of r1"), "{why}");
        assert_eq!(world_gate(&dir, &["r1", "r2"], "stretchy", "escape.sh"), None);
    }

    #[test_id(tmpeei1)]
    #[test]
    fn call_argv_carries_the_dwarf_unit_properties_and_environment() {
        let dir = testkit::TempDir::new("escape-argv");
        let rel = dir.join("rel");
        std::fs::create_dir_all(&rel).unwrap();
        let _g = testkit::env(&[(spira_config::RELEASE_ENV, Some(rel.to_str().unwrap())), ("HOME", Some("h")), (spira_config::process::TOML_ENV, Some("x"))]);
        let a = call_argv(&dir, "stretchy", None);
        let a = a.expect("argv");
        assert!(a.iter().any(|f| f == "--property=TimeoutStartSec=3600"), "{a:?}");
        assert!(a.iter().any(|f| f.starts_with("--setenv=SPIRA_RELEASE=")), "{a:?}");
    }

    #[test_id(tfptylr)]
    #[test]
    fn run_capacity_paused_does_not_call_and_exits_1() {
        let dir = testkit::TempDir::new("escape-run");
        let exec = FakeExec(std::sync::Mutex::new(None));
        let sink = util::MemSink::default();
        std::fs::write(dir.join("capacity-pause"), "1120 iso why\n").unwrap();
        let rc = call(&dir, &FakeSeam { ready: "4".into(), ready_rc: 0 }, &exec, &sink, &[]);
        assert_eq!(rc, 1);
        assert!(exec.0.lock().unwrap().is_none());
        assert!(sink.all().contains("account out of capacity for another 120s"));
    }
}
