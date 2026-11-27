//! The sandbox unit runner (sp-yx0yu9): a filtered unit run's `spira/unit-tests.sh` under
//! bubblewrap on the host instead of in the fixture container. The container exists to stop a
//! test reaching the host — the harness repo's git config, `$HOME`, the user manager, the
//! production config — and a kernel namespace buys that without an image, a volume or a second
//! compile. `testenv.unit_runner` selects it; `container` keeps every run exactly as before, so
//! the container path stays the retreat.
//!
//! Inside: the root read-only, a fresh tmpfs `$HOME` and `/tmp`, only the run's worktree and its
//! branch's cargo target writable, the cargo registry writable (cargo locks it), the
//! environment cleared to an allow-list, no network, its own PID/IPC/UTS namespaces, killed with
//! testenv.

use std::fs;
use std::io::{BufRead, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Which runner a unit run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    Container,
    Sandbox,
}

impl Runner {
    pub fn parse(v: &str) -> Result<Runner, String> {
        match v.trim() {
            "container" => Ok(Runner::Container),
            "sandbox" => Ok(Runner::Sandbox),
            other => Err(format!("must be container or sandbox, got: {other:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Runner::Container => "container",
            Runner::Sandbox => "sandbox",
        }
    }
}

/// What one sandboxed run sees, every path absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The run's worktree, writable: the script and the crates it builds.
    pub tree: PathBuf,
    /// The real uid of the process that starts `bwrap`.
    pub caller_uid: u32,
    /// This branch's cargo target, writable, kept between runs.
    pub target: PathBuf,
    /// The caller's home directory, replaced by a tmpfs inside.
    pub home: PathBuf,
    /// The git directory the worktree's `.git` file names, read-only inside.
    pub gitdir: Option<PathBuf>,
    /// Read-only paths under `home` the build needs back: the rustup toolchains, cargo's
    /// proxies, the pinned tool env.
    pub ro_under_home: Vec<PathBuf>,
    /// Writable paths under `home`: cargo's registry and git checkouts (cargo locks them).
    pub rw_under_home: Vec<PathBuf>,
    /// The `PATH` inside.
    pub path: String,
    /// `RUST_TEST_THREADS` inside.
    pub test_threads: u64,
    /// The compilation cache every branch's run shares; `None` compiles uncached.
    pub cache: Option<Cache>,
}

/// A host-local sccache store and the binary that serves it. It is a directory, not the box's
/// network store: the sandbox has no network, and each run's own server is gone with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    pub dir: PathBuf,
    pub sccache: PathBuf,
}

/// Where under home the sandbox's tools live: on its `PATH`, in this order.
const TOOL_DIRS: [&str; 2] = [".local/lib/spira-env/bin", ".cargo/bin"];

/// The store inside the target root. A dot name, so [`prune_targets`] never counts it a branch.
pub const CACHE_DIR_NAME: &str = ".sccache";

impl Cache {
    /// The store under `root`, served by the `sccache` among `ro_under_home`'s bin directories;
    /// `None` when there is no sccache to serve it.
    pub fn under(root: &Path, home: &Path) -> Option<Cache> {
        if root.as_os_str().is_empty() {
            return None;
        }
        let sccache = TOOL_DIRS.iter().map(|d| home.join(d).join("sccache")).find(|p| p.is_file())?;
        Some(Cache { dir: root.join(CACHE_DIR_NAME), sccache })
    }
}

impl Profile {
    /// The profile for `tree` with its target at `target`, from this process's home. `root` is
    /// the directory the targets live in, which also holds the shared compilation store.
    pub fn for_run(tree: &Path, target: &Path, root: &Path, home: &Path, test_threads: u64) -> Profile {
        let under = |rel: &str| home.join(rel);
        let ro = [".rustup", ".cargo/bin", ".local/lib/spira-env"];
        let rw = [".cargo/registry", ".cargo/git"];
        let path = TOOL_DIRS
            .iter()
            .map(|d| under(d).display().to_string())
            .chain([system_paths::USR_LOCAL_BIN.into(), system_paths::USR_BIN.into(), system_paths::BIN.into()])
            .collect::<Vec<_>>()
            .join(":");
        Profile {
            tree: tree.to_path_buf(),
            // SAFETY: getuid has no preconditions and cannot fail.
            caller_uid: unsafe { libc::getuid() },
            target: target.to_path_buf(),
            home: home.to_path_buf(),
            gitdir: worktree_gitdir(tree),
            ro_under_home: ro.iter().map(|r| under(r)).filter(|p| p.exists()).collect(),
            rw_under_home: rw.iter().map(|r| under(r)).filter(|p| p.exists()).collect(),
            path,
            test_threads,
            cache: Cache::under(root, home),
        }
    }

