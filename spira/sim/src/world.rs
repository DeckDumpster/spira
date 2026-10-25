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
/// Host-touching tools the loop calls by bare name at the end of a pass: `unit-ensure`
/// (re)installs the user's systemd units, `target-reap` removes shared build directories. In a
/// world each is `sim` invoked under that name, and does nothing.
pub const INERT_TOOLS: &[&str] = &["unit-ensure", "target-reap"];
/// What the world's `SPIRA_SUMMON` runs: `sim` under this name, standing where `systemd-run` stands.
pub const SUMMON_TOOL: &str = "sim-summon";

/// The world's `SPIRA_AGENT`: a launcher giving the stub agent the scenario and state paths the aeon's restricted environment drops.
pub const AGENT_LAUNCHER: &str = "sim-aeon-agent";

/// The script at [`AGENT_LAUNCHER`].
pub fn agent_launcher(agent: &Path, scenario: &Path, state: &Path) -> String {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    format!("#!/bin/sh\nexport {}={} {}={}\nexec {} \"$@\"\n", crate::agent::SCENARIO_VAR, q(scenario), crate::agent::STATE_VAR, q(state), q(agent))
}
/// The release binaries the world itself calls; a prebuilt release without them is refused.
const RELEASE_BINS: &[&str] = &["bin/spira-config", "bin/spira-lc"];
/// The world's lifecycle service socket, under the world dir (sp-hq1v76).
const LC_SOCKET: &str = "lc.sock";
const HOME: &str = "home";
/// The file the legacy bash readers still require, with the one row the world registers.
const REGISTRY_ROWS: &str = "registry-rows";
/// The name the world's one repository is registered under (`repo.<name>` in its config).
pub const REPO_NAME: &str = "sim";
/// The land modes a world's repository may be registered under.
pub const MODES: &[&str] = &["push", "pr", "hold", "queue", "queue.forge", "queue.local"];
const DEFAULT_MODE: &str = "queue.local";
const DEFAULT_TRUNK: &str = "main";
/// Where the repositories beyond the world's own live: `repos/<name>/{work,origin.git}`.
const EXTRA_REPOS: &str = "repos";
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
/// overrides each locator a world must own. Its `lc_socket` is empty (direct mode) and
/// is always overwritten below.
const CONFIG_BASE: &str = include_str!("../../../spira-config/tests/fixtures/complete.toml");

/// One repository of a world: `<name>:<mode>[:<trunk>]`. The world's own repository (`sim`) is
/// always present, `queue.local` unless named; any other name is a repository of its own,
/// created empty with its trunk branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSpec {
    pub name: String,
    pub mode: String,
    pub trunk: String,
}

impl RepoSpec {
    pub fn parse(spec: &str) -> Result<RepoSpec, String> {
        let bad = |why: &str| format!("usage: --repo {spec:?} is not <name>:<mode>[:<trunk>]: {why}");
        let parts: Vec<&str> = spec.split(':').collect();
        let (name, mode, trunk) = match parts.as_slice() {
            [n, m] => (*n, *m, DEFAULT_TRUNK),
            [n, m, t] => (*n, *m, *t),
            _ => return Err(bad("expected two or three fields")),
        };
        let word = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
        if !word(name) {
            return Err(bad("the name is lowercase letters, digits, - and _"));
        }
        if !MODES.contains(&mode) {
            return Err(bad(&format!("the mode is one of {}", MODES.join(", "))));
        }
        if !word(trunk) {
            return Err(bad("the trunk is a plain branch name"));
        }
        if name == REPO_NAME && trunk != DEFAULT_TRUNK {
            return Err(bad(&format!("the world's own repository keeps its {DEFAULT_TRUNK} trunk")));
        }
        Ok(RepoSpec { name: name.to_string(), mode: mode.to_string(), trunk: trunk.to_string() })
    }

