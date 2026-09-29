//! `landing-pass halt` — stop a running pass cleanly — and `landing-pass sweep-red`.
//!
//! halt signals the pass named in landing.run, waits for it (SPIRA_HALT_GRACE, default 30 s)
//! before SIGKILL, tears its containers down by name, removes unpushed batch branches of
//! queue(forge) repositories, and records why. --dry-run touches nothing.

use crate::model::{LandMode, LandState, RepoRow, RunRecord};
use crate::ports::Git;
use crate::records::Files;
use crate::util::{atomic_write, iso_utc};
use std::fs;
use std::path::{Path, PathBuf};

pub trait HaltPorts {
    fn pid_alive(&self, pid: &str) -> bool;
    fn signal(&self, pid: &str, sig: i32);
    fn sleep1(&self);
    fn now(&self) -> u64;
    /// `podman ps --format {{.Names}}`.
    fn running_containers(&self) -> Vec<String>;
    /// `testenv.sh down --name <c> --volumes --force-foreign`.
    fn teardown(&self, name: &str) -> bool;
}

pub struct HaltArgs {
    pub reason: String,
    pub dry_run: bool,
}

pub struct HaltCtx<'a> {
    pub files: Files,
    pub grace: u64,
    pub self_pid: u32,
    /// None when the context seam could not run: signal, but skip branch cleanup.
    pub repos: Option<&'a [RepoRow]>,
    pub queue_dir: PathBuf,
}

/// Returns (exit code, stdout lines, stderr lines).
pub fn halt(cx: &HaltCtx, a: &HaltArgs, ports: &dyn HaltPorts, git: &dyn Git) -> (i32, Vec<String>, Vec<String>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let run = cx.files.read_run().unwrap_or_default();
    let containers: Vec<String> = fs::read_to_string(cx.files.containers())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    let running = !run.pid.is_empty() && run.pid != cx.self_pid.to_string() && ports.pid_alive(&run.pid);
    let dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
    let elapsed = || match run.started.parse::<u64>() {
        Ok(st) => format!("{}s", ports.now().saturating_sub(st)),
        Err(_) => "-".into(),
    };

    if a.dry_run {
        if !running {
            out.push("landing: no pass running".into());
            return (1, out, err);
        }
        out.push(format!(
            "landing: pass running — pid={} elapsed={} repo={} branch={} phase={}",
            run.pid,
            elapsed(),
            dash(&run.repo),
            dash(&run.branch),
            dash(&run.phase)
        ));
        for c in &containers {
            out.push(format!("landing: would tear down container {c}"));
        }
        return (0, out, err);
    }
    if !running {
        err.push("landing: no pass running — nothing to halt".into());
        return (1, out, err);
    }
    let el = elapsed();
    let reason = if a.reason.is_empty() { "unstated".to_string() } else { a.reason.replace('\n', " ") };
    let rec = format!(
        "halted={}\nreason={reason}\npid={}\nelapsed={el}\nrepo={}\nbranch={}\nphase={}\n",
        iso_utc(ports.now()),
        dash(&run.pid),
        dash(&run.repo),
        dash(&run.branch),
        dash(&run.phase)
    );
    let _ = atomic_write(&cx.files.interrupted(), &rec);
    out.push(format!(
        "landing: halting pass pid={} elapsed={el} repo={} branch={} phase={} reason={reason}",
        run.pid,
        dash(&run.repo),
        dash(&run.branch),
        dash(&run.phase)
    ));
    ports.signal(&run.pid, libc::SIGTERM);
    let mut waited = 0;
    while waited < cx.grace && ports.pid_alive(&run.pid) {
        ports.sleep1();
        waited += 1;
    }
    if ports.pid_alive(&run.pid) {
        out.push(format!("landing: pass did not stop after {}s — sending SIGKILL", cx.grace));
        ports.signal(&run.pid, libc::SIGKILL);
        ports.sleep1();
    }
    // After SIGKILL the pass's own exit path does not run.
    cx.files.clear_run();

    let live = ports.running_containers();
    for c in &containers {
        if live.iter().any(|l| l == c) {
            out.push(format!("landing: tearing down container {c}"));
            if !ports.teardown(c) {
                out.push(format!("landing: WARNING — could not tear down container {c}"));
            }
        }
    }
    if let Some(repos) = cx.repos {
        cleanup_orphan_batch_branches(repos, &cx.queue_dir, git, &mut out);
    }
    out.push(format!("landing: halted — interrupted at phase={} in {} ({el})", dash(&run.phase), dash(&run.repo)));
    (0, out, err)
}