    /// The environment inside, in full: nothing of the caller's passes but these.
    pub fn env(&self) -> Vec<(String, String)> {
        let s = |p: &Path| p.display().to_string();
        let mut env = vec![
            ("PATH".into(), self.path.clone()),
            ("HOME".into(), s(&self.home)),
            ("USER".into(), self.home.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            ("TMPDIR".into(), system_paths::TMP.into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("CARGO_HOME".into(), s(&self.home.join(".cargo"))),
            ("RUSTUP_HOME".into(), s(&self.home.join(".rustup"))),
            ("CARGO_TARGET_DIR".into(), TARGET_MOUNT.into()),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
            ("CARGO_TERM_COLOR".into(), "never".into()),
            ("RUST_TEST_THREADS".into(), self.test_threads.to_string()),
            // The image's system-level git identity (spira/testenv/Containerfile), at the same
            // lowest priority, so a test's own repo identity still wins (sp-fdsfq1).
            ("GIT_CONFIG_SYSTEM".into(), s(&self.gitconfig())),
        ];
        if let Some(c) = &self.cache {
            env.extend([
                ("RUSTC_WRAPPER".into(), s(&c.sccache)),
                ("SCCACHE_DIR".into(), s(&c.dir)),
                ("SCCACHE_IGNORE_SERVER_IO_ERROR".into(), "1".into()),
                // sccache keys on every CARGO_* variable, and does not cache an incremental crate.
                ("CARGO_INCREMENTAL".into(), "0".into()),
            ]);
        }
        env
    }

    /// `bwrap`'s arguments up to (not including) the command. Order matters: a later mount
    /// lands on top of an earlier one, so the tmpfs over `home` and `/tmp` come before the
    /// paths bound back under them.
    pub fn bwrap_args(&self) -> Vec<String> {
        let s = |p: &Path| p.display().to_string();
        let mut a: Vec<String> = [
            "--die-with-parent",
            "--new-session",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-net",
            "--unshare-cgroup-try",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            system_paths::DEV,
            "--proc",
            system_paths::PROC,
            "--tmpfs",
            system_paths::TMP,
            "--tmpfs",
            system_paths::RUN,
        ]
        .iter()
        .map(|x| x.to_string())
        .collect();
        // No test runs as uid 0, as in tools/test-sandbox/runner.
        if self.caller_uid == 0 {
            let id = crate::container::SPIRA_UID.to_string();
            a.extend(["--unshare-user".into(), "--uid".into(), id.clone(), "--gid".into(), id]);
        }
        a.extend(["--tmpfs".into(), s(&self.home)]);
        for p in &self.ro_under_home {
            a.extend(["--ro-bind".into(), s(p), s(p)]);
        }
        for p in &self.rw_under_home {
            a.extend(["--bind".into(), s(p), s(p)]);
        }
        if let Some(g) = &self.gitdir {
            a.extend(["--ro-bind".into(), s(g), s(g)]);
        }
        // The tree at one path whatever the host's worktree is called (sp-a2thtg): a branch's target
        // is shared by every run of that branch, and the source paths cargo compiles in must still
        // name a real file when the next run's worktree lives somewhere else.
        a.extend(["--bind".into(), s(&self.tree), TREE_MOUNT.into()]);
        a.extend(["--bind".into(), s(&self.target), TARGET_MOUNT.into()]);
        a.extend(["--ro-bind-try".into(), s(&self.tree.join(CARGO_CONFIG_INSIDE)), format!("{TREE_MOUNT}/.cargo/config.toml")]);
        if let Some(c) = &self.cache {
            a.extend(["--bind".into(), s(&c.dir), s(&c.dir)]);
        }
        a.extend(["--chdir".into(), TREE_MOUNT.into()]);
        a.push("--clearenv".into());
        for (k, v) in self.env() {
            a.extend(["--setenv".into(), k, v]);
        }
        a
    }
}

/// Where the run's tree is inside the sandbox, the same for every run: under the sandbox's own
/// tmpfs /tmp, since the read-only root cannot take a new mountpoint.
pub const TREE_MOUNT: &str = system_paths::guest::TMP_SANDBOX_TREE;

/// The cargo config a test sees: the workspace's without its target runner, so cargo inside the
/// sandbox launches each test binary directly (`tools/test-sandbox/runner --around` is the entry).
const CARGO_CONFIG_INSIDE: &str = "tools/test-sandbox/cargo-config.toml";

/// Where the run's cargo target is inside the sandbox, the same for every branch: sccache keys on
/// `CARGO_TARGET_DIR`, so one path is what lets branches share compiled dependencies.
pub const TARGET_MOUNT: &str = system_paths::guest::TMP_SANDBOX_TARGET;

/// The unit script as the sandbox sees it.
pub fn mounted_script(script: &str) -> PathBuf {
    Path::new(TREE_MOUNT).join("spira").join(script)
}

/// The image's system git identity (`git config --system` in spira/testenv/Containerfile).
pub const GIT_IDENTITY: &str = "[user]\n\tname = spirauser\n\temail = spirauser@localhost\n";

impl Profile {
    /// The system gitconfig the sandbox names, kept beside the branch target.
    pub fn gitconfig(&self) -> PathBuf {
        let mut p = self.target.clone().into_os_string();
        p.push(".gitconfig");
        PathBuf::from(p)
    }

    /// Writes [`GIT_IDENTITY`] where [`Self::gitconfig`] names it.
    pub fn write_gitconfig(&self) -> Result<(), String> {
        fs::write(self.gitconfig(), GIT_IDENTITY).map_err(|e| format!("cannot write {}: {e}", self.gitconfig().display()))
    }
}

/// The repository's git directory a worktree's `.git` file points into (`gitdir: …/worktrees/x`),
/// as the common `.git` that holds it; `None` for a plain checkout or an unreadable file.
pub fn worktree_gitdir(tree: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(tree.join(".git")).ok()?;
    let dir = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
    // `<repo>/.git/worktrees/<name>` → `<repo>/.git`, which the worktree's objects live in.
    let common = dir.parent().filter(|p| p.ends_with("worktrees")).and_then(Path::parent).map(Path::to_path_buf);
    Some(common.unwrap_or(dir))
}

/// This branch's target under `root`, keyed as the container's volume is.
pub fn target_dir(root: &Path, volume: &str) -> PathBuf {
    root.join(volume)
}

/// The cargo target a sandboxed unit run builds in: the calling session's owned build directory
/// (sp-ke0uk8) when it has one, so the run shares the session's one target; else this branch's
/// target under `root`. None when neither exists (an empty `root` and no owned directory).
pub fn unit_target(owned: Option<PathBuf>, root: &str, volume: &str) -> Option<PathBuf> {
    let root = root.trim();
    owned.or_else(|| (!root.is_empty()).then(|| target_dir(Path::new(root), volume)))
}

/// The tests that need a running system and so stay in the container (sp-7p5la8), read from
/// the tree's `testenv/container-tests.txt`: one name per line, `#` comments. A missing file
/// lists none.
pub fn container_tests(tree: &Path) -> Vec<String> {
    fs::read_to_string(tree.join(CONTAINER_TESTS_REL))
        .unwrap_or_default()
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

pub const CONTAINER_TESTS_REL: &str = "testenv/container-tests.txt";

/// The named tests that are listed as needing the container; a run naming any of them runs
/// there. A filter matches a listed name as cargo's filter would: by substring.
pub fn needing_container<'a>(filters: &'a [String], listed: &[String]) -> Vec<&'a str> {
    filters.iter().map(String::as_str).filter(|f| listed.iter().any(|l| l.contains(f) || f.contains(l.as_str()))).collect()
}

/// Where one unit run goes. `because` names the listed tests that sent a filtered run to the
/// container; it is empty when the run is simply not a sandbox run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Sandbox { skip: Vec<String> },
    Container { because: String },
}

