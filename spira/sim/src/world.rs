use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const PRODUCTION_LOCATORS: &[&str] = &["SPIRA_LC_PASSWORD_FILE", "SPIRA_LC_SOCKET", "SPIRA_RUN", "SPIRA_DB"];
pub const MARKER: &str = ".sim-world";
/// The world's landing ref: `repo.sim.base`, and the ref a LANDED bead's commit must reach.
pub const LANDING_BASE: &str = "local/main";
const CALL_DEADLINE: Duration = Duration::from_secs(120); // batch-job: git/tar/cp/testenv steps of building a world
const BUILD_DEADLINE: Duration = Duration::from_secs(3600); // batch-job: release build of the tree
/// A prebuilt release directory (shaped like `spira-releases/<sha>`: `bin/` and `spira/`) the
/// world runs instead of building one (sp-o4s4t4).
pub const RELEASE_ENV: &str = "SPIRA_SIM_RELEASE";
/// testenv's mark on every suite it runs (testenv/src/fixture.rs). It only ever widens the
/// refusal below, so a forged value can stop a build but never start one.
pub const TESTENV_ENV: &str = "SPIRA_IN_TESTENV";
/// The release binaries the world itself calls; a prebuilt release without them is refused.
const RELEASE_BINS: &[&str] = &["bin/spira-config", "bin/spira-lc"];
/// The world's lifecycle service socket, under the world dir (sp-hq1v76).
const LC_SOCKET: &str = "lc.sock";
const HOME: &str = "home";
/// The file the legacy bash readers still require, with the one row the world registers.
const REGISTRY_ROWS: &str = "registry-rows";
/// The name the world's one repository is registered under (`repo.<name>` in its config).
const REPO_NAME: &str = "sim";
/// The home every path in the complete fixture config is rooted at (spira-config/tests/fixtures).
const FIXTURE_HOME: &str = "/fixture/userhome";
/// The PID world up recorded for the world's `spira-lc serve`; world down signals only it.
const LC_PID: &str = "lc-serve.pid";
const LC_LOG: &str = "lc-serve.log";
/// `sockaddr_un.sun_path` is 108 bytes with its NUL; a longer path cannot be bound.
const SUN_PATH_MAX: usize = 107;
const LC_READY_DEADLINE: Duration = Duration::from_secs(30); // batch-job: serve bind + first DB round trip
const LC_STOP_DEADLINE: Duration = Duration::from_secs(5); // batch-job: serve exit after SIGTERM
/// Every key declared: a process resolves its config only from a file that declares them all
/// (spira-config/src/process.rs), so the world's config starts from the complete fixture and
/// overrides each locator a world must own. Its `lc_socket` is the production default and
/// is always overwritten below.
const CONFIG_BASE: &str = include_str!("../../../spira-config/tests/fixtures/complete.toml");

/// Where a world's release comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseSource {
    /// `SPIRA_SIM_RELEASE`'s directory, canonical, used as is: no build.
    Prebuilt(PathBuf),
    /// A `release build` of the tree, cached by tree hash (the host only).
    Build,
}

/// Resolve the world's release before anything is built. Inside testenv an unset
/// `SPIRA_SIM_RELEASE` is refused: a cold workspace build alone outlasts the suite cap and is
/// an uncapped build on the shared host, so it is never done silently.
pub fn release_source(env: &dyn Fn(&str) -> Option<String>) -> Result<ReleaseSource, String> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty());
    match set(RELEASE_ENV) {
        Some(dir) => prebuilt_release(Path::new(&dir)).map(ReleaseSource::Prebuilt),
        None if set(TESTENV_ENV).is_some() => Err(format!(
            "{RELEASE_ENV} is not set: inside testenv ({TESTENV_ENV} is set) a sim world runs a prebuilt release and never builds one; set {RELEASE_ENV}=<release dir with bin/ and spira/>, e.g. {RELEASE_ENV}=\"$SPIRA_RELEASE\""
        )),
        None => Ok(ReleaseSource::Build),
    }
}

