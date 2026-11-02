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
use std::io::Read;
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
    pub test_threads: u32,
}

impl Profile {
    /// The profile for `tree` with its target at `target`, from this process's home.
    pub fn for_run(tree: &Path, target: &Path, home: &Path, test_threads: u32) -> Profile {
        let under = |rel: &str| home.join(rel);
        let ro = [".rustup", ".cargo/bin", ".local/lib/spira-env"];
        let rw = [".cargo/registry", ".cargo/git"];
        let path = [
            under(".local/lib/spira-env/bin").display().to_string(),
            under(".cargo/bin").display().to_string(),
            "/usr/local/bin".into(),
            "/usr/bin".into(),
            "/bin".into(),
        ]
        .join(":");
        Profile {
            tree: tree.to_path_buf(),
            target: target.to_path_buf(),
            home: home.to_path_buf(),
            gitdir: worktree_gitdir(tree),
            ro_under_home: ro.iter().map(|r| under(r)).filter(|p| p.exists()).collect(),
            rw_under_home: rw.iter().map(|r| under(r)).filter(|p| p.exists()).collect(),
            path,
            test_threads,
        }
    }

    /// The environment inside, in full: nothing of the caller's passes but these.
    pub fn env(&self) -> Vec<(String, String)> {
        let s = |p: &Path| p.display().to_string();
        vec![
            ("PATH".into(), self.path.clone()),
            ("HOME".into(), s(&self.home)),
            ("USER".into(), self.home.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            ("TMPDIR".into(), "/tmp".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("CARGO_HOME".into(), s(&self.home.join(".cargo"))),
            ("RUSTUP_HOME".into(), s(&self.home.join(".rustup"))),
            ("CARGO_TARGET_DIR".into(), s(&self.target)),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
            ("CARGO_TERM_COLOR".into(), "never".into()),
            ("RUST_TEST_THREADS".into(), self.test_threads.to_string()),
            // The image's system-level git identity (spira/testenv/Containerfile), at the same
            // lowest priority, so a test's own repo identity still wins (sp-fdsfq1).
            ("GIT_CONFIG_SYSTEM".into(), s(&self.gitconfig())),
        ]
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
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/tmp",
            "--tmpfs",
            "/run",
        ]
        .iter()
        .map(|x| x.to_string())
        .collect();
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
        a.extend(["--bind".into(), s(&self.target), s(&self.target)]);
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
pub const TREE_MOUNT: &str = "/tmp/spira-sandbox-tree";

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

/// Removes the least recently used branch targets under `root` past `keep`, never `current`.
/// Returns the paths removed. A target's age is its directory's mtime, which every run touches.
pub fn prune_targets(root: &Path, current: &Path, keep: usize) -> Vec<PathBuf> {
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
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

/// Marks `target` as just used, so pruning keeps it.
pub fn touch(target: &Path) {
    let _ = fs::File::open(target).and_then(|f| f.set_modified(std::time::SystemTime::now()));
}

/// How one sandboxed run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub rc: Option<i32>,
    pub secs: u64,
    pub timed_out: bool,
}

/// Runs `script` in `profile` with `bwrap`, writing its output to `out`, cut at `deadline`.
/// Err: the sandbox could not be started at all (no bwrap, a refused namespace).
pub fn run(bwrap: &str, profile: &Profile, script: &Path, out: &Path, deadline: Option<Duration>) -> Result<Ran, String> {
    fs::create_dir_all(&profile.target).map_err(|e| format!("cannot create {}: {e}", profile.target.display()))?;
    profile.write_gitconfig()?;
    let log = fs::File::create(out).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let started = Instant::now();
    // batch-job: the unit tier of a testenv trial, cut at the trial's deadline below
    let mut child = Command::new(bwrap)
        .args(profile.bwrap_args())
        .arg("--")
        .arg("bash")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("cannot start {bwrap}: {e}"))?;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(Ran { rc: status.code(), secs: started.elapsed().as_secs(), timed_out: false });
        }
        if deadline.is_some_and(|d| started.elapsed() >= d) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Ran { rc: None, secs: started.elapsed().as_secs(), timed_out: true });
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

    fn profile() -> Profile {
        Profile {
            tree: "/tmp/spira-testenv-x/tree".into(),
            target: "/srv/t/spira-unit-cargo-target-b".into(),
            home: "/h/u".into(),
            gitdir: Some("/h/u/repo/.git".into()),
            ro_under_home: vec!["/h/u/.rustup".into()],
            rw_under_home: vec!["/h/u/.cargo/registry".into()],
            path: "/h/u/.cargo/bin:/usr/bin".into(),
            test_threads: 4,
        }
    }

    fn pairs(a: &[String], flag: &str) -> Vec<(String, String)> {
        a.windows(3).filter(|w| w[0] == flag).map(|w| (w[1].clone(), w[2].clone())).collect()
    }

    #[test]
    fn the_runner_is_container_or_sandbox_and_nothing_else() {
        assert_eq!(Runner::parse("sandbox"), Ok(Runner::Sandbox));
        assert_eq!(Runner::parse(" container\n"), Ok(Runner::Container));
        assert!(Runner::parse("podman").is_err());
        assert!(Runner::parse("").is_err());
    }

