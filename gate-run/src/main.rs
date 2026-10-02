//! `gate-run [--status|--exec] <branch> [repo-name]` — see DESIGN.md.

mod engine;
mod ports;
mod real;

use engine::{Key, Snapshot, StatusOutcome};
use ports::World;
use real::Real;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

struct Paths {
    dir: PathBuf,
    pid: PathBuf,
    rc: PathBuf,
    out: PathBuf,
    key: PathBuf,
    started: PathBuf,
    suites: PathBuf,
}

impl Paths {
    fn new(run: &Path, slug: &str) -> Paths {
        let dir = run.join("gate-run").join(slug);
        Paths {
            pid: dir.join("pid"),
            rc: dir.join("rc"),
            out: dir.join("out"),
            key: dir.join("key"),
            started: dir.join("started"),
            suites: dir.join("suites"),
            dir,
        }
    }
}

fn default_home() -> PathBuf {
    if let Some(h) = std::env::var_os("SPIRA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().and_then(|d| d.parent()).map(|r| r.join("spira")))
        .unwrap_or_else(|| PathBuf::from("spira"))
}

fn default_run(home: &Path) -> PathBuf {
    std::env::var_os("SPIRA_RUN").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join("../run").to_path_buf())
}

/// `spira_home_repo` via the same `lib.sh` seam `Real` uses for the repository map, only reached
/// when a caller omits `repo-name` (no production caller does — DESIGN.md "Intent").
fn default_repo_name(home: &Path) -> String {
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(". \"$1/lib.sh\" >/dev/null 2>&1 && spira_home_repo")
        .arg("gate-run-home-repo")
        .arg(home)
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
        // release's bin/+spira/ on the CHILD's PATH, never only inherited.
        .envs(spira_config::release_env::child_path_env_for_process())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    out.ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

enum Mode {
    Wait,
    Status,
    Exec,
}

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    // `--home <dir>` is not part of the bash's own CLI (which always resolved its own
    // directory via `$(dirname "$0")`); it exists so this binary — installed apart from
    // `spira/`, like `gate` — can be told where `gate.sh`, `lib.sh` and its own detached
    // re-invocation live, and so the shim (`spira/gate-run.sh`) can pass it explicitly the
    // same way `gate.sh` passes it to the `gate` binary.
    let mut home = None;
    if argv.first().map(String::as_str) == Some("--home") {
        if argv.len() < 2 {
            eprintln!("gate-run: --home needs a directory");
            return ExitCode::from(1);
        }
        home = Some(PathBuf::from(argv.remove(1)));
        argv.remove(0);
    }
    let mut mode = Mode::Wait;
    let mut rest = argv.as_slice();
    if let Some(first) = argv.first() {
        match first.as_str() {
            "--status" => {
                mode = Mode::Status;
                rest = &argv[1..];
            }
            "--exec" => {
                mode = Mode::Exec;
                rest = &argv[1..];
            }
            s if s.starts_with("--") => {
                eprintln!("gate-run: unknown option {s}");
                return ExitCode::from(1);
            }
            _ => {}
        }
    }
    let Some(branch) = rest.first().filter(|b| !b.is_empty()) else {
        eprintln!("gate-run: usage: gate-run.sh [--status] <branch> [repo-name]");
        return ExitCode::from(1);
    };
    let branch = branch.clone();
    let home = home.unwrap_or_else(default_home);
    let repo_name = rest.get(1).cloned().filter(|r| !r.is_empty()).unwrap_or_else(|| default_repo_name(&home));
    let run = default_run(&home);
    let world = Real::new(home.clone());

    let code = match mode {
        Mode::Exec => run_exec(&world, &home, &run, &branch, &repo_name),
        Mode::Status => run_status(&world, &run, &branch, &repo_name),
        Mode::Wait => run_wait(&world, &run, &branch, &repo_name),
    };
    ExitCode::from(code as u8)
}