/// An unfiltered sandbox run skips every listed test; a filtered one that names a listed test
/// goes to the container, since skipping would run nothing it asked for.
pub fn route(tests: Option<&[String]>, unit_only: bool, runner: Runner, listed: &[String]) -> Route {
    if !unit_only || runner != Runner::Sandbox {
        return Route::Container { because: String::new() };
    }
    match tests {
        None => Route::Sandbox { skip: listed.to_vec() },
        Some(t) => match needing_container(t, listed).join(",") {
            b if b.is_empty() => Route::Sandbox { skip: Vec::new() },
            because => Route::Container { because },
        },
    }
}

/// Removes the least recently used branch targets under `root` past `keep`, never `current`.
/// Returns the paths removed. A target's age is its directory's mtime, which every run touches.
pub fn prune_targets(root: &Path, current: &Path, keep: usize) -> Vec<PathBuf> {
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path()))
        .collect();
    dirs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut kept = usize::from(dirs.iter().any(|(_, p)| p == current));
    let mut removed = Vec::new();
    for (_, p) in dirs {
        if p == current {
            continue;
        }
        if kept < keep {
            kept += 1;
        } else if fs::remove_dir_all(&p).is_ok() {
            removed.push(p);
        }
    }
    removed
}

pub use spira_config::foreign_cache::touch;

/// How one sandboxed run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub rc: Option<i32>,
    pub secs: u64,
    pub timed_out: bool,
    /// The signal that stopped the run from outside, and who sent it; `None` for a run that
    /// ended by itself or at its own deadline.
    pub stopped: Option<Stopped>,
}

pub use gate::stopped::Stopped;

/// Where the supervisor records a stop for the run whose output is `out`: a file the sandbox
/// cannot write, so nothing the tests print passes for a sender.
fn stop_file(out: &Path) -> PathBuf {
    let mut p = out.as_os_str().to_owned();
    p.push(".stopped");
    PathBuf::from(p)
}

/// The first stop the supervisor recorded.
fn stopped_in(stop_file: &Path) -> Option<Stopped> {
    let file = fs::File::open(stop_file).ok()?;
    std::io::BufReader::new(file).split(b'\n').flatten().find_map(|l| Stopped::parse(&String::from_utf8_lossy(&l)))
}

/// The first argument of the hidden subcommand that runs [`supervise`].
pub const SUPERVISE_ARG: &str = "sandbox-supervise";

fn proc_path(pid: u32, file: &str) -> PathBuf {
    Path::new(system_paths::PROC).join(pid.to_string()).join(file)
}