    #[test]
    fn the_sandbox_root_is_read_only_and_only_the_tree_target_and_registry_are_writable() {
        let a = profile().bwrap_args();
        assert_eq!(pairs(&a, "--ro-bind").first(), Some(&("/".into(), "/".into())), "the root is bound read-only first: {a:?}");
        let rw: Vec<String> = pairs(&a, "--bind").into_iter().map(|(src, _)| src).collect();
        assert_eq!(rw, ["/h/u/.cargo/registry", "/tmp/spira-testenv-x/tree", "/srv/t/spira-unit-cargo-target-b"]);
        assert!(pairs(&a, "--bind").contains(&("/tmp/spira-testenv-x/tree".into(), TREE_MOUNT.into())), "the tree is bound at the fixed path: {a:?}");
        assert!(pairs(&a, "--ro-bind").contains(&("/h/u/repo/.git".into(), "/h/u/repo/.git".into())), "the repo's git dir is read-only: {a:?}");
    }

    #[test]
    fn home_and_tmp_are_fresh_and_bound_back_only_after_the_tmpfs() {
        let a = profile().bwrap_args();
        let at = |x: &str| a.iter().position(|v| v == x).unwrap();
        let home_tmpfs = a.windows(2).position(|w| w[0] == "--tmpfs" && w[1] == "/h/u").unwrap();
        assert!(home_tmpfs < at("/h/u/.rustup"), "a path under home is bound back after home's tmpfs, or the tmpfs hides it: {a:?}");
        assert!(a.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/tmp"));
        let tree_bind = a.windows(2).position(|w| w[0] == "--bind" && w[1] == "/tmp/spira-testenv-x/tree").unwrap();
        let tmp_tmpfs = a.windows(2).position(|w| w[0] == "--tmpfs" && w[1] == "/tmp").unwrap();
        assert!(tmp_tmpfs < tree_bind, "a worktree under /tmp is bound after /tmp's tmpfs: {a:?}");
    }

    #[test]
    fn the_sandbox_has_no_network_and_its_own_process_namespace() {
        let a = profile().bwrap_args();
        for f in ["--unshare-net", "--unshare-pid", "--die-with-parent", "--clearenv"] {
            assert!(a.iter().any(|x| x == f), "{f} missing: {a:?}");
        }
    }

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

    #[test]
    fn a_worktree_names_its_repositorys_common_git_dir() {
        let d = testkit::TempDir::new("sandbox-gitdir");
        fs::write(d.path().join(".git"), "gitdir: /h/u/repo/.git/worktrees/x\n").unwrap();
        assert_eq!(worktree_gitdir(d.path()), Some(PathBuf::from("/h/u/repo/.git")));
        let plain = testkit::TempDir::new("sandbox-plain");
        fs::create_dir(plain.path().join(".git")).unwrap();
        assert_eq!(worktree_gitdir(plain.path()), None);
    }

    #[test]
    fn a_run_naming_a_listed_test_goes_to_the_container_and_one_naming_none_stays() {
        let d = testkit::TempDir::new("sandbox-listed");
        fs::create_dir_all(d.path().join("testenv")).unwrap();
        fs::write(d.path().join(CONTAINER_TESTS_REL), "# needs a dolt server\nthe_store_answers_over_its_socket\n\n").unwrap();
        let listed = container_tests(d.path());
        assert_eq!(listed, ["the_store_answers_over_its_socket"]);
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(needing_container(&f(&["a_pure_test", "the_store_answers_over_its_socket"]), &listed), ["the_store_answers_over_its_socket"]);
        assert!(needing_container(&f(&["a_pure_test"]), &listed).is_empty());
        assert!(container_tests(testkit::TempDir::new("sandbox-none").path()).is_empty());
    }

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

    #[test]
    fn the_sandbox_carries_the_images_system_git_identity_and_never_overrides_a_tests_own() {
        let p = profile();
        let env = p.env();
        let sys = env.iter().find(|(k, _)| k == "GIT_CONFIG_SYSTEM").map(|(_, v)| v.clone());
        assert_eq!(sys.as_deref(), Some("/srv/t/spira-unit-cargo-target-b.gitconfig"));
        assert!(env.iter().all(|(k, _)| !k.starts_with("GIT_AUTHOR") && !k.starts_with("GIT_COMMITTER")), "{env:?}");
        let d = testkit::TempDir::new("sandbox-gitconfig");
        let q = Profile { target: d.path().join("t"), ..profile() };
        q.write_gitconfig().unwrap();
        let text = fs::read_to_string(q.gitconfig()).unwrap();
        assert!(text.contains("name = spirauser") && text.contains("email = spirauser@localhost"), "{text}");
    }

    #[test]
    fn every_run_sees_its_tree_at_the_same_path_whatever_the_host_worktree_is_called() {
        let one = Profile { tree: "/tmp/spira-testenv-a/.testenv-warm-0".into(), ..profile() }.bwrap_args();
        let two = Profile { tree: "/tmp/spira-testenv-b/.testenv-warm-1".into(), ..profile() }.bwrap_args();
        let dest = |a: &[String], src: &str| pairs(a, "--bind").into_iter().find(|(s, _)| s == src).map(|(_, d)| d);
        assert_eq!(dest(&one, "/tmp/spira-testenv-a/.testenv-warm-0").as_deref(), Some(TREE_MOUNT));
        assert_eq!(dest(&two, "/tmp/spira-testenv-b/.testenv-warm-1").as_deref(), Some(TREE_MOUNT));
        assert!(one.windows(2).any(|w| w[0] == "--chdir" && w[1] == TREE_MOUNT), "{one:?}");
        assert_eq!(mounted_script("unit-tests.sh"), PathBuf::from(TREE_MOUNT).join("spira/unit-tests.sh"));
    }

    #[test]
    fn a_branch_target_is_keyed_as_its_volume() {
        assert_eq!(target_dir(Path::new("/srv/t"), "spira-unit-cargo-target-sp-x"), PathBuf::from("/srv/t/spira-unit-cargo-target-sp-x"));
    }
}
