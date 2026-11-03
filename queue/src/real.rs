//! The ports against the host: git, bd, the lib.sh seam, the harness scripts, the forge,
//! spira-lc, the config (spira-config library only), the clock, the environment, stdio.

/// round-vm enforces the cap on the tests itself; the outer `timeout` only catches a hang in the
/// unbounded rest (pulls, post-processing).
const ROUND_HANG_GUARD_FACTOR: u64 = 2;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::model::{BeadRow, LandMode, LcBeadRow, RangeCommit};
use crate::ports::*;
use crate::records::Kv;
use crate::seam::{self, Op};

/// Close every fd >= 3 in the child before it execs. This process's own fds are already
/// close-on-exec (Rust's std opens them that way), but a caller's lock fd this process
/// never opened itself — inherited from whatever bash wrapper invoked `queue` (an
/// `exec 9>…` lock with no O_CLOEXEC) — carries none of that protection. `gate.sh` and
/// `testenv` are reached from here (observe-flake, the verdict pass's gate call), and
/// testenv starts podman, whose conmon daemonizes and would hold such a leaked fd forever
/// (sp-ohwg7). close_range is the fast path; a kernel too old for it (< 5.9) falls back to
/// fcntl(F_SETFD) per fd.
fn close_inherited_fds(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
            let r = libc::syscall(
                libc::SYS_close_range,
                3u32,
                libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if r != 0 {
                for fd in 3..4096 {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
            }
            Ok(())
        });
    }
}

/// Run `cmd` with stdout and stderr interleaved into one capture (the `2>&1` of a
/// `$(…)`), through a temp file both descriptors share.
fn run_combined(cmd: &mut Command) -> (i32, String) {
    let path = std::env::temp_dir().join(format!("queue-out-{}-{}", std::process::id(), unique()));
    let Ok(f) = File::create(&path) else {
        return (127, String::new());
    };
    let Ok(f2) = f.try_clone() else {
        return (127, String::new());
    };
    close_inherited_fds(cmd);
    let rc = cmd
        .stdin(Stdio::null())
        .stdout(f)
        .stderr(f2)
        .status()
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(127);
    let out = fs::read_to_string(&path).unwrap_or_default();
    let _ = fs::remove_file(&path);
    (rc, out)
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

fn ok(cmd: &mut Command) -> bool {
    close_inherited_fds(cmd);
    cmd.stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn stdout_of(cmd: &mut Command) -> Option<String> {
    close_inherited_fds(cmd);
    let o = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).to_string())
}

// ------------------------------------------------------------------------------------ git

pub struct RealGit;

fn git(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo);
    c
}

impl Git for RealGit {
    fn rev_parse(&self, repo: &Path, rev: &str) -> Option<String> {
        stdout_of(git(repo).args(["rev-parse", "--verify", "-q", rev]))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    fn is_ancestor(&self, repo: &Path, a: &str, b: &str) -> bool {
        ok(git(repo)
            .args(["merge-base", "--is-ancestor", a, b])
            .stderr(Stdio::null()))
    }
    fn merge_base(&self, repo: &Path, a: &str, b: &str) -> Option<String> {
        stdout_of(git(repo).args(["merge-base", a, b]).stderr(Stdio::null()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    fn update_ref(&self, repo: &Path, refname: &str, new: &str, old: Option<&str>) -> bool {
        let mut c = git(repo);
        c.args(["update-ref", refname, new]);
        if let Some(o) = old {
            c.arg(o);
        }
        ok(c.stderr(Stdio::null()))
    }
    fn ref_exists(&self, repo: &Path, refname: &str) -> bool {
        ok(git(repo)
            .args(["show-ref", "--verify", "--quiet", refname])
            .stderr(Stdio::null()))
    }
    fn current_branch(&self, repo: &Path) -> Option<String> {
        stdout_of(git(repo).args(["symbolic-ref", "-q", "--short", "HEAD"]))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    fn fetch(&self, repo: &Path, remote: &str, branch: &str) -> bool {
        let mut c = git(repo);
        c.args(["fetch", "-q", remote]);
        if !branch.is_empty() {
            c.arg(branch);
        }
        ok(c.stdout(Stdio::null()).stderr(Stdio::null()))
    }
    fn log_range(&self, repo: &Path, range: &str) -> Result<Vec<RangeCommit>, String> {
        let o = git(repo)
            .args(["log", "-z", "--format=%H%x1f%P%x1f%s", range])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
        Ok(crate::publish_range::parse_log(&String::from_utf8_lossy(
            &o.stdout,
        )))
    }
    fn commit_exists(&self, repo: &Path, sha: &str) -> bool {
        ok(git(repo)
            .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .stderr(Stdio::null()))
    }
    fn numstat(&self, repo: &Path, a: &str, b: &str) -> Result<Vec<(String, u32, u32)>, String> {
        let o = git(repo)
            .args(["diff", "--numstat", "--no-renames", a, b])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| {
                let mut f = l.splitn(3, '\t');
                let (add, del, path) = (f.next()?, f.next()?, f.next()?);
                Some((
                    path.to_string(),
                    add.parse().unwrap_or(0),
                    del.parse().unwrap_or(0),
                ))
            })
            .collect())
    }
    fn path_exists(&self, repo: &Path, rev: &str, path: &str) -> bool {
        ok(git(repo)
            .args(["cat-file", "-e", &format!("{rev}:{path}")])
            .stderr(Stdio::null()))
    }
    fn file_at(&self, repo: &Path, rev: &str, path: &str) -> Option<String> {
        stdout_of(git(repo).args(["show", &format!("{rev}:{path}")]))
    }
    fn grep_files(&self, repo: &Path, rev: &str, needle: &str, glob: &str) -> Vec<String> {
        stdout_of(git(repo).args([
            "grep",
            "-l",
            "-F",
            "-e",
            needle,
            rev,
            "--",
            &format!(":(glob){glob}"),
        ]))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix(&format!("{rev}:")).map(String::from))
        .collect()
    }
    fn branch_set(&self, repo: &Path, name: &str, sha: &str, force: bool) -> bool {
        let mut c = git(repo);
        c.arg("branch");
        if force {
            c.arg("-f");
        }
        ok(c.args([name, sha])
            .stdout(Stdio::null())
            .stderr(Stdio::null()))
    }
    fn branch_delete(&self, repo: &Path, name: &str) -> bool {
        ok(git(repo)
            .args(["branch", "-D", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null()))
    }
    fn branch_delete_sanctioned(&self, repo: &Path, name: &str) -> bool {
        ok(git(repo)
            .args(["branch", "-D", name])
            .env("SPIRA_REF_SANCTIONED", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null()))
    }
    fn branches(&self, repo: &Path, prefix: &str) -> Vec<(String, String)> {
        stdout_of(git(repo).args([
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            prefix,
        ]))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            l.split_once(' ')
                .map(|(a, b)| (a.to_string(), b.to_string()))
        })
        .collect()
    }
    fn worktree_prune(&self, repo: &Path) {
        let _ = ok(git(repo).args(["worktree", "prune"]).stderr(Stdio::null()));
    }
    fn worktree_add_detached(&self, repo: &Path, path: &Path, sha: &str) -> bool {
        ok(git(repo)
            .args(["worktree", "add", "-q", "--detach"])
            .arg(path)
            .arg(sha)
            .stdout(Stdio::null())
            .stderr(Stdio::null()))
    }
    fn worktree_remove(&self, repo: &Path, path: &Path) {
        let _ = ok(git(repo)
            .args(["worktree", "remove", "-f"])
            .arg(path)
            .stderr(Stdio::null()));
    }
    fn merge_no_ff(
        &self,
        wt: &Path,
        message: &str,
        tip: &str,
        git_name: &str,
        git_email: &str,
    ) -> bool {
        // The subject carries a bead title (free text): a file, never argv.
        let msg = std::env::temp_dir().join(format!(
            "queue-merge-msg-{}-{}",
            std::process::id(),
            unique()
        ));
        if fs::write(&msg, message).is_err() {
            return false;
        }
        let (name, email) = (git_name, git_email);
        let r = ok(git(wt)
            .arg("-c")
            .arg(format!("user.name={name}"))
            .arg("-c")
            .arg(format!("user.email={email}"))
            .args(["merge", "--no-edit", "--no-ff", "-F"])
            .arg(&msg)
            .arg(tip)
            .stdout(Stdio::null())
            .stderr(Stdio::null()));
        let _ = fs::remove_file(&msg);
        r
    }
    fn merge_abort(&self, wt: &Path) {
        let _ = ok(git(wt).args(["merge", "--abort"]).stderr(Stdio::null()));
    }
    fn rebase_onto(
        &self,
        repo: &Path,
        scratch: &Path,
        tip: &str,
        upstream: &str,
        onto: &str,
        git_name: &str,
        git_email: &str,
    ) -> Result<String, Vec<String>> {
        self.worktree_prune(repo);
        self.worktree_remove(repo, scratch);
        if !self.worktree_add_detached(repo, scratch, tip) {
            return Err(Vec::new());
        }
        let done = ok(git(scratch)
            .arg("-c")
            .arg(format!("user.name={git_name}"))
            .arg("-c")
            .arg(format!("user.email={git_email}"))
            .args(["rebase", "--onto", onto, upstream])
            .stdout(Stdio::null())
            .stderr(Stdio::null()));
        let result = if done {
            self.rev_parse(scratch, "HEAD").ok_or_else(Vec::new)
        } else {
            let paths = stdout_of(git(scratch).args(["diff", "--name-only", "--diff-filter=U"]))
                .unwrap_or_default();
            let _ = ok(git(scratch)
                .args(["rebase", "--abort"])
                .stderr(Stdio::null()));
            Err(paths.lines().map(str::to_string).collect())
        };
        self.worktree_remove(repo, scratch);
        result
    }
    fn is_clean(&self, wt: &Path) -> bool {
        stdout_of(git(wt).args(["status", "--porcelain"]))
            .map(|s| s.trim().is_empty())
            .unwrap_or(false)
    }
}

// ------------------------------------------------------------------------------------- bd

pub struct RealBd {
    /// The sanctioned bd wrapper; it reads the store from the config itself.
    pub bdq: String,
}

impl Bd for RealBd {
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        let timeout = std::env::var("BD_TIMEOUT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "180".into());
        let mut out = Vec::new();
        for chunk in ids.chunks(100) {
            let o = Command::new("timeout")
                .arg(&timeout)
                .arg(&self.bdq)
                .arg("show")
                .args(chunk)
                .arg("--json")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
            if !o.status.success() {
                return Err(format!("bd show exited {:?}", o.status.code()));
            }
            out.extend(parse_bd_json(&String::from_utf8_lossy(&o.stdout))?);
        }
        Ok(out)
    }
}

/// `bdjson`'s `json_only`: from the first line starting `[` or `{`; one object or a list.
pub fn parse_bd_json(text: &str) -> Result<Vec<BeadRow>, String> {
    let start = text
        .lines()
        .position(|l| l.starts_with('[') || l.starts_with('{'))
        .ok_or("bd answered no JSON")?;
    let body: String = text.lines().skip(start).collect::<Vec<_>>().join("\n");
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    Ok(items
        .into_iter()
        .filter_map(|i| serde_json::from_value(i).ok())
        .collect())
}

// ------------------------------------------------------------------------------- lib seam

pub struct RealLib {
    pub home: PathBuf,
}

impl RealLib {
    fn home_str(&self) -> String {
        self.home.display().to_string()
    }