fn proc_text(pid: u32, file: &str) -> Option<String> {
    let raw = fs::read(proc_path(pid, file)).ok()?;
    let text = String::from_utf8_lossy(&raw).replace(['\0', '\n'], " ");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn sender(sig: i32, pid: u32) -> Stopped {
    if pid == 0 {
        return Stopped { sig, by_pid: None, cmd: "?".into(), cgroup: "?".into() };
    }
    let cmd = crate::cap::argv_of(pid).map(|a| a.join(" ")).filter(|c| !c.is_empty()).map(|c| c.replace('\n', " ")).or_else(|| proc_text(pid, "comm").map(|c| format!("[{c}]"))).unwrap_or_else(|| "?".into());
    let cgroup = fs::read_to_string(proc_path(pid, "cgroup"))
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("0::")).map(|p| p.trim().to_string()))
        .unwrap_or_else(|| "?".into());
    Stopped { sig, by_pid: Some(pid), cmd, cgroup }
}

/// The supervisor: runs `argv` as its child and sits on the host as the head of the run's process
/// group. A TERM, INT or HUP is read with its sender (`si_pid`, which only a process on the
/// sender's own side of the sandbox's PID namespace can name), written to stderr as one
/// [`Stopped`] line (also appended to the record file `argv[0]`), and passed on to the child. Returns the child's exit code, 128+n for a signal.
pub fn supervise(argv: &[String]) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    let [record, program, args @ ..] = argv else {
        eprintln!("testenv: {SUPERVISE_ARG} needs a record file and a command");
        return 2;
    };
    let wanted = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGCHLD];
    // SAFETY: plain libc calls on a locally owned set, before any thread exists.
    let set = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        wanted.iter().for_each(|s| {
            libc::sigaddset(&mut set, *s);
        });
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    };
    // batch-job: the sandbox itself, bounded by its caller's deadline, which kills this supervisor
    let mut cmd = Command::new(program);
    cmd.args(args);
    // SAFETY: only sigprocmask, which is async-signal-safe, between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("testenv: cannot start {program}: {e}");
            return 127;
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0)),
            Ok(None) => {}
            Err(_) => return 1,
        }
        // SAFETY: `info` is written by the kernel; the set is the one blocked above.
        let sig = unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let got = libc::sigwaitinfo(&set, &mut info);
            (got > 0 && got != libc::SIGCHLD).then(|| (got, info.si_pid() as u32))
        };
        if let Some((sig, pid)) = sig {
            use std::io::Write;
            let line = sender(sig, pid).line();
            let _ = writeln!(std::io::stderr(), "{line}");
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(record) {
                let _ = writeln!(f, "{line}");
            }
            // SAFETY: signals the child this function spawned.
            unsafe { libc::kill(child.id() as i32, sig) };
        }
    }
}

/// The command that runs [`supervise`]: this executable's hidden subcommand. A unit test's
/// executable is the test binary, which serves it before `main` (`supervise_at_load`).
#[cfg(test)]
const SUPERVISE_AT_LOAD: &str = "TESTENV_SUPERVISE_AT_LOAD";
#[cfg(not(test))]
const SUPERVISE_AT_LOAD: &str = "";

/// In a unit-test binary, runs [`supervise`] before libtest starts any thread: a signal sent to
/// the process must reach the one thread that blocks it.
#[cfg(test)]
#[used]
#[link_section = ".init_array"]
static SUPERVISE_ON_LOAD: extern "C" fn() = {
    extern "C" fn supervise_at_load() {
        if std::env::var_os(SUPERVISE_AT_LOAD).is_none() {
            return;
        }
        let raw = fs::read(Path::new(system_paths::PROC).join("self/cmdline")).unwrap_or_default();
        let argv: Vec<String> = raw.split(|b| *b == 0).skip_while(|a| *a != b"--").skip(1).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        // SAFETY: leaves the process without running libtest's startup.
        unsafe { libc::_exit(supervise(&argv)) }
    }
    supervise_at_load
};

fn supervisor_command() -> Result<Command, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?;
    let mut c = Command::new(exe);
    if cfg!(test) {
        c.arg("--").env(SUPERVISE_AT_LOAD, "1");
    } else {
        c.arg(SUPERVISE_ARG);
    }
    Ok(c)
}

/// Runs `script` in `profile` with `bwrap`, writing its output to `out`, cut at `deadline`.
/// Err: the sandbox could not be started at all (no bwrap, a refused namespace).
pub fn run(bwrap: &str, profile: &Profile, script: &Path, out: &Path, deadline: Option<Duration>) -> Result<Ran, String> {
    fs::create_dir_all(&profile.target).map_err(|e| format!("cannot create {}: {e}", profile.target.display()))?;
    if let Some(c) = &profile.cache {
        fs::create_dir_all(&c.dir).map_err(|e| format!("cannot create {}: {e}", c.dir.display()))?;
    }
    profile.write_gitconfig()?;
    let log = fs::File::create(out).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let started = Instant::now();
    // batch-job: the unit tier of a testenv trial, cut at the trial's deadline below
    let record = stop_file(out);
    let _ = fs::remove_file(&record);
    let mut cmd = supervisor_command()?;
    cmd.arg(&record).arg(bwrap).args(profile.bwrap_args()).arg("--").arg("bash").arg(script).stdin(Stdio::null()).stdout(log).stderr(err).process_group(0);
    // SAFETY: only prctl, which is async-signal-safe, between fork and exec: the supervisor
    // dies with testenv, and the sandbox with it.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot start {bwrap}: {e}"))?;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            let stopped = if status.code() == Some(0) { None } else { stopped_in(&record) };
            return Ok(Ran { rc: status.code(), secs: started.elapsed().as_secs(), timed_out: false, stopped });
        }
        if crate::runtime::cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Ran { rc: None, secs: started.elapsed().as_secs(), timed_out: false, stopped: None });
        }
        if deadline.is_some_and(|d| started.elapsed() >= d) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Ran { rc: None, secs: started.elapsed().as_secs(), timed_out: true, stopped: None });
        }
        std::thread::sleep(Duration::from_millis(200)); // clock-safe: poll interval under the deadline above
    }
}