/// Check that `dir` is a release (`bin/`, `spira/`, and the binaries a world calls) and return
/// it canonical, so the world's `release` link never depends on the caller's cwd.
pub fn prebuilt_release(dir: &Path) -> Result<PathBuf, String> {
    let shown = dir.display();
    let dir = dir.canonicalize().map_err(|e| format!("{RELEASE_ENV}={shown}: {e}"))?;
    for sub in ["bin", "spira"] {
        if !dir.join(sub).is_dir() {
            return Err(format!("{RELEASE_ENV}={shown} is not a release: it has no {sub}/"));
        }
    }
    for exe in RELEASE_BINS {
        if !dir.join(exe).is_file() {
            return Err(format!("{RELEASE_ENV}={shown} is not a release: it has no {exe}"));
        }
    }
    Ok(dir)
}

pub fn refuse_production(env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let seen: Vec<&str> = PRODUCTION_LOCATORS
        .iter()
        .copied()
        .filter(|k| env(k).is_some_and(|v| !v.is_empty()))
        .collect();
    if seen.is_empty() {
        Ok(())
    } else {
        Err(format!("refusing to build a world while a production locator is set: {}", seen.join(", ")))
    }
}

/// What a world needs from outside itself: a release build and a private Dolt server.
pub trait Steps {
    fn release(&self, repo: &Path, tree: &str, cache: &Path) -> Result<PathBuf, String>;
    fn db_up(&self, world: &Path) -> Result<String, String>;
    fn db_down(&self, fixture: &str) -> Result<(), String>;
    fn lifecycle(&self, release: &Path, fixture: &str, lifecycle: &Path, config: &Path) -> Result<(), String>;
    fn config_set(&self, release: &Path, file: &Path, key: &str, value: &str) -> Result<(), String>;
    /// Start the world's `spira-lc serve` on [`lc_socket`], write its PID to [`lc_pid_file`]
    /// as soon as it exists, and return once it answers a request.
    fn lc_serve(&self, world: &Path, fixture: &str) -> Result<(), String>;
    /// Stop the serve world up recorded as `pid`, only if `/proc/<pid>/cmdline` is that
    /// world's serve ([`is_world_serve`]); a PID that is gone or now another process is left alone.
    fn lc_stop(&self, world: &Path, pid: u32) -> Result<(), String>;
}

/// The world's lifecycle socket.
pub fn lc_socket(world: &Path) -> PathBuf {
    world.join(LC_SOCKET)
}

/// Where world up records the PID of the world's `spira-lc serve`.
pub fn lc_pid_file(world: &Path) -> PathBuf {
    world.join(LC_PID)
}

/// The argv world up starts the world's serve with: the world's own release link, so the
/// command line names the world and no other world's serve (or production's) matches it.
pub fn serve_argv(world: &Path) -> Vec<String> {
    vec![
        world.join("release/bin/spira-lc").display().to_string(),
        "serve".to_string(),
        lc_socket(world).display().to_string(),
    ]
}

/// Whether a `/proc/<pid>/cmdline` (NUL-separated) is exactly this world's serve.
pub fn is_world_serve(cmdline: &[u8], world: &Path) -> bool {
    let args: Vec<&[u8]> = cmdline.strip_suffix(&[0]).unwrap_or(cmdline).split(|b| *b == 0).collect();
    let want = serve_argv(world);
    args.len() == want.len() && args.iter().zip(&want).all(|(a, w)| *a == w.as_bytes())
}

/// A world's socket path must fit `sun_path`; refused before anything is built.
pub fn check_socket_path(world: &Path) -> Result<(), String> {
    let s = lc_socket(world);
    let n = s.as_os_str().len();
    if n > SUN_PATH_MAX {
        return Err(format!("the world's lifecycle socket {} is {n} bytes, over the {SUN_PATH_MAX}-byte unix socket limit: use a shorter world dir", s.display()));
    }
    Ok(())
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;
const WNOHANG: i32 = 1;

/// Alive and not a zombie; reaps it first when it is this process's own child.
fn alive(pid: u32) -> bool {
    let mut status = 0;
    // SAFETY: waitpid with WNOHANG on a pid only reaps our own exited child; otherwise -1.
    unsafe { waitpid(pid as i32, &mut status, WNOHANG) };
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().next()).is_some_and(|st| st != "Z"),
        Err(_) => false,
    }
}

fn signal(pid: u32, sig: i32) {
    // SAFETY: kill(2) on one PID that was just checked to be this world's serve.
    unsafe { kill(pid as i32, sig) };
}

