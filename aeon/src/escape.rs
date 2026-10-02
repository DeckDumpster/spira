//! escape — direct-summon an aeon for a named fayth, bypassing pool and lane checks
//! (replaces `spira/escape.sh`, sp-zpaq0). `aeon --escape <fayth> [--dry-run]`.
//!
//! WHEN TO USE. The sentinel decides how many aeons may start by comparing live aeon
//! counts against pool and lane budgets. A bug in that arithmetic — wrong pool
//! calculation, a lane accidentally counted against the pool — can prevent the bead that
//! REPAIRS the bug from being claimed: the scheduler cannot schedule its own fix. This is
//! the path around that. It skips the pool and lane capacity checks entirely and invokes
//! the aeon directly.
//!
//! THE CONTROL PLANE MUST NOT DEPEND ON THE DATA PLANE IT CONTROLS. Normal scheduling is
//! the data plane; a broken scheduler is exactly when this — the control plane — must be
//! reachable. Ryan's own case: eight P0s skipped because the bug fixing the ordering
//! logic was itself subject to the ordering it would fix.
//!
//! WHAT THIS DOES NOT BYPASS. The fayth must exist (checked by the caller, the same
//! `home/chamber/<fayth>.fayth` test every mode uses). Its partition must have ready
//! work. The account must have API capacity. The bead must not be poisoned. The fayth's
//! own FAYTH_MAX_CONCURRENT is enforced by the summoned aeon internally. All of that
//! still applies — `world_gate` and `fayth_ready`/`summon_argv` are reached exactly as
//! `summon_fayth` reaches them, through lib.sh (§5's seam), because a drain past its TTL
//! being lifted and logged is family G's (not this bead's) to port. The capacity check is
//! in-process now (`capacity::check_and_probe`, wave 4.26 — family K's home is this
//! crate, and aeon is its probe's one owner), so a probe that clears the pause file early
//! happens exactly once no matter how many of this crate's own entry points ask.
//!
//! WHEN NOT TO USE. Normal scheduling is observable, coordinated, and lower-cost. Reserve
//! this for a scheduler that is demonstrably failing to summon a fayth that has ready work.
//!
//! RETIRED RATHER THAN PORTED AS A SEPARATE SCRIPT. escape.sh's only job was to gate and
//! then invoke the `aeon` binary; the binary now does both.

use std::path::Path;

use crate::conf::Conf;
use crate::ports::{Env, Exec, Seam};
use crate::util::{self, Sink};

/// What escape decided to do, independent of how the facts were gathered — the part this
/// module's tests hold to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Gated (halted, draining, or the account is paused). Exit 1, nothing summoned.
    Gated,
    /// The partition's ready count could not be read. Exit 1 (`die`), nothing summoned.
    CannotReadPartition,
    /// Nothing ready. Exit 0, nothing summoned — the positive control that distinguishes
    /// "gated" from "correctly declined".
    NothingReady,
    /// `aeon` is not on PATH. Exit 1 (`die`), nothing summoned.
    AeonMissing,
    /// Summoned: the `systemd-run` argv that was (or would be) run.
    Summoned(Vec<String>),
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
/// rc and its stdout seconds-left, fayth_ready's rc and its stdout count, and whether `aeon`
/// resolved on PATH). Pure — no IO, fully covered by unit tests below.
pub struct Facts {
    pub world_gate_ok: bool,
    pub capacity_left: Option<String>, // Some(seconds) when paused
    pub ready: Result<i64, ()>,        // Err on a fayth_ready failure (rc != 0)
    pub aeon_path: Option<String>,
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
    let Some(_) = f.aeon_path.as_ref() else {
        return Outcome::AeonMissing;
    };
    Outcome::Summoned(Vec::new()) // argv filled in by build_argv, kept apart from decide()
}

