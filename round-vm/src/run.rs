//! `round-vm run` (DESIGN.md §2.2): mirror → acquire → batch on the VM → pull results,
//! telemetry and binaries → manifest → install by tree sha → release.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use serde_json::Value;

use crate::config::Config;
use crate::pool::{Deps, Pool};
use crate::procs::{command, FileLock};
use crate::schema::{AcquireMode, Manifest, ProcId, Vm};

pub const RUN_USAGE: &str =
    "round-vm run: usage: round-vm run <tree-dir> [--suites <csv>] [--maxpar <n>] [--toolchain <ver>] [--results-dir <dir>]";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunArgs {
    pub tree_dir: PathBuf,
    pub suites: Option<String>,
    pub maxpar: Option<u32>,
    pub toolchain: Option<String>,
    pub results_dir: Option<PathBuf>,
}

/// Parses `run`'s arguments; `--opt value` and `--opt=value` both work.
pub fn parse_run_args(args: &[String]) -> Result<RunArgs, String> {
    let mut it = args.iter();
    let tree = it.next().filter(|a| !a.starts_with("--")).ok_or(RUN_USAGE)?;
    let mut r = RunArgs { tree_dir: PathBuf::from(tree), ..Default::default() };
    while let Some(a) = it.next() {
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut val = || inline.clone().or_else(|| it.next().cloned()).ok_or(format!("round-vm run: {key} needs a value"));
        match key {
            "--suites" => r.suites = Some(val()?).filter(|v| !v.is_empty()),
            "--maxpar" => {
                let v = val()?;
                r.maxpar = Some(v.parse().map_err(|_| format!("round-vm run: --maxpar not a number: {v:?}"))?);
            }
            "--toolchain" => r.toolchain = Some(val()?).filter(|v| !v.is_empty()),
            "--results-dir" => r.results_dir = Some(PathBuf::from(val()?)),
            other => return Err(format!("round-vm run: unknown option: {other}")),
        }
    }
    Ok(r)
}

/// The host side: the tree's identity and the mirror the VM clones from.
pub trait Host {
    fn is_checkout(&self, tree: &Path) -> bool;
    /// (commit sha, tree sha) of `tree`'s HEAD.
    fn head(&self, tree: &Path) -> Result<(String, String), String>;
    /// Points the mirror at `tree`'s HEAD and makes sure the daemon serving it is up.
    fn prepare_mirror(&self, tree: &Path) -> Result<(), String>;
}

pub struct BatchJob {
    pub host_addr: String,
    pub mirror_port: u16,
    pub suites: Option<String>,
    pub maxpar: u32,
    pub toolchain: Option<String>,
}

/// The VM side, reached only by address.
pub trait Remote {
    fn reachable(&self, addr: &str) -> bool;
    /// Runs the batch on the VM; the remote testenv runner's exit code (255: ssh itself).
    fn run_batch(&self, addr: &str, job: &BatchJob) -> Result<i32, String>;
    /// Copies the remote directory `remote_path` into `local`.
    fn pull(&self, addr: &str, remote_path: &str, local: &Path) -> Result<(), String>;
}

/// Single-quotes `s` for a POSIX shell, so an empty argument survives ssh's re-joining of
/// the remote command line (the bash lost an empty --suites and shifted every later one).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The VM side of a round: clone the mirror, run the round's own `testenv` (built from the
/// round's own tree, release profile — its binaries ship), then stage the round's
/// executables from `target/release` into `~/round-bins/` so the host pulls the binaries and
/// not cargo's whole target directory. testenv builds the clone in place (its HEAD is the
/// round's commit), so `~/round-work/target/release` is the round's build.
pub const REMOTE_SCRIPT: &str = r#"set -euo pipefail
host_addr="$1" port="$2" suites="$3" maxpar="$4" toolchain="$5"
rm -rf ~/round-work ~/round-bins
git clone --quiet "git://${host_addr}:${port}/mirror.git" ~/round-work
cd ~/round-work
export SPIRA_BATCH_MAXPAR="$maxpar"
if [ -n "$toolchain" ]; then export RUSTUP_TOOLCHAIN="$toolchain"; fi
set +e
if [ -n "$suites" ]; then
    cargo run -q --profile release -p testenv -- --mode parallel --profile release --suites "$suites" round