    /// The ref its branches land on: a local branch for `queue.local`, the remote's trunk otherwise.
    pub fn base(&self) -> String {
        match self.mode.as_str() {
            "queue.local" => format!("local/{}", self.trunk),
            _ => format!("origin/{}", self.trunk),
        }
    }

    pub fn is_home(&self) -> bool {
        self.name == REPO_NAME
    }

    /// Its checkout inside `world`.
    pub fn work(&self, world: &Path) -> PathBuf {
        if self.is_home() { world.join("work") } else { world.join(EXTRA_REPOS).join(&self.name).join("work") }
    }

    pub fn origin(&self, world: &Path) -> PathBuf {
        if self.is_home() { world.join("origin.git") } else { world.join(EXTRA_REPOS).join(&self.name).join("origin.git") }
    }
}

/// The repositories of a world: the home repository first (as named, else `queue.local`), then
/// the others in the order given. A name given twice is refused.
pub fn repo_set(specs: &[RepoSpec]) -> Result<Vec<RepoSpec>, String> {
    let mut out: Vec<RepoSpec> = Vec::new();
    for s in specs {
        if out.iter().any(|o| o.name == s.name) {
            return Err(format!("usage: --repo names {} twice", s.name));
        }
        out.push(s.clone());
    }
    let home = match out.iter().position(RepoSpec::is_home) {
        Some(i) => out.remove(i),
        None => RepoSpec { name: REPO_NAME.to_string(), mode: DEFAULT_MODE.to_string(), trunk: DEFAULT_TRUNK.to_string() },
    };
    out.insert(0, home);
    Ok(out)
}

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
    /// The absolute `bd` the world's config names; its bead store is only reachable through it.
    fn bd(&self) -> Result<PathBuf, String>;
    fn lifecycle(&self, release: &Path, fixture: &str, lifecycle: &Path, config: &Path) -> Result<(), String>;
    fn config_set(&self, release: &Path, file: &Path, key: &str, value: &str) -> Result<(), String>;
    fn config_set_all(&self, release: &Path, file: &Path, pairs: &[(String, String)]) -> Result<(), String> {
        pairs.iter().try_for_each(|(k, v)| self.config_set(release, file, k, v))
    }
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
    ask_body(socket, argv).map(|(code, _)| i64::from(code))
}

/// One request to a lifecycle serve, as the CLI's own socket client makes it: `(exit code, stdout)`.
pub fn ask_body(socket: &Path, argv: &[&str]) -> Option<(i32, String)> {
    use std::io::{BufRead, BufReader, Read, Write};
    let stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
    writeln!(&stream, "{}", serde_json::to_string(argv).ok()?).ok()?;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let resp: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let code = i32::try_from(resp.get("exit_code")?.as_i64()?).ok()?;
    let mut body = vec![0u8; usize::try_from(resp.get("len")?.as_u64()?).ok()?];
    reader.read_exact(&mut body).ok()?;
    Some((code, String::from_utf8(body).ok()?))
}

#[derive(Default)]
pub struct ProcessSteps;

pub fn run(cmd: &mut Command, deadline: Duration) -> Result<String, String> {
    let name = format!("{:?}", cmd.get_program());
    let (status, stdout, stderr) = run_capture(cmd, deadline)?;
    if status.success() {
        Ok(stdout)
    } else {
        let why = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        Err(format!("{name}: {status}: {why}"))
    }
}

pub fn run_capture(cmd: &mut Command, deadline: Duration) -> Result<(std::process::ExitStatus, String, String), String> {
    run_capture_traced(cmd, deadline, None)
}