    /// The repo registry, in-process (sp-o88bx, "wave 4.12": family W — `spira_landref`/
    /// `spira_publish_forge` — joins the family-U lookups `context()`/`readback()` no
    /// longer wait on a bash+spira-config round trip for). `Registry::from_env` (sp-k6lku,
    /// following through on sp-z3eyk's `registry_env`) is the one production door onto a
    /// registry: it resolves `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO`/`SPIRA_REPO`/
    /// `SPIRA_REPO_DERIVED` in-process when this process's own (bare, unit-launched)
    /// environment lacks them, which it always does — conf.sh exports none of them.
    fn repo_registry(&self) -> spira_config::repos::Registry {
        spira_config::repos::Registry::from_env(std::env::vars().collect(), &self.home)
    }

    /// Run one seam op. `capture`: stdout is returned (logs before the answer mark are
    /// re-printed); otherwise stdout is inherited, as the bash call's was.
    fn call(&self, op: Op, vals: &[&str], capture: bool) -> (i32, String) {
        let home = self.home_str();
        let mut all = vec![home.as_str()];
        all.extend_from_slice(vals);
        // batch-job: this runs whatever its caller names, as long as that takes
        let mut cmd = Command::new("bash");
        cmd.stdin(Stdio::piped()).stderr(Stdio::inherit());
        cmd.stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });
        match spira_config::release_env::child_path_env_for_process() {
            Ok(e) => cmd.envs(e),
            Err(e) => return (127, e),
        };
        let Ok(mut child) = cmd.spawn() else {
            return (127, String::new());
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(&seam::stdin_bytes(op, &all));
        }
        let mut out = String::new();
        if let Some(mut so) = child.stdout.take() {
            let mut buf = Vec::new();
            let _ = so.read_to_end(&mut buf);
            out = String::from_utf8_lossy(&buf).to_string();
        }
        let rc = child.wait().ok().and_then(|s| s.code()).unwrap_or(127);
        (rc, out)
    }

    fn answer(&self, op: Op, vals: &[&str]) -> (i32, String) {
        let (rc, out) = self.call(op, vals, true);
        let (logs, ans) = seam::split_answer(&out);
        if !logs.is_empty() {
            print!("{logs}");
        }
        (rc, ans.to_string())
    }
}