/// Send one request to the serve and read its reply: `Some(exit_code)` when it answered.
fn ask(socket: &Path, argv: &[&str]) -> Option<i64> {
    use std::io::{BufRead, BufReader, Write};
    let stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
    writeln!(&stream, "{}", serde_json::to_string(argv).ok()?).ok()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    serde_json::from_str::<serde_json::Value>(line.trim()).ok()?.get("exit_code")?.as_i64()
}

#[derive(Default)]
pub struct ProcessSteps;

pub fn run(cmd: &mut Command, deadline: Duration) -> Result<String, String> {
    let name = format!("{:?}", cmd.get_program());
    let (status, stdout, stderr) = run_capture(cmd, deadline)?;
    if status.success() {
        Ok(stdout)
    } else {
        Err(format!("{name}: {status}: {}", stderr.trim()))
    }
}

pub fn run_capture(cmd: &mut Command, deadline: Duration) -> Result<(std::process::ExitStatus, String, String), String> {
    let name = format!("{:?}", cmd.get_program());
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("{name}: {e}"))?;
    let mut out = child.stdout.take().ok_or("no stdout")?;
    let mut err = child.stderr.take().ok_or("no stderr")?;
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{name}: no result within {}s", deadline.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = t_out.join().unwrap_or_default();
    let stderr = t_err.join().unwrap_or_default();
    Ok((status, stdout, stderr))
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "sim")
            .env("GIT_AUTHOR_EMAIL", "sim@sim.invalid")
            .env("GIT_COMMITTER_NAME", "sim")
            .env("GIT_COMMITTER_EMAIL", "sim@sim.invalid")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z"),
        CALL_DEADLINE,
    )
}

impl Steps for ProcessSteps {
    fn release(&self, repo: &Path, tree: &str, cache: &Path) -> Result<PathBuf, String> {
        let hash = git(repo, &["rev-parse", &format!("{tree}^{{tree}}")])?.trim().to_string();
        let out = cache.join(&hash);
        if out.join(".complete").is_file() {
            return Ok(out);
        }
        let src = cache.join(format!("{hash}.src"));
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(&src).map_err(|e| e.to_string())?;
        let tarball = cache.join(format!("{hash}.tar"));
        run(Command::new("git").arg("-C").arg(repo).args(["archive", "--format=tar", "-o"]).arg(&tarball).arg(tree), CALL_DEADLINE)?;
        run(Command::new("tar").arg("-xf").arg(&tarball).arg("-C").arg(&src), CALL_DEADLINE)?;
        run(
            Command::new("cargo").args(["build", "--release", "--workspace", "--bins"]).current_dir(&src),
            BUILD_DEADLINE,
        )?;
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        run(Command::new("cp").arg("-r").arg(src.join("target/release")).arg(out.join("bin")), CALL_DEADLINE)?;
        std::fs::write(out.join(".complete"), "").map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_file(&tarball);
        Ok(out)
    }
    fn db_up(&self, world: &Path) -> Result<String, String> {
        let out = run(
            Command::new("testenv").args(["testdb", "up", "--tag", "simworld", "--bd", "bd", "--dolt", "dolt", "--root"]).arg(db_root(world, &|k| std::env::var(k).ok())),
            CALL_DEADLINE,
        )?;
        out.lines()
            .find_map(|l| l.strip_prefix("TESTDB_FIXTURE="))
            .map(str::to_string)
            .ok_or_else(|| "testenv testdb up printed no TESTDB_FIXTURE".to_string())
    }

    fn config_set(&self, release: &Path, file: &Path, key: &str, value: &str) -> Result<(), String> {
        run(Command::new(release.join("bin/spira-config")).args(["set", key, value]).arg(file), CALL_DEADLINE).map(|_| ())
    }

    fn lifecycle(&self, release: &Path, fixture: &str, lifecycle: &Path, config: &Path) -> Result<(), String> {
        let port = std::fs::read_to_string(Path::new(fixture).join("server.port")).map_err(|e| format!("fixture has no server.port: {e}"))?;
        let toml = config.join("lc.toml");
        let credential = config.join("lc-credential");
        std::fs::write(&credential, "").map_err(|e| e.to_string())?;
        self.config_set(release, &toml, "spira.lc_password_file", &credential.display().to_string())?;
        let world = config.parent().ok_or("config dir has no parent")?;
        self.config_set(release, &toml, "spira.lc_socket", &lc_socket(world).display().to_string())?;
        let lc = |verb: &str, arg: PathBuf| run(lc_command(release, config, &port).arg(verb).arg(arg), CALL_DEADLINE).map(|_| ());
        lc("admin-apply-ddl", lifecycle.join("schema.sql"))?;
        lc("admin-migrate", lifecycle.join("migrations"))
    }