/// `systemd-run --user --collect --quiet --unit=spira-aeon-<fayth>-escape-<epoch> <summon
/// argv...> <aeon-path> --home <home> <fayth> [--dry-run]` — escape.sh:52-58, unchanged.
pub fn build_argv(fayth: &str, home: &Path, aeon_path: &str, summon_argv: &[String], epoch: i64, dry_run: bool) -> Vec<String> {
    let mut a = vec!["--user".to_string(), "--collect".to_string(), "--quiet".to_string(), format!("--unit=spira-aeon-{fayth}-escape-{epoch}")];
    a.extend(summon_argv.iter().cloned());
    a.push(aeon_path.to_string());
    a.push("--home".to_string());
    a.push(home.display().to_string());
    a.push(fayth.to_string());
    if dry_run {
        a.push("--dry-run".to_string());
    }
    a
}

/// The full orchestration: gathers the facts through the seam, decides, and (unless
/// merely deciding) spawns `systemd-run`. Returns the process exit code.
pub fn run(seam: &dyn Seam, exec: &dyn Exec, env: &Env, sink: &dyn Sink, conf: &Conf, summon_bin: &str, home: &Path, fayth: &str, dry_run: bool, now: i64) -> i32 {
    let wg = seam.call("_aeon_world_gate", &["escape.sh".to_string()]);
    for l in wg.stderr.lines() {
        sink.out(l);
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
    let fr = seam.call("_aeon_fayth_ready", &[]);
    let path = env.child().get("PATH").cloned().unwrap_or_default();
    let aeon_path = find_on_path(&path, "aeon", |p| is_executable(p));

    let facts = Facts {
        world_gate_ok: wg.code == 0,
        capacity_left,
        ready: if fr.code == 0 { fr.stdout.trim().parse::<i64>().map_err(|_| ()) } else { Err(()) },
        aeon_path: aeon_path.clone(),
    };

    match decide(&facts) {
        Outcome::Gated => {
            if facts.world_gate_ok {
                // world_gate passed; the capacity pause is what gated it — escape.sh's own line.
                sink.out(&util::log_line(now, &format!("escape.sh {fayth}: account out of capacity for another {}s — not summoning", facts.capacity_left.unwrap_or_default())));
            }
            1
        }
        Outcome::CannotReadPartition => {
            sink.err(&util::log_line(now, &format!("FATAL {fayth}: cannot read its partition")));
            1
        }
        Outcome::NothingReady => {
            sink.out(&util::log_line(now, &format!("escape.sh {fayth}: nothing ready in its partition — nothing to summon")));
            0
        }
        Outcome::AeonMissing => {
            sink.err(&util::log_line(now, &format!("FATAL escape.sh {fayth}: aeon is not on PATH")));
            1
        }
        Outcome::Summoned(_) => {
            let ready = facts.ready.unwrap_or(0);
            sink.out(&util::log_line(now, &format!("escape.sh {fayth}: {ready} ready — summoning directly (pool and lane checks bypassed)")));
            let sa = seam.call("_aeon_summon_argv", &[]);
            let summon_argv: Vec<String> = sa.stdout.lines().map(|s| s.to_string()).collect();
            let argv = build_argv(fayth, home, aeon_path.as_deref().unwrap_or("aeon"), &summon_argv, now, dry_run);
            let o = exec.exec(summon_bin, &argv, None, None);
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
        Facts { world_gate_ok: true, capacity_left: None, ready: Ok(3), aeon_path: Some("/rel/bin/aeon".into()) }
    }

    #[test]
    fn halted_or_draining_gates_before_anything_else() {
        let f = Facts { world_gate_ok: false, ..ok() };
        assert_eq!(decide(&f), Outcome::Gated);
    }

    #[test]
    fn capacity_paused_gates() {
        let f = Facts { capacity_left: Some("120".into()), ..ok() };
        assert_eq!(decide(&f), Outcome::Gated);
    }

    #[test]
    fn unreadable_partition_is_fatal_not_a_decline() {
        let f = Facts { ready: Err(()), ..ok() };
        assert_eq!(decide(&f), Outcome::CannotReadPartition);
    }

    #[test]
    fn zero_ready_declines_cleanly_exit_0() {
        let f = Facts { ready: Ok(0), ..ok() };
        assert_eq!(decide(&f), Outcome::NothingReady);
    }

    #[test]
    fn positive_control_ready_and_ungated_summons() {
        assert!(matches!(decide(&ok()), Outcome::Summoned(_)));
    }

    #[test]
    fn aeon_missing_from_path_is_fatal() {
        let f = Facts { aeon_path: None, ..ok() };
        assert_eq!(decide(&f), Outcome::AeonMissing);
    }

    #[test]
    fn find_on_path_returns_first_hit() {
        let hit = |p: &Path| p == Path::new("/b/aeon");
        assert_eq!(find_on_path("/a:/b:/c", "aeon", hit), Some("/b/aeon".to_string()));
    }

    #[test]
    fn find_on_path_none_when_absent() {
        assert_eq!(find_on_path("/a:/b", "aeon", |_| false), None);
    }

    #[test]
    fn build_argv_shape_matches_escape_sh() {
        let a = build_argv("stretchy", Path::new("/home/spira"), "/rel/bin/aeon", &["--setenv=HOME=/h".to_string()], 1000, false);
        assert_eq!(
            a,
            vec!["--user", "--collect", "--quiet", "--unit=spira-aeon-stretchy-escape-1000", "--setenv=HOME=/h", "/rel/bin/aeon", "--home", "/home/spira", "stretchy"]
        );
    }

    #[test]
    fn build_argv_appends_dry_run_last() {
        let a = build_argv("f", Path::new("/h"), "/bin/aeon", &[], 1, true);
        assert_eq!(a.last().unwrap(), "--dry-run");
    }

    // ---- run(): the seam/exec-driven orchestration, with fakes -------------------------

    struct FakeSeam {
        world_gate_rc: i32,
        ready: String,
        ready_rc: i32,
        summon_argv: String,
    }
    impl Seam for FakeSeam {
        fn call(&self, func: &str, _args: &[String]) -> util::Out {
            match func {
                "_aeon_world_gate" => util::Out { code: self.world_gate_rc, stdout: String::new(), stderr: String::new() },
                "_aeon_fayth_ready" => util::Out { code: self.ready_rc, stdout: self.ready.clone(), stderr: String::new() },
                "_aeon_summon_argv" => util::Out::ok(self.summon_argv.clone()),
                other => panic!("unexpected seam call: {other}"),
            }
        }
    }

    /// A `Conf` whose capacity pause file lives under `run` — real-file-driven now that
    /// family K is in-process (wave 4.26), replacing the old FakeSeam's
    /// `_aeon_capacity_paused` stub.
    fn conf_at(run: &Path) -> Conf {
        let snap = crate::seam::Snapshot {
            vars: std::collections::BTreeMap::from([("SPIRA_RUN".to_string(), run.display().to_string())]),
            ..Default::default()
        };
        Conf::new(&snap, Path::new("/home/spira"))
    }
    struct FakeExec(std::sync::Mutex<Option<(String, Vec<String>)>>);
    impl Exec for FakeExec {
        fn exec(&self, prog: &str, args: &[String], _stdin: Option<Vec<u8>>, _cwd: Option<&Path>) -> util::Out {
            *self.0.lock().unwrap() = Some((prog.to_string(), args.to_vec()));
            util::Out::ok("")
        }
    }

    fn env_with_aeon_on_path(bin: &Path) -> Env {
        let mut base = std::collections::BTreeMap::new();
        base.insert("PATH".to_string(), bin.display().to_string());
        Env::new(base.clone(), base)
    }

    #[test]
    fn run_declines_cleanly_when_nothing_ready() {
        let dir = testkit::TempDir::new("escape-run");
        let aeon_bin = dir.join("aeon");
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
        testkit::write_exe(&aeon_bin, "#!/bin/sh\n");
        let env = env_with_aeon_on_path(&dir);
        let seam = FakeSeam { world_gate_rc: 0, ready: "0".into(), ready_rc: 0, summon_argv: String::new() };
        let exec = FakeExec(std::sync::Mutex::new(None));
        let sink = util::MemSink::default();
        let conf = conf_at(&dir);
        let rc = run(&seam, &exec, &env, &sink, &conf, "systemd-run", Path::new("/home/spira"), "stretchy", false, 1000);
        assert_eq!(rc, 0);
        assert!(exec.0.lock().unwrap().is_none(), "nothing ready must not summon");
        assert!(sink.all().contains("nothing ready in its partition"));
    }

    #[test]
    fn run_summons_with_the_right_argv_when_ready() {
        let dir = testkit::TempDir::new("escape-run");
        let aeon_bin = dir.join("aeon");
        // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
        testkit::write_exe(&aeon_bin, "#!/bin/sh\n");
        let env = env_with_aeon_on_path(&dir);
        let seam = FakeSeam { world_gate_rc: 0, ready: "4".into(), ready_rc: 0, summon_argv: "--setenv=HOME=/h\n--property=TimeoutStartSec=3600".into() };
        let exec = FakeExec(std::sync::Mutex::new(None));
        let sink = util::MemSink::default();
        let conf = conf_at(&dir);
        let rc = run(&seam, &exec, &env, &sink, &conf, "systemd-run", Path::new("/home/spira"), "stretchy", false, 1000);
        assert_eq!(rc, 0);
        let (prog, args) = exec.0.lock().unwrap().clone().expect("summoned");
        assert_eq!(prog, "systemd-run");
        assert_eq!(args[0], "--user");
        assert!(args.iter().any(|a| a == "--unit=spira-aeon-stretchy-escape-1000"));
        assert!(args.contains(&"--setenv=HOME=/h".to_string()));
        assert_eq!(args.last().unwrap(), "stretchy");
        assert!(sink.all().contains("4 ready — summoning directly"));
    }

    #[test]
    fn run_halted_does_not_summon_and_exits_1() {
        let dir = testkit::TempDir::new("escape-run");
        let env = env_with_aeon_on_path(&dir);
        let seam = FakeSeam { world_gate_rc: 1, ready: "4".into(), ready_rc: 0, summon_argv: String::new() };
        let exec = FakeExec(std::sync::Mutex::new(None));
        let sink = util::MemSink::default();
        let conf = conf_at(&dir);
        let rc = run(&seam, &exec, &env, &sink, &conf, "systemd-run", Path::new("/home/spira"), "stretchy", false, 1000);
        assert_eq!(rc, 1);
        assert!(exec.0.lock().unwrap().is_none());
    }

    #[test]
    fn run_capacity_paused_does_not_summon_and_exits_1() {
        let dir = testkit::TempDir::new("escape-run");
        let env = env_with_aeon_on_path(&dir);
        let seam = FakeSeam { world_gate_rc: 0, ready: "4".into(), ready_rc: 0, summon_argv: String::new() };
        let exec = FakeExec(std::sync::Mutex::new(None));
        let sink = util::MemSink::default();
        let conf = conf_at(&dir);
        // In-process now (wave 4.26): a real pause file replaces the old FakeSeam stub.
        std::fs::write(dir.join("capacity-pause"), "1120 iso why\n").unwrap();
        let rc = run(&seam, &exec, &env, &sink, &conf, "systemd-run", Path::new("/home/spira"), "stretchy", false, 1000);
        assert_eq!(rc, 1);
        assert!(exec.0.lock().unwrap().is_none());
        assert!(sink.all().contains("account out of capacity for another 120s"));
    }
}