else
    cargo run -q --profile release -p testenv -- --mode parallel --profile release round
fi
rc=$?
mkdir -p ~/round-bins
find target/release -maxdepth 1 -type f -executable -exec cp {} ~/round-bins/ \; 2>/dev/null
exit "$rc"
"#;

/// Where REMOTE_SCRIPT stages the round's executables, relative to the VM user's home.
pub const REMOTE_BINS: &str = "round-bins/";

pub fn remote_command(job: &BatchJob) -> String {
    let args = [
        job.host_addr.clone(),
        job.mirror_port.to_string(),
        job.suites.clone().unwrap_or_default(),
        job.maxpar.to_string(),
        job.toolchain.clone().unwrap_or_default(),
    ];
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    format!("bash -s -- {}", quoted.join(" "))
}

pub struct SshRemote {
    pub user: String,
    pub port: u16,
    pub key: PathBuf,
}

impl SshRemote {
    fn ssh_opts(&self) -> Vec<String> {
        [
            "-i", &self.key.to_string_lossy(), "-p", &self.port.to_string(),
            "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
            "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "LogLevel=ERROR",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

impl Remote for SshRemote {
    fn reachable(&self, addr: &str) -> bool {
        command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run_batch(&self, addr: &str, job: &BatchJob) -> Result<i32, String> {
        let mut child = command("ssh")
            .args(self.ssh_opts())
            .arg(format!("{}@{addr}", self.user))
            .arg(remote_command(job))
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ssh: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            si.write_all(REMOTE_SCRIPT.as_bytes()).map_err(|e| format!("ssh stdin: {e}"))?;
        }
        let st = child.wait().map_err(|e| format!("ssh: {e}"))?;
        Ok(st.code().unwrap_or(255))
    }

    fn pull(&self, addr: &str, remote_path: &str, local: &Path) -> Result<(), String> {
        fs::create_dir_all(local).map_err(|e| format!("{}: {e}", local.display()))?;
        let st = command("rsync")
            .arg("-a")
            .arg("-e")
            .arg(format!("ssh {}", self.ssh_opts().join(" ")))
            .arg(format!("{}@{addr}:{remote_path}", self.user))
            .arg(format!("{}/", local.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("rsync: {e}"))?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("rsync {remote_path} exited {}", st.code().unwrap_or(-1)))
        }
    }
}

pub struct GitHost {
    pub state_dir: PathBuf,
    pub mirror_port: u16,
}

fn git(args: &[&str]) -> Result<String, String> {
    let out = command("git").args(args).stdin(Stdio::null()).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

impl Host for GitHost {
    fn is_checkout(&self, tree: &Path) -> bool {
        git(&["-C", &tree.to_string_lossy(), "rev-parse", "--git-dir"]).is_ok()
    }

    fn head(&self, tree: &Path) -> Result<(String, String), String> {
        let t = tree.to_string_lossy();
        Ok((git(&["-C", &t, "rev-parse", "HEAD"])?, git(&["-C", &t, "rev-parse", "HEAD^{tree}"])?))
    }

    fn prepare_mirror(&self, tree: &Path) -> Result<(), String> {
        fs::create_dir_all(&self.state_dir).map_err(|e| e.to_string())?;
        let mirror = self.state_dir.join("mirror.git");
        let m = mirror.to_string_lossy().to_string();
        if !mirror.is_dir() {
            git(&["init", "--quiet", "--bare", &m])?;
        }
        git(&["--git-dir", &m, "fetch", "--quiet", &tree.to_string_lossy(), "+HEAD:refs/heads/round"])?;
        git(&["--git-dir", &m, "symbolic-ref", "HEAD", "refs/heads/round"])?;

        let pidfile = self.state_dir.join("git-daemon.pid");
        let running = |p: &Path| fs::read_to_string(p).ok().and_then(|s| s.trim().parse().ok()).map(pid_alive).unwrap_or(false);
        if running(&pidfile) {
            return Ok(());
        }
        let _ = fs::remove_file(&pidfile);
        let st = command("git")
            .arg("daemon")
            .arg("--reuseaddr")
            .arg("--listen=0.0.0.0")
            .arg(format!("--port={}", self.mirror_port))
            .arg(format!("--base-path={}", self.state_dir.display()))
            .arg("--export-all")
            .arg(format!("--pid-file={}", pidfile.display()))
            .arg("--detach")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("git daemon: {e}"))?;
        for _ in 0..20 {
            if running(&pidfile) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(format!("git daemon did not start on port {} (exit {:?})", self.mirror_port, st.code()))
    }
}

/// The directory that holds the pulled `.result` files: `dir` itself, or the one
/// BATCH_KEY subdirectory the testenv runner nests them under.
pub fn results_leaf(dir: &Path) -> Option<PathBuf> {
    let has_result = |d: &Path| {
        fs::read_dir(d)
            .map(|rd| rd.flatten().any(|e| e.path().extension().map(|x| x == "result").unwrap_or(false)))
            .unwrap_or(false)
    };
    if has_result(dir) {
        return Some(dir.to_path_buf());
    }
    let mut subs: Vec<PathBuf> = fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    subs.sort();
    subs.into_iter().find(|p| has_result(p))
}

/// Sum of every suite's own wall time: the third field of each `<suite>.result`.
pub fn suite_wall_sum(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .filter(|e| e.path().extension().map(|x| x == "result").unwrap_or(false))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| t.split_whitespace().nth(2).and_then(|s| s.parse::<u64>().ok()))
        .sum()
}

/// testenv's `build_wall_s` from `runner.meta`, present only when it built.
pub fn build_wall(dir: &Path) -> Option<u64> {
    let t = fs::read_to_string(dir.join("runner.meta")).ok()?;
    t.lines().find_map(|l| l.strip_prefix("build_wall_s=")).and_then(|v| v.trim().parse().ok())
}

/// The tree the VM's testenv run built and tested: `tree=` in the pulled `batch.meta`.
pub fn reported_tree(results: &Path) -> Option<String> {
    let t = fs::read_to_string(results.join("batch.meta")).ok()?;
    t.lines().find_map(|l| l.strip_prefix("tree=")).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The executables directly in `dir` (the pulled `round-bins/`).
fn executables(dir: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for e in fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?.flatten() {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let ft = e.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else if ft.is_symlink() {
            let target = fs::read_link(&from).map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&to);
            std::os::unix::fs::symlink(target, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        } else {
            fs::copy(&from, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
    }
    Ok(())
}

/// Installs the pulled executables into `<target>` — the round worktree's own
/// `target/release`, where `queue land-local --worktree` reads them (G8). The VM's testenv
/// must report (batch.meta `tree=`) exactly the tree the host sent; any other tree, or none,
/// installs nothing and is an error naming both. No executables at all (the batch never
/// reached its build) is not a refusal.
pub fn install_bins(pulled: &Path, expected: &str, reported: Option<&str>, target: &Path) -> Result<(), String> {
    let exes = executables(pulled);
    if exes.is_empty() {
        return Ok(());
    }
    if reported != Some(expected) {
        return Err(format!(
            "round-vm run: refusing binaries: tree sha {} does not match expected {expected} — nothing installed",
            reported.unwrap_or("<none reported>")
        ));
    }
    fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
    for exe in exes {
        let to = target.join(exe.file_name().unwrap());
        let _ = fs::remove_file(&to);
        fs::copy(&exe, &to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(())
}

/// Appends the VM's tsd rows to the host's `<run>/tsd/<family>.jsonl`, each tagged with
/// `ran_on`, `vcpus` and `maxpar`, skipping a row already present, under `<file>.lock`.
pub fn merge_tsd(src: &Path, run_dir: &Path, vm: &str, vcpus: u32, maxpar: u32) -> Result<usize, String> {
    let Ok(rd) = fs::read_dir(src) else { return Ok(0) };
    let mut added = 0;
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false)).collect();
    files.sort();
    for f in files {
        let dest = run_dir.join("tsd").join(f.file_name().unwrap());
        let mut lock_path = dest.clone().into_os_string();
        lock_path.push(".lock");
        let _lock = FileLock::exclusive(Path::new(&lock_path))?;
        let mut seen: HashSet<String> = fs::read_to_string(&dest).unwrap_or_default().lines().map(str::to_string).collect();
        let mut out = fs::OpenOptions::new().create(true).append(true).open(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        for line in fs::read_to_string(&f).unwrap_or_default().lines() {
            let Ok(Value::Object(mut row)) = serde_json::from_str::<Value>(line) else { continue };
            row.insert("ran_on".into(), Value::from(vm));
            row.insert("vcpus".into(), Value::from(vcpus));
            row.insert("maxpar".into(), Value::from(maxpar));
            let s = Value::Object(row).to_string();
            if seen.insert(s.clone()) {
                writeln!(out, "{s}").map_err(|e| format!("{}: {e}", dest.display()))?;
                added += 1;
            }
        }
    }
    Ok(added)
}

pub struct RunEnv<'a> {
    pub cfg: &'a Config,
    pub pool: &'a Pool,
    pub deps: &'a Deps<'a>,
    pub host: &'a dyn Host,
    pub remote: &'a dyn Remote,
}

/// `round-vm run`. Returns the process exit code (DESIGN.md §2.2).
pub fn run(env: &RunEnv, args: &RunArgs) -> i32 {
    let cfg = env.cfg;
    if !env.host.is_checkout(&args.tree_dir) {
        eprintln!("round-vm run: not a git checkout: {}", args.tree_dir.display());
        return 2;
    }
    let Some(host_addr) = cfg.host_addr.clone() else {
        eprintln!("round-vm run: SPIRA_ROUND_VM_HOST_ADDR not set");
        return 2;
    };
    if fs::File::open(&cfg.host_key).is_err() {
        eprintln!("round-vm run: SPIRA_ROUND_VM_HOST_KEY not readable: {}", cfg.host_key.display());
        return 2;
    }
    let (commit_sha, tree_sha) = match env.host.head(&args.tree_dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("round-vm run: {e}");
            return 2;
        }
    };
    if let Err(e) = env.host.prepare_mirror(&args.tree_dir) {
        eprintln!("round-vm run: cannot prepare the mirror from {}: {e}", args.tree_dir.display());
        return 2;
    }
    let (vm, mode) = match env.pool.acquire(env.deps, Some(ProcId::current())) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("round-vm run: acquire failed: {e}");
            return 2;
        }
    };
    eprintln!("round-vm run: {} {} {}", vm.handle, vm.addr, mode.as_str());
    let code = on_vm(env, args, &vm, mode, &host_addr, &commit_sha, &tree_sha);
    if let Err(e) = env.pool.release(&vm.handle, env.deps.factory) {
        eprintln!("round-vm run: warning: release of {} failed: {e}", vm.handle);
    }
    code
}

fn on_vm(env: &RunEnv, args: &RunArgs, vm: &Vm, mode: AcquireMode, host_addr: &str, commit_sha: &str, tree_sha: &str) -> i32 {
    let cfg = env.cfg;
    let maxpar = args.maxpar.unwrap_or(cfg.maxpar);
    let reachable = (0..cfg.ssh_tries.max(1)).any(|i| {
        if i > 0 {
            std::thread::sleep(cfg.boot_poll);
        }
        env.remote.reachable(&vm.addr)
    });
    if !reachable {
        eprintln!("round-vm run: VM {} at {} never became reachable over ssh", vm.handle, vm.addr);
        return 2;
    }
    let job = BatchJob {
        host_addr: host_addr.to_string(),
        mirror_port: cfg.mirror_port,
        suites: args.suites.clone(),
        maxpar,
        toolchain: args.toolchain.clone(),
    };
    let t0 = Instant::now();
    let remote_rc = match env.remote.run_batch(&vm.addr, &job) {
        Ok(255) => {
            eprintln!("round-vm run: ssh to {} failed (exit 255)", vm.addr);
            return 2;
        }
        Ok(rc) => rc,
        Err(e) => {
            eprintln!("round-vm run: {e}");
            return 2;
        }
    };
    let wall = t0.elapsed().as_secs();

    let pid = std::process::id();
    let scratch = |name: &str| cfg.state_dir.join(format!(".pulled-{name}.{pid}"));
    let results_dir = args.results_dir.clone().unwrap_or_else(|| cfg.run_dir.join("batch-results"));
    let mut own_fault = false;

    let pulled = scratch("results");
    let _ = fs::remove_dir_all(&pulled);
    let pulled_ok = env.remote.pull(&vm.addr, "round-work/.runtime/spira/batch-results/", &pulled).is_ok();
    let leaf = results_leaf(&pulled);
    if let Some(leaf) = &leaf {
        if let Err(e) = copy_tree(leaf, &results_dir) {
            eprintln!("round-vm run: {e}");
            own_fault = true;
        }
    } else if !pulled_ok || remote_rc == 0 {
        eprintln!("round-vm run: no suite results came back from {}", vm.handle);
        own_fault = true;
    }
    let _ = fs::remove_dir_all(&pulled);
    let suite_sum = suite_wall_sum(&results_dir);
    let build = build_wall(&results_dir);

    let tsd = scratch("tsd");
    let _ = fs::remove_dir_all(&tsd);
    if env.remote.pull(&vm.addr, "round-work/.runtime/spira/tsd/", &tsd).is_ok() {
        if let Err(e) = merge_tsd(&tsd, &cfg.run_dir, &vm.handle, cfg.vcpus, maxpar) {
            eprintln!("round-vm run: tsd merge: {e}");
        }
    }
    let _ = fs::remove_dir_all(&tsd);

    let bins = scratch("bins");
    let _ = fs::remove_dir_all(&bins);
    let _ = env.remote.pull(&vm.addr, REMOTE_BINS, &bins);
    let reported = reported_tree(&results_dir);
    let manifest = Manifest {
        tree_sha: tree_sha.to_string(),
        tree_sha_found: reported.clone(),
        commit_sha: commit_sha.to_string(),
        vm: vm.handle.clone(),
        acquire: mode,
        vcpus: cfg.vcpus,
        maxpar,
        batch_wall_secs: wall,
        build_wall_secs: build,
        suite_wall_secs_sum: suite_sum,
    };
    let mdir = cfg.state_dir.join("manifests");
    let written = fs::create_dir_all(&mdir)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::to_string(&manifest).map_err(|e| e.to_string()))
        .and_then(|j| fs::write(mdir.join(format!("{tree_sha}.json")), j).map_err(|e| e.to_string()));
    if let Err(e) = written {
        eprintln!("round-vm run: manifest: {e}");
        own_fault = true;
    }
    let target = args.tree_dir.join("target").join("release");
    if let Err(e) = install_bins(&bins, tree_sha, reported.as_deref(), &target) {
        eprintln!("{e}");
        own_fault = true;
    }
    let _ = fs::remove_dir_all(&bins);

    if remote_rc != 0 {
        return remote_rc;
    }
    if own_fault {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{Attempt, Spawner};
    use crate::provider::Timing;
    use crate::testutil::{FakeAlarm, FakeProvider, TempDir};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_args_accept_both_spellings() {
        let r = parse_run_args(&args(&["/t", "--suites", "a,b", "--maxpar=24", "--toolchain", "1.82.0", "--results-dir=/r"])).unwrap();
        assert_eq!(r.tree_dir, PathBuf::from("/t"));
        assert_eq!(r.suites.as_deref(), Some("a,b"));
        assert_eq!(r.maxpar, Some(24));
        assert_eq!(r.toolchain.as_deref(), Some("1.82.0"));
        assert_eq!(r.results_dir, Some(PathBuf::from("/r")));
    }

    #[test]
    fn run_args_errors_name_the_problem() {
        assert_eq!(parse_run_args(&[]).unwrap_err(), RUN_USAGE);
        assert!(parse_run_args(&args(&["/t", "--bogus"])).unwrap_err().contains("unknown option: --bogus"));
        assert!(parse_run_args(&args(&["/t", "--maxpar", "x"])).unwrap_err().contains("--maxpar"));
        assert!(parse_run_args(&args(&["/t", "--suites"])).unwrap_err().contains("needs a value"));
    }

    #[test]
    fn remote_command_keeps_empty_arguments_in_place() {
        let cmd = remote_command(&BatchJob { host_addr: "10.0.0.1".into(), mirror_port: 9430, suites: None, maxpar: 16, toolchain: Some("1.82.0".into()) });
        assert_eq!(cmd, "bash -s -- '10.0.0.1' '9430' '' '16' '1.82.0'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn measurement_reads_flat_nested_and_empty_results() {
        let d = TempDir::new();
        let flat = d.path().join("flat");
        fs::create_dir_all(&flat).unwrap();
        fs::write(flat.join("test-a.sh.result"), "ok 1 3 - serial explicit 0\n").unwrap();
        fs::write(flat.join("test-b.sh.result"), "ok 1 4 - serial explicit 0\n").unwrap();
        assert_eq!(results_leaf(&flat), Some(flat.clone()));
        assert_eq!(suite_wall_sum(&flat), 7);
        let nested = d.path().join("nested");
        fs::create_dir_all(nested.join("deadbeef")).unwrap();
        fs::write(nested.join("deadbeef/test-c.sh.result"), "ok 1 5 - serial explicit 0\n").unwrap();
        assert_eq!(results_leaf(&nested), Some(nested.join("deadbeef")));
        let empty = d.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(results_leaf(&empty), None);
        assert_eq!(suite_wall_sum(&empty), 0);
        fs::write(flat.join("runner.meta"), "nproc=8\nmaxpar=16\nbuild_wall_s=42\n").unwrap();
        assert_eq!(build_wall(&flat), Some(42));
        assert_eq!(build_wall(&empty), None);
    }

    fn exe(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "x").unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn binaries_of_another_tree_are_refused_and_nothing_installed() {
        let d = TempDir::new();
        let (pulled, target) = (d.path().join("pulled"), d.path().join("wt/target/release"));
        exe(&pulled.join("fakebin"));
        let e = install_bins(&pulled, "expectedsha", Some("wrongsha"), &target).unwrap_err();
        assert!(e.contains("wrongsha") && e.contains("expectedsha"), "{e}");
        assert!(install_bins(&pulled, "expectedsha", None, &target).is_err(), "no reported tree is not a match");
        assert!(!target.exists());
    }

    #[test]
    fn binaries_of_this_tree_land_where_land_local_reads_them() {
        let d = TempDir::new();
        let (pulled, target) = (d.path().join("pulled"), d.path().join("wt/target/release"));
        exe(&pulled.join("fakebin"));
        fs::write(pulled.join("not-executable"), "x").unwrap();
        install_bins(&pulled, "expectedsha", Some("expectedsha"), &target).unwrap();
        assert!(target.join("fakebin").is_file());
        assert!(!target.join("not-executable").exists());
        install_bins(&d.path().join("absent"), "expectedsha", None, &target).unwrap();
    }

    #[test]
    fn tsd_rows_are_tagged_and_deduplicated() {
        let d = TempDir::new();
        let src = d.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("suite.jsonl"), "{\"suite\":\"a\",\"secs\":3}\nnot json\n{\"suite\":\"b\"}\n").unwrap();
        let run = d.path().join("run");
        assert_eq!(merge_tsd(&src, &run, "123", 16, 24).unwrap(), 2);
        assert_eq!(merge_tsd(&src, &run, "123", 16, 24).unwrap(), 0, "a re-merge adds nothing");
        let rows: Vec<Value> = fs::read_to_string(run.join("tsd/suite.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["ran_on"], "123");
        assert_eq!(rows[0]["vcpus"], 16);
        assert_eq!(rows[0]["maxpar"], 24);
        assert_eq!(rows[0]["secs"], 3);
    }

    // ── run end to end against fakes ────────────────────────────────────────────────

    struct FakeHost;
    impl Host for FakeHost {
        fn is_checkout(&self, tree: &Path) -> bool {
            !tree.ends_with("not-a-repo")
        }
        fn head(&self, _: &Path) -> Result<(String, String), String> {
            Ok(("commitsha".into(), "treesha".into()))
        }
        fn prepare_mirror(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }
    }

    /// A VM whose remote tree is a map of remote path → files it holds.
    struct FakeRemote {
        reachable: bool,
        rc: Result<i32, String>,
        files: BTreeMap<&'static str, Vec<(&'static str, &'static str)>>,
        jobs: RefCell<Vec<String>>,
    }
    impl FakeRemote {
        fn green() -> FakeRemote {
            let mut files = BTreeMap::new();
            files.insert("round-work/.runtime/spira/batch-results/", vec![
                ("KEY/test-a.sh.result", "ok 1 30 - p e 0\n"),
                ("KEY/test-b.sh.result", "ok 1 12 - p e 0\n"),
                ("KEY/runner.meta", "build_wall_s=90\n"),
                ("KEY/batch.meta", "key=KEY\ntree=treesha\n"),
            ]);
            files.insert("round-work/.runtime/spira/tsd/", vec![("suite.jsonl", "{\"suite\":\"a\"}\n")]);
            files.insert(REMOTE_BINS, vec![("batcher", "bin")]);
            FakeRemote { reachable: true, rc: Ok(0), files, jobs: RefCell::new(vec![]) }
        }
    }
    impl Remote for FakeRemote {
        fn reachable(&self, _: &str) -> bool {
            self.reachable
        }
        fn run_batch(&self, _: &str, job: &BatchJob) -> Result<i32, String> {
            self.jobs.borrow_mut().push(remote_command(job));
            self.rc.clone()
        }
        fn pull(&self, _: &str, remote_path: &str, local: &Path) -> Result<(), String> {
            let files = self.files.get(remote_path).ok_or("no such remote dir")?;
            for (rel, body) in files {
                let p = local.join(rel);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, body).unwrap();
                if remote_path == REMOTE_BINS {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
                }
            }
            Ok(())
        }
    }

    struct NoSpawn;
    impl Spawner for NoSpawn {
        fn spawn_next(&self) -> Result<ProcId, String> {
            Err("not in tests".into())
        }
    }

    struct Fixture {
        d: TempDir,
        fp: FakeProvider,
        cfg: Config,
    }

    fn fixture() -> Fixture {
        let d = TempDir::new();
        let run_dir = d.path().join("run");
        let key = d.path().join("host_key");
        fs::write(&key, "k").unwrap();
        let mut src = BTreeMap::new();
        src.insert("SPIRA_RUN".to_string(), run_dir.to_string_lossy().to_string());
        src.insert("SPIRA_ROUND_VM_HOST_ADDR".to_string(), "192.168.1.10".to_string());
        src.insert("SPIRA_ROUND_VM_HOST_KEY".to_string(), key.to_string_lossy().to_string());
        src.insert("SPIRA_ROUND_VM_SSH_TRIES".to_string(), "2".to_string());
        src.insert("SPIRA_ROUND_VM_BOOT_POLL".to_string(), "0".to_string());
        let cfg = Config::load(&src).unwrap();
        Fixture { d, fp: FakeProvider::new(), cfg }
    }

    fn go(fx: &Fixture, remote: &FakeRemote, a: &RunArgs) -> i32 {
        let pool = Pool { state_dir: fx.cfg.state_dir.clone(), retry_interval: Duration::ZERO, max_retries: 1, wait_poll: Duration::ZERO };
        let fp = fx.fp.clone();
        let f = move || {
            Ok(Attempt {
                provider: Box::new(fp.clone()),
                iface: "ens18".into(),
                ssh_user: "root".into(),
                pubkey: "k".into(),
                timing: Timing { boot_tries: 1, poll: Duration::ZERO, gone_tries: 1 },
            })
        };
        let alarm = FakeAlarm::default();
        let deps = Deps { factory: &f, alarm: &alarm, spawner: &NoSpawn };
        run(&RunEnv { cfg: &fx.cfg, pool: &pool, deps: &deps, host: &FakeHost, remote }, a)
    }

    fn tree(fx: &Fixture) -> RunArgs {
        RunArgs { tree_dir: fx.d.path().join("tree"), maxpar: Some(24), ..Default::default() }
    }

    #[test]
    fn green_run_pulls_everything_writes_the_manifest_installs_and_releases() {
        let fx = fixture();
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 0);
        assert!(fx.fp.live_vms().is_empty(), "released: {:?}", fx.fp.live_vms());
        let results = fx.cfg.run_dir.join("batch-results");
        assert!(results.join("test-a.sh.result").is_file(), "flattened out of the BATCH_KEY dir");
        let m: Manifest = serde_json::from_str(&fs::read_to_string(fx.cfg.state_dir.join("manifests/treesha.json")).unwrap()).unwrap();
        assert_eq!((m.vm.as_str(), m.acquire, m.vcpus, m.maxpar), ("100", AcquireMode::Cold, 16, 24));
        assert_eq!((m.build_wall_secs, m.suite_wall_secs_sum), (Some(90), 42));
        assert_eq!(m.tree_sha_found.as_deref(), Some("treesha"));
        assert_eq!(m.commit_sha, "commitsha");
        assert!(tree(&fx).tree_dir.join("target/release/batcher").is_file(), "installed into the round worktree");
        assert!(fs::read_to_string(fx.cfg.run_dir.join("tsd/suite.jsonl")).unwrap().contains("\"ran_on\":\"100\""));
        assert!(remote.jobs.borrow()[0].ends_with("'' '24' ''"), "{:?}", remote.jobs.borrow());
    }

    #[test]
    fn a_red_remote_exit_code_passes_through_and_the_vm_is_released() {
        for rc in [1, 3, 4] {
            let fx = fixture();
            let remote = FakeRemote { rc: Ok(rc), ..FakeRemote::green() };
            assert_eq!(go(&fx, &remote, &tree(&fx)), rc);
            assert!(fx.fp.live_vms().is_empty());
        }
    }

    #[test]
    fn every_failure_on_the_vm_is_exit_2_and_releases_it() {
        let cases: Vec<FakeRemote> = vec![
            FakeRemote { reachable: false, ..FakeRemote::green() },
            FakeRemote { rc: Ok(255), ..FakeRemote::green() },
            FakeRemote { rc: Err("ssh: spawn failed".into()), ..FakeRemote::green() },
            FakeRemote { files: BTreeMap::new(), ..FakeRemote::green() },
        ];
        for remote in cases {
            let fx = fixture();
            assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
            assert!(fx.fp.live_vms().is_empty(), "leaked {:?}", fx.fp.live_vms());
        }
    }

    #[test]
    fn a_green_run_with_another_tree_s_binaries_is_exit_2_and_installs_nothing() {
        let fx = fixture();
        let mut remote = FakeRemote::green();
        remote.files.insert("round-work/.runtime/spira/batch-results/", vec![
            ("KEY/test-a.sh.result", "ok 1 30 - p e 0\n"),
            ("KEY/batch.meta", "key=KEY\ntree=othersha\n"),
        ]);
        assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
        assert!(!tree(&fx).tree_dir.join("target/release/batcher").exists());
        assert!(fx.fp.live_vms().is_empty());
    }

    #[test]
    fn preflight_errors_exit_2_before_any_vm_is_cloned() {
        let fx = fixture();
        let a = RunArgs { tree_dir: fx.d.path().join("not-a-repo"), ..Default::default() };
        assert_eq!(go(&fx, &FakeRemote::green(), &a), 2);

        let mut fx2 = fixture();
        fx2.cfg.host_addr = None;
        assert_eq!(go(&fx2, &FakeRemote::green(), &tree(&fx2)), 2);

        let mut fx3 = fixture();
        fx3.cfg.host_key = fx3.d.path().join("no-such-key");
        assert_eq!(go(&fx3, &FakeRemote::green(), &tree(&fx3)), 2);
        for f in [&fx, &fx2, &fx3] {
            assert_eq!(f.fp.count("clone"), 0);
        }
    }

    #[test]
    fn an_acquire_that_gives_up_is_exit_2_and_never_runs_the_batch() {
        let fx = fixture();
        fx.fp.fail(crate::testutil::Step::NextId);
        let remote = FakeRemote::green();
        assert_eq!(go(&fx, &remote, &tree(&fx)), 2);
        assert!(remote.jobs.borrow().is_empty(), "no local mode, no batch without a VM");
    }
}