fn pb(s: &str) -> Option<PathBuf> {
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// `git -C <repo> remote`, space-joined — matching the bash's own `git -C "$__p" remote
/// 2>/dev/null | tr '\n' ' '`, which `real.rs`'s callers already split back apart with
/// `split_whitespace`.
fn git_remotes(repo: &str) -> Vec<String> {
    // batch-job: git history or network operation, as long as the repository is large
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("remote")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Every key `context` must resolve; `queue check-config` refuses a registry that lacks one.
pub const CONFIG_KEYS: &[&str] = &[
    "SPIRA_CERTIFY_SUITES",
    "SPIRA_GIT_NAME",
    "SPIRA_GIT_EMAIL",
    "SPIRA_MAIL_SESSION_MAILBOX",
    "SPIRA_ROUND_CERTIFY_WALL_SECS",
    "SPIRA_ROUND_AMI_WAIT_SECS",
    "SPIRA_QUEUE_ROUNDS_MAX",
    "SPIRA_PUBLISH_RED_PRIORITY",
    "SPIRA_PUBLISH_RED_ESCALATE_AFTER",
    "SPIRA_PUBLISH_SETTLE_DEFER_MAX",
];

impl Lib for RealLib {
    fn context(&self, repo: Option<&str>) -> Result<(Settings, RepoCtx), String> {
        // spira_home_repo/repo_root/repo_land/repo_field (family U) and spira_landref/
        // spira_publish_forge (family W, sp-o88bx "wave 4.12") no longer a bash seam call
        // (sp-k6lku, "wave 4.13"): the name is resolved in-process first, then threaded
        // into the (slimmed) CONTEXT seam as $1 only so its CI-timeout per-repo override
        // lookup still has it, and every repo-registry field below reads the same
        // Registry the shims used to shell out to.
        let reg = self.repo_registry();
        let name = repo
            .map(str::to_string)
            .unwrap_or_else(|| reg.home_repo().to_string());
        let (rc, ans) = self.answer(Op::Context, &[&name]);
        if rc != 0 || ans.is_empty() {
            return Err(format!("lib.sh context seam exited {rc}"));
        }
        let kv = seam::parse_kv0(&ans);
        let g = |k: &str| kv.get(k).cloned().unwrap_or_default();
        let n = |k: &str, d: u64| kv.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        if g("run").is_empty() {
            return Err("SPIRA_RUN is unset after sourcing lib.sh".into());
        }
        // law-one-source-of-config: these five are not part of the lib.sh seam's kv
        // answer above (seam R1 predates spira-config) — read straight from $SPIRA_TOML
        // through the one door, never from the process environment.
        let certify_suites = spira_config::process::cfg("SPIRA_CERTIFY_SUITES")?;
        let git_name = spira_config::process::cfg("SPIRA_GIT_NAME")?;
        let git_email = spira_config::process::cfg("SPIRA_GIT_EMAIL")?;
        let mailbox = spira_config::process::cfg("SPIRA_MAIL_SESSION_MAILBOX")?;
        let round_wall_secs =
            spira_config::process::cfg_parse::<u64>("SPIRA_ROUND_CERTIFY_WALL_SECS")?;
        let round_ami_wait_secs =
            spira_config::process::cfg_parse::<u64>("SPIRA_ROUND_AMI_WAIT_SECS")?;
        let rounds_max = spira_config::process::cfg_parse::<usize>("SPIRA_QUEUE_ROUNDS_MAX")?;
        let publish_red_priority = spira_config::process::cfg("SPIRA_PUBLISH_RED_PRIORITY")?;
        let publish_red_escalate_after =
            spira_config::process::cfg_parse::<usize>("SPIRA_PUBLISH_RED_ESCALATE_AFTER")?;
        let publish_defer_max = match spira_config::process::cfg_parse::<usize>("SPIRA_PUBLISH_SETTLE_DEFER_MAX") {
            Ok(n) => Some(n),
            Err(e) if e.contains("not a registered config key") => None,
            Err(e) => return Err(e),
        };
        let s = Settings {
            home: PathBuf::from(g("home")),
            run: PathBuf::from(g("run")),
            queue_dir: PathBuf::from(g("queue_dir")),
            releases: pb(&g("releases")),
            forge: PathBuf::from(g("forge")),
            repo_map: pb(&g("repo_map")),
            // Every harness tool by name, on the launcher's PATH (sp-gypjk).
            batcher_bin: Some(PathBuf::from(spira_config::release_env::tool("batcher")?)),
            batcher_off: g("batcher_enable").trim() == "0",
            lc_bin: Some(PathBuf::from(spira_config::release_env::tool("spira-lc")?)),
            submitted_label: g("submitted_label"),
            home_repo: reg.home_repo().to_string(),
            db: g("db"),
            transition_pollsec: n("pollsec", 5),
            transition_maxsec: n("maxsec", 1800),
            preflight_wall_secs: n("preflight", 240),
            verdict: VerdictSettings {
                ci_maxsec: n("ci_maxsec", 3600),
                ci_idle_sec: n("ci_idle", 600),
                infra_retries: n("infra_retries", 2),
                lock_wait: n("lock_wait", 90),
                lock_starve_max: n("starve_max", 5),
                capacity_backoff_min: n("capacity_backoff_min", 300),
                capacity_backoff_max: n("capacity_backoff_max", 3600),
                incident_priority: Some(g("incident_priority"))
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(|| "1".into()),
                red_tracker_label: g("red_tracker_label"),
                mender_label: g("mender_label"),
            },
            certify_suites,
            git_name,
            git_email,
            mailbox,
            round_wall_secs,
            round_ami_wait_secs,
            round_spill: spira_config::process::cfg("SPIRA_ROUND_VM_SPILL").unwrap_or_default(),
            rounds_max,
            publish_red_priority,
            publish_red_escalate_after,
            publish_defer_max,
        };
        let path = reg.root(&name);
        let landref = spira_config::repos::landref(&reg, &name).filter(|s| !s.is_empty());
        let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let publish = spira_config::repos::publish_forge(&reg, &name, &env_map);
        let unpublished = spira_config::repos::publishes_nowhere(&reg, &name, &env_map);
        let publish_mode = spira_config::repos::publish_mode(&reg, &name);
        let remotes = path.as_deref().map(git_remotes).unwrap_or_default();
        let path = path.map(PathBuf::from);
        let r = RepoCtx {
            mode: LandMode::parse(&reg.land(&name)).unwrap_or(LandMode::Push),
            map_land: reg
                .field(&name, spira_config::repos::Column::Land)
                .unwrap_or_default(),
            map_base: reg
                .field(&name, spira_config::repos::Column::Base)
                .unwrap_or_default(),
            name,
            path,
            landref,
            publish,
            unpublished,
            publish_mode,
            remotes,
        };
        Ok((s, r))
    }
    fn repos(&self) -> Result<Vec<String>, String> {
        // spira_repos (family U) in-process (sp-k6lku, "wave 4.13"); was `_spira_config_repo
        // all` through a bash seam call. Always non-empty: the home repo is unconditionally
        // first.
        Ok(self.repo_registry().all())
    }
    fn toml_path(&self) -> Option<PathBuf> {
        let (_, ans) = self.answer(Op::TomlPath, &[]);
        pb(ans.trim())
    }
    fn readback(&self, name: &str) -> (String, String) {
        // repo_land/spira_landref (sp-k6lku, "wave 4.13"; spira_landref since sp-o88bx
        // "wave 4.12") both in-process now — no bash seam call at all for this op.
        let reg = self.repo_registry();
        let land = reg.land(name);
        let landref = spira_config::repos::landref(&reg, name).unwrap_or_default();
        (land, landref)
    }
    fn bead_reopen(&self, id: &str, cause: &str, suites: &str) -> bool {
        self.call(Op::BeadReopen, &[id, cause, suites], false).0 == 0
    }
    fn cause_event(&self, id: &str, cause: &str) {
        self.call(Op::CauseEvent, &[id, cause], false);
    }
    fn release_claim(&self, id: &str) {
        self.call(Op::ReleaseClaim, &[id], false);
    }
    // Through the lifecycle machine (sp-3fue0j), never a raw bd close.
    fn bead_close(&self, id: &str, reason: &str) -> bool {
        spira_config::lifecycle_row::close_landed(id, reason, "queue").is_ok()
    }
    fn bead_retire(&self, id: &str, reason: &str) -> Result<(), String> {
        spira_config::lifecycle_row::close(id, reason, "queue", None)
    }
    fn reap_landed_branch(
        &self,
        id: &str,
        repo: &str,
        branch: &str,
        why: &str,
    ) -> Result<bool, String> {
        let Some(root) = self.repo_registry().root(repo) else {
            return Ok(false);
        };
        if !RealGit.ref_exists(Path::new(&root), &format!("refs/heads/{branch}")) {
            return Ok(false);
        }
        // batch-job: the Hel's verified branch+worktree deletion (a content fence over
        // the branch's diff), bounded by its own SPIRA_REAP_TIMEOUT below.
        let timeout = std::env::var("SPIRA_REAP_TIMEOUT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "300".into());
        let mut c = Command::new("timeout");
        c.arg(timeout).arg("hel").arg("reap-landed-branch");
        if let Ok(f) = std::env::var("SPIRA_STATUS_FILE") {
            c.arg("--status-from").arg(f);
        }
        c.args([id, branch, &root, why])
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        match c.output() {
            Ok(o) if o.status.success() => Ok(true),
            Ok(o) => {
                let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
                Err(if t.is_empty() { "unknown".into() } else { t })
            }
            Err(e) => Err(e.to_string()),
        }
    }
    /// `gh-intake closeout <id> <sha> <repo>` (sp-j3fim, "wave 4.31"): gh_issue_closeout
    /// moved natively into gh-intake; this crate shells to the compiled binary by bare
    /// name now, like the other compiled tools — no lib.sh snippet
    /// backs this any more. stdout/stderr inherit straight through, exactly as the
    /// dropped seam call's own `capture: false` did.
    fn gh_issue_closeout(&self, id: &str, sha: &str, repo: &Path) {
        let _ = spira_config::bounded::bounded("gh-intake")
            .args(["closeout", id, sha, &repo.display().to_string()])
            .status();
    }
    fn comment(&self, id: &str, text: &str) {
        self.call(Op::Comment, &[id, text], false);
    }
    fn notify(&self, mailbox: &str, repo: &str, subject: &str, body: &str) {
        // In-process (sp-hwjsq, "wave 4.32"): queue owns this family now, so its own seam
        // call onto queue_notify_concierge is gone — lib.sh's copy is retired outright.
        crate::ops::helpers::notify(mailbox, repo, subject, body);
    }
    fn event(&self, kind: &str, title: &str, detail: &str) {
        self.call(Op::Event, &[kind, title, detail], false);
    }
    fn divergence(
        &self,
        mailbox: &str,
        queue_dir: &Path,
        repo: &str,
        path: &Path,
        forge: &str,
        local: &str,
    ) -> Divergence {
        // In-process (sp-hwjsq, "wave 4.32"); lib.sh's queue_local_check_divergence is
        // retired outright — nothing else ever called it.
        crate::ops::helpers::check_divergence(mailbox, queue_dir, repo, path, forge, local)
    }
    fn push(&self, path: &Path, remote: &str, refspec: &str) -> bool {
        // In-process (sp-hwjsq, "wave 4.32"). Stderr discarded here, as the old seam body
        // (`spira_git_push ... 2>/dev/null`) did for this call site specifically —
        // spira_git_push itself never redirected anything; that stays the caller's choice,
        // which is why the CLI shim for bash callers (below, in main.rs) does not.
        ok(crate::ops::helpers::git_push_cmd(
            path,
            &["-q".to_string(), remote.to_string(), refspec.to_string()],
        )
        .stderr(Stdio::null()))
    }
    fn rebase(&self, branch: &str, onto: &str, path: &Path, name: &str) -> Result<(), String> {
        let (rc, why) = self.answer(
            Op::Rebase,
            &[branch, onto, &path.display().to_string(), name],
        );
        if rc == 0 {
            Ok(())
        } else {
            Err(why)
        }
    }
    fn land_subject(&self, id: &str) -> String {
        // land_subject prints one line (the title is whitespace-collapsed); anything before
        // it inside the command substitution is not the subject.
        let (_, ans) = self.answer(Op::LandSubject, &[id]);
        let ans = ans
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string();
        if ans.is_empty() {
            format!("spira: land {id}")
        } else {
            ans
        }
    }
    fn sort_rows(
        &self,
        express: &HashSet<String>,
        path: &Path,
        base: &str,
        prio_json: &str,
        rows: &str,
    ) -> Vec<(String, String)> {
        // In-process (sp-hwjsq, "wave 4.32"): queue_sort_rows and its internal
        // queue_is_suite_transition, both ported here. queue_is_suite_transition has no
        // caller left at all once this goes in-process, so lib.sh's copy is retired
        // outright; queue_sort_rows itself keeps a shim (test-queue-sort-large.sh,
        // cockpit-collect still call it by name).
        let with_trans: Vec<(String, String, i64, bool)> = crate::ops::helpers::parse_rows(rows)
            .into_iter()
            .map(|(id, tip, epoch)| {
                let is_trans = crate::ops::helpers::is_suite_transition(path, &tip, base);
                (id, tip, epoch, is_trans)
            })
            .collect();
        let (ranked, warning) = crate::ops::helpers::sort_rows(&with_trans, prio_json, express);
        if let Some(w) = warning {
            eprintln!("{w}");
        }
        ranked.into_iter().map(|r| (r.id, r.tip)).collect()
    }
    fn cancel_runs(&self, forge: &Path, path: &Path, branch: &str) {
        // In-process (sp-hwjsq, "wave 4.32"); lib.sh's queue_cancel_branch_runs is retired
        // outright — nothing else ever called it. Best-effort, as the old seam body
        // (`... || true; exit 0`) was: a failed cancel is logged, never surfaced to the
        // caller, who has no retry path of its own here (the next abandon does).
        crate::ops::helpers::cancel_branch_runs(forge, path, branch, "QUEUE");
    }
    fn format_batch(&self, wt: &Path, base: &str, name: &str) {
        self.call(
            Op::FormatBatch,
            &[&wt.display().to_string(), base, name],
            false,
        );
    }
    fn base_conflict(&self, path: &Path, base: &str, tip: &str) -> bool {
        self.call(
            Op::BaseConflict,
            &[&path.display().to_string(), base, tip],
            false,
        )
        .0 == 0
    }
    fn pf_gate(&self, branch: &str, name: &str, stamp: &str, wall_secs: u64) -> (i32, String) {
        self.answer(Op::PfGate, &[branch, name, stamp, &wall_secs.to_string()])
    }
    fn create_bug(
        &self,
        actor: &str,
        title: &str,
        priority: &str,
        labels: &str,
        body: &str,
    ) -> Option<String> {
        let (_, ans) = self.answer(Op::CreateBug, &[actor, title, priority, labels, body]);
        Some(ans.trim().to_string()).filter(|id| !id.is_empty())
    }
    fn amend_bug(&self, actor: &str, id: &str, note: &str) -> bool {
        self.answer(Op::AmendBug, &[actor, id, note]).1.trim() == "amended"
    }
    fn bead_live(&self, id: &str) -> bool {
        self.answer(Op::BeadLive, &[id]).1.trim() == "live"
    }
}

// -------------------------------------------------------------------------------- scripts

pub const RESUBMIT_CHECK_REASON: &str = "resubmit check of a new tip, not a retry";

/// The first resubmit check of a tip is not a retry of the red that ejected the bead; a second
/// check of the same tip is, and keeps testenv's repeat guard.
fn suite_run_command(testenv: &str, check: &str, tip: &str, repo: &str, first_check: bool) -> Command {
    let mut c = Command::new(testenv);
    if first_check {
        c.args(["--repeat-reason", RESUBMIT_CHECK_REASON]);
    }
    c.args(["--suites", check, tip, repo]);
    c
}

pub struct RealScripts {
    pub home: PathBuf,
}

impl Scripts for RealScripts {
    fn lint(&self, tip: &str, repo: &Path, verify: bool) -> (i32, String) {
        let testenv = match spira_config::release_env::tool("testenv") {
            Ok(t) => t,
            Err(e) => return (2, e),
        };
        let mut c = Command::new(testenv);
        c.arg("lint");
        if verify {
            c.arg("--verify");
        }
        run_combined(c.arg(tip).arg(repo))
    }
    fn suite_run(&self, check: &str, tip: &str, repo: &str, first_check: bool) -> (i32, String) {
        match spira_config::release_env::tool("testenv") {
            Ok(testenv) => run_combined(&mut suite_run_command(&testenv, check, tip, repo, first_check)),
            Err(e) => (2, e),
        }
    }
    fn gate(&self, branch: &str, repo: &str, bead: &str, suites: &str) -> (i32, String) {
        run_combined(
            Command::new("gate.sh")
                .arg(branch)
                .arg(repo)
                .env("SPIRA_GATE_BEAD", bead)
                .env("SPIRA_GATE_SUITES", suites),
        )
    }
    fn judgement_ci(
        &self,
        bin: &Path,
        s: &Settings,
        repo: &str,
        suites: &str,
        members: &str,
        evidence: &str,
    ) -> RunOut {
        let (rc, out) = run_combined(
            Command::new(bin)
                .arg("judgement-ci")
                .arg(repo)
                .args([
                    "--suites",
                    suites,
                    "--members",
                    members,
                    "--evidence",
                    evidence,
                ])
                .arg("--home")
                .arg(&s.home)
                .arg("--run")
                .arg(&s.run)
                .arg("--db")
                .arg(&s.db),
        );
        RunOut {
            rc,
            out,
            err: String::new(),
        }
    }
    fn observe_flake(&self, suite: &str, sha: &str) {
        let Ok(testenv) = spira_config::release_env::tool("testenv") else { return };
        let _ = ok(Command::new(testenv)
            .args(["suites", "observe-flake", suite, sha])
            .stdout(Stdio::null())
            .stderr(Stdio::null()));
    }
    fn mail_operator(&self, subject: &str, body: &str) {
        let Ok(mut child) = spira_config::bounded::bounded("mail")
            .args([
                "send",
                "operator",
                "--from",
                "Spira Queue <queue@spira>",
                "--subject",
                subject,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(body.as_bytes());
        }
        let _ = child.wait();
    }
    fn batcher_cut(&self, bin: &Path, repo: &str, wait_zero: bool) -> i32 {
        // batch-job: this runs whatever its caller names, as long as that takes
        let mut c = Command::new(bin);
        c.arg("cut").arg(repo).arg("--home").arg(&self.home);
        if wait_zero {
            c.env("SPIRA_QUEUE_BATCH_WAIT", "0");
        }
        c.stdin(Stdio::null())
            .status()
            .ok()
            .and_then(|s| s.code())
            .unwrap_or(127)
    }
    fn path_attribution(&self, bin: &Path, repo: &str, members: &str, reds: &Path) -> String {
        // batch-job: path-attribute reads git diffs only
        let out = Command::new(bin)
            .args(["path-attribute", repo, "--home"])
            .arg(&self.home)
            .args(["--members", members, "--reds"])
            .arg(reds)
            .stdin(Stdio::null())
            .output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }
    fn czar_fence(&self, class: &str) -> bool {
        ok(Command::new("czar-fence.sh").arg(class))
    }
    fn release_tool(&self) -> Result<PathBuf, String> {
        spira_config::release_env::tool("release").map(PathBuf::from)
    }
    fn release(&self, bin: &Path, args: &[String], db: &str) -> RunOut {
        // batch-job: this runs whatever its caller names, as long as that takes
        match Command::new(bin)
            .args(args)
            .env("SPIRA_DB", db)
            .stdin(Stdio::null())
            .output()
        {
            Ok(o) => RunOut {
                rc: o.status.code().unwrap_or(127),
                out: String::from_utf8_lossy(&o.stdout).to_string(),
                err: String::from_utf8_lossy(&o.stderr).to_string(),
            },
            Err(e) => RunOut {
                rc: 127,
                out: String::new(),
                err: format!("cannot run release: {e}"),
            },
        }
    }
    fn unit_active_state(&self, unit: &str) -> String {
        // batch-job: one systemctl read of one unit
        Command::new("systemctl")
            .args(["--user", "show", "-p", "ActiveState", "--value", unit])
            .stdin(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }
    fn round_vm(
        &self,
        tree: &Path,
        results: &Path,
        base: &str,
        round: (&str, &str),
        wall_secs: u64,
        handle: &Path,
        ami: Option<&str>,
    ) -> RunOut {
        run_round_vm(
            tree,
            results,
            ["--base", base],
            round,
            wall_secs,
            handle,
            ami,
        )
    }
    fn recipe_sha(&self, tree: Option<&Path>) -> Option<String> {
        let mut cmd = Command::new(spira_config::release_env::tool("round-vm").ok()?);
        cmd.arg("recipe-sha");
        if let Some(t) = tree {
            cmd.arg("--tree").arg(t);
        }
        stdout_of(&mut cmd)
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }
    fn ami_for(&self, sha: &str) -> AmiLookup {
        let round_vm = match spira_config::release_env::tool("round-vm") {
            Ok(r) => r,
            Err(e) => return AmiLookup::Unreadable(e),
        };
        // batch-job: one AWS describe-images read, hang-guarded at 60 s
        match Command::new("timeout")
            .args([
                "60",
                round_vm.as_str(),
                "ami-for",
                sha,
            ])
            .stdin(Stdio::null())
            .output()
        {
            Ok(o) if o.status.code() == Some(0) => {
                AmiLookup::Found(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            Ok(o) if o.status.code() == Some(1) => AmiLookup::Missing,
            Ok(o) => AmiLookup::Unreadable(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => AmiLookup::Unreadable(format!("cannot run round-vm: {e}")),
        }
    }
    fn repo_round_vm(
        &self,
        tree: &Path,
        results: &Path,
        entry: &str,
        round: (&str, &str),
        wall_secs: u64,
        handle: &Path,
    ) -> RunOut {
        run_round_vm(
            tree,
            results,
            ["--ci-entry", entry],
            round,
            wall_secs,
            handle,
            None,
        )
    }
    fn pass_terminate(&self, pid: u32) {
        // SAFETY: a plain SIGTERM to the pid the run wrote to its handle.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }
    fn pass_alive(&self, pid: u32) -> bool {
        // SAFETY: signal 0 only checks existence.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    fn rebase_stale(&self, ids: &[String], repo: &str) -> Vec<(String, RunOut)> {
        let rebase_stale = match spira_config::release_env::tool("rebase-stale") {
            Ok(r) => r,
            Err(e) => return ids.iter().map(|id| (id.clone(), RunOut { rc: 127, out: String::new(), err: e.clone() })).collect(),
        };
        let width = ids.len().clamp(1, REBASE_WIDTH);
        let next = std::sync::atomic::AtomicUsize::new(0);
        let slots: Vec<std::sync::Mutex<Option<RunOut>>> =
            ids.iter().map(|_| std::sync::Mutex::new(None)).collect();
        std::thread::scope(|sc| {
            for _ in 0..width {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some(id) = ids.get(i) else { break };
                    // batch-job: rebase-stale bounds itself by its own lock wait and gate
                    let out = match Command::new(&rebase_stale)
                        .arg(id)
                        .arg(repo)
                        .stdin(Stdio::null())
                        .output()
                    {
                        Ok(o) => RunOut {
                            rc: o.status.code().unwrap_or(127),
                            out: String::from_utf8_lossy(&o.stdout).to_string(),
                            err: String::from_utf8_lossy(&o.stderr).to_string(),
                        },
                        Err(e) => RunOut {
                            rc: 127,
                            out: String::new(),
                            err: format!("cannot run rebase-stale: {e}"),
                        },
                    };
                    *slots[i].lock().unwrap_or_else(|p| p.into_inner()) = Some(out);
                });
            }
        });
        ids.iter()
            .cloned()
            .zip(slots.into_iter().map(|s| {
                s.into_inner()
                    .unwrap_or_else(|p| p.into_inner())
                    .unwrap_or_default()
            }))
            .collect()
    }
    fn boundary_sync(&self, tree: &Path) -> RunOut {
        // batch-job: a git commit in the wiki repo, bounded by git itself
        match Command::new("bash")
            .arg(tree.join("spira").join("boundary.sh"))
            .arg("sync")
            .stdin(Stdio::null())
            .output()
        {
            Ok(o) => RunOut {
                rc: o.status.code().unwrap_or(127),
                out: String::from_utf8_lossy(&o.stdout).to_string(),
                err: String::from_utf8_lossy(&o.stderr).to_string(),
            },
            Err(e) => RunOut {
                rc: 127,
                out: String::new(),
                err: format!("cannot run boundary.sh: {e}"),
            },
        }
    }
    fn rebase_waiting_start(&self, repo: &str) -> bool {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        // batch-job: the rebase pass runs as long as its gates take, detached from the landing
        Command::new(exe)
            .args(["rebase-waiting", repo])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .is_ok()
    }
    fn pass_restart(&self, batch: &str, repo: &str) -> bool {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        // batch-job: the next pass runs as long as its round-vm wall allows, detached from this verb
        Command::new(exe)
            .args(["round", "certify", batch, repo])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .is_ok()
    }
}

fn run_round_vm(
    tree: &Path,
    results: &Path,
    judged_by: [&str; 2],
    round: (&str, &str),
    wall_secs: u64,
    handle: &Path,
    ami: Option<&str>,
) -> RunOut {
    // batch-job: round-vm enforces the cap itself, on the suites and bare lane alone; this is its hang guard
    let mut cmd = Command::new("timeout");
    if let Some(ami) = ami {
        cmd.env("SPIRA_ROUND_VM_EC2_AMI", ami);
    }
    let spawned = cmd
        .args([
            "-k",
            "5",
            &wall_secs
                .saturating_mul(ROUND_HANG_GUARD_FACTOR)
                .to_string(),
            "round-vm",
            "run",
        ])
        .arg(tree)
        .arg("--results-dir")
        .arg(results)
        .args(judged_by)
        .args(["--round-batch", round.0, "--round-repo", round.1])
        .args(on_red_hook(round.0, round.1))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let child = match spawned {
        Ok(c) => c,
        Err(e) => {
            return RunOut {
                rc: 127,
                out: String::new(),
                err: format!("cannot run round-vm: {e}"),
            }
        }
    };
    let _ = fs::write(handle, child.id().to_string());
    match child.wait_with_output() {
        Ok(o) => RunOut {
            rc: o.status.code().unwrap_or(127),
            out: String::from_utf8_lossy(&o.stdout).to_string(),
            err: String::from_utf8_lossy(&o.stderr).to_string(),
        },
        Err(e) => RunOut {
            rc: 127,
            out: String::new(),
            err: format!("cannot run round-vm: {e}"),
        },
    }
}

/// round-vm's `--on-red` argument: this binary's `round failed`, the suite name appended by round-vm.
/// round-vm splits it on whitespace, so a path that holds any gets no hook rather than a wrong one.
fn on_red_hook(batch: &str, repo: &str) -> Vec<String> {
    let Ok(exe) = std::env::current_exe() else {
        return vec![];
    };
    match on_red_cmd(&exe.to_string_lossy(), batch, repo) {
        Some(cmd) => vec!["--on-red".into(), cmd],
        None => {
            eprintln!("queue: no --on-red hook for {batch}: {} cannot be split on whitespace; reds are raised when the pass ends", exe.display());
            vec![]
        }
    }
}

/// A binary replaced while this process runs reads back as `<path> (deleted)`; the path is still the new binary.
fn on_red_cmd(exe: &str, batch: &str, repo: &str) -> Option<String> {
    let exe = exe.strip_suffix(" (deleted)").unwrap_or(exe);
    let cmd = format!("{exe} round failed {batch} {repo} --suite");
    (cmd.split_whitespace().count() == 6).then_some(cmd)
}

// ---------------------------------------------------------------------------------- forge

/// How many `rebase-stale` runs go at once; the engine's own per-repo lock serialises the
/// rebase itself, so this bounds only the waiting.
const REBASE_WIDTH: usize = 4;

pub struct RealForge;

impl Forge for RealForge {
    fn pr_preflight(&self, forge: &Path, repo: &Path, head: &str) -> Result<(), String> {
        let o = spira_config::bounded::bounded(forge)
            .arg("pr-preflight")
            .arg(repo)
            .arg(head)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run the forge: {e}"))?;
        if o.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&o.stderr);
        let err = err.trim();
        Err(if err.is_empty() {
            format!("rc={}: (no stderr)", o.status.code().unwrap_or(-1))
        } else {
            err.to_string()
        })
    }
    fn pr_create(
        &self,
        forge: &Path,
        repo: &Path,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, String> {
        let mut child = spira_config::bounded::bounded(forge)
            .arg("pr-create")
            .arg(repo)
            .arg(head)
            .arg(base)
            .arg(title)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run the forge: {e}"))?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(body.as_bytes());
        }
        let o = child
            .wait_with_output()
            .map_err(|e| format!("forge wait failed: {e}"))?;
        if !o.status.success() {
            let err = String::from_utf8_lossy(&o.stderr);
            let err = err.trim();
            return Err(format!(
                "rc={}: {}",
                o.status.code().unwrap_or(-1),
                if err.is_empty() { "(no stderr)" } else { err }
            ));
        }
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    }
    fn pr_automerge(&self, forge: &Path, repo: &Path, pr: &str) -> bool {
        ok(Command::new(forge)
            .arg("pr-automerge")
            .arg(repo)
            .arg(pr)
            .arg("merge")
            .stderr(Stdio::null()))
    }
    fn pr_close(&self, forge: &Path, repo: &Path, pr: &str) {
        let _ = ok(Command::new(forge)
            .arg("pr-close")
            .arg(repo)
            .arg(pr)
            .stderr(Stdio::null()));
    }
    fn queue_prs(&self, forge: &Path, repo: &Path) -> Option<Vec<(String, String)>> {
        let out = stdout_of(Command::new(forge).arg("pr-list-open").arg(repo))?;
        Some(
            out.lines()
                .filter_map(|l| l.split_once(' '))
                .filter(|(_, b)| b.starts_with("spira/queue/"))
                .map(|(n, b)| (n.to_string(), b.to_string()))
                .collect(),
        )
    }
    fn publish_prs(&self, forge: &Path, repo: &Path) -> Option<Vec<(String, String)>> {
        let out = stdout_of(Command::new(forge).arg("pr-list-open").arg(repo))?;
        Some(
            out.lines()
                .filter_map(|l| l.split_once(' '))
                .filter(|(_, b)| b.starts_with("spira/publish/"))
                .map(|(n, b)| (n.to_string(), b.to_string()))
                .collect(),
        )
    }
    fn pr_comment(&self, forge: &Path, repo: &Path, pr: &str, line: &str) {
        let _ = ok(Command::new(forge)
            .arg("pr-comment")
            .arg(repo)
            .arg(pr)
            .arg(line)
            .stderr(Stdio::null()));
    }
    fn branch_protect(&self, forge: &Path, repo: &Path, branch: &str) -> bool {
        ok(Command::new(forge)
            .arg("branch-protect")
            .arg(repo)
            .arg(branch))
    }
    fn pr_state(&self, forge: &Path, repo: &Path, pr: &str) -> Option<String> {
        stdout_of(Command::new(forge).arg("pr-state").arg(repo).arg(pr))
            .map(|s| s.trim().to_string())
    }
    fn pr_merge_commit(&self, forge: &Path, repo: &Path, pr: &str) -> Option<String> {
        stdout_of(Command::new(forge).arg("pr-merge-commit").arg(repo).arg(pr))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    fn check_status(&self, forge: &Path, repo: &Path, pr: &str, branch: &str) -> Option<String> {
        stdout_of(
            Command::new(forge)
                .arg("check-status")
                .arg(repo)
                .arg(pr)
                .arg(branch),
        )
    }
    fn run_id(&self, forge: &Path, repo: &Path, branch: &str) -> Option<String> {
        stdout_of(Command::new(forge).arg("run-id").arg(repo).arg(branch))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    fn run_metadata(&self, forge: &Path, repo: &Path, run: &str) -> String {
        stdout_of(Command::new(forge).arg("run-metadata").arg(repo).arg(run)).unwrap_or_default()
    }
    fn fail_lines(&self, forge: &Path, repo: &Path, run: &str, suites: &str) -> String {
        stdout_of(
            Command::new(forge)
                .arg("fail-lines")
                .arg(repo)
                .arg(run)
                .arg(suites),
        )
        .unwrap_or_default()
    }
    fn run_cancel(&self, forge: &Path, repo: &Path, run: &str) {
        let _ = ok(Command::new(forge)
            .arg("run-cancel")
            .arg(repo)
            .arg(run)
            .stderr(Stdio::null()));
    }
    fn workflow_rerun(&self, forge: &Path, repo: &Path, run: &str) -> Rerun {
        // batch-job: the forge's own gh calls carry the deadline, as for every forge verb here
        let mut cmd = Command::new(forge);
        cmd.arg("workflow-rerun").arg(repo).arg(run);
        close_inherited_fds(&mut cmd);
        let Ok(o) = cmd.stdin(Stdio::null()).output() else {
            return Rerun::Failed;
        };
        let err = String::from_utf8_lossy(&o.stderr);
        eprint!("{err}");
        match o.status.code() {
            Some(0) => Rerun::Started,
            Some(3) => Rerun::Decided(
                err.lines()
                    .find_map(|l| l.strip_prefix("forge: workflow-rerun refused: "))
                    .unwrap_or("the forge refused the re-run")
                    .to_string(),
            ),
            _ => Rerun::Failed,
        }
    }
    fn ami_runs(&self, forge: &Path, repo: &Path) -> Option<Vec<String>> {
        let text = stdout_of(Command::new(forge).arg("ami-runs").arg(repo))?;
        if text.lines().any(|l| l.trim() == "?") {
            return None;
        }
        Some(
            text.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
        )
    }
    fn ami_dispatch(&self, forge: &Path, repo: &Path, ref_: &str) -> bool {
        ok(Command::new(forge).arg("ami-dispatch").arg(repo).arg(ref_))
    }
    fn ami_result(&self, forge: &Path, repo: &Path, branch: &str) -> Option<AmiBuild> {
        AmiBuild::parse(&stdout_of(
            Command::new(forge).arg("ami-result").arg(repo).arg(branch),
        )?)
    }
}

// ------------------------------------------------------------------------------- spira-lc

pub struct RealLc {
    pub bin: Option<PathBuf>,
}

impl RealLc {
    fn run(&self, args: &[&str]) -> (i32, String) {
        let Some(bin) = &self.bin else {
            return (2, "no spira-lc program".into());
        };
        let timeout = match crate::conf::nonempty("SPIRA_LC_TIMEOUT") {
            Ok(t) => t,
            Err(e) => return (2, e),
        };
        run_combined(Command::new("timeout").arg(timeout).arg(bin).args(args))
    }
    fn stdout(&self, args: &[&str]) -> Result<String, String> {
        let bin = self.bin.as_ref().ok_or("no spira-lc program")?;
        let timeout = crate::conf::nonempty("SPIRA_LC_TIMEOUT")?;
        let mut last = String::new();
        for attempt in 0..2 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            let o = Command::new("timeout")
                .arg(&timeout)
                .arg(bin)
                .args(args)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
            if o.status.success() {
                return Ok(String::from_utf8_lossy(&o.stdout).to_string());
            }
            let err = String::from_utf8_lossy(&o.stderr);
            last = format!(
                "spira-lc {} exited {:?}: {}",
                args.first().unwrap_or(&""),
                o.status.code(),
                err.trim()
            );
        }
        Err(last)
    }
}

impl RealLc {
    /// `spira-lc rc <verb> --json -`, the body on stdin, decoded as the door's wire answers.
    fn rc<T: serde::de::DeserializeOwned>(
        &self,
        verb: state_store::wire::Verb,
        body: &impl serde::Serialize,
    ) -> Result<T, String> {
        let what = format!("spira-lc rc {}", verb.word());
        let bin = self.bin.as_ref().ok_or("no spira-lc program")?;
        let timeout = crate::conf::nonempty("SPIRA_LC_TIMEOUT")?;
        let body = serde_json::to_vec(body).map_err(|e| format!("{what}: {e}"))?;
        let mut child = Command::new("timeout")
            .arg(&timeout)
            .arg(bin)
            .args(["rc", verb.word(), "--json", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{what}: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&body);
        }
        let o = child.wait_with_output().map_err(|e| format!("{what}: {e}"))?;
        let Some(code) = o.status.code() else {
            return Err(format!("{what} was killed: {}", String::from_utf8_lossy(&o.stderr).trim()));
        };
        state_store::wire::decode(code, &String::from_utf8_lossy(&o.stdout))
            .map_err(|e| format!("{what}: {e}"))
    }
}

/// Whether spira-lc serves the batch machine from the control store: the store is configured and
/// the lifecycle verbs are switched onto it. Read once per process, as spira-lc reads it.
/// Parsed as spira-lc parses it, so the two never disagree on one value. Err: the config does
/// not resolve, which no reader may take for "the files are the record".
fn rounds_on_store() -> &'static Result<bool, String> {
    static ON: std::sync::OnceLock<Result<bool, String>> = std::sync::OnceLock::new();
    ON.get_or_init(|| {
        let path = crate::conf::value("SPIRA_LC_STORE_PATH")?;
        if path.trim().is_empty() {
            return Ok(false);
        }
        let on = crate::conf::value("SPIRA_LC_LIFECYCLE_ON_STORE")?;
        on.trim()
            .parse::<u8>()
            .map(|v| v != 0)
            .map_err(|e| format!("SPIRA_LC_LIFECYCLE_ON_STORE = {on:?} does not parse: {e}"))
    })
}

/// A request id no other call mints, so a retried write is its own request.
fn rc_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("queue/{}/{nanos}/{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

impl crate::batches::Door for RealLc {
    fn get(&self, key: &str) -> Result<Option<state_store::Item>, String> {
        self.rc(state_store::wire::Verb::Get, &state_store::wire::Get { key: key.into() })
    }
    fn list(&self, prefix: &str) -> Result<Vec<state_store::Item>, String> {
        let body = state_store::wire::List {
            prefixes: vec![prefix.into()],
            selector: state_store::Selector::Everything,
        };
        self.rc(state_store::wire::Verb::List, &body)
            .map(|l: state_store::wire::Listed| l.items)
    }
    fn txn(&self, t: &state_store::Txn, kind: &str) -> Result<state_store::TxnResult, String> {
        let body = state_store::wire::TxnCall {
            txn: t.clone(),
            request: state_store::Request::new(&rc_request_id(), crate::ops::round::LC_ACTOR, kind),
        };
        self.rc(state_store::wire::Verb::Txn, &body)
    }
}

/// `--round`'s value: the queue's fields of the round as one JSON object.
fn round_flag(round: Option<&Kv>) -> Option<String> {
    round.map(|kv| serde_json::Value::Object(crate::batches::fields(kv)).to_string())
}

fn json_str(v: &serde_json::Value, k: &str) -> Option<String> {
    match v.get(k)? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        o => Some(o.to_string()),
    }
}

impl Lc for RealLc {
    fn available(&self) -> bool {
        // A program by name: whether it answers is the probe's question, not a file test.
        self.bin.is_some()
    }
    fn rounds_door(&self) -> Option<&dyn crate::batches::Door> {
        static UNRESOLVED: std::sync::OnceLock<crate::batches::Unresolved> = std::sync::OnceLock::new();
        match rounds_on_store() {
            Ok(true) => Some(self),
            Ok(false) => None,
            Err(e) => Some(UNRESOLVED.get_or_init(|| {
                crate::batches::Unresolved(format!("cannot tell where the rounds are: {e}"))
            })),
        }
    }
    fn batch_state(&self, batch_id: &str) -> Option<(String, String)> {
        let out = self.stdout(&["show-batch", batch_id]).ok()?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        Some((json_str(&v, "state")?, json_str(&v, "version")?))
    }
    fn batch_pass(&self, batch_id: &str) -> Option<(u32, String)> {
        let out = self.stdout(&["show-batch", batch_id]).ok()?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        Some((
            v.get("pass")?.as_u64()? as u32,
            json_str(&v, "phase").unwrap_or_default(),
        ))
    }
    fn create_bead(&self, id: &str) {
        let _ = self.run(&["create-bead", id, "--ready"]);
    }
    fn create_bead_in(&self, id: &str, repo: &str) {
        let _ = self.run(&["create-bead", id, "--repo", repo, "--ready"]);
    }
    fn cut(
        &self,
        batch_id: &str,
        repo: &str,
        head: &str,
        base: &str,
        members: &str,
        actor: &str,
        round: Option<&Kv>,
    ) -> Result<String, (i32, String)> {
        let fields = round_flag(round);
        let mut argv = vec![
            "cut",
            batch_id,
            "--repo",
            repo,
            "--head",
            head,
            "--base",
            base,
            "--members",
            members,
            "--actor",
            actor,
        ];
        argv.extend(fields.iter().flat_map(|f| ["--round", f.as_str()]));
        let (rc, out) = self.run(&argv);
        if rc != 0 {
            return Err((rc, out.trim().to_string()));
        }
        Ok(self
            .batch_state(batch_id)
            .map(|(_, version)| version)
            .unwrap_or_default())
    }
    fn stage(
        &self,
        batch_id: &str,
        repo: &str,
        head: &str,
        base: &str,
        members: &str,
        parent: &str,
        actor: &str,
    ) -> Result<String, (i32, String)> {
        let (rc, out) = self.run(&[
            "stage",
            batch_id,
            "--repo",
            repo,
            "--head",
            head,
            "--base",
            base,
            "--members",
            members,
            "--parent",
            parent,
            "--actor",
            actor,
        ]);
        if rc != 0 {
            return Err((rc, out.trim().to_string()));
        }
        Ok(self
            .batch_state(batch_id)
            .map(|(_, version)| version)
            .unwrap_or_default())
    }
    fn promote(
        &self,
        batch_id: &str,
        head: &str,
        base: &str,
        actor: &str,
        round: Option<&Kv>,
    ) -> Result<(), (i32, String)> {
        let fields = round_flag(round);
        let mut argv = vec![
            "promote", batch_id, "--head", head, "--base", base, "--actor", actor,
        ];
        argv.extend(fields.iter().flat_map(|f| ["--round", f.as_str()]));
        let (rc, out) = self.run(&argv);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn abandon_batch(
        &self,
        batch_id: &str,
        state: &str,
        version: &str,
        actor: &str,
        reason: &str,
    ) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&[
            "abandon-batch",
            batch_id,
            "--expect",
            state,
            "--version",
            version,
            "--actor",
            actor,
            "--reason",
            reason,
        ]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn eject_member(
        &self,
        batch_id: &str,
        bead: &str,
        state: &str,
        version: &str,
        actor: &str,
        reason: &str,
    ) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&[
            "eject-member",
            batch_id,
            "--bead-id",
            bead,
            "--expect",
            state,
            "--version",
            version,
            "--actor",
            actor,
            "--reason",
            reason,
        ]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn batch_event(
        &self,
        batch_id: &str,
        state: &str,
        version: &str,
        actor: &str,
        kind: &str,
        round: Option<&Kv>,
    ) -> Result<(), (i32, String)> {
        let fields = round_flag(round);
        let mut argv = vec![
            "event",
            "batch",
            batch_id,
            "--expect",
            state,
            "--version",
            version,
            "--actor",
            actor,
            "--kind",
            kind,
        ];
        argv.extend(fields.iter().flat_map(|f| ["--round", f.as_str()]));
        let (rc, out) = self.run(&argv);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn requeue_orphans(&self, actor: &str) -> Result<String, (i32, String)> {
        let (rc, out) = self.run(&["requeue-orphans", "--actor", actor, "--apply"]);
        if rc == 0 {
            Ok(out)
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn bead_state(&self, bead: &str) -> Option<(String, String)> {
        let out = self.stdout(&["show", bead]).ok()?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        let row = v.get("bead")?;
        Some((json_str(row, "state")?, json_str(row, "version")?))
    }
    fn bead_event(
        &self,
        bead: &str,
        state: &str,
        version: &str,
        actor: &str,
        kind: &str,
    ) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&[
            "event",
            "bead",
            bead,
            "--expect",
            state,
            "--version",
            version,
            "--actor",
            actor,
            "--kind",
            kind,
        ]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn land_batch(
        &self,
        batch_id: &str,
        version: &str,
        actor: &str,
        sha: &str,
    ) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&[
            "land",
            batch_id,
            "--expect",
            "GREEN",
            "--version",
            version,
            "--actor",
            actor,
            "--sha",
            sha,
        ]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn probe(&self) -> Result<(), String> {
        let out = self.stdout(&["list", "--state", "IN_DELIVERY"])?;
        serde_json::from_str::<serde_json::Value>(&out)
            .map(|_| ())
            .map_err(|e| format!("spira-lc list: {e}"))
    }
    fn bead_rows(&self, state: Option<&str>) -> Result<Vec<LcBeadRow>, String> {
        let out = match state {
            Some(s) => self.stdout(&["list", "--state", s])?,
            None => self.stdout(&["list"])?,
        };
        serde_json::from_str(&out).map_err(|e| format!("spira-lc list: {e}"))
    }
    fn polled_rows(&self, state: &str) -> Result<Vec<LcBeadRow>, String> {
        let out = self.stdout(&["list", "--state", state, "--snapshot"])?;
        serde_json::from_str(&out).map_err(|e| format!("spira-lc list: {e}"))
    }
    fn express_ids(&self) -> Result<Vec<String>, String> {
        let out = self.stdout(&["list", "--express", "--live", "--snapshot"])?;
        let rows: Vec<LcBeadRow> =
            serde_json::from_str(&out).map_err(|e| format!("spira-lc list: {e}"))?;
        Ok(rows.into_iter().map(|r| r.bead_id).collect())
    }
    fn bead_row(&self, bead: &str) -> Option<LcBeadRow> {
        let out = self.stdout(&["show", bead]).ok()?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        serde_json::from_value(v.get("bead")?.clone()).ok()
    }
    fn mending_event(
        &self,
        batch_id: &str,
        pass: u32,
        suite: &str,
        kind: &str,
        actor: &str,
    ) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&[
            "mending",
            "event",
            batch_id,
            "--pass",
            &pass.to_string(),
            "--suite",
            suite,
            "--kind",
            kind,
            "--actor",
            actor,
        ]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn mending_close_ended(&self, actor: &str) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&["mending", "close-ended", "--actor", actor]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn mend(
        &self,
        batch_id: &str,
        pass: u32,
        suite: &str,
        members: &str,
        facts: &str,
        fences: &str,
        actor: &str,
    ) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&[
            "mending",
            "mend",
            batch_id,
            "--pass",
            &pass.to_string(),
            "--suite",
            suite,
            "--members",
            members,
            "--facts",
            facts,
            "--fences",
            fences,
            "--actor",
            actor,
        ]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn mend_tips(&self, beads: &[String]) -> Result<BTreeMap<String, String>, String> {
        let out = self.stdout(&["mending", "tips", "--beads", &beads.join(",")])?;
        let v: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&out).map_err(|e| format!("spira-lc mending tips: {e}"))?;
        Ok(v.into_iter()
            .filter_map(|(k, t)| t.as_str().map(|t| (k, t.to_string())))
            .collect())
    }
    fn churn_facts(&self, since: i64) -> Result<String, String> {
        self.stdout(&["churn-facts", "--since", &since.to_string()])
    }
    fn certify(
        &self,
        bead: &str,
        tip: &str,
        detail: &str,
        actor: &str,
    ) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&["certify", bead, tip, "pass", detail, actor]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn retip(
        &self,
        bead: &str,
        tip: &str,
        actor: &str,
        reason: &str,
        evidence: &str,
        evidence_tip: &str,
    ) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&["retip", bead, tip, actor, reason, evidence, evidence_tip]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn absolve(
        &self,
        bead: &str,
        tip: &str,
        actor: &str,
        reason: &str,
        evidence: &str,
        evidence_tip: &str,
    ) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&["absolve", bead, tip, actor, reason, evidence, evidence_tip]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn round_lease(&self, repo: &str, batch: &str) -> Result<Option<RoundLease>, String> {
        let bin = self.bin.as_ref().ok_or("no spira-lc program")?;
        let timeout = crate::conf::nonempty("SPIRA_LC_TIMEOUT")?;
        let resource = format!("round/{repo}/{batch}");
        let o = Command::new("timeout")
            .arg(&timeout)
            .arg(bin)
            .args(["lock", "show", &resource])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("spira-lc lock show {resource}: {e}"))?;
        let out = String::from_utf8_lossy(&o.stdout);
        let said = || {
            format!(
                "spira-lc lock show {resource} exited {:?}: {} {}",
                o.status.code(),
                out.trim(),
                String::from_utf8_lossy(&o.stderr).trim()
            )
        };
        match o.status.code() {
            Some(0) => serde_json::from_str(out.trim())
                .ok()
                .and_then(|v| round_lease_of(&v))
                .map(Some)
                .ok_or_else(said),
            Some(1) => Ok(None),
            _ => Err(said()),
        }
    }
}

/// `lock show`'s answer: its token is `<main>.<sub>`, a holder's `--lease-token` is `<main>-<sub>`.
pub fn round_lease_of(v: &serde_json::Value) -> Option<RoundLease> {
    let (main, sub) = v.get("token")?.as_str()?.split_once('.')?;
    Some(RoundLease {
        holder: v.get("holder").and_then(|h| h.as_str()).map(str::to_string),
        token: format!("{main}-{sub}"),
        ttl_left_secs: v.get("ttl_left_secs")?.as_u64()?,
    })
}

// ------------------------------------------------------------ config (spira-config library)

pub struct RealConfig;

impl ConfigStore for RealConfig {
    fn repo_row(&self, toml: &Path, name: &str) -> Result<(String, String), String> {
        let doc = spira_config::load(toml)?;
        Ok((
            spira_config::get_path(&doc, &format!("repo.{name}.mode")).unwrap_or_default(),
            spira_config::get_path(&doc, &format!("repo.{name}.base")).unwrap_or_default(),
        ))
    }
    fn set_repo_row(&self, toml: &Path, name: &str, mode: &str, base: &str) -> Result<(), String> {
        let (mk, bk) = (format!("repo.{name}.mode"), format!("repo.{name}.base"));
        spira_config::set_paths_in_file(toml, &[(&mk, mode), (&bk, base)])
    }
    fn set_legacy_map_row(
        &self,
        map: &Path,
        name: &str,
        land: &str,
        base: &str,
    ) -> Result<(), String> {
        spira_config::legacy_map::set_row_in_file(map, name, land, base)
    }
}

// ------------------------------------------------------------------ clock, env and stdio

pub struct SysClock;

impl Clock for SysClock {
    fn now(&self) -> u64 {
        spira_config::vtime::now_epoch()
    }
    fn stamp(&self) -> String {
        utc_stamp(self.now())
    }
    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

/// `%Y%m%dT%H%M%SZ` for an epoch, without a date crate (civil-from-days).
pub fn utc_stamp(t: u64) -> String {
    let days = (t / 86_400) as i64;
    let secs = t % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

pub struct SysEnv;

impl Env for SysEnv {
    fn var(&self, k: &str) -> Option<String> {
        std::env::var(k).ok()
    }
    fn pid(&self) -> u32 {
        std::process::id()
    }
    fn read_stdin(&self) -> Result<String, String> {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| e.to_string())?;
        Ok(s)
    }
    fn read_file(&self, p: &Path) -> Result<String, String> {
        fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
    }
    fn config(&self, k: &str) -> Result<String, String> {
        crate::conf::value(k)
    }
}

/// stdout/stderr, flushed per write so interleaving with child processes stays in order.
pub struct StdEmit {
    _lock: RefCell<()>,
}

impl StdEmit {
    pub fn new() -> StdEmit {
        StdEmit {
            _lock: RefCell::new(()),
        }
    }
}

impl Default for StdEmit {
    fn default() -> Self {
        Self::new()
    }
}

impl Emit for StdEmit {
    fn out(&self, s: &str) {
        let mut o = std::io::stdout();
        let _ = o.write_all(s.as_bytes());
        let _ = o.flush();
    }
    fn err(&self, s: &str) {
        let mut e = std::io::stderr();
        let _ = e.write_all(s.as_bytes());
        let _ = e.flush();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_config_names_every_key_context_reads() {
        let src = include_str!("real.rs");
        let body = &src[src.find("fn context(").unwrap()..src.find("let s = Settings {").unwrap()];
        let mut read = Vec::new();
        for part in body.split("process::cfg").skip(1) {
            let after = &part[part.find("(\"").unwrap() + 2..];
            read.push(&after[..after.find('"').unwrap()]);
        }
        assert!(read.len() >= 8, "the scan must find the reads it is checking: {read:?}");
        for k in &read {
            assert!(CONFIG_KEYS.contains(k), "{k} is read by context but not checked by check-config");
        }
    }


    #[test]
    fn a_resubmit_suite_check_passes_testenv_a_repeat_reason_flag() {
        let args = |first| -> Vec<String> {
            suite_run_command("testenv", "test-x.sh", "t", "r", first)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let first = args(true);
        let at = first.iter().position(|a| a == "--repeat-reason").unwrap();
        assert!(first[at + 1].chars().count() >= 10);
        assert!(!args(false).iter().any(|a| a == "--repeat-reason"));
    }

    #[test]
    fn the_on_red_hook_survives_a_replaced_binary_and_refuses_a_spaced_path() {
        assert_eq!(
            on_red_cmd("/r/queue (deleted)", "b1", "repo").as_deref(),
            Some("/r/queue round failed b1 repo --suite")
        );
        assert_eq!(
            on_red_cmd("/r/queue", "b1", "repo").as_deref(),
            Some("/r/queue round failed b1 repo --suite")
        );
        assert_eq!(on_red_cmd("/my dir/queue", "b1", "repo"), None);
    }
    use super::*;

    #[test]
    fn utc_stamp_matches_date() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_790_000_000), "20260921T141320Z");
        assert_eq!(utc_stamp(951_782_400), "20000229T000000Z");
    }

    #[test]
    fn bd_json_skips_a_preamble_and_accepts_one_object() {
        let rows = parse_bd_json(
            "warning: x\n[{\"id\":\"sp-a\",\"status\":\"closed\",\"labels\":[\"l\"]}]",
        )
        .unwrap();
        assert_eq!(rows[0].id, "sp-a");
        let one = parse_bd_json("{\"id\":\"sp-b\"}").unwrap();
        assert_eq!(one[0].id, "sp-b");
        assert!(parse_bd_json("nothing").is_err());
    }

    // -------------------------------------------------------------------------- sp-hwjsq
    // notify/divergence/push/sort_rows/cancel_runs run in process now ("wave 4.32"); these
    // prove RealLib's own implementation against real git, a fake `mail` and a fake forge
    // binary — not the TestLib fakes ops.rs tests use elsewhere.

    #[test]
    fn push_adds_app_credentials_only_when_configured() {
        let args = ["-q".to_string(), "origin".to_string(), "main".to_string()];
        let argv = |app| {
            crate::ops::helpers::push_command(Path::new("/repo"), &args, app)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let plain = argv(false);
        assert!(!plain.contains("credential.helper"), "{plain}");
        assert!(plain.contains("push"), "{plain}");
        let with_creds = argv(true);
        assert!(with_creds.contains("credential.helper"), "{with_creds}");
        assert!(
            with_creds.contains("insteadOf=git@github.com:"),
            "{with_creds}"
        );
        assert!(with_creds.contains("push"), "{with_creds}");
    }

    // ---------------------------------------------------------------------------- sp-uwhx0
    // format_batch/base_conflict/pf_gate used to source batch.sh (deleted); these run the
    // inlined bodies against a real git repo and real bash, not the FLib fakes ops.rs tests
    // use elsewhere — parity proof that the inlining kept batch.sh's own behaviour.
}

#[cfg(test)]
mod vtime_tests {
    use super::*;
    use crate::ports::Clock;

    #[test]
    fn sys_clock_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, || SysClock.now());
        assert_eq!(got, 1_900_000_000);
    }
}