fn resolve_key(world: &dyn World, branch: &str, repo_name: &str) -> Result<(PathBuf, Key), i32> {
    let repo = match world.resolve_repo(repo_name) {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("{msg}");
            return Err(1);
        }
    };
    let Some(tip) = world.rev_parse(&repo, branch) else {
        eprintln!("gate-run: {repo_name} has no branch '{branch}'");
        return Err(1);
    };
    let base = world.landref(&repo).and_then(|b| world.rev_parse(&repo, &b));
    Ok((repo, Key::new(tip, base)))
}

fn gather(world: &dyn World, paths: &Paths, want_recorded_key: bool) -> Snapshot {
    let dir_exists = world.exists(&paths.dir);
    let pid = world.read(&paths.pid).and_then(|s| s.trim().parse::<i64>().ok());
    let proc_exists = pid.map(|p| world.proc_exists(p)).unwrap_or(false);
    let cmdline = if proc_exists { pid.and_then(|p| world.proc_cmdline(p)) } else { None };
    let managed_alive = engine::is_managed_alive(pid, proc_exists, cmdline.as_deref());
    let started = world.read(&paths.started).and_then(|s| s.trim().parse::<u64>().ok());
    let recorded_key = if dir_exists && want_recorded_key { world.read(&paths.key) } else { None };
    let rc = world.read_i32(&paths.rc);
    let suites = world.read(&paths.suites);
    let out_full = world.read_raw(&paths.out).unwrap_or_default();
    Snapshot { dir_exists, managed_alive, pid, started, recorded_key, rc, suites, out_full }
}

fn print_report(world: &dyn World, branch: &str, repo_name: &str, key: &str, elapsed: u64, snap: &Snapshot) -> i32 {
    let v = engine::decide_report(snap);
    if matches!(v, engine::Verdict::Died) {
        world.eprintln(&engine::died_tail(snap));
    }
    let (text, code) = engine::render_verdict(branch, repo_name, elapsed, key, &v);
    world.print(&text);
    code
}

fn run_status(world: &dyn World, run: &Path, branch: &str, repo_name: &str) -> i32 {
    let (_, key) = match resolve_key(world, branch, repo_name) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let slug = engine::slug(repo_name, branch);
    let paths = Paths::new(run, &slug);
    let snap = gather(world, &paths, true);
    match engine::decide_status(&key, &snap) {
        StatusOutcome::StillRunning { stale } => {
            let elapsed = engine::elapsed(world.now(), snap.started);
            let (text, code) = engine::render_still_running(branch, &key.format(), elapsed, snap.pid.unwrap_or(0), stale);
            world.print(&text);
            code
        }
        StatusOutcome::StaleVerdict { recorded_key } => {
            let (text, code) = engine::render_stale_verdict(branch, &recorded_key, &key.format());
            world.print(&text);
            code
        }
        StatusOutcome::Verdict => {
            let elapsed = engine::elapsed(world.now(), snap.started);
            print_report(world, branch, repo_name, &key.format(), elapsed, &snap)
        }
        StatusOutcome::NoGate => {
            let self_pid = world.pid();
            let procs = world.list_other_procs(self_pid);
            match engine::find_unmanaged(branch, self_pid, &procs) {
                Some(pid) => {
                    let (text, code) = engine::render_unmanaged(branch, &key.format(), pid);
                    world.print(&text);
                    code
                }
                None => {
                    let (text, code) = engine::render_no_gate(branch, &key.format());
                    world.eprintln(text.trim_end());
                    code
                }
            }
        }
    }
}

