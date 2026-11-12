//! A round on this host (DESIGN.md §2.6a): the VM round script run in the testenv container here,
//! under a home of its own so the build target stays warm between rounds. One host round at a
//! time, held by a kernel lock that dies with its holder; anything else goes to the spill provider.

use std::fs;
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::procs::{command, feed_script, FileLock};
use crate::run::{remote_command, BatchJob, Remote, FOREIGN_SCRIPT, REMOTE_EXIT_FILE, REMOTE_SCRIPT};
use crate::spill::{HostLoad, HostRule};

pub const HOST_HANDLE: &str = "host";
pub const SLOT_FILE: &str = "host-round.lock";
pub const PASS_GROUP_FILE: &str = "round-pass.group";

/// The home a host round runs under: one per `run_dir`, shared by every pass on this host.
pub fn round_home(run_dir: &Path) -> PathBuf {
    run_dir.join("host-round")
}

pub struct HostPlan {
    pub rule: HostRule,
    pub home: PathBuf,
    pub load: Box<dyn Fn() -> HostLoad + Sync>,
    pub remote: Box<dyn Remote>,
}

impl HostPlan {
    /// `None` when `SPIRA_ROUND_VM_HOST` is `off`; any other word is refused rather than read as on.
    pub fn from_config(run_dir: &Path, get: &dyn Fn(&str) -> Option<String>) -> Result<Option<HostPlan>, String> {
        match get("SPIRA_ROUND_VM_HOST").as_deref().unwrap_or("on") {
            "off" => return Ok(None),
            "on" => {}
            other => return Err(format!("round-vm: SPIRA_ROUND_VM_HOST = {other:?} is not on or off")),
        }
        let num = |key: &str, default: f64| -> Result<f64, String> {
            match get(key) {
                None => Ok(default),
                Some(v) => v.parse().map_err(|_| format!("round-vm: {key} = {v:?} is not a number")),
            }
        };
        Ok(Some(HostPlan {
            rule: HostRule { io_full_avg60: num("SPIRA_ROUND_VM_HOST_IO_FULL_AVG60", 30.0)?, cpu_some_avg60: num("SPIRA_ROUND_VM_HOST_CPU_SOME_AVG60", 60.0)? },
            home: round_home(run_dir),
            remote: Box::new(LocalRemote { home: round_home(run_dir) }),
            load: Box::new(|| HostLoad {
                io_full_avg60: crate::spill::read_io_full_avg60(crate::spill::PSI_IO),
                cpu_some_avg60: crate::spill::read_cpu_some_avg60(crate::spill::PSI_CPU),
            }),
        }))
    }
}

/// The host round slot. Dropping it, or the holder dying, frees it.
pub struct HostSlot {
    _lock: FileLock,
}

impl HostSlot {
    /// `None` when another host round holds it.
    pub fn try_take(state_dir: &Path) -> Result<Option<HostSlot>, String> {
        Ok(FileLock::try_exclusive(&state_dir.join(SLOT_FILE))?.map(|_lock| HostSlot { _lock }))
    }
}

/// What the round's private `HOME` would hide that the host already has: the operator's own
/// container store and container config. Declared here, once; the round's `HOME` stays its own.
pub fn operator_dirs(get: &dyn Fn(&str) -> Option<String>) -> Vec<(&'static str, String)> {
    let Some(home) = get("HOME").filter(|h| !h.is_empty()) else { return Vec::new() };
    let pick = |key: &str, rel: &str| get(key).filter(|v| !v.is_empty()).unwrap_or_else(|| format!("{home}/{rel}"));
    vec![("XDG_DATA_HOME", pick("XDG_DATA_HOME", ".local/share")), ("XDG_CONFIG_HOME", pick("XDG_CONFIG_HOME", ".config"))]
}

/// [`Remote`] over this machine: the VM's `$HOME` is `home`, and no ssh or rsync is involved.
pub struct LocalRemote {
    pub home: PathBuf,
}

impl LocalRemote {
    fn at(&self, rel: &str) -> PathBuf {
        let p = Path::new(rel);
        if p.is_absolute() { p.to_path_buf() } else { self.home.join(p) }
    }