/// `run_capture`, and with `trace` set, the process tree the command spawned (`proctrace`).
pub fn run_capture_traced(cmd: &mut Command, deadline: Duration, trace: Option<(&Path, &str)>) -> Result<(std::process::ExitStatus, String, String), String> {
    let name = format!("{:?}", cmd.get_program());
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("{name}: {e}"))?;
    let tracer = trace.map(|_| crate::proctrace::start(child.id()));
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
    if let (Some(t), Some((dir, label))) = (tracer, trace) {
        use std::io::Write;
        let text = crate::proctrace::render(label, &t.finish());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(crate::proctrace::TRACE_FILE)) {
            let _ = f.write_all(text.as_bytes());
        }
    }
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
            Command::new(spira_config::release_env::tool("testenv")).args(["testdb", "up", "--tag", "simworld", "--bd", "bd", "--dolt", "dolt", "--root"]).arg(db_root(world, &|k| std::env::var(k).ok())),
            CALL_DEADLINE,
        )?;
        out.lines()
            .find_map(|l| l.strip_prefix("TESTDB_FIXTURE="))
            .map(str::to_string)
            .ok_or_else(|| "testenv testdb up printed no TESTDB_FIXTURE".to_string())
    }

    fn config_set(&self, release: &Path, file: &Path, key: &str, value: &str) -> Result<(), String> {
        self.config_set_all(release, file, &[(key.to_string(), value.to_string())])
    }

    fn config_set_all(&self, _release: &Path, file: &Path, pairs: &[(String, String)]) -> Result<(), String> {
        if !file.exists() {
            std::fs::write(file, "").map_err(|e| format!("{}: {e}", file.display()))?;
        }
        let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        spira_config::set_paths_in_file(file, &pairs)
    }

    fn lifecycle(&self, release: &Path, fixture: &str, lifecycle: &Path, config: &Path) -> Result<(), String> {
        let port = std::fs::read_to_string(Path::new(fixture).join("server.port")).map_err(|e| format!("fixture has no server.port: {e}"))?;
        let toml = config.join("lc.toml");
        let credential = config.join("lc-credential");
        std::fs::write(&credential, "").map_err(|e| e.to_string())?;
        let world = config.parent().ok_or("config dir has no parent")?;
        // A complete config (spira-lc resolves only from one that declares every key) in
        // declared direct mode: a dead or absent socket is no longer a route to the store
        // (sp-yvyvzd), so these admin and probe reads name direct mode outright.
        if !toml.exists() {
            std::fs::write(&toml, base_config(&world.join(HOME))).map_err(|e| format!("{}: {e}", toml.display()))?;
        }
        self.config_set(release, &toml, "spira.lc_password_file", &credential.display().to_string())?;
        self.config_set(release, &toml, "spira.lc_socket", "")?;
        let lc = |verb: &str, arg: PathBuf| run(lc_command(release, config, &port).arg(verb).arg(arg), CALL_DEADLINE).map(|_| ());
        let t = Instant::now();
        lc("admin-apply-ddl", lifecycle.join("schema.sql"))?;
        mark(world, "up.lifecycle-schema.apply-ddl", t);
        let t = Instant::now();
        lc("admin-migrate", lifecycle.join("migrations"))?;
        mark(world, "up.lifecycle-schema.migrate", t);
        Ok(())
    }

    fn bd(&self) -> Result<PathBuf, String> {
        on_path("bd", &std::env::var("PATH").unwrap_or_default()).ok_or_else(|| "bd is not on PATH: a world's bead store is read through it".to_string())
    }

    fn db_down(&self, fixture: &str) -> Result<(), String> {
        run(Command::new(spira_config::release_env::tool("testenv")).args(["testdb", "down", "--fixture", fixture]), CALL_DEADLINE).map(|_| ())
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
            std::thread::sleep(Duration::from_millis(20));
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

/// `spira-lc` against the world's own lifecycle store and nothing else: a cleared environment
/// but for the world's release (where spira-lc locates its config registry), the world's
/// `config/lc.toml` (complete, direct mode), and its private Dolt server's port.
pub fn lc_command(release: &Path, config: &Path, port: &str) -> Command {
    let mut cmd = Command::new(release.join("bin/spira-lc"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("SPIRA_RELEASE", release)
        .env("SPIRA_TOML", config.join("lc.toml"))
        .env("SPIRA_LC_HOST", "127.0.0.1")
        .env("SPIRA_LC_PORT", port.trim())
        .env("SPIRA_LC_USER", "root")
        .env("SPIRA_LC_ADMIN_USER", "root");
    cmd
}

/// How often the batcher polls a round's jobs inside a world: they finish in milliseconds.
const BATCHER_POLL_SECS: &str = "0.1";

/// A world's `bd` is a Go binary started dozens of times for one short call each: its collector
/// and extra threads are most of the CPU a call spends beyond the work.
const GO_RUNTIME_ENV: &str = "GOGC=off\nGOMAXPROCS=2\n";

pub const PROFILE: &str = "profile.tsv";

/// Appends `<phase>\t<ms since start>` to the world's profile; the phase costs of a run.
pub fn mark(dir: &Path, phase: &str, start: Instant) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(PROFILE)) {
        let _ = writeln!(f, "{phase}\t{}", start.elapsed().as_millis());
    }
}

pub fn is_world(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

pub fn up(repo: &Path, dir: &Path, tree: &str, env: &dyn Fn(&str) -> Option<String>, steps: &dyn Steps) -> Result<(), String> {
    up_resolving(repo, dir, tree, env, steps, &|_| Ok(()))
}

/// [`up`], and `resolved` runs once the world's files and seed are in place, beside the store's
/// schema: what a caller does to the world that does not need the store.
pub fn up_resolving(repo: &Path, dir: &Path, tree: &str, env: &dyn Fn(&str) -> Option<String>, steps: &dyn Steps, resolved: &(dyn Fn(&Path) -> Result<(), String> + Sync)) -> Result<(), String> {
    up_repos(repo, dir, tree, env, steps, &[], resolved)
}

/// [`up_resolving`] with the world's repositories named (see [`repo_set`]).
pub fn up_repos(repo: &Path, dir: &Path, tree: &str, env: &dyn Fn(&str) -> Option<String>, steps: &dyn Steps, repos: &[RepoSpec], resolved: &(dyn Fn(&Path) -> Result<(), String> + Sync)) -> Result<(), String> {
    let repos = repo_set(repos)?;
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
    match build(&dir, &repo, tree, &source, steps, &repos, resolved) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = down(&dir, steps);
            Err(e)
        }
    }
}