/// The detached run itself (`--exec`, not for hand use — DESIGN.md "Contract").
fn run_exec(world: &dyn World, home: &Path, run: &Path, branch: &str, repo_name: &str) -> i32 {
    let slug = engine::slug(repo_name, branch);
    let paths = Paths::new(run, &slug);
    world.mkdir_p(&paths.dir);
    world.write_atomic(&paths.pid, &world.pid().to_string());
    let rc = world.run_gate(home, branch, repo_name, &paths.out);
    let out = world.read_raw(&paths.out).unwrap_or_default();
    let suites = engine::extract_suites(&out);
    world.write_atomic(&paths.suites, &suites);
    world.write_atomic(&paths.rc, &rc.to_string());
    rc
}

fn stop_run(world: &dyn World, paths: &Paths) {
    if let Some(pid) = world.read(&paths.pid).and_then(|s| s.trim().parse::<i64>().ok()) {
        let group = world.proc_pgid(pid) == Some(pid);
        world.kill(pid, group);
    }
    world.remove_dir_all(&paths.dir);
}

fn run_wait(world: &dyn World, run: &Path, branch: &str, repo_name: &str) -> i32 {
    let (_, key) = match resolve_key(world, branch, repo_name) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let key_text = key.format();
    let slug = engine::slug(repo_name, branch);
    let paths = Paths::new(run, &slug);

    // A run recorded for a different tree is a different question — stop it and start over
    // rather than answering from (or waiting on) it.
    if world.exists(&paths.dir) {
        if let Some(recorded) = world.read(&paths.key) {
            if key.is_stale(&recorded) {
                world.eprintln(engine::render_stale_restart().trim_end());
                stop_run(world, &paths);
            }
        }
    }

    if world.exists(&paths.rc) {
        let snap = gather(world, &paths, false);
        let elapsed = engine::elapsed(world.now(), snap.started);
        return print_report(world, branch, repo_name, &key_text, elapsed, &snap);
    }

    let snap = gather(world, &paths, false);
    if !snap.managed_alive {
        if world.exists(&paths.dir) {
            world.eprintln(engine::render_gone_restart(branch).trim_end());
            stop_run(world, &paths);
        }
        world.mkdir_p(&paths.dir);
        world.write_atomic(&paths.key, &key_text);
        world.write_atomic(&paths.started, &world.now().to_string());
        world.write_atomic(&paths.out, "");
        world.spawn_detached(branch, repo_name);
        for _ in 0..10 {
            let s = gather(world, &paths, false);
            if s.managed_alive || world.exists(&paths.rc) {
                break;
            }
            world.sleep_ms(500);
        }
        world.eprintln(engine::render_wait_started(branch, repo_name, poll_secs()).trim_end());
    }

    let poll = poll_secs();
    let tick = tick_secs();
    let deadline = world.now() + poll;
    loop {
        if world.exists(&paths.rc) {
            break;
        }
        if world.now() >= deadline {
            break;
        }
        let s = gather(world, &paths, false);
        if !s.managed_alive {
            break;
        }
        world.sleep(tick);
    }

    if world.exists(&paths.rc) {
        let snap = gather(world, &paths, false);
        let elapsed = engine::elapsed(world.now(), snap.started);
        return print_report(world, branch, repo_name, &key_text, elapsed, &snap);
    }
    let snap = gather(world, &paths, false);
    if !snap.managed_alive {
        // Gone without a verdict: report() fails closed on it, same as the bash.
        let elapsed = engine::elapsed(world.now(), snap.started);
        let code = print_report(world, branch, repo_name, &key_text, elapsed, &snap);
        world.remove_dir_all(&paths.dir);
        return code;
    }

    let elapsed = engine::elapsed(world.now(), snap.started);
    let (text, code) = engine::render_wait_timeout(branch, poll, elapsed);
    world.print(&text);
    code
}

fn poll_secs() -> u64 {
    std::env::var("SPIRA_GATE_POLL").ok().and_then(|v| v.parse().ok()).unwrap_or(480)
}
fn tick_secs() -> u64 {
    std::env::var("SPIRA_GATE_TICK").ok().and_then(|v| v.parse().ok()).unwrap_or(3)
}