    fn shell(&self, script: &str) -> std::process::Command {
        let mut c = command("bash");
        c.arg("-c").arg(script).current_dir(&self.home).env("HOME", &self.home).env_remove("SPIRA_HOME");
        for (k, v) in operator_dirs(&|k| std::env::var(k).ok()) {
            c.env(k, v);
        }
        c
    }

    fn group_file(&self) -> PathBuf {
        self.home.join(PASS_GROUP_FILE)
    }

    /// Kills the pass script this home last started, with everything it started. A pass left
    /// running here shares the checkout, the build target and the results of the pass after it.
    pub fn stop_pass(&self) {
        self.stop_pass_of(None);
    }

    /// [`Self::stop_pass`], only when the recorded pass was started by `owner`: a round that
    /// ends must not kill the pass of another round-vm process.
    pub fn stop_pass_of(&self, owner: Option<u32>) {
        let text = fs::read_to_string(self.group_file()).unwrap_or_default();
        let mut words = text.split_whitespace();
        let pgid = words.next().and_then(|p| p.parse::<i32>().ok()).filter(|p| *p > 1);
        let started = words.next().map(str::to_string);
        let by = words.next().and_then(|p| p.parse::<u32>().ok());
        if owner.is_some() && by != owner {
            return;
        }
        let _ = fs::remove_file(self.group_file());
        let Some(pgid) = pgid else { return };
        let ours = match (proc_start(pgid), started) {
            (Some(now), Some(then)) => now == then,
            (None, _) => true,
            (Some(_), None) => false,
        };
        if !ours || !group_live(pgid) {
            return;
        }
        // SAFETY: SIGKILL to the process group this home's pass script leads.
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
        // clock-safe: a cap on the wait for the killed group to be reaped
        for _ in 0..500 {
            if !group_live(pgid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn start_pass(&self, mut cmd: std::process::Command) -> std::io::Result<std::process::Child> {
        self.stop_pass();
        let child = cmd.process_group(0).spawn()?;
        let id = child.id();
        let line = format!("{id} {} {}\n", proc_start(id as i32).unwrap_or_default(), std::process::id());
        let _ = fs::write(self.group_file(), line);
        Ok(child)
    }

    fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
        fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
        let st = command("cp").arg("-a").arg(format!("{}/.", from.display())).arg(to).stdin(Stdio::null()).output().map_err(|e| format!("cp: {e}"))?;
        if st.status.success() {
            Ok(())
        } else {
            Err(format!("cp {} exited {}: {}", from.display(), st.status.code().unwrap_or(-1), String::from_utf8_lossy(&st.stderr).trim()))
        }
    }
}

impl Remote for LocalRemote {
    fn reachable(&self, _addr: &str) -> bool {
        fs::create_dir_all(&self.home).is_ok()
    }

    fn run_batch(&self, _addr: &str, job: &BatchJob) -> Result<i32, String> {
        fs::create_dir_all(&self.home).map_err(|e| format!("{}: {e}", self.home.display()))?;
        let mut shell = self.shell(&remote_command(job));
        shell.stdin(Stdio::piped());
        let mut child = self.start_pass(shell).map_err(|e| format!("bash: {e}"))?;
        feed_script(&mut child, if job.ci_entry.is_some() { FOREIGN_SCRIPT } else { REMOTE_SCRIPT }.as_bytes()).map_err(|e| format!("stdin: {e}"))?;
        loop {
            if let Some(st) = child.try_wait().map_err(|e| format!("bash: {e}"))? {
                self.stop_pass();
                return Ok(st.code().unwrap_or(255));
            }
            if job.cancel.load(std::sync::atomic::Ordering::SeqCst) {
                self.stop_pass();
                let _ = child.wait();
                return Ok(255);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn clear_results(&self, _addr: &str) -> Result<(), String> {
        let dir = self.at(crate::run::REMOTE_RESULTS);
        match fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", dir.display())),
            _ => Ok(()),
        }
    }

    fn round_started(&self, _addr: &str) -> bool {
        fs::metadata(self.home.join(crate::vm_fault::SCRIPT_LOG_FILE)).map(|m| m.len() > 0).unwrap_or(false)
    }

    fn boot_log_tail(&self, _addr: &str) -> Result<String, String> {
        Ok(String::new())
    }

    fn pull(&self, _addr: &str, remote_path: &str, local: &Path) -> Result<(), String> {
        let from = self.at(remote_path);
        if from.is_dir() {
            Self::copy_dir(&from, local)
        } else {
            fs::create_dir_all(local).map_err(|e| format!("{}: {e}", local.display()))?;
            let to = local.join(from.file_name().ok_or_else(|| format!("{remote_path}: no file name"))?);
            fs::copy(&from, &to).map(|_| ()).map_err(|e| format!("{}: {e}", from.display()))
        }
    }

    fn push(&self, _addr: &str, local: &Path, remote_path: &str) -> Result<(), String> {
        Self::copy_dir(local, &self.at(remote_path))
    }

    fn await_exit(&self, _addr: &str, within: Duration, _ssh_log: &Path) -> Result<Option<i32>, String> {
        let deadline = Instant::now() + within;
        let file = self.home.join(REMOTE_EXIT_FILE);
        loop {
            if let Ok(text) = fs::read_to_string(&file) {
                return match text.trim().strip_prefix("rc=").and_then(|n| n.parse::<i32>().ok()) {
                    Some(rc) => Ok(Some(rc)),
                    None => Err(format!("{REMOTE_EXIT_FILE} holds {:?}, not rc=N", text.trim())),
                };
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(200).min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn run_attr(&self, _addr: &str, job: &crate::spool::AttrJob) -> Result<i32, String> {
        let mut child = self.shell(&crate::spool::attr_command(job)).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().map_err(|e| format!("bash: {e}"))?;
        feed_script(&mut child, crate::spool::REMOTE_ATTR_SCRIPT.as_bytes()).map_err(|e| format!("stdin: {e}"))?;
        Ok(child.wait().map_err(|e| format!("bash: {e}"))?.code().unwrap_or(255))
    }
}

/// Whether any process still running (a zombie is not) belongs to process group `pgid`.
fn group_live(pgid: i32) -> bool {
    let Ok(rd) = fs::read_dir("/proc") else { return false };
    rd.flatten().any(|e| {
        let Ok(stat) = fs::read_to_string(e.path().join("stat")) else { return false };
        let mut f = stat.rsplit_once(')').map(|x| x.1).unwrap_or("").split_whitespace();
        let state = f.next();
        let group = f.nth(1).and_then(|g| g.parse::<i32>().ok());
        state.is_some_and(|s| s != "Z") && group == Some(pgid)
    })
}

/// When the process started, which tells a pid's reuse.
fn proc_start(pid: i32) -> Option<String> {
    use spira_config::admission::{Procs, RealProcs};
    RealProcs.start_of(u32::try_from(pid).ok()?).map(|t| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free(d: &Path) -> bool {
        matches!(HostSlot::try_take(d), Ok(Some(_)))
    }

    fn dir(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("round-vm-host-{tag}"))
    }

    #[test]
    fn the_round_keeps_the_operators_container_store_not_the_one_under_its_own_home() {
        let env = |k: &str| match k {
            "HOME" => Some("/ops/home".to_string()),
            _ => None,
        };
        assert_eq!(operator_dirs(&env), vec![("XDG_DATA_HOME", "/ops/home/.local/share".to_string()), ("XDG_CONFIG_HOME", "/ops/home/.config".to_string())]);
        let set = |k: &str| match k {
            "HOME" => Some("/ops/home".to_string()),
            "XDG_DATA_HOME" => Some("/data".to_string()),
            _ => None,
        };
        assert_eq!(operator_dirs(&set)[0], ("XDG_DATA_HOME", "/data".to_string()));
        assert!(operator_dirs(&|_| None).is_empty());
    }

    #[test]
    fn a_second_host_round_finds_the_slot_taken_until_the_first_ends() {
        let d = dir("slot");
        let first = HostSlot::try_take(d.path()).unwrap();
        assert!(first.is_some());
        assert!(!free(d.path()));
        assert!(HostSlot::try_take(d.path()).unwrap().is_none());
        drop(first);
        // A sibling test's fork holds the descriptor until its exec closes it.
        let freed = (0..200).any(|_| free(d.path()) || { std::thread::sleep(Duration::from_millis(10)); false }); // clock-safe: a cap on the wait for a forked sibling's exec to close the descriptor
        assert!(freed, "the slot stayed taken after its holder dropped it");
    }

    #[test]
    fn the_host_is_on_unless_the_operator_says_off_and_a_stray_word_is_refused() {
        let run = Path::new("/r");
        let get = |v: Option<&'static str>| move |k: &str| (k == "SPIRA_ROUND_VM_HOST").then(|| v.map(String::from)).flatten();
        assert!(HostPlan::from_config(run, &get(None)).unwrap().is_some());
        assert!(HostPlan::from_config(run, &get(Some("on"))).unwrap().is_some());
        assert!(HostPlan::from_config(run, &get(Some("off"))).unwrap().is_none());
        assert!(HostPlan::from_config(run, &get(Some("yes"))).err().is_some_and(|e| e.contains("SPIRA_ROUND_VM_HOST")));
    }

    #[test]
    fn the_thresholds_and_the_round_home_come_from_config_not_from_the_defaults() {
        let get = |k: &str| match k {
            "SPIRA_ROUND_VM_HOST_IO_FULL_AVG60" => Some("7.5".to_string()),
            "SPIRA_ROUND_VM_HOST_CPU_SOME_AVG60" => Some("11".to_string()),
            _ => None,
        };
        let p = HostPlan::from_config(Path::new("/some/run"), &get).unwrap().unwrap();
        assert_eq!((p.rule.io_full_avg60, p.rule.cpu_some_avg60), (7.5, 11.0));
        assert_eq!(p.home, Path::new("/some/run/host-round"));
        let bad = |k: &str| (k == "SPIRA_ROUND_VM_HOST_CPU_SOME_AVG60").then(|| "lots".to_string());
        assert!(HostPlan::from_config(Path::new("/r"), &bad).err().is_some());
    }

    #[test]
    fn a_local_remote_drops_the_earlier_rounds_results_before_the_next_pull() {
        let d = dir("clear");
        let r = LocalRemote { home: d.path().to_path_buf() };
        let res = d.path().join(crate::run::REMOTE_RESULTS);
        fs::create_dir_all(&res).unwrap();
        fs::write(res.join("test-old.sh.result"), "ok 1 3 - p e 0\n").unwrap();
        r.clear_results("").unwrap();
        r.clear_results("").unwrap();
        let got = d.path().join("got");
        fs::create_dir_all(&res).unwrap();
        fs::write(res.join("test-new.sh.result"), "ok 1 3 - p e 0\n").unwrap();
        r.pull("", crate::run::REMOTE_RESULTS, &got).unwrap();
        assert!(got.join("test-new.sh.result").exists());
        assert!(!got.join("test-old.sh.result").exists());
    }

    #[test]
    fn a_local_remote_pulls_and_pushes_paths_relative_to_its_home_and_reads_the_exit_file() {
        let d = dir("remote");
        let home = d.path().join("home");
        fs::create_dir_all(home.join("round-work/results")).unwrap();
        fs::write(home.join("round-work/results/a.result"), "ok").unwrap();
        fs::write(home.join(REMOTE_EXIT_FILE), "rc=5\n").unwrap();
        let r = LocalRemote { home: home.clone() };
        let out = d.path().join("out");
        r.pull("127.0.0.1", "round-work/results", &out).unwrap();
        assert_eq!(fs::read_to_string(out.join("a.result")).unwrap(), "ok");
        r.push("127.0.0.1", &out, "staged").unwrap();
        assert_eq!(fs::read_to_string(home.join("staged/a.result")).unwrap(), "ok");
        assert_eq!(r.await_exit("127.0.0.1", Duration::from_secs(1), Path::new("")).unwrap(), Some(5));
        assert!(!r.round_started("x"));
        fs::write(home.join(crate::vm_fault::SCRIPT_LOG_FILE), "x").unwrap();
        assert!(r.round_started("x"));
    }

    #[test]
    fn an_attribution_job_runs_under_the_round_home() {
        let d = dir("attr");
        let r = LocalRemote { home: d.path().to_path_buf() };
        let mut c = r.shell("pwd; echo $HOME");
        let out = c.output().unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string();
        assert_eq!(said.lines().collect::<Vec<_>>(), vec![d.path().to_str().unwrap(); 2]);
    }

    #[test]
    fn a_host_round_does_not_inherit_the_schedulers_registry_home() {
        let d = dir("home-unset");
        let r = LocalRemote { home: d.path().to_path_buf() };
        let c = r.shell("true");
        let cleared = c.get_envs().any(|(k, v)| k == "SPIRA_HOME" && v.is_none());
        assert!(cleared, "the host shell removes SPIRA_HOME from the child's inherited environment");
    }

    fn alive(pid: i32) -> bool {
        fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.rsplit_once(')').is_some_and(|x| !x.1.trim_start().starts_with('Z')))
    }

    #[test]
    fn starting_a_pass_kills_what_the_earlier_pass_left_running_in_the_home() {
        let home = dir("stop-pass");
        let remote = LocalRemote { home: home.path().to_path_buf() };
        let mut sh = remote.shell("sleep 300 & echo $! > straggler.tmp; mv straggler.tmp straggler; wait");
        sh.stdin(Stdio::null());
        let mut child = remote.start_pass(sh).unwrap();
        let straggler = (0..500).find_map(|_| fs::read_to_string(home.path().join("straggler")).ok().or_else(|| { std::thread::sleep(Duration::from_millis(10)); None })).expect("the script started its job"); // clock-safe: a cap on the wait for the shell to start its job
        let straggler: i32 = straggler.trim().parse().unwrap();
        assert!(alive(straggler));
        let mut next = remote.shell("exit 0");
        next.stdin(Stdio::null());
        let mut second = remote.start_pass(next).unwrap();
        let gone = !alive(straggler);
        remote.stop_pass();
        let _ = child.kill();
        let _ = child.wait();
        let _ = second.wait();
        assert!(gone, "the earlier pass's job outlived the start of the next pass");
    }

    #[test]
    fn a_round_that_ends_stops_its_own_pass_and_leaves_another_round_vms_running() {
        let home = dir("owner");
        let remote = LocalRemote { home: home.path().to_path_buf() };
        let mut sh = remote.shell("sleep 300");
        sh.stdin(Stdio::null());
        let mut child = remote.start_pass(sh).unwrap();
        let id = child.id() as i32;
        remote.stop_pass_of(Some(std::process::id() + 1));
        assert!(alive(id), "another round-vm's end killed this pass");
        remote.stop_pass_of(Some(std::process::id()));
        let _ = child.wait();
        assert!(!alive(id));
    }

    #[test]
    fn a_group_file_naming_a_recycled_pid_kills_nothing() {
        let home = dir("recycled");
        let remote = LocalRemote { home: home.path().to_path_buf() };
        let mut bystander = command("sleep").arg("300").process_group(0).spawn().unwrap();
        let id = bystander.id();
        fs::write(home.path().join(PASS_GROUP_FILE), format!("{id} 1 1\n")).unwrap();
        remote.stop_pass();
        assert!(alive(id as i32), "a process that is not the recorded script was killed");
        let _ = bystander.kill();
        let _ = bystander.wait();
    }

    fn attr_job() -> crate::spool::AttrJob {
        crate::spool::AttrJob { host_addr: "h".into(), mirror_port: 1, job: "j".into(), mirror_ref: "r".into(), suites: String::new(), build: crate::spool::Build::Artifacts, toolchain: None }
    }

    #[test]
    fn a_host_script_that_refuses_before_reading_itself_is_reported_by_its_exit_not_as_a_stdin_error() {
        let home = dir("attr-refuses");
        let remote = LocalRemote { home: home.path().to_path_buf() };
        let rc = remote.run_attr("h", &attr_job()).expect("the script's exit, not a stdin error");
        assert_ne!(rc, 0);
    }
}