fn build(dir: &Path, repo: &Path, tree: &str, source: &ReleaseSource, steps: &dyn Steps, repos: &[RepoSpec], resolved: &(dyn Fn(&Path) -> Result<(), String> + Sync)) -> Result<(), String> {
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

    let origin = dir.join("origin.git");
    let work = dir.join("work");
    let runner = dir.join("config/suite-runner");
    let run_dir = dir.join("run");
    let config = dir.join("config");
    std::fs::create_dir_all(&run_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&config).map_err(|e| e.to_string())?;
    let seed = ensure_seed(repo, tree)?;
    let write_files = |fixture: &str| -> Result<(), String> {
        let runner = config.join("suite-runner");
        std::fs::write(&runner, SUITE_RUNNER).map_err(|e| e.to_string())?;
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
        link(&sim_exe, &bin.join(SUMMON_TOOL))?;
        let launcher = bin.join(AGENT_LAUNCHER);
        std::fs::write(&launcher, agent_launcher(&sim_exe.with_file_name("sim-agent"), &dir.join("agent.scenario"), &dir.join("agent-state"))).map_err(|e| e.to_string())?;
        set_exec(&launcher)?;
        for tool in INERT_TOOLS {
            link(&sim_exe, &bin.join(tool))?;
        }
        crate::roundvm::write_verdict(dir, &crate::roundvm::Verdict::Green)?;
        let home = dir.join(HOME);
        std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        std::fs::write(home.join(REGISTRY_ROWS), registry_rows(dir, repos)).map_err(|e| e.to_string())?;
        std::fs::write(&file, base_config(&home)).map_err(|e| e.to_string())?;
        let t = Instant::now();
        let bd = steps.bd()?;
        let settings: Vec<(String, String)> = config_settings(dir, repos, &gh_bin).into_iter().chain(home_settings(dir, fixture, &bd)).chain(lc_settings(dir)).collect();
        steps.config_set_all(&release, &file, &settings)?;
        mark(dir, "up.config-set", t);
        let port = std::fs::read_to_string(Path::new(fixture).join("server.port")).unwrap_or_default();
        std::fs::write(
            config.join("sim.env"),
            format!(
                "SPIRA_RUN={}\nSPIRA_HOME={}\nSPIRA_SIM_GATE_RUNNER={}\nSIM_GH_DIR={}\nSIM_BIN={}\nSIM_PROBE={}\n{}={}\nSPIRA_RELEASE={}\nSPIRA_LC_SOCKET={}\nSPIRA_LC_HOST=127.0.0.1\nSPIRA_LC_PORT={}\nSPIRA_LC_USER=root\nSIM_BEADS_DB={}\n{}={}\n{}={}\nSPIRA_BATCHER_POLL_SECS={}\nSPIRA_SUMMON={}\n{}",
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
                bead_db(fixture).display(),
                crate::agent::SCENARIO_VAR,
                dir.join("agent.scenario").display(),
                crate::agent::STATE_VAR,
                dir.join("agent-state").display(),
                BATCHER_POLL_SECS,
                bin.join(SUMMON_TOOL).display(),
                GO_RUNTIME_ENV
            ),
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    };
    // The seed copy and the store's schema do not meet until the config names the repo; the
    // resolved config needs only the seed and the written files, so it runs beside the schema.
    let fixture = std::thread::scope(|scope| -> Result<String, String> {
        // `written` drops with this closure, so a failure here releases the thread that waits on it.
        let (written, ready) = std::sync::mpsc::channel::<()>();
        let (seed_ref, origin_ref, work_ref, runner_ref) = (&seed, &origin, &work, &runner);
        let repos_ref = repos;
        let placed = scope.spawn(move || {
            let t = Instant::now();
            let r = place_seed(seed_ref, origin_ref, work_ref, runner_ref);
            mark(dir, "up.seed-repo", t);
            r?;
            for spec in repos_ref.iter().filter(|r| !r.is_home()) {
                place_extra(dir, spec, runner_ref)?;
            }
            ready.recv().map_err(|_| "the world's files were not written".to_string())?;
            let t = Instant::now();
            let r = resolved(dir);
            mark(dir, "up.conf-resolve", t);
            r
        });
        let t = Instant::now();
        let fixture = steps.db_up(dir)?;
        std::fs::write(dir.join("db.fixture"), &fixture).map_err(|e| e.to_string())?;
        mark(dir, "up.db", t);
        write_files(&fixture)?;
        let _ = written.send(());
        let t = Instant::now();
        steps.lifecycle(&release, &fixture, &seed.join("work/lifecycle"), &config)?;
        mark(dir, "up.lifecycle-schema", t);
        placed.join().map_err(|_| "the seed copy panicked".to_string())??;
        Ok(fixture)
    })?;
    let t = Instant::now();
    let served = steps.lc_serve(dir, &fixture);
    mark(dir, "up.lc-serve", t);
    served
}

const SEED_KEEP: usize = 4;
const SEED_IDLE: Duration = Duration::from_secs(3600);

/// Where seed repos are kept between worlds: `SPIRA_SIM_CACHE`, else a directory of the
/// temp dir, so every world of a run (and of a suite's TMPDIR) shares one.
fn seed_cache_dir() -> PathBuf {
    match std::env::var_os("SPIRA_SIM_CACHE") {
        Some(c) if !c.is_empty() => PathBuf::from(c),
        _ => std::env::temp_dir().join("sim-cache"),
    }
}

/// A world's landing repository: the seed copied in, plus the world's own `gate.steps` as one
/// commit, since that names the world's runner.
fn place_seed(seed: &Path, origin: &Path, work: &Path, runner: &Path) -> Result<(), String> {
    let p = |x: &Path| x.to_string_lossy().to_string();
    let _ = std::fs::File::open(seed).and_then(|d| d.set_modified(std::time::SystemTime::now()));
    copy_repo(&seed.join("origin.git"), origin).map_err(|e| format!("{}: {e}", origin.display()))?;
    copy_repo(&seed.join("work"), work).map_err(|e| format!("{}: {e}", work.display()))?;
    git(work, &["remote", "set-url", "origin", &p(origin)])?;
    std::fs::write(work.join("gate.steps"), stub_gate_steps(runner)).map_err(|e| e.to_string())?;
    git(work, &["add", "gate.steps"])?;
    git(work, &["commit", "-q", "-m", "sim gate steps"])?;
    git(work, &["push", "-q", "origin", "main"])?;
    git(work, &["branch", LANDING_BASE, "main"])?;
    Ok(())
}

/// A repository beyond the world's own: one commit (the stub gate string, so its gate runs the
/// world's runner) on its trunk, pushed to its bare origin; a `queue.local` one also gets its
/// local landing branch.
fn place_extra(world: &Path, spec: &RepoSpec, runner: &Path) -> Result<(), String> {
    let (origin, work) = (spec.origin(world), spec.work(world));
    let p = |x: &Path| x.to_string_lossy().to_string();
    let branch = format!("--initial-branch={}", spec.trunk);
    let parent = work.parent().ok_or("a repository has a parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    git(parent, &["init", "-q", &branch, &p(&work)])?;
    git(parent, &["init", "-q", "--bare", &branch, &p(&origin)])?;
    std::fs::write(work.join("gate.steps"), stub_gate_steps(runner)).map_err(|e| e.to_string())?;
    git(&work, &["add", "gate.steps"])?;
    git(&work, &["commit", "-q", "-m", &format!("sim repo {}", spec.name)])?;
    git(&work, &["remote", "add", "origin", &p(&origin)])?;
    git(&work, &["push", "-q", "-u", "origin", &spec.trunk])?;
    if spec.mode == "queue.local" {
        git(&work, &["branch", &spec.base(), &spec.trunk])?;
    }
    Ok(())
}

/// The landing repository's seed: the tree under test committed once with an origin it is
/// pushed to, built the first time a tree is seen. `sim world seed` builds it if no world has yet, so the worlds that
/// follow (side by side, or in a timed run) only copy it.
pub fn ensure_seed(repo: &Path, tree: &str) -> Result<PathBuf, String> {
    let repo = PathBuf::from(git(repo, &["rev-parse", "--show-toplevel"])?.trim());
    let hash = git(&repo, &["rev-parse", &format!("{tree}^{{tree}}")])?.trim().to_string();
    let cache = seed_cache_dir();
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let seed = cache.join(format!("seed-{hash}"));
    if !seed.join(".complete").is_file() {
        build_seed(&repo, tree, &cache, &seed)?;
    }
    Ok(seed)
}

/// What a world's checkouts hold: the root files and the directories a world path reads from a
/// worktree (workflows, lifecycle schema, the agent's `out/`, the sim's tables, timer units,
/// harness scripts, the allow list the commit hook's fence-scripts rule reads), not crate sources, docs, test suites or the config registry. A new
/// worktree inherits the patterns, so every checkout the landing code makes is a fraction of the tree.
const SPARSE_PATTERNS: &str = "/*\n!/*/\n/.github/\n/lifecycle/\n/out/\n/sim/\n/spira/\n/systemd/\n!/spira/test-*.sh\n!/spira/conf.d/\n/spira-lint/fence-scripts-allow\n";

/// A copy of a seed repo, its pack files linked rather than copied: a pack is never written to
/// again, where refs, the index and the reflogs are, and those are copied.
fn copy_repo(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_repo(&src, &dst)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else if src.parent().is_some_and(|d| d.ends_with("objects/pack")) && std::fs::hard_link(&src, &dst).is_ok() {
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn build_seed(repo: &Path, tree: &str, cache: &Path, seed: &Path) -> Result<(), String> {
    let name = seed.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    static BUILDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = BUILDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = cache.join(format!("{name}.build-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (origin, work) = (tmp.join("origin.git"), tmp.join("work"));
    let p = |x: &Path| x.to_string_lossy().to_string();
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    git(&tmp, &["init", "--bare", "--initial-branch=main", &p(&origin)])?;
    git(&tmp, &["init", "--initial-branch=main", &p(&work)])?;
    let tarball = tmp.join("seed.tar");
    run(Command::new("git").arg("-C").arg(repo).args(["archive", "--format=tar", "-o"]).arg(&tarball).arg(tree), BUILD_DEADLINE)?;
    run(Command::new("tar").arg("-xf").arg(&tarball).arg("-C").arg(&work), CALL_DEADLINE)?;
    std::fs::remove_file(&tarball).map_err(|e| e.to_string())?;
    git(&work, &["add", "-A"])?;
    git(&work, &["commit", "-q", "-m", "sim seed"])?;
    git(&work, &["remote", "add", "origin", &p(&origin)])?;
    git(&work, &["push", "-q", "origin", "main"])?;
    git(&work, &["branch", "--set-upstream-to=origin/main", "main"])?;
    git(&work, &["config", "core.sparseCheckout", "true"])?;
    std::fs::write(work.join(".git/info/sparse-checkout"), SPARSE_PATTERNS).map_err(|e| e.to_string())?;
    git(&work, &["read-tree", "-mu", "HEAD"])?;
    // A pack is a file to copy where the loose objects were thousands.
    git(&work, &["repack", "-adq"])?;
    git(&origin, &["repack", "-adq"])?;
    std::fs::write(tmp.join(".complete"), "").map_err(|e| e.to_string())?;
    // A world that finished first already put the seed there: keep that one.
    if std::fs::rename(&tmp, seed).is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
        if !seed.join(".complete").is_file() {
            return Err(format!("{}: could not be created", seed.display()));
        }
    }
    prune_seeds(cache, seed);
    Ok(())
}

fn prune_seeds(cache: &Path, keep: &Path) {
    let mut seeds: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(cache)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("seed-") && !e.file_name().to_string_lossy().contains(".build-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    seeds.sort();
    let excess = seeds.len().saturating_sub(SEED_KEEP);
    let idle = std::time::SystemTime::now() - SEED_IDLE;
    for (_, path) in seeds.into_iter().take(excess).filter(|(used, p)| p != keep && *used < idle) {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// The world's suite runner. The last line of `gate-control` naming the branch's bead picks its verdict:
/// `fail` (exit 1), `fail-once` (exit 1, then passes), `no-verdict` (75, a timeout) or `infra` (75, a harness
/// fault); any other mode, or none, passes. Every run is appended to `gate-calls`.
pub const SUITE_RUNNER: &str = r#"#!/bin/sh
dir=$(dirname "$0")
bead=${SPIRA_GATE_BRANCH#spira/}
echo "$bead" >> "$dir/gate-calls"
mode=$(awk -v b="$bead" '$1 == b { m = $2 } END { print m }' "$dir/gate-control" 2>/dev/null)
case "$mode" in
fail) echo "gate: VERDICT=FAIL reason=suite-red suite=sim-planted.sh"; exit 1 ;;
fail-once) echo "$bead pass" >> "$dir/gate-control"; echo "gate: VERDICT=FAIL reason=suite-red suite=sim-planted.sh"; exit 1 ;;
no-verdict) echo "gate: VERDICT=NO_VERDICT reason=timeout"; exit 75 ;;
infra) echo "gate: VERDICT=NO_VERDICT reason=harness-fault"; exit 75 ;;
esac
exit 0
"#;

/// The gate string the tree under test carries inside a world: the stub suite runner alone, whose
/// verdict a scenario decides. The tree's own gate builds tools with cargo and runs suites.
pub fn stub_gate_steps(runner: &Path) -> String {
    format!("step \"{}\"\n", runner.display())
}

/// The registry rows of a world, one per repository: `name|path|land|base|format|gate|lanes`.
pub fn registry_rows(world: &Path, repos: &[RepoSpec]) -> String {
    repos.iter().map(|r| format!("{}|{}|{}|{}|||plan\n", r.name, r.work(world).display(), r.mode, r.base())).collect()
}

/// The complete base with every path of the fixture's home moved under the world's `home`,
/// and without the fixture's own `repo.spira`: a world registers only the repository it built.
pub fn base_config(home: &Path) -> String {
    let release = home.parent().unwrap_or(home).join("release");
    let moved = CONFIG_BASE
        .replace(&format!("{FIXTURE_HOME}/spira/spira-releases/current"), &release.display().to_string())
        .replace(FIXTURE_HOME, &home.display().to_string());
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

/// The workspace of a testdb fixture: where `bd` finds the bead store.
pub fn bead_db(fixture: &str) -> PathBuf {
    Path::new(fixture.trim()).join("ws")
}

/// `name` as an absolute path from `path`'s directories (a world's config names tools absolutely).
pub fn on_path(name: &str, path: &str) -> Option<PathBuf> {
    path.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join(name)).find(|p| p.is_file())
}

/// What the world's tools find at locators the base's moved paths leave empty: the release's
/// own harness tree, and the world's bead store.
pub fn home_settings(world: &Path, fixture: &str, bd: &Path) -> Vec<(String, String)> {
    let spira = world.join("release/spira");
    vec![
        ("spira.bd".to_string(), bd.display().to_string()),
        ("spira.prod".to_string(), spira.display().to_string()),
        ("spira.chamber".to_string(), spira.join("chamber").display().to_string()),
        ("spira.db".to_string(), bead_db(fixture).display().to_string()),
        ("spira.repo_map".to_string(), world.join(HOME).join(REGISTRY_ROWS).display().to_string()),
        ("spira.home_repo".to_string(), REPO_NAME.to_string()),
        ("spira.agent".to_string(), world.join("bin").join(AGENT_LAUNCHER).display().to_string()),
        ("spira.aeon_warm_base".to_string(), world.join("warm").display().to_string()),
        ("spira.summon_jitter".to_string(), "0".to_string()),
    ]
}

/// The world's own lifecycle locators (sp-hq1v76), written over the complete base so none of
/// them is the base's value: its socket, the empty credential world up
/// wrote, and its run dir. `SPIRA_LC_HOST`/`PORT`/`USER` in `sim.env` name the world's own
/// Dolt fixture for a reader that declares direct mode (`spira.lc_socket = ""`); a declared
/// socket never falls back to it (sp-yvyvzd).
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
pub fn config_settings(world: &Path, repos: &[RepoSpec], gh: &Path) -> Vec<(String, String)> {
    let section = |r: &RepoSpec| (format!("repo.{}", r.name), serde_json::json!({ "path": r.work(world).display().to_string(), "mode": r.mode, "base": r.base() }).to_string());
    repos
        .iter()
        .map(section)
        .chain([("spira.gh".to_string(), gh.display().to_string()), ("spira.batcher_enable".to_string(), "1".to_string()), ("spira.mail_mute".to_string(), "false".to_string())])
        .collect()
}

/// The root of the world's Maildirs: the complete base's `mail`, moved under the world's home.
pub fn mail_root(world: &Path) -> PathBuf {
    world.join(HOME).join("spira/run/mail")
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