/// Whether `bwrap` can make a sandbox here at all, with the reason when it cannot (a missing
/// binary, or user namespaces refused by the kernel or AppArmor).
pub fn usable(bwrap: &str) -> Result<(), String> {
    // batch-job: a one-shot probe, bounded by bwrap's own exit
    let mut c = Command::new(bwrap)
        .args(["--die-with-parent", "--unshare-all", "--ro-bind", "/", "/", "--", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {bwrap}: {e}"))?;
    let mut msg = String::new();
    if let Some(mut e) = c.stderr.take() {
        let _ = e.read_to_string(&mut msg);
    }
    let st = c.wait().map_err(|e| e.to_string())?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("{bwrap} cannot make a sandbox here: {}", msg.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(tmrt8av)]
    #[test]
    fn an_unfiltered_unit_only_sandbox_run_is_sandboxed_with_the_listed_names_as_its_skip_list() {
        let listed = names(&["a_run_pushes"]);
        assert_eq!(route(None, true, Runner::Sandbox, &listed), Route::Sandbox { skip: listed.clone() });
    }

    #[test_id(tnuw5b6)]
    #[test]
    fn a_filtered_run_naming_a_listed_test_goes_to_the_container() {
        let t = names(&["a_run_pushes"]);
        assert_eq!(route(Some(&t), true, Runner::Sandbox, &t), Route::Container { because: "a_run_pushes".into() });
    }

    #[test_id(tjv88qk)]
    #[test]
    fn a_filtered_run_naming_no_listed_test_is_sandboxed_with_nothing_skipped() {
        let t = names(&["other"]);
        assert_eq!(route(Some(&t), true, Runner::Sandbox, &names(&["a_run_pushes"])), Route::Sandbox { skip: vec![] });
    }

    #[test_id(ttk8fjn)]
    #[test]
    fn the_container_runner_and_a_run_of_other_suites_go_to_the_container() {
        let listed = names(&["a_run_pushes"]);
        assert_eq!(route(None, true, Runner::Container, &listed), Route::Container { because: String::new() });
        assert_eq!(route(None, false, Runner::Sandbox, &listed), Route::Container { because: String::new() });
    }

    fn profile() -> Profile {
        Profile {
            tree: "tmp/spira-testenv-x/tree".into(),
            caller_uid: 1000,
            target: "r/t/spira-unit-cargo-target-b".into(),
            home: "h/u".into(),
            gitdir: Some("h/u/repo/.git".into()),
            ro_under_home: vec!["h/u/.rustup".into()],
            rw_under_home: vec!["h/u/.cargo/registry".into()],
            path: "h/u/.cargo/bin:/usr/bin".into(),
            test_threads: 4,
            cache: None,
        }
    }

    fn cached() -> Profile {
        Profile { cache: Some(Cache { dir: "srv/t/.sccache".into(), sccache: "h/u/.cargo/bin/sccache".into() }), ..profile() }
    }

    fn value(env: &[(String, String)], k: &str) -> Option<String> {
        env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    fn pairs(a: &[String], flag: &str) -> Vec<(String, String)> {
        a.windows(3).filter(|w| w[0] == flag).map(|w| (w[1].clone(), w[2].clone())).collect()
    }

    #[test_id(ttblogv)]
    #[test]
    fn the_runner_is_container_or_sandbox_and_nothing_else() {
        assert_eq!(Runner::parse("sandbox"), Ok(Runner::Sandbox));
        assert_eq!(Runner::parse(" container\n"), Ok(Runner::Container));
        assert!(Runner::parse("podman").is_err());
        assert!(Runner::parse("").is_err());
    }

    #[test_id(tgs4yn9)]
    #[test]
    fn the_sandbox_root_is_read_only_and_only_the_tree_target_and_registry_are_writable() {
        let a = profile().bwrap_args();
        assert_eq!(pairs(&a, "--ro-bind").first(), Some(&("/".into(), "/".into())), "the root is bound read-only first: {a:?}");
        let rw: Vec<String> = pairs(&a, "--bind").into_iter().map(|(src, _)| src).collect();
        assert_eq!(rw, ["h/u/.cargo/registry", "tmp/spira-testenv-x/tree", "r/t/spira-unit-cargo-target-b"]);
        assert!(pairs(&a, "--bind").contains(&("tmp/spira-testenv-x/tree".into(), TREE_MOUNT.into())), "the tree is bound at the fixed path: {a:?}");
        assert!(pairs(&a, "--ro-bind").contains(&("h/u/repo/.git".into(), "h/u/repo/.git".into())), "the repo's git dir is read-only: {a:?}");
    }

    #[test_id(tdojaxd)]
    #[test]
    fn the_sandbox_binds_the_runner_less_cargo_config_over_the_trees_after_the_tree_bind() {
        let a = profile().bwrap_args();
        let tree = a.iter().position(|x| x == TREE_MOUNT).unwrap();
        let want = ["--ro-bind-try", "tmp/spira-testenv-x/tree/tools/test-sandbox/cargo-config.toml", &format!("{TREE_MOUNT}/.cargo/config.toml")];
        let at = a.windows(3).position(|w| w == want).unwrap_or_else(|| panic!("{want:?} missing: {a:?}"));
        assert!(at > tree, "the overlay lands on top of the tree: {a:?}");
    }

    #[test_id(tsupdwx)]
    #[test]
    fn the_sandbox_has_no_network_and_its_own_process_namespace() {
        let a = profile().bwrap_args();
        for f in ["--unshare-net", "--unshare-pid", "--die-with-parent", "--clearenv"] {
            assert!(a.iter().any(|x| x == f), "{f} missing: {a:?}");
        }
    }

    #[test_id(tcliuio)]
    #[test]
    fn a_root_callers_sandbox_runs_as_the_test_user_and_any_other_callers_keeps_its_own_identity() {
        let root = Profile { caller_uid: 0, ..profile() }.bwrap_args();
        let want = ["--unshare-user", "--uid", "1001", "--gid", "1001"];
        assert!(root.windows(want.len()).any(|w| w == want), "{want:?} missing: {root:?}");
        let other = profile().bwrap_args();
        assert!(other.iter().all(|a| !["--unshare-user", "--uid", "--gid"].contains(&a.as_str())), "{other:?}");
    }

    #[test_id(tekb5m0)]
    #[test]
    fn the_environment_is_cleared_to_the_allow_list_and_names_no_spira_key() {
        let a = profile().bwrap_args();
        let clear = a.iter().position(|x| x == "--clearenv").unwrap();
        let set: Vec<String> = pairs(&a, "--setenv").into_iter().map(|(k, _)| k).collect();
        assert!(a.windows(3).filter(|w| w[0] == "--setenv").all(|w| a.iter().position(|x| std::ptr::eq(x, &w[0])).unwrap() > clear));
        assert!(set.iter().all(|k| !k.starts_with("SPIRA")), "a SPIRA_ key reached the sandbox: {set:?}");
        for k in ["PATH", "HOME", "CARGO_TARGET_DIR", "CARGO_NET_OFFLINE", "RUST_TEST_THREADS"] {
            assert!(set.iter().any(|s| s == k), "{k} not set: {set:?}");
        }
    }

    #[test_id(tqyq5h8)]
    #[test]
    fn a_worktree_names_its_repositorys_common_git_dir() {
        let d = testkit::TempDir::new("sandbox-gitdir");
        let repo = d.path().join("h/u/repo/.git");
        fs::write(d.path().join(".git"), format!("gitdir: {}/worktrees/x\n", repo.display())).unwrap();
        assert_eq!(worktree_gitdir(d.path()), Some(repo));
        let plain = testkit::TempDir::new("sandbox-plain");
        fs::create_dir(plain.path().join(".git")).unwrap();
        assert_eq!(worktree_gitdir(plain.path()), None);
    }

    #[test_id(tse3x8n)]
    #[test]
    fn a_sandboxed_unit_run_resolves_to_the_sessions_owned_build_directory() {
        let owned = PathBuf::from("srv/ssd-b/dwarf-build/dir.2");
        assert_eq!(unit_target(Some(owned.clone()), "srv/ssd-a/unit-target", "v-b"), Some(owned.clone()), "the session's one target, not a per-bead one");
        assert_eq!(unit_target(Some(owned.clone()), "", "v-b"), Some(owned), "an owned directory needs no sandbox root");
        assert_eq!(unit_target(None, "srv/t", "v-b"), Some(PathBuf::from("srv/t/v-b")), "no session: the branch's target");
        assert_eq!(unit_target(None, " ", "v-b"), None);
    }

    #[test_id(t8gtibj)]
    #[test]
    fn pruning_keeps_the_most_recent_targets_and_never_the_current_one() {
        let d = testkit::TempDir::new("sandbox-prune");
        let mk = |n: &str, age: u64| {
            let p = d.path().join(n);
            fs::create_dir(&p).unwrap();
            fs::File::open(&p).unwrap().set_modified(std::time::SystemTime::now() - Duration::from_secs(age)).unwrap();
            p
        };
        let oldest = mk("t-oldest", 300);
        let middle = mk("t-middle", 200);
        let current = mk("t-current", 400);
        let newest = mk("t-newest", 100);
        let gone = prune_targets(d.path(), &current, 2);
        assert_eq!(gone, [middle.clone(), oldest.clone()]);
        assert!(current.exists() && newest.exists() && !middle.exists() && !oldest.exists());
    }

    #[test_id(t5bwiu5)]
    #[test]
    fn the_sandbox_carries_the_images_system_git_identity_and_never_overrides_a_tests_own() {
        let p = profile();
        let env = p.env();
        let sys = env.iter().find(|(k, _)| k == "GIT_CONFIG_SYSTEM").map(|(_, v)| v.clone());
        assert_eq!(sys.as_deref(), Some("r/t/spira-unit-cargo-target-b.gitconfig"));
        assert!(env.iter().all(|(k, _)| !k.starts_with("GIT_AUTHOR") && !k.starts_with("GIT_COMMITTER")), "{env:?}");
        let d = testkit::TempDir::new("sandbox-gitconfig");
        let q = Profile { target: d.path().join("t"), ..profile() };
        q.write_gitconfig().unwrap();
        let text = fs::read_to_string(q.gitconfig()).unwrap();
        assert!(text.contains("name = spirauser") && text.contains("email = spirauser@localhost"), "{text}");
    }

    #[test_id(tpcx64v)]
    #[test]
    fn two_branches_see_the_same_target_path_so_their_compiled_dependencies_share_cache_keys() {
        let other = Profile { target: "r/t/spira-unit-cargo-target-c".into(), ..cached() };
        for p in [cached(), other] {
            assert_eq!(value(&p.env(), "CARGO_TARGET_DIR").as_deref(), Some(TARGET_MOUNT));
            assert!(pairs(&p.bwrap_args(), "--bind").contains(&(p.target.display().to_string(), TARGET_MOUNT.into())));
        }
    }

    #[test_id(t7exhzf)]
    #[test]
    fn a_cached_run_compiles_through_the_shared_store_and_may_write_only_that_store() {
        let p = cached();
        let env = p.env();
        assert_eq!(value(&env, "RUSTC_WRAPPER").as_deref(), Some("h/u/.cargo/bin/sccache"));
        assert_eq!(value(&env, "SCCACHE_DIR").as_deref(), Some("srv/t/.sccache"));
        assert_eq!(value(&env, "CARGO_INCREMENTAL").as_deref(), Some("0"));
        assert!(pairs(&p.bwrap_args(), "--bind").contains(&("srv/t/.sccache".into(), "srv/t/.sccache".into())));
    }

    #[test_id(tj4qm4a)]
    #[test]
    fn an_uncached_run_sets_no_wrapper_and_binds_no_store() {
        let p = profile();
        assert!(p.env().iter().all(|(k, _)| k != "RUSTC_WRAPPER" && k != "SCCACHE_DIR"));
        assert!(pairs(&p.bwrap_args(), "--bind").iter().all(|(s, _)| !s.contains("sccache")));
    }

    #[test_id(tcfr4cc)]
    #[test]
    fn the_store_is_found_beside_the_toolchain_and_absent_without_an_sccache() {
        let home = testkit::TempDir::new("sandbox-cache-home");
        let root = testkit::TempDir::new("sandbox-cache-root");
        assert_eq!(Cache::under(root.path(), home.path()), None);
        fs::create_dir_all(home.path().join(".cargo/bin")).unwrap();
        fs::write(home.path().join(".cargo/bin/sccache"), "").unwrap();
        let c = Cache::under(root.path(), home.path()).unwrap();
        assert_eq!(c.dir, root.path().join(CACHE_DIR_NAME));
        assert_eq!(c.sccache, home.path().join(".cargo/bin/sccache"));
        assert_eq!(Cache::under(Path::new(""), home.path()), None, "an empty root must not make a store in the cwd");
    }

    #[test_id(ttc2c7f)]
    #[test]
    fn pruning_never_removes_the_shared_store() {
        let d = testkit::TempDir::new("sandbox-prune-store");
        let store = d.path().join(CACHE_DIR_NAME);
        fs::create_dir(&store).unwrap();
        fs::File::open(&store).unwrap().set_modified(std::time::SystemTime::now() - Duration::from_secs(900)).unwrap();
        let a = d.path().join("t-a");
        let b = d.path().join("t-b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        assert!(prune_targets(d.path(), &a, 2).is_empty());
        assert!(store.exists());
    }

    #[test_id(tcrivjq)]
    #[test]
    fn a_run_profile_gets_the_store_in_the_target_root_and_the_sccache_it_finds() {
        let home = testkit::TempDir::new("sandbox-run-home");
        let root = testkit::TempDir::new("sandbox-run-root");
        fs::create_dir_all(home.path().join(".cargo/bin")).unwrap();
        fs::write(home.path().join(".cargo/bin/sccache"), "").unwrap();
        let p = Profile::for_run(Path::new("w"), &root.path().join("t"), root.path(), home.path(), 4);
        assert_eq!(p.cache.as_ref().map(|c| c.dir.clone()), Some(root.path().join(CACHE_DIR_NAME)));
        assert_eq!(value(&p.env(), "RUSTC_WRAPPER"), Some(home.path().join(".cargo/bin/sccache").display().to_string()));
    }

    #[test_id(t03hfky)]
    #[test]
    fn the_script_in_a_real_sandbox_sees_the_wrapper_the_store_and_the_fixed_target() {
        if usable("bwrap").is_err() {
            return;
        }
        let tree = testkit::TempDir::new("sandbox-real-tree");
        let root = testkit::TempDir::new("sandbox-real-root");
        let home = testkit::TempDir::new("sandbox-real-home");
        fs::write(tree.path().join("probe.sh"), "echo \"$RUSTC_WRAPPER|$SCCACHE_DIR|$CARGO_TARGET_DIR|$CARGO_INCREMENTAL\"\nls -d \"$CARGO_TARGET_DIR\" \"$SCCACHE_DIR\"\n").unwrap();
        let mut p = Profile::for_run(tree.path(), &root.path().join("t"), root.path(), home.path(), 1);
        p.cache = Some(Cache { dir: root.path().join(CACHE_DIR_NAME), sccache: system_paths::USR_BIN_TRUE.into() });
        let out = root.path().join("out");
        let ran = run("bwrap", &p, &Path::new(TREE_MOUNT).join("probe.sh"), &out, Some(Duration::from_secs(60))).unwrap();
        let text = fs::read_to_string(&out).unwrap();
        assert_eq!(ran.rc, Some(0), "{text}");
        let store = root.path().join(CACHE_DIR_NAME).display().to_string();
        assert!(text.contains(&format!("{}|{store}|{TARGET_MOUNT}|0", system_paths::USR_BIN_TRUE)), "{text}");
    }

    fn proc_field(pid: &str, file: &str) -> String {
        fs::read(Path::new(system_paths::PROC).join(pid).join(file)).map(|b| String::from_utf8_lossy(&b).replace('\0', " ")).unwrap_or_default()
    }

    #[test_id(tdw7zyc)]
    #[test]
    fn a_run_stopped_by_another_process_names_the_signal_and_its_sender() {
        if usable("bwrap").is_err() {
            return;
        }
        let tree = testkit::TempDir::new("sandbox-stop-tree");
        let root = testkit::TempDir::new("sandbox-stop-root");
        let home = testkit::TempDir::new("sandbox-stop-home");
        fs::write(tree.path().join("stop.sh"), "sleep 29.731\n").unwrap();
        let p = Profile::for_run(tree.path(), &root.path().join("t"), root.path(), home.path(), 1);
        let out = root.path().join("out");
        let runner = {
            let out = out.clone();
            std::thread::spawn(move || run("bwrap", &p, &Path::new(TREE_MOUNT).join("stop.sh"), &out, Some(Duration::from_secs(60))))
        };
        let supervisor = loop {
            let all: Vec<String> = fs::read_dir(system_paths::PROC).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.chars().all(|c| c.is_ascii_digit())).collect();
            let sleeping = all.iter().any(|p| proc_field(p, "cmdline").trim() == "sleep 29.731");
            let head = all.iter().find(|p| {
                let stat = proc_field(p, "stat");
                let pgrp = stat.rsplit_once(')').and_then(|(_, r)| r.split_whitespace().nth(2).map(str::to_string));
                sleeping && pgrp.as_deref() == Some(p.as_str()) && proc_field(p, "cmdline").split(' ').skip(1).step_by(2).take(2).collect::<Vec<_>>() == ["--", "bwrap"]
            });
            if let Some(h) = head {
                break h.clone();
            }
            assert!(!runner.is_finished(), "the run ended before it was stopped");
            std::thread::yield_now();
        };
        let mut killer = Command::new("kill").args(["-TERM", &supervisor]).spawn().unwrap();
        let ran = runner.join().unwrap().unwrap();
        let stopped = ran.stopped.clone().unwrap_or_else(|| panic!("no stop recorded: {ran:?} {:?}", fs::read_to_string(&out)));
        let text = fs::read_to_string(&out).unwrap();
        assert_eq!((stopped.sig, stopped.by_pid), (15, Some(killer.id())), "{ran:?}\n{text}");
        assert_eq!(stopped.comm(), "kill");
        assert!(!ran.timed_out && stopped.cgroup.starts_with('/'), "{ran:?}");
        assert!(text.contains("testenv: stopped from outside: sig=15 from pid="), "{text}");
        killer.wait().unwrap();
    }

    #[test_id(ti8m5k5)]
    #[test]
    fn a_stopped_line_reads_back_whatever_the_sender_command_contains() {
        let s = Stopped { sig: 15, by_pid: Some(77), cmd: "kill -TERM 5 cgroup=x".into(), cgroup: "/user.slice/a.service".into() }; // not a path: a cgroup name
        assert_eq!(Stopped::parse(&s.line()), Some(s.clone()));
        assert_eq!((s.comm(), s.unit()), ("kill", "a.service"));
        let unresolved = Stopped { by_pid: None, ..s };
        assert!(unresolved.line().contains("pid=? (outside the sandbox's namespace)"));
        assert_eq!(Stopped::parse(&unresolved.line()), Some(unresolved));
    }
}