/// Unpushed `spira/queue/*` branches other than the open batch's, in queue(forge) repos.
fn cleanup_orphan_batch_branches(repos: &[RepoRow], queue_dir: &Path, git: &dyn Git, out: &mut Vec<String>) {
    for r in repos.iter().filter(|r| r.mode == LandMode::Queue && !r.path.as_os_str().is_empty()) {
        let open_br = fs::read_to_string(queue_dir.join(&r.name).join("open"))
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("branch=").map(String::from))
            .unwrap_or_default();
        for br in git.refs_matching(&r.path, "refs/heads/spira/queue/*") {
            if br == open_br {
                continue;
            }
            if !git.refs_matching(&r.path, &format!("refs/remotes/*/{br}")).is_empty() {
                continue;
            }
            out.push(format!("landing halt: removing orphaned batch branch {br} in {}", r.name));
            if !git.delete_branch(&r.path, &br) {
                out.push(format!("landing halt: WARNING — could not remove {br}"));
            }
        }
    }
}

/// `sweep-red`: every RED landstate record as `id\ttip\tat\treason`.
pub fn sweep_red(landstate: &Path) -> (i32, Vec<String>, Vec<String>) {
    let Ok(rd) = fs::read_dir(landstate) else {
        return (1, vec![], vec![format!("sweep-red: landstate dir not found: {}", landstate.display())]);
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = Vec::new();
    for n in names {
        let Some(ls) = fs::read_to_string(landstate.join(&n)).ok().and_then(|t| first_line_state(&t)) else { continue };
        if ls.state == "RED" {
            out.push(format!("{n}\t{}\t{}\t{}", ls.tip, ls.at, ls.reason));
        }
    }
    if out.is_empty() {
        out.push("sweep-red: no RED landstate entries found".into());
    }
    (0, out, vec![])
}

/// `read -r state tip at reason < f` reads the first line only.
fn first_line_state(t: &str) -> Option<LandState> {
    LandState::parse(t.split('\n').next().unwrap_or(""))
}

pub struct RealHalt {
    pub prod: PathBuf,
    /// PATH for podman and testenv.sh (see [`child_path`]); None inherits the caller's.
    pub path: Option<String>,
}

/// The PATH halt's podman and testenv.sh run under. landing.sh halt sourced conf.sh, which
/// rebuilt PATH with `SPIRA_PATH` first; the binary is exec'd directly (by the czar, by an
/// operator), so it must apply the same seam or a podman on `SPIRA_PATH` is never found.
/// conf.sh's own answer (the context seam's `path`) wins; without a context, `SPIRA_PATH`
/// is prepended to the inherited PATH; with neither, None (inherit).
pub fn child_path(ctx_path: Option<&str>, spira_path: Option<&str>, inherited: Option<&str>) -> Option<String> {
    if let Some(p) = ctx_path.filter(|p| !p.is_empty()) {
        return Some(p.to_string());
    }
    let sp = spira_path.filter(|p| !p.is_empty())?;
    Some(match inherited.filter(|p| !p.is_empty()) {
        Some(i) => format!("{sp}:{i}"),
        None => sp.to_string(),
    })
}

impl HaltPorts for RealHalt {
    fn pid_alive(&self, pid: &str) -> bool {
        crate::real::pid_alive(pid)
    }
    fn signal(&self, pid: &str, sig: i32) {
        if let Ok(n) = pid.trim().parse::<i32>() {
            if n > 0 {
                unsafe {
                    libc::kill(n, sig);
                }
            }
        }
    }
    fn sleep1(&self) {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    fn now(&self) -> u64 {
        crate::util::unix_now()
    }
    fn running_containers(&self) -> Vec<String> {
        let mut c = crate::util::command("podman");
        if let Some(p) = &self.path {
            c.env("PATH", p);
        }
        c.args(["ps", "--format", "{{.Names}}"]);
        let (_, so, _) = crate::util::run_capture(c);
        String::from_utf8_lossy(&so).lines().map(String::from).collect()
    }
    fn teardown(&self, name: &str) -> bool {
        // --force-foreign: the pass that owned this container was just signalled to death,
        // so its owner may be an orphan with no ancestor relation to this process.
        let mut c = crate::util::command("bash");
        if let Some(p) = &self.path {
            c.env("PATH", p);
        }
        c.arg(self.prod.join("testenv.sh")).args(["down", "--name", name, "--volumes", "--force-foreign"]);
        crate::util::run_capture(c).0 == 0
    }
}

pub fn run_record_for_tests(pid: &str, started: &str, repo: &str, branch: &str, phase: &str) -> RunRecord {
    RunRecord { pid: pid.into(), started: started.into(), repo: repo.into(), branch: branch.into(), phase: phase.into() }
}