    fn db_down(&self, fixture: &str) -> Result<(), String> {
        run(Command::new("testenv").args(["testdb", "down", "--fixture", fixture]), CALL_DEADLINE).map(|_| ())
    }

    fn lc_serve(&self, world: &Path, fixture: &str) -> Result<(), String> {
        let port = std::fs::read_to_string(Path::new(fixture).join("server.port")).map_err(|e| format!("fixture has no server.port: {e}"))?;
        let socket = lc_socket(world);
        let argv = serve_argv(world);
        let log = std::fs::File::create(world.join(LC_LOG)).map_err(|e| e.to_string())?;
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("SPIRA_TOML", world.join("config/sim.toml"))
            .env("SPIRA_RELEASE", world.join("release"))
            .env("SPIRA_LC_HOST", "127.0.0.1")
            .env("SPIRA_LC_PORT", port.trim())
            .env("SPIRA_LC_USER", "root")
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("{}: {e}", argv[0]))?;
        std::fs::write(lc_pid_file(world), child.id().to_string()).map_err(|e| e.to_string())?;
        let start = Instant::now();
        loop {
            // `list` reads the bead table: an answer with exit 0 is a serve whose DB is reachable.
            if ask(&socket, &["list"]) == Some(0) {
                return Ok(());
            }
            if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
                let log = std::fs::read_to_string(world.join(LC_LOG)).unwrap_or_default();
                return Err(format!("spira-lc serve exited before it answered: {st}: {}", log.trim()));
            }
            if start.elapsed() > LC_READY_DEADLINE {
                let log = std::fs::read_to_string(world.join(LC_LOG)).unwrap_or_default();
                return Err(format!("spira-lc serve on {} did not answer within {}s: {}", socket.display(), LC_READY_DEADLINE.as_secs(), log.trim()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn lc_stop(&self, world: &Path, pid: u32) -> Result<(), String> {
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if !alive(pid) || !is_world_serve(&cmdline, world) {
            eprintln!("sim: lifecycle serve pid {pid} is gone or is no longer this world's serve; nothing signalled");
            return Ok(());
        }
        for (sig, wait) in [(SIGTERM, LC_STOP_DEADLINE), (SIGKILL, LC_STOP_DEADLINE)] {
            signal(pid, sig);
            let start = Instant::now();
            while start.elapsed() < wait {
                if !alive(pid) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Err(format!("the world's spira-lc serve (pid {pid}) survived SIGTERM and SIGKILL"))
    }
}

/// `spira-lc` against the world's own lifecycle store and nothing else: a cleared environment,
/// the world's `config/lc.toml`, and its private Dolt server's port.
pub fn lc_command(release: &Path, config: &Path, port: &str) -> Command {
    let mut cmd = Command::new(release.join("bin/spira-lc"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("SPIRA_TOML", config.join("lc.toml"))
        .env("SPIRA_LC_HOST", "127.0.0.1")
        .env("SPIRA_LC_PORT", port.trim())
        .env("SPIRA_LC_USER", "root")
        .env("SPIRA_LC_ADMIN_USER", "root");
    cmd
}

pub fn is_world(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

pub fn up(repo: &Path, dir: &Path, tree: &str, env: &dyn Fn(&str) -> Option<String>, steps: &dyn Steps) -> Result<(), String> {
    refuse_production(env)?;
    let source = release_source(env)?;
    if let Ok(abs) = std::path::absolute(dir) {
        check_socket_path(&abs)?;
    }
    if dir.exists() && std::fs::read_dir(dir).map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("{} is not empty", dir.display()));
    }
    let repo = PathBuf::from(git(repo, &["rev-parse", "--show-toplevel"])?.trim());
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dir = dir.canonicalize().map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MARKER), "").map_err(|e| e.to_string())?;
    match build(&dir, &repo, tree, &source, steps) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = down(&dir, steps);
            Err(e)
        }
    }
}

fn build(dir: &Path, repo: &Path, tree: &str, source: &ReleaseSource, steps: &dyn Steps) -> Result<(), String> {
    check_socket_path(dir)?;
    let release = match source {
        ReleaseSource::Prebuilt(p) => p.clone(),
        ReleaseSource::Build => {
            let cache = cache_dir(dir);
            std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
            steps.release(repo, tree, &cache)?
        }
    };
    link(&release, &dir.join("release"))?;

    let fixture = steps.db_up(dir)?;
    std::fs::write(dir.join("db.fixture"), &fixture).map_err(|e| e.to_string())?;

    let origin = dir.join("origin.git");
    let work = dir.join("work");
    let p = |x: &Path| x.to_string_lossy().to_string();
    git(dir, &["init", "--bare", "--initial-branch=main", &p(&origin)])?;
    git(dir, &["init", "--initial-branch=main", &p(&work)])?;
    let tarball = dir.join("seed.tar");
    run(
        Command::new("git").arg("-C").arg(repo).args(["archive", "--format=tar", "-o"]).arg(&tarball).arg(tree),
        BUILD_DEADLINE,
    )?;
    run(Command::new("tar").arg("-xf").arg(&tarball).arg("-C").arg(&work), CALL_DEADLINE)?;
    std::fs::remove_file(&tarball).map_err(|e| e.to_string())?;
    git(&work, &["add", "-A"])?;
    git(&work, &["commit", "-q", "-m", "sim seed"])?;
    git(&work, &["remote", "add", "origin", &p(&origin)])?;
    git(&work, &["push", "-q", "origin", "main"])?;
    git(&work, &["branch", LANDING_BASE, "main"])?;
    git(&work, &["branch", "--set-upstream-to=origin/main", "main"])?;

    let run_dir = dir.join("run");
    let config = dir.join("config");
    std::fs::create_dir_all(&run_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&config).map_err(|e| e.to_string())?;
    steps.lifecycle(&release, &fixture, &work.join("lifecycle"), &config)?;
    let runner = config.join("suite-runner");
    std::fs::write(&runner, "#!/bin/sh\nexit 0\n").map_err(|e| e.to_string())?;
    set_exec(&runner)?;
    let file = config.join("sim.toml");
    let gh_state = dir.join("gh");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&gh_state).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    let gh_bin = bin.join("gh");
    let sim_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    link(&sim_exe, &gh_bin)?;
    link(&sim_exe, &bin.join("round-vm"))?;
    link(&sim_exe, &bin.join("sim"))?;
    crate::roundvm::write_verdict(dir, &crate::roundvm::Verdict::Green)?;
    let home = dir.join(HOME);
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    std::fs::write(home.join(REGISTRY_ROWS), registry_rows(&work)).map_err(|e| e.to_string())?;
    std::fs::write(&file, base_config(&home)).map_err(|e| e.to_string())?;
    for (k, v) in config_settings(&work, &gh_bin).into_iter().chain(home_settings(dir, &fixture)) {
        steps.config_set(&release, &file, &k, &v)?;
    }
    for (k, v) in lc_settings(dir) {
        steps.config_set(&release, &file, &k, &v)?;
    }
    let port = std::fs::read_to_string(Path::new(&fixture).join("server.port")).unwrap_or_default();
    std::fs::write(
        config.join("sim.env"),
        format!(
            "SPIRA_RUN={}\nSPIRA_HOME={}\nSPIRA_SIM_GATE_RUNNER={}\nSIM_GH_DIR={}\nSIM_BIN={}\nSIM_PROBE={}\n{}={}\nSPIRA_RELEASE={}\nSPIRA_LC_SOCKET={}\nSPIRA_LC_HOST=127.0.0.1\nSPIRA_LC_PORT={}\nSPIRA_LC_USER=root\n{}={}\n{}={}\n",
            run_dir.display(),
            dir.join("release/spira").display(),
            runner.display(),
            gh_state.display(),
            bin.display(),
            probe_command(&std::env::current_exe().map_err(|e| e.to_string())?, dir),
            crate::roundvm::WORLD_ENV,
            dir.display(),
            dir.join("release").display(),
            lc_socket(dir).display(),
            port.trim(),
            crate::agent::SCENARIO_VAR,
            dir.join("agent.scenario").display(),
            crate::agent::STATE_VAR,
            dir.join("agent-state").display()
        ),
    )
    .map_err(|e| e.to_string())?;
    steps.lc_serve(dir, &fixture)
}

/// The one registry row of a world: `name|path|land|base|format|gate|lanes`.
pub fn registry_rows(work: &Path) -> String {
    format!("{REPO_NAME}|{}|queue.local|{LANDING_BASE}|||plan\n", work.display())
}

/// The complete base with every path of the fixture's home moved under the world's `home`,
/// and without the fixture's own `repo.spira`: a world registers only the repository it built.
pub fn base_config(home: &Path) -> String {
    let moved = CONFIG_BASE.replace(FIXTURE_HOME, &home.display().to_string());
    let mut out = String::new();
    let mut skipping = false;
    for line in moved.lines() {
        if line.starts_with('[') {
            skipping = line == "[repo.spira]";
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// What the world's tools find at locators the base's moved paths leave empty: the release's
/// own harness tree, and the world's bead store.
pub fn home_settings(world: &Path, fixture: &str) -> Vec<(String, String)> {
    let spira = world.join("release/spira");
    vec![
        ("spira.prod".to_string(), spira.display().to_string()),
        ("spira.chamber".to_string(), spira.join("chamber").display().to_string()),
        ("spira.db".to_string(), fixture.trim().to_string()),
        ("spira.repo_map".to_string(), world.join(HOME).join(REGISTRY_ROWS).display().to_string()),
        ("spira.home_repo".to_string(), REPO_NAME.to_string()),
    ]
}

/// The world's own lifecycle locators (sp-hq1v76), written over the complete base so none of
/// them is the base's (production-default) value: its socket, the empty credential world up
/// wrote, and its run dir. `SPIRA_LC_HOST`/`PORT`/`USER` in `sim.env` pin a same-user
/// fallback connection to the world's own Dolt fixture as well.
pub fn lc_settings(world: &Path) -> Vec<(String, String)> {
    vec![
        ("spira.lc_socket".to_string(), lc_socket(world).display().to_string()),
        ("spira.lc_password_file".to_string(), world.join("config/lc-credential").display().to_string()),
        ("spira.run".to_string(), world.join("run").display().to_string()),
    ]
}

/// The `SIM_PROBE` line's command: `sim probe <world>`, shell-quoted, since it runs under `sh -c`.
pub fn probe_command(sim: &Path, world: &Path) -> String {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    format!("{} probe {}", q(sim), q(world))
}

/// The world's config settings, as `spira-config set` pairs. `spira-config set`
/// validates the whole document after every write, so the repository section goes in as
/// one JSON table: set field by field, `repo.sim.path` alone is refused (no `mode` yet).
pub fn config_settings(work: &Path, gh: &Path) -> Vec<(String, String)> {
    let repo = serde_json::json!({ "path": work.display().to_string(), "mode": "queue.local", "base": LANDING_BASE });
    vec![
        (format!("repo.{REPO_NAME}"), repo.to_string()),
        ("spira.gh".to_string(), gh.display().to_string()),
        ("spira.batcher_enable".to_string(), "1".to_string()),
    ]
}

/// Where the world's Dolt fixture lives. Nested inside testenv, `TESTDB_ROOT` is the
/// container's tmpfs root holding the template setup already built, and `testenv testdb`
/// there refuses (TESTDB_REQUIRE_TMPFS=1) any root not on a tmpfs; so a world uses that root
/// and a fixture costs a copy, not a fresh `bd init`. Elsewhere the world keeps its own.
pub fn db_root(world: &Path, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    match env("TESTDB_ROOT").filter(|v| !v.is_empty()) {
        Some(root) => PathBuf::from(root),
        None => world.join("db"),
    }
}

fn cache_dir(dir: &Path) -> PathBuf {
    match std::env::var_os("SPIRA_SIM_CACHE") {
        Some(c) if !c.is_empty() => PathBuf::from(c),
        _ => dir.join("cache"),
    }
}

fn link(target: &Path, at: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, at).map_err(|e| e.to_string())
}

fn set_exec(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}

pub fn down(dir: &Path, steps: &dyn Steps) -> Result<(), String> {
    if !is_world(dir) {
        return Err(format!("{} is not a sim world", dir.display()));
    }
    // The serve goes first: it holds a connection to the fixture db_down removes.
    if let Ok(pid) = std::fs::read_to_string(lc_pid_file(dir)) {
        let pid: u32 = pid.trim().parse().map_err(|e| format!("{}: {e}", lc_pid_file(dir).display()))?;
        steps.lc_stop(dir, pid)?;
    }
    if let Ok(fixture) = std::fs::read_to_string(dir.join("db.fixture")) {
        steps.db_down(fixture.trim())?;
    }
    std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
}
