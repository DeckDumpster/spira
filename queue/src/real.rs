//! The ports against the host: git, bd, the lib.sh seam, the harness scripts, the forge,
//! spira-lc, the config (spira-config library only), the clock, the environment, stdio.

/// round-vm enforces the cap on the tests itself; the outer `timeout` only catches a hang in the
/// unbounded rest (pulls, post-processing).
const ROUND_HANG_GUARD_FACTOR: u64 = 2;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::process::{Command, Stdio};

use crate::model::{BeadRow, LandMode, LcBeadRow, RangeCommit};
use crate::ports::*;
use crate::records::Kv;
use crate::seam::{self, Op};
use spira_store::lc_call::LcClient;

/// Close every fd >= 3 in the child before it execs. This process's own fds are already
/// close-on-exec (Rust's std opens them that way), but a caller's lock fd this process
/// never opened itself — inherited from whatever bash wrapper invoked `queue` (an
/// `exec 9>…` lock with no O_CLOEXEC) — carries none of that protection. `gate.sh` and
/// `testenv` are reached from here (the verdict pass's gate call), and
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

fn git_checked(cmd: &mut Command) -> Result<(), String> {
    close_inherited_fds(cmd);
    let o = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if o.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&o.stderr);
    let lines: Vec<&str> = err.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(8)..].join("; ");
    Err(if tail.is_empty() { format!("git exited {}", o.status) } else { tail })
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

fn parse_image_check(out: &str) -> ImageCheck {
    match out.trim().split_once(' ') {
        None if out.trim() == "present" => ImageCheck::Present,
        Some(("absent", tag)) if !tag.trim().is_empty() => ImageCheck::Absent(tag.trim().to_string()),
        _ => ImageCheck::Unreadable(format!("unrecognised image-check answer {:?}", out.trim())),
    }
}

pub struct RealGit;

fn git(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo);
    c
}

/// The queue's ref writes are the sanctioned ones: they pass the reference-transaction hook's
/// landing-ref refusal from any environment.
fn update_ref_cmd(repo: &Path, refname: &str, new: &str, old: Option<&str>) -> Command {
    let mut c = git(repo);
    c.args(["update-ref", refname, new]);
    if let Some(o) = old {
        c.arg(o);
    }
    c.env("SPIRA_REF_SANCTIONED", "1");
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
    fn base_conflict_paths(&self, repo: &Path, base: &str, tip: &str) -> Option<Vec<String>> {
        let o = git(repo)
            .args([
                "-c",
                &format!("attr.tree={base}"),
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                base,
                tip,
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let so = String::from_utf8_lossy(&o.stdout);
        let tree = so.lines().next().unwrap_or("");
        if o.status.code() != Some(1) || tree.len() < 40 || !tree.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut paths: Vec<String> = Vec::new();
        for l in so.lines().skip(1).filter(|l| !l.is_empty()) {
            if !paths.iter().any(|p| p == l) {
                paths.push(l.to_string());
            }
        }
        Some(paths)
    }
    fn update_ref(&self, repo: &Path, refname: &str, new: &str, old: Option<&str>) -> bool {
        ok(update_ref_cmd(repo, refname, new, old).stderr(Stdio::null()))
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
        if let Some(tips) = spira_config::git_refs::branch_tips(repo, prefix) {
            return tips;
        }
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
    fn worktree_reset(&self, repo: &Path, wt: &Path, sha: &str) -> Result<(), String> {
        if wt.join(".git").exists() {
            git_checked(git(wt).args(["reset", "-q", "--hard", sha]))?;
            let _ = ok(git(wt).args(["clean", "-qfd"]).stderr(Stdio::null()));
            return Ok(());
        }
        let _ = ok(git(repo).args(["worktree", "prune"]).stderr(Stdio::null()));
        if let Some(p) = wt.parent() {
            let _ = fs::create_dir_all(p);
        }
        git_checked(
            git(repo)
                .args(["worktree", "add", "-q", "--detach"])
                .arg(wt)
                .arg(sha),
        )
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
    /// The store's door; bead content is its `content` verb.
    pub lc: Result<LcClient, String>,
}

impl Bd for RealBd {
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        let lc = self.lc.as_ref().map_err(Clone::clone)?;
        let mut out = Vec::new();
        for chunk in ids.chunks(100) {
            let argv: Vec<&str> = ["content", "show"]
                .into_iter()
                .chain(chunk.iter().map(String::as_str))
                .chain(["--json"])
                .collect();
            let r = lc.call(&argv).map_err(String::from)?;
            if r.code != 0 {
                return Err(format!("content show exited {}: {}", r.code, r.why()));
            }
            out.extend(parse_bd_json(&r.out)?);
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
    /// The store's door, for the beads queue files, notes and comments on.
    pub store: RealLc,
    /// Where a store write that did not apply says why.
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
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
        // FORK-EDGE: needs Rust homes for the lib.sh functions the remaining ops call (bead_reopen, release_claim, rebase_branch, land_subject, repo_format, pf_gate, spira_event)
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

    /// One bead-content write: its stdout, or why it did not apply.
    fn content(&self, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
        let c = self.store.client()?;
        let argv: Vec<&str> = std::iter::once("content").chain(args.iter().copied()).collect();
        let r = match stdin {
            Some(body) => c.call_stdin(&argv, body),
            None => c.call(&argv),
        }
        .map_err(String::from)?;
        if r.code != 0 {
            return Err(format!("exit {}: {}", r.code, r.why()));
        }
        Ok(r.out)
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

fn gate_command(branch: &str, repo: &str, bead: &str, lock_wait: &str) -> Command {
    // FORK-EDGE: machine leg gate.sh
    let mut c = Command::new("gate.sh");
    c.arg(branch)
        .arg(repo)
        .env("SPIRA_GATE_BEAD", bead)
        .env("SPIRA_GATE_LOCK_WAIT", lock_wait);
    c
}

/// A key the toml in force gains only at activation, after pre-activate: absent is `None`, so it
/// stays out of `CONFIG_KEYS`, whose check would refuse the release that introduces it.
fn unregistered_is_none<T>(r: Result<T, String>) -> Result<Option<T>, String> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.contains("not a registered config key") => Ok(None),
        Err(e) => Err(e),
    }
}

/// Every key `context` must resolve; `queue check-config` refuses a registry that lacks one.
pub const CONFIG_KEYS: &[&str] = &[
    "SPIRA_GIT_NAME",
    "SPIRA_GIT_EMAIL",
    "SPIRA_MAIL_SESSION_MAILBOX",
    "SPIRA_ROUND_CERTIFY_WALL_SECS",
    "SPIRA_ROUND_AMI_WAIT_SECS",
    "SPIRA_QUEUE_ROUNDS_MAX",
    "SPIRA_QUEUE_TEXT_ONLY",
    "SPIRA_PUBLISH_RED_PRIORITY",
    "SPIRA_PUBLISH_RED_ESCALATE_AFTER",
    "SPIRA_PUBLISH_SETTLE_DEFER_MAX",
    "SPIRA_DWARF_BUILD_BASE",
];

/// The settings lib.sh's CONTEXT seam answered, from the config's declared values (`value`):
/// each by its key, the CI wall and idle limits by the repo's own key when it declares one.
pub fn context_kv(
    home: &Path,
    name: &str,
    value: &dyn Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, String> {
    const KEYS: [(&str, &str); 21] = [
        ("run", "SPIRA_RUN"),
        ("queue_dir", "SPIRA_QUEUE_DIR"),
        ("releases", "SPIRA_RELEASES"),
        ("forge", "SPIRA_FORGE"),
        ("repo_map", "SPIRA_REPO_MAP"),
        ("batcher_enable", "SPIRA_BATCHER_ENABLE"),
        ("submitted_label", "SPIRA_SUBMITTED_LABEL"),
        ("pollsec", "SPIRA_QUEUE_TRANSITION_POLLSEC"),
        ("maxsec", "SPIRA_QUEUE_TRANSITION_MAXSEC"),
        ("preflight", "SPIRA_PREFLIGHT_WALL_SECS"),
        ("db", "SPIRA_DB"),
        ("ci_maxsec", "SPIRA_QUEUE_CI_MAXSEC"),
        ("ci_idle", "SPIRA_QUEUE_CI_IDLE_SEC"),
        ("infra_retries", "SPIRA_QUEUE_INFRA_RETRIES"),
        ("lock_wait", "SPIRA_QUEUE_LOCK_WAIT"),
        ("starve_max", "SPIRA_QUEUE_LOCK_STARVE_MAX"),
        ("capacity_backoff_min", "SPIRA_QUEUE_CAPACITY_BACKOFF_MIN"),
        ("capacity_backoff_max", "SPIRA_QUEUE_CAPACITY_BACKOFF_MAX"),
        ("incident_priority", "SPIRA_INCIDENT_PRIORITY"),
        ("red_tracker_label", "SPIRA_RED_TRACKER_LABEL"),
        ("mender_label", "SPIRA_MENDER_LABEL"),
    ];
    let upper: String = name
        .chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect();
    let suffix = if upper
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        upper
    } else {
        String::new()
    };
    let mut kv = BTreeMap::new();
    kv.insert("home".to_string(), home.display().to_string());
    for (field, key) in KEYS {
        let own = matches!(field, "ci_maxsec" | "ci_idle")
            .then(|| value(&format!("{key}_{suffix}")))
            .flatten()
            .filter(|v| !v.is_empty());
        let v = match own {
            Some(v) => v,
            None => value(key).ok_or_else(|| {
                format!("{key} is not a registered config key (spira/conf.d) — refusing")
            })?,
        };
        kv.insert(field.to_string(), v);
    }
    Ok(kv)
}

/// `spira_toml_resolve`: the spec, when every layer it names exists.
pub(crate) fn toml_path_of(spec: Option<&str>) -> Option<PathBuf> {
    let spec = spec.filter(|s| !s.is_empty())?;
    spec.split(':')
        .all(|f| Path::new(f).is_file())
        .then(|| PathBuf::from(spec))
}

impl Lib for RealLib {
    fn context(&self, repo: Option<&str>) -> Result<(Settings, RepoCtx), String> {
        // Every field resolves in-process: the repo registry (families U and W, sp-k6lku) and
        // the settings lib.sh's CONTEXT seam once printed, read from the one config door.
        let reg = self.repo_registry();
        let name = repo
            .map(str::to_string)
            .unwrap_or_else(|| reg.home_repo().to_string());
        let resolved = spira_config::process::config()?;
        let kv = context_kv(&self.home, &name, &|k| resolved.values.get(k).cloned())?;
        let g = |k: &str| kv.get(k).cloned().unwrap_or_default();
        let n = |k: &str, d: u64| kv.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        if g("run").is_empty() {
            return Err("SPIRA_RUN is unset after sourcing lib.sh".into());
        }
        // law-one-source-of-config: these five are not part of the lib.sh seam's kv
        // answer above (seam R1 predates spira-config) — read straight from $SPIRA_TOML
        // through the one door, never from the process environment.
        let git_name = spira_config::process::cfg("SPIRA_GIT_NAME")?;
        let git_email = spira_config::process::cfg("SPIRA_GIT_EMAIL")?;
        let mailbox = spira_config::process::cfg("SPIRA_MAIL_SESSION_MAILBOX")?;
        let round_wall_secs =
            spira_config::process::cfg_parse::<u64>("SPIRA_ROUND_CERTIFY_WALL_SECS")?;
        let results_retention = spira_config::results_retention::Retention::from_config()?;
        let round_ami_wait_secs =
            spira_config::process::cfg_parse::<u64>("SPIRA_ROUND_AMI_WAIT_SECS")?;
        let rounds_max = spira_config::process::cfg_parse::<usize>("SPIRA_QUEUE_ROUNDS_MAX")?;
        let text_only =
            crate::text_only::parse_allow(&spira_config::process::cfg("SPIRA_QUEUE_TEXT_ONLY")?);
        let publish_red_priority = spira_config::process::cfg("SPIRA_PUBLISH_RED_PRIORITY")?;
        let publish_red_escalate_after =
            spira_config::process::cfg_parse::<usize>("SPIRA_PUBLISH_RED_ESCALATE_AFTER")?;
        let publish_defer_max =
            match spira_config::process::cfg_parse::<usize>("SPIRA_PUBLISH_SETTLE_DEFER_MAX") {
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
            lc_bin: Some(PathBuf::from(spira_store::lifecycle_row::lc_bin()?)),
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
            git_name,
            git_email,
            mailbox,
            round_wall_secs,
            results_retention,
            round_ami_wait_secs,
            round_spill: spira_config::process::cfg("SPIRA_ROUND_VM_SPILL").unwrap_or_default(),
            rounds_max,
            text_only,
            publish_red_priority,
            publish_red_escalate_after,
            publish_defer_max,
            publish_no_run_secs: unregistered_is_none(
                spira_config::process::cfg_parse::<u64>("SPIRA_PUBLISH_NO_RUN_SECS"),
            )?,
            gate_timeout: unregistered_is_none(spira_config::process::cfg_parse::<u64>(
                "SPIRA_GATE_TIMEOUT",
            ))?,
            gate_lock_wait: unregistered_is_none(spira_config::process::cfg("SPIRA_GATE_LOCK_WAIT"))?,
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
        toml_path_of(spira_config::process::spec().ok().as_deref())
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
        spira_store::lifecycle_row::close_landed(id, reason, "queue").is_ok()
    }
    fn bead_retire(&self, id: &str, reason: &str) -> Result<(), String> {
        spira_store::lifecycle_row::close(id, reason, "queue", None)
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
        // FORK-EDGE: needs a hel library entry for reap-landed-branch (hel's verified branch+worktree deletion)
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
        // FORK-EDGE: needs a gh_intake library entry for closeout (only its binary has it)
        let _ = spira_config::bounded::bounded("gh-intake")
            .args(["closeout", id, sha, &repo.display().to_string()])
            .status();
    }
    fn comment(&self, id: &str, text: &str) {
        if let Err(why) = self.content(&["comments", "add", id, text], None) {
            (self.warn)(&format!("queue: comment on {id} not recorded: {why}"));
        }
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
        prio_json: &str,
        rows: &str,
    ) -> Vec<(String, String)> {
        let rows = crate::ops::helpers::parse_rows(rows);
        let (ranked, warning) = crate::ops::helpers::sort_rows(&rows, prio_json, express);
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
        // The store's writer is spira-lc whoever calls, so the actor is recorded in the text.
        let body = format!("Filed by {actor}.\n\n{body}");
        let filed = self.content(
            &["create", title, "--type", "bug", "--priority", priority, "--labels", labels, "--body-file", "-", "--silent"],
            Some(&body),
        );
        match filed.map(|out| out.trim().to_string()) {
            Ok(id) if !id.is_empty() => Some(id),
            Ok(_) => {
                (self.warn)(&format!("queue: bug {title:?} not filed: the store named no id"));
                None
            }
            Err(why) => {
                (self.warn)(&format!("queue: bug {title:?} not filed: {why}"));
                None
            }
        }
    }
    fn amend_bug(&self, actor: &str, id: &str, note: &str) -> bool {
        if !self.bead_live(id) {
            return false;
        }
        let note = format!("Noted by {actor}.\n\n{note}");
        self.content(&["note", id, "--stdin"], Some(&note))
            .map_err(|why| (self.warn)(&format!("queue: note on {id} not recorded: {why}")))
            .is_ok()
    }
    fn bead_live(&self, id: &str) -> bool {
        let shown = self.store.client().and_then(|c| spira_store::lc_state::row_on(&c, id));
        matches!(shown, Ok(Some(row)) if !row.state.is_empty() && !row.terminal())
    }
}

// -------------------------------------------------------------------------------- scripts

pub const RESUBMIT_CHECK_REASON: &str = "resubmit check of a new tip, not a retry";

/// The first resubmit check of a tip is not a retry of the red that ejected the bead; a second
/// check of the same tip is, and keeps testenv's repeat guard.
fn suite_run_command(
    testenv: &str,
    check: &str,
    tip: &str,
    repo: &str,
    first_check: bool,
    tests: &[String],
    deny_list: &Path,
) -> Command {
    // FORK-EDGE: machine leg testenv (suite run)
    let mut c = Command::new(testenv);
    c.env("SPIRA_TEST_DENY_LIST", deny_list);
    if first_check {
        c.args(["--repeat-reason", RESUBMIT_CHECK_REASON]);
    }
    c.args(["--suites", check]);
    if !tests.is_empty() {
        c.args(["--tests", &tests.join(",")]);
    }
    c.args([tip, repo]);
    c
}

/// czar-fence.sh, in-process: a czar mutation of `class` passes only when
/// `SPIRA_CZAR_STAGE_<CLASS>` (from `env`, the variable the script read) is `act`; unset is
/// shadow, so a new class is contained until the operator enables it (law-guard-binds-the-caller).
pub fn czar_stage(class: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    if class.is_empty() {
        return Err("czar-fence: class required".into());
    }
    let var = format!(
        "SPIRA_CZAR_STAGE_{}",
        class.to_uppercase().replace('-', "_")
    );
    let stage = env(&var)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "shadow".into());
    match stage.as_str() {
        "act" => Ok(()),
        "shadow" => Err(format!(
            "czar-fence: {class} is shadow — mutation refused ({var}=act to enable)"
        )),
        other => Err(format!(
            "czar-fence: unknown stage {other} in {var} (shadow or act)"
        )),
    }
}

/// `systemctl --user show -p ActiveState --value`'s word for a unit's facts; empty when they
/// could not be read, as the command printed nothing then.
pub fn active_state_of(facts: Option<runtime::units::UnitFacts>) -> String {
    use runtime::units::UnitState::*;
    match facts.map(|f| f.state) {
        None => "",
        Some(Active) => "active",
        Some(Activating) => "activating",
        Some(Deactivating) => "deactivating",
        Some(Inactive | Absent) => "inactive",
        Some(Failed) => "failed",
    }
    .to_string()
}

struct HeldBuild(Option<spira_config::build_dir::Lease>);

impl Drop for HeldBuild {
    fn drop(&mut self) {
        if let Some(l) = self.0.take() {
            l.release();
        }
    }
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
        // FORK-EDGE: machine leg testenv (lint)
        let mut c = Command::new(testenv);
        c.arg("lint");
        if verify {
            c.arg("--verify");
        }
        run_combined(c.arg(tip).arg(repo))
    }
    fn suite_run(&self, check: &str, tip: &str, repo: &str, first_check: bool, tests: &[String], deny_list: &Path) -> (i32, String) {
        match spira_config::release_env::tool("testenv") {
            Ok(testenv) => run_combined(&mut suite_run_command(
                &testenv,
                check,
                tip,
                repo,
                first_check,
                tests,
                deny_list,
            )),
            Err(e) => (2, e),
        }
    }
    fn gate(&self, branch: &str, repo: &str, bead: &str, lock_wait: &str) -> (i32, String) {
        run_combined(&mut gate_command(branch, repo, bead, lock_wait))
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
            // FORK-EDGE: machine leg queue-judgement-ci
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
    fn mail_operator(&self, subject: &str, body: &str) {
        // FORK-EDGE: needs a mail library entry that is `mail send` (kind routing, the attempt record); the suites stub `mail` on PATH
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
        // FORK-EDGE: machine leg batcher-cut
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
        // FORK-EDGE: machine leg path-attribution
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
        match czar_stage(class, &|k| std::env::var(k).ok()) {
            Ok(()) => true,
            Err(why) => {
                eprintln!("{why}");
                false
            }
        }
    }
    fn release_tool(&self) -> Result<PathBuf, String> {
        spira_config::release_env::tool("release").map(PathBuf::from)
    }
    fn build_release(&self, wt: &Path, who: &str, repo: &str) -> Result<Box<dyn std::any::Any>, String> {
        use spira_config::build_dir as bd;
        let base = spira_config::process::cfg(bd::BASE_ENV)?;
        let bases = bd::parse_bases(&base);
        if bases.is_empty() {
            return Err(format!("{} is not set — set dwarf_build_base in the config document", bd::BASE_ENV));
        }
        let lease = bd::lease_for(&bases, who, Some(repo)).map_err(|e| format!("cannot lease a build directory under {base}: {e}"))?;
        let held = HeldBuild(Some(lease));
        let dir = held.0.as_ref().map(|l| l.dir().to_path_buf()).unwrap_or_default();
        let script = spira_config::resolve::locate_home_for_process()?.join(bd::CLEAN_SCRIPT);
        let path = std::env::var("PATH").unwrap_or_default();
        if let Err(e) = bd::clean(&script, wt, &dir, &path, false) {
            bd::empty(&dir);
            return Err(format!("selective clean of {} failed ({e}) — emptied, nothing was built", dir.display()));
        }
        // FORK-EDGE: machine leg cargo (release build)
        let (rc, out) = run_combined(
            Command::new("cargo")
                .args(["build", "-q", "--profile", "release", "--workspace"])
                .args(spira_config::build::one_shot("release"))
                .envs(spira_config::build::cargo_home_env_from_process())
                .current_dir(wt)
                .env("CARGO_TARGET_DIR", &dir),
        );
        if rc != 0 {
            let tail: Vec<&str> = out.lines().rev().take(40).collect();
            return Err(format!("cargo build --profile release failed (rc={rc}): {}", tail.into_iter().rev().collect::<Vec<_>>().join("\n")));
        }
        let target = wt.join("target");
        fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(dir.join("release"), target.join("release")).map_err(|e| e.to_string())?;
        Ok(Box::new(held))
    }
    fn release(&self, bin: &Path, args: &[String], db: &str) -> RunOut {
        // batch-job: this runs whatever its caller names, as long as that takes
        // FORK-EDGE: machine leg release
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
    fn own_unit_restarted_by(&self, rel: &Path) -> Option<String> {
        let cgroup = std::fs::read_to_string(format!("{}/cgroup", system_paths::PROC_SELF)).ok()?;
        let unit = unit_of_cgroup(&cgroup)?;
        match spira_config::process::cfg("SPIRA_INSTANCE").ok().filter(|i| !i.is_empty()) {
            Some(instance) => release::units::is_shipped_by(rel, &instance, &unit).then_some(unit),
            None => Some(unit),
        }
    }
    /// A unit that is `Absent` was collected on exit: its exit is unknown, so rc is 0 and the log
    /// is `err`; `deploy::activate_once` decides from `<releases>/current`.
    fn release_detached(&self, key: &str, bin: &Path, args: &[String], db: &str, log: &Path) -> RunOut {
        use runtime::units::{Exit, UnitHandle, UnitProps, UnitSpec, UnitState, Units};
        let fail = |why: String| RunOut { rc: 127, out: String::new(), err: why };
        let units = match runtime::units::SystemdUnits::connect(std::sync::Arc::new(runtime::ports::WallClock)) {
            Ok(u) => u,
            Err(e) => return fail(format!("cannot reach systemd to start the release in a unit of its own: {}", e.0)),
        };
        let _ = fs::remove_file(log);
        let mut env: Vec<(String, String)> = std::env::vars().collect();
        env.retain(|(k, _)| k != "SPIRA_DB");
        env.push(("SPIRA_DB".into(), db.into()));
        let mut argv = vec![bin.display().to_string()];
        argv.extend(args.iter().cloned());
        let spec = UnitSpec { task: "release-activate".into(), key: key.into(), argv, env, cwd: std::env::current_dir().ok() };
        let props = UnitProps { output_append: Some(log.to_path_buf()), ..UnitProps::default() };
        let handle: UnitHandle = match units.start_with(&spec, &props) {
            Ok(h) => h,
            Err(e) => return fail(format!("cannot start release in a unit of its own: {}", e.0)),
        };
        let rc = loop {
            match units.facts(&handle) {
                Ok(f) => match f.state {
                    UnitState::Absent if fs::metadata(log).is_ok_and(|m| m.len() > 0) => break 0,
                    UnitState::Absent => return fail(format!("{} ended without a trace in {}", handle.name, log.display())),
                    UnitState::Failed | UnitState::Inactive => {
                        let _ = units.reset_failed(&handle);
                        break match f.exit {
                            Some(Exit::Code(c)) => c,
                            _ => 1,
                        };
                    }
                    _ => {}
                },
                Err(e) => return fail(format!("cannot read the state of {}: {}", handle.name, e.0)),
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        };
        let err = fs::read_to_string(log).unwrap_or_default();
        RunOut { rc, out: String::new(), err }
    }
    fn stop_round_legs(&self, repo: &str, batch: &str) -> Result<Vec<String>, String> {
        let units =
            runtime::units::SystemdUnits::connect(std::sync::Arc::new(runtime::ports::WallClock))
                .map_err(|e| e.0)?;
        let cgroup = std::fs::read_to_string(format!("{}/cgroup", system_paths::PROC_SELF)).unwrap_or_default();
        runtime::units::stop_round_legs(&units, repo, batch, &|name| cgroup.contains(name))
    }
    fn stop_tree_holders(&self, tree: &Path) -> Result<Vec<String>, String> {
        runtime::holders::stop_holders(
            Path::new(system_paths::PROC),
            tree,
            &[std::process::id()],
            std::time::Duration::from_secs(5),
        )
        .map(|stopped| stopped.iter().map(|h| h.to_string()).collect())
    }
    fn round_legs_live(&self, repo: &str, batch: &str) -> bool {
        runtime::units::SystemdUnits::connect(std::sync::Arc::new(runtime::ports::WallClock))
            .map_err(|e| e.0)
            .and_then(|u| runtime::units::live_round_units(&u, repo, batch, &|_| false))
            .map_or(true, |live| !live.is_empty())
    }
    fn stop_pass_scopes(&self, repo: &str, batch: &str) -> Result<Vec<String>, String> {
        let units =
            runtime::units::SystemdUnits::connect(std::sync::Arc::new(runtime::ports::WallClock))
                .map_err(|e| e.0)?;
        runtime::units::stop_pass_scopes(&units, repo, batch)
    }
    fn unit_active_state(&self, unit: &str) -> String {
        #[cfg(target_os = "linux")]
        {
            let units = runtime::units::SystemdUnits::connect(std::sync::Arc::new(
                runtime::ports::WallClock,
            ));
            active_state_of(
                units
                    .and_then(|u| {
                        runtime::units::Units::facts(&u, &runtime::units::UnitHandle::named(unit))
                    })
                    .ok(),
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = unit;
            active_state_of(None)
        }
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
        candidate_recipe: Option<&str>,
        deny_list: Option<&Path>,
        host_only: bool,
        only: Option<&str>,
    ) -> RunOut {
        run_round_vm(
            tree,
            results,
            &["--base", base],
            round,
            wall_secs,
            handle,
            ami,
            candidate_recipe,
            deny_list,
            host_only,
            only,
        )
    }
    fn deny_list(&self, at: u64) -> Result<String, String> {
        let bin = spira_config::release_env::tool("test-registry")?;
        // FORK-EDGE: machine leg test-registry (deny-list snapshot at the round's first pass)
        let out = spira_config::bounded::bounded(&bin)
            .args(["deny-list", "--at", &at.to_string()])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{bin} deny-list: {e}"))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        Err(format!(
            "{bin} deny-list --at {at} exited {}: {}{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
    fn clear_landed(&self, beads: &[String]) -> Result<Vec<String>, String> {
        let bin = spira_config::release_env::tool("test-registry")?;
        // FORK-EDGE: machine leg test-registry (clear-landed: the row delete and its history row are one transaction only the registry's binary makes)
        let mut cmd = spira_config::bounded::bounded(&bin);
        cmd.arg("clear-landed");
        for b in beads {
            cmd.args(["--bead", b]);
        }
        let out = cmd
            .args(["--actor", "queue"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{bin} clear-landed: {e}"))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect());
        }
        Err(format!(
            "{bin} clear-landed exited {}: {}{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
    fn recipe_sha(&self, tree: Option<&Path>) -> Option<String> {
        // FORK-EDGE: machine leg round-vm (recipe-sha)
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
        // FORK-EDGE: machine leg round-vm (ami-for)
        match Command::new("timeout")
            .args(["60", round_vm.as_str(), "ami-for", sha])
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
    fn image_check(&self, tree: &Path, rev: &str) -> ImageCheck {
        let round_vm = match spira_config::release_env::tool("round-vm") {
            Ok(r) => r,
            Err(e) => return ImageCheck::Unreadable(e),
        };
        // batch-job: git archive and a tag computation, hang-guarded at 60 s
        // FORK-EDGE: machine leg round-vm (image-check)
        match Command::new("timeout")
            .args(["60", round_vm.as_str(), "image-check", "--tree"])
            .arg(tree)
            .args(["--rev", rev])
            .stdin(Stdio::null())
            .output()
        {
            Ok(o) if o.status.code() == Some(0) => {
                parse_image_check(&String::from_utf8_lossy(&o.stdout))
            }
            Ok(o) => ImageCheck::Unreadable(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => ImageCheck::Unreadable(format!("cannot run round-vm: {e}")),
        }
    }
    fn repo_round_vm(
        &self,
        tree: &Path,
        results: &Path,
        entry: &gate::own::CiEntry,
        round: (&str, &str),
        wall_secs: u64,
        handle: &Path,
    ) -> RunOut {
        run_round_vm(
            tree,
            results,
            &[
                "--ci-entry",
                entry.command.as_str(),
                "--ci-image",
                entry.image.as_deref().unwrap_or_default(),
                "--ci-workdir",
                entry.workdir.as_str(),
            ],
            round,
            wall_secs,
            handle,
            None,
            None,
            None,
            false,
            None,
        )
    }
    fn pass_alive(&self, pid: u32) -> bool {
        // SAFETY: signal 0 only checks existence.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    fn rebase_stale(&self, ids: &[String], repo: &str) -> Vec<(String, RunOut)> {
        let rebase_stale = match spira_config::release_env::tool("rebase-stale") {
            Ok(r) => r,
            Err(e) => {
                return ids
                    .iter()
                    .map(|id| {
                        (
                            id.clone(),
                            RunOut {
                                rc: 127,
                                out: String::new(),
                                err: e.clone(),
                            },
                        )
                    })
                    .collect()
            }
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
                    // FORK-EDGE: machine leg rebase-stale
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
        // FORK-EDGE: boundary.sh runs from the round's own tree (that tree's version, not the release's); a port would change which code runs
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
        // FORK-EDGE: machine leg self re-exec (rebase-waiting)
        Command::new(exe)
            .args(["rebase-waiting", repo])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .is_ok()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_round_vm(
    tree: &Path,
    results: &Path,
    judged_by: &[&str],
    round: (&str, &str),
    wall_secs: u64,
    handle: &Path,
    ami: Option<&str>,
    candidate_recipe: Option<&str>,
    deny_list: Option<&Path>,
    host_only: bool,
    only: Option<&str>,
) -> RunOut {
    // batch-job: round-vm enforces the cap itself, on the suites alone; this is its hang guard
    // FORK-EDGE: machine leg round-vm (certify)
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
        .args(candidate_recipe.iter().flat_map(|r| ["--candidate-recipe", *r]))
        .args(deny_list.iter().flat_map(|d| [OsStr::new("--deny-list"), d.as_os_str()]))
        .args(host_only.then_some(["--place", "host"]).into_iter().flatten())
        .args(only.iter().flat_map(|o| ["--only", *o]))
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

/// The forge, in this process (`forge::client`): `gh` is its only child. A configured forge
/// that is not the default name is a fixture's stand-in, run as a program.
pub struct RealForge;

/// One forge verb on the configured `forge`.
fn forge_call(
    forge: &Path,
    verb: &str,
    repo: &Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> forge::client::Reply {
    forge::client::run(&forge::client::route_of(forge), verb, repo, args, input)
}

/// What a verb whose stderr went to this process's said there, said here.
fn said(r: &forge::client::Reply) -> &forge::client::Reply {
    eprint!("{}", r.err);
    r
}

/// A verb whose stdout went to this process's: printed here, as the child printed it.
fn shown(r: forge::client::Reply) -> forge::client::Reply {
    print!("{}", r.out);
    r
}

/// `pr-preflight`/`pr-create`'s refusal: the forge's own stderr, or its exit code when it said nothing.
pub(crate) fn forge_refusal(r: &forge::client::Reply, with_rc: bool) -> String {
    let err = r.err.trim();
    match (err.is_empty(), with_rc) {
        (true, _) => format!("rc={}: (no stderr)", r.code),
        (false, true) => format!("rc={}: {err}", r.code),
        (false, false) => err.to_string(),
    }
}

/// `workflow-rerun`'s answer: 0 started, 3 the forge decided against it (its reason on stderr).
pub(crate) fn rerun_of(r: &forge::client::Reply) -> Rerun {
    match r.code {
        0 => Rerun::Started,
        3 => Rerun::Decided(
            r.err
                .lines()
                .find_map(|l| l.strip_prefix("forge: workflow-rerun refused: "))
                .unwrap_or("the forge refused the re-run")
                .to_string(),
        ),
        _ => Rerun::Failed,
    }
}

pub(crate) fn prs_on(out: &str, prefix: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| l.split_once(' '))
        .filter(|(_, b)| b.starts_with(prefix))
        .map(|(n, b)| (n.to_string(), b.to_string()))
        .collect()
}

impl Forge for RealForge {
    fn pr_preflight(&self, forge: &Path, repo: &Path, head: &str) -> Result<(), String> {
        let r = forge_call(forge, "pr-preflight", repo, &[head], None);
        if r.ok() {
            return Ok(());
        }
        Err(forge_refusal(&r, false))
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
        let r = forge_call(
            forge,
            "pr-create",
            repo,
            &[head, base, title],
            Some(body.as_bytes()),
        );
        if !r.ok() {
            return Err(forge_refusal(&r, true));
        }
        Ok(r.out.trim().to_string())
    }
    fn pr_automerge(&self, forge: &Path, repo: &Path, pr: &str) -> bool {
        shown(forge_call(
            forge,
            "pr-automerge",
            repo,
            &[pr, "merge"],
            None,
        ))
        .ok()
    }
    fn pr_close(&self, forge: &Path, repo: &Path, pr: &str) {
        let _ = shown(forge_call(forge, "pr-close", repo, &[pr], None));
    }
    fn queue_prs(&self, forge: &Path, repo: &Path) -> Option<Vec<(String, String)>> {
        Some(prs_on(
            &forge_call(forge, "pr-list-open", repo, &[], None).stdout()?,
            "spira/queue/",
        ))
    }
    fn publish_prs(&self, forge: &Path, repo: &Path) -> Option<Vec<(String, String)>> {
        Some(prs_on(
            &forge_call(forge, "pr-list-open", repo, &[], None).stdout()?,
            "spira/publish/",
        ))
    }
    fn pr_comment(&self, forge: &Path, repo: &Path, pr: &str, line: &str) {
        let _ = shown(forge_call(forge, "pr-comment", repo, &[pr, line], None));
    }
    fn branch_protect(&self, forge: &Path, repo: &Path, branch: &str) -> bool {
        said(&shown(forge_call(
            forge,
            "branch-protect",
            repo,
            &[branch],
            None,
        )))
        .ok()
    }
    fn pr_state(&self, forge: &Path, repo: &Path, pr: &str) -> Option<String> {
        forge_call(forge, "pr-state", repo, &[pr], None)
            .stdout()
            .map(|s| s.trim().to_string())
    }
    fn pr_merge_commit(&self, forge: &Path, repo: &Path, pr: &str) -> Option<String> {
        forge_call(forge, "pr-merge-commit", repo, &[pr], None)
            .stdout()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    fn check_status(&self, forge: &Path, repo: &Path, pr: &str, branch: &str) -> Option<String> {
        forge_call(forge, "check-status", repo, &[pr, branch], None).stdout()
    }
    fn pr_checks(&self, forge: &Path, repo: &Path, pr: &str) -> Option<Vec<(String, String)>> {
        let out = forge_call(forge, "pr-checks", repo, &[pr], None);
        let text = out.stdout()?;
        Some(
            text.lines()
                .filter_map(|l| {
                    let mut f = l.split_whitespace();
                    Some((f.next()?.to_string(), f.next().unwrap_or("").to_string()))
                })
                .collect(),
        )
    }
    fn run_id(&self, forge: &Path, repo: &Path, branch: &str) -> Option<String> {
        forge_call(forge, "run-id", repo, &[branch], None)
            .stdout()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    fn run_metadata(&self, forge: &Path, repo: &Path, run: &str) -> String {
        forge_call(forge, "run-metadata", repo, &[run], None)
            .stdout()
            .unwrap_or_default()
    }
    fn fail_lines(&self, forge: &Path, repo: &Path, run: &str, suites: &str) -> String {
        forge_call(forge, "fail-lines", repo, &[run, suites], None)
            .stdout()
            .unwrap_or_default()
    }
    fn run_cancel(&self, forge: &Path, repo: &Path, run: &str) {
        let _ = shown(forge_call(forge, "run-cancel", repo, &[run], None));
    }
    fn workflow_dispatch(&self, forge: &Path, repo: &Path, ref_: &str, suites: &str) -> bool {
        said(&shown(forge_call(
            forge,
            "dispatch",
            repo,
            &[ref_, suites],
            None,
        )))
        .ok()
    }
    fn workflow_rerun(&self, forge: &Path, repo: &Path, run: &str) -> Rerun {
        rerun_of(said(&forge_call(
            forge,
            "workflow-rerun",
            repo,
            &[run],
            None,
        )))
    }
    fn ami_runs(&self, forge: &Path, repo: &Path) -> Option<Vec<String>> {
        let text = forge_call(forge, "ami-runs", repo, &[], None).stdout()?;
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
        said(&shown(forge_call(
            forge,
            "ami-dispatch",
            repo,
            &[ref_],
            None,
        )))
        .ok()
    }
    fn ami_result(&self, forge: &Path, repo: &Path, branch: &str) -> Option<AmiBuild> {
        AmiBuild::parse(&forge_call(forge, "ami-result", repo, &[branch], None).stdout()?)
    }
}

// ------------------------------------------------------------------------------- spira-lc

/// The store, through its one door ([`LcClient`]): the service's socket in-process, or the
/// configured program when the config declares a stand-in or direct mode.
pub struct RealLc {
    pub bin: Option<PathBuf>,
    /// The door itself; `None` takes the route the config declares for `bin`.
    pub client: Option<LcClient>,
}

/// `SPIRA_LC_TIMEOUT`, whole seconds as `timeout` read it.
fn lc_timeout() -> Result<std::time::Duration, String> {
    let t = crate::conf::nonempty("SPIRA_LC_TIMEOUT")?;
    let secs: u64 = t
        .trim()
        .trim_end_matches('s')
        .parse()
        .map_err(|e| format!("SPIRA_LC_TIMEOUT = {t:?} does not parse: {e}"))?;
    Ok(std::time::Duration::from_secs(secs))
}

impl RealLc {
    pub fn configured(bin: Option<PathBuf>) -> Self {
        RealLc { bin, client: None }
    }

    fn client(&self) -> Result<LcClient, String> {
        if let Some(c) = &self.client {
            return Ok(c.clone());
        }
        let bin = self.bin.as_ref().ok_or("no spira-lc program")?;
        Ok(LcClient::for_bin(&bin.to_string_lossy())?.within(lc_timeout()?))
    }

    /// Each member's commits above `base`, read here with git and recorded for the service, which
    /// runs no git: a range it holds is how a cascade finds the members stacked on one another.
    /// A member whose range cannot be read or recorded is simply not recorded.
    fn record_ranges(&self, repo: &str, base: &str, members: &str) {
        let Ok(home) = spira_config::resolve::locate_home_for_process() else { return };
        let Some(root) = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home).root(repo) else { return };
        for (_, tip) in members.split(',').filter_map(|m| m.split_once(':')) {
            let Some(out) = stdout_of(git(Path::new(&root)).args(["rev-list", tip, &format!("^{base}")])) else { continue };
            let commits: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            let body = serde_json::json!({ "repo": repo, "base": base, "tip": tip, "commits": commits }).to_string();
            let _ = self.run(&["git-fact", "put", "range", "--json", &body]);
        }
    }

    /// Any answer: its code and everything it printed.
    fn run(&self, args: &[&str]) -> (i32, String) {
        let c = match self.client() {
            Ok(c) => c,
            Err(e) => return (2, e),
        };
        match c.call(args) {
            Ok(r) => (r.code, format!("{}{}", r.out, r.err)),
            Err(e) => (2, e.to_string()),
        }
    }

    /// Stdout of an answer that applied; asked twice before giving up.
    fn stdout(&self, args: &[&str]) -> Result<String, String> {
        let c = self.client()?;
        let mut last = String::new();
        for attempt in 0..2 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            match c.call(args) {
                Ok(r) if r.code == 0 => return Ok(r.out),
                Ok(r) => {
                    let why = if r.err.trim().is_empty() {
                        r.out.trim()
                    } else {
                        r.err.trim()
                    };
                    last = format!(
                        "spira-lc {} exited {:?}: {why}",
                        args.first().unwrap_or(&""),
                        Some(r.code)
                    );
                }
                Err(e) => last = format!("spira-lc {}: {e}", args.first().unwrap_or(&"")),
            }
        }
        Err(last)
    }

    /// `rc <verb> --json <body>`, decoded as the door's wire answers.
    fn rc<T: serde::de::DeserializeOwned>(
        &self,
        verb: state_store::wire::Verb,
        body: &impl serde::Serialize,
    ) -> Result<T, String> {
        let what = format!("spira-lc rc {}", verb.word());
        let c = self.client()?;
        let body = serde_json::to_string(body).map_err(|e| format!("{what}: {e}"))?;
        let r = c
            .call(&["rc", verb.word(), "--json", &body])
            .map_err(|e| format!("{what}: {e}"))?;
        state_store::wire::decode(r.code, &r.out).map_err(|e| format!("{what}: {e}"))
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
    format!(
        "queue/{}/{nanos}/{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

impl crate::batches::Door for RealLc {
    fn get(&self, key: &str) -> Result<Option<state_store::Item>, String> {
        self.rc(
            state_store::wire::Verb::Get,
            &state_store::wire::Get { key: key.into() },
        )
    }
    fn list(&self, prefix: &str) -> Result<Vec<state_store::Item>, String> {
        let body = state_store::wire::List {
            prefixes: vec![prefix.into()],
            selector: state_store::Selector::Everything,
            states: Vec::new(),
            limit: None,
        };
        self.rc(state_store::wire::Verb::List, &body)
            .map(|l: state_store::wire::Listed| l.items)
    }
    fn list_in_states(
        &self,
        prefix: &str,
        states: &[String],
        limit: usize,
    ) -> Result<Vec<state_store::Item>, String> {
        let body = state_store::wire::List {
            prefixes: vec![prefix.into()],
            selector: state_store::Selector::Everything,
            states: states.to_vec(),
            limit: Some(limit),
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
        static UNRESOLVED: std::sync::OnceLock<crate::batches::Unresolved> =
            std::sync::OnceLock::new();
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
    fn batch_passes(&self, batch_id: &str) -> Vec<serde_json::Value> {
        let Ok(out) = self.stdout(&["list", "--batches", "--batch", batch_id]) else {
            return Vec::new();
        };
        let rows: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
        rows.as_array()
            .and_then(|r| r.first())
            .and_then(|r| r["passes"].as_array().cloned())
            .unwrap_or_default()
    }
    fn batch_progress(&self, batch_id: &str) -> Option<serde_json::Value> {
        let out = self.stdout(&["show-batch", batch_id]).ok()?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        v.get("progress").filter(|p| !p.is_null()).cloned()
    }
    fn create_bead(&self, id: &str) {
        let _ = self.run(&["create-bead", id, "--ready"]);
    }
    fn create_bead_in(&self, id: &str, repo: &str) {
        let _ = self.run(&["create-bead", id, "--repo", repo, "--ready"]);
    }
    fn fact(&self, bead: &str, kind: &str, actor: &str, cause: &str) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&["fact", "add", bead, "--kind", kind, "--actor", actor, "--cause", cause]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn set_carrier(&self, members: &[&str], by: Option<&str>, only_if_unset: bool) -> Result<String, (i32, String)> {
        let mut argv = vec!["set-carrier"];
        argv.extend(members);
        match by {
            Some(id) => argv.extend(["--by", id]),
            None => argv.push("--clear"),
        }
        if only_if_unset {
            argv.push("--only-if-unset");
        }
        match self.run(&argv) {
            (0, out) => Ok(out),
            (rc, out) => Err((rc, out)),
        }
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
        self.record_ranges(repo, base, members);
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
        self.record_ranges(repo, base, members);
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
    fn requeue_orphans(
        &self,
        actor: &str,
        running: &dyn Fn(&str, &str) -> bool,
    ) -> Result<String, (i32, String)> {
        let (rc, out) = self.run(&["requeue-orphans", "--actor", actor]);
        if rc != 0 {
            return Err((rc, out.trim().to_string()));
        }
        let Ok(seen) = serde_json::from_str::<serde_json::Value>(&out) else {
            return Err((
                rc,
                format!("requeue-orphans dry run unreadable: {}", out.trim()),
            ));
        };
        let live: Vec<String> = seen
            .get("repos")
            .and_then(|r| r.as_object())
            .into_iter()
            .flatten()
            .filter(|(batch, repo)| repo.as_str().is_some_and(|repo| running(repo, batch)))
            .map(|(batch, _)| batch.clone())
            .collect();
        let mut argv = vec!["requeue-orphans", "--actor", actor, "--apply"];
        argv.extend(live.iter().flat_map(|b| ["--live-round", b.as_str()]));
        let (rc, out) = self.run(&argv);
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
        let out = self.stdout(&["event-seq", "--head"])?;
        let head: serde_json::Value = serde_json::from_str(&out).map_err(|e| format!("spira-lc event-seq: {e}"))?;
        match head.get("seq") {
            Some(seq) if seq.is_u64() || seq.is_i64() => Ok(()),
            _ => Err(format!("spira-lc event-seq: no integer seq in {out:?}")),
        }
    }
    fn bead_rows(&self, state: Option<&str>) -> Result<Vec<LcBeadRow>, String> {
        let out = match state {
            Some(s) => self.stdout(&["list", "--state", s])?,
            None => self.stdout(&["list", "--live"])?,
        };
        serde_json::from_str(&out).map_err(|e| format!("spira-lc list: {e}"))
    }
    fn bead_rows_of(&self, ids: &[String]) -> Result<Vec<LcBeadRow>, String> {
        let mut rows = Vec::new();
        for chunk in ids.chunks(100) {
            let out = self.stdout(&["list", "--ids", &chunk.join(",")])?;
            rows.extend(serde_json::from_str::<Vec<LcBeadRow>>(&out).map_err(|e| format!("spira-lc list: {e}"))?);
        }
        Ok(rows)
    }
    fn polled_rows(&self, state: &str) -> Result<Vec<LcBeadRow>, String> {
        let out = self.stdout(&["list", "--state", state])?;
        serde_json::from_str(&out).map_err(|e| format!("spira-lc list: {e}"))
    }
    fn express_ids(&self) -> Result<Vec<String>, String> {
        let out = self.stdout(&["list", "--express", "--live"])?;
        let rows: Vec<LcBeadRow> =
            serde_json::from_str(&out).map_err(|e| format!("spira-lc list: {e}"))?;
        Ok(rows.into_iter().map(|r| r.bead_id).collect())
    }
    fn fresh_read(&self, args: &[&str]) -> Result<String, String> {
        let reply = LcClient::configured()?.call(args).map_err(|e| e.to_string())?;
        if reply.code == 0 {
            return Ok(reply.out);
        }
        Err(format!("`spira-lc {}` exited {}: {}", args.join(" "), reply.code, reply.text()))
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
        let resource = format!("round/{repo}/{batch}");
        let r = self
            .client()?
            .call(&["lock", "show", &resource])
            .map_err(|e| format!("spira-lc lock show {resource}: {e}"))?;
        let said = || {
            format!(
                "spira-lc lock show {resource} exited {:?}: {} {}",
                Some(r.code),
                r.out.trim(),
                r.err.trim()
            )
        };
        match r.code {
            0 => serde_json::from_str(r.out.trim())
                .ok()
                .and_then(|v| round_lease_of(&v))
                .map(Some)
                .ok_or_else(said),
            1 => Ok(None),
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
    fn now_ms(&self) -> u64 {
        match spira_config::vtime::override_epoch() {
            Some(t) => t * 1000,
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
        }
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
    #[test_id(tv7idjr)]
    #[test]
    fn the_image_check_answer_is_present_absent_with_a_tag_or_unreadable() {
        assert_eq!(parse_image_check("present\n"), ImageCheck::Present);
        assert_eq!(parse_image_check("absent f2656\n"), ImageCheck::Absent("f2656".into()));
        assert!(matches!(parse_image_check("absent \n"), ImageCheck::Unreadable(_)));
        assert!(matches!(parse_image_check(""), ImageCheck::Unreadable(_)));
    }

    fn crate_tree(root: &Path, a_bytes: &str) {
        for c in ["a", "b"] {
            fs::create_dir_all(root.join(c).join("src")).unwrap();
            fs::write(root.join(c).join("Cargo.toml"), format!("[package]\nname = \"crate-{c}\"\n")).unwrap();
            fs::write(root.join(c).join("src/lib.rs"), if c == "a" { a_bytes } else { "pub fn f() {}\n" }).unwrap();
        }
        fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"a\", \"b\"]\n").unwrap();
        fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
    }

    type World = (testkit::TempDir, Vec<(&'static str, Option<String>)>, PathBuf, PathBuf, PathBuf);

    fn release_world(name: &str, cargo_body: &str) -> World {
        let d = testkit::TempDir::new(name);
        let base = d.path().join("pool");
        let toml = spira_config::process::fixture_toml(d.path(), &[("SPIRA_DWARF_BUILD_BASE", base.to_str().unwrap())]);
        let bindir = d.path().join("bin");
        let log = d.path().join("cargo.log");
        fs::create_dir_all(&bindir).unwrap();
        testkit::write_exe(bindir.join("cargo"), &format!("#!/bin/sh\necho \"$*\" >> {}\n{cargo_body}", log.display()));
        let home = spira_config::resolve::locate_home_for_process().unwrap();
        let path = format!("{}:{}", bindir.display(), std::env::var("PATH").unwrap_or_default());
        let env = vec![
            ("SPIRA_TOML", Some(toml.display().to_string())),
            ("SPIRA_HOME", Some(home.display().to_string())),
            ("PATH", Some(path)),
        ];
        (d, env, base, log, home)
    }

    fn with_env(env: &[(&'static str, Option<String>)]) -> testkit::EnvGuard {
        let edits: Vec<(&str, Option<&str>)> = env.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        testkit::env(&edits)
    }

    #[test_id(t03vgla)]
    #[test]
    fn the_release_build_cleans_what_differs_from_the_directorys_record_before_building_and_still_reports_a_failed_build() {
        let (d, env, base, log, home) = release_world("kcuvbg-clean", "d=\"$(dirname \"$0\")\"; [ ! -e \"$d/fail\" ] || case \"$1\" in build) echo boom; exit 3;; esac; [ ! -e \"$d/failclean\" ] || case \"$1\" in clean) echo no; exit 4;; esac\n");
        let _env = with_env(&env);
        let (old, tip) = (d.path().join("old"), d.path().join("tip"));
        crate_tree(&old, "pub fn f() {}\n");
        crate_tree(&tip, "pub fn f() { let _ = 1; }\n");
        let slot = base.join("dir.1");
        fs::create_dir_all(&slot).unwrap();
        fs::write(base.join("dir.1.lock"), "").unwrap();
        let path = std::env::var("PATH").unwrap();
        spira_config::build_dir::clean(&home.join("target-clean.sh"), &old, &slot, &path, true).unwrap();
        let wt = d.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        crate_tree(&wt, "pub fn f() { let _ = 1; }\n");
        let held = RealScripts { home: home.clone() }.build_release(&wt, "hand-1", "spira").expect("builds");
        let lines: Vec<String> = fs::read_to_string(&log).unwrap().lines().map(String::from).collect();
        let clean = lines.iter().position(|l| l == "clean -q -p crate-a").unwrap_or_else(|| panic!("no clean of crate-a: {lines:?}"));
        let build = lines.iter().position(|l| l.starts_with("build -q --profile release")).expect("built");
        assert!(clean < build, "clean runs before the build: {lines:?}");
        assert!(wt.join("target/release").is_symlink(), "the release symlink is made");
        drop(held);
        fs::write(d.path().join("bin/fail"), "").unwrap();
        let err = RealScripts { home: home.clone() }.build_release(&wt, "hand-2", "spira").expect_err("a failing build fails");
        assert!(err.contains("cargo build --profile release failed (rc=3)"), "{err}");
        fs::remove_file(d.path().join("bin/fail")).unwrap();
        fs::write(d.path().join("bin/failclean"), "").unwrap();
        fs::write(wt.join("a/src/lib.rs"), "pub fn f() { let _ = 2; }\n").unwrap();
        let builds = fs::read_to_string(&log).unwrap().matches("build -q").count();
        let err = RealScripts { home }.build_release(&wt, "hand-3", "spira").expect_err("a failing clean fails");
        assert!(err.contains("selective clean") && err.contains("emptied"), "{err}");
        assert!(!slot.join(".unit-tests-built-manifest").exists(), "the untrusted directory is emptied");
        assert_eq!(fs::read_to_string(&log).unwrap().matches("build -q").count(), builds, "nothing is built after a failed clean");
    }

    #[test_id(tq0vdkg)]
    #[test]
    fn gate_command_hands_gate_sh_the_lock_wait() {
        let c = super::gate_command("spira/sp-a", "r", "sp-a", "5400");
        let env = |k: &str| {
            c.get_envs()
                .find(|(n, _)| *n == k)
                .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
        };
        assert_eq!(env("SPIRA_GATE_LOCK_WAIT").as_deref(), Some("5400"));
        assert_eq!(env("SPIRA_GATE_BEAD").as_deref(), Some("sp-a"));
    }

    #[test_id(tmdbq51)]
    #[test]
    fn update_ref_is_the_sanctioned_writer() {
        let c = super::update_ref_cmd(std::path::Path::new("r"), "refs/heads/x", "new", Some("old"));
        let env = c.get_envs().find(|(k, _)| *k == "SPIRA_REF_SANCTIONED");
        assert_eq!(env.and_then(|(_, v)| v), Some(std::ffi::OsStr::new("1")));
    }

    use spira_store::lc_call::{Reply, Transport};

    struct Seen(std::sync::Mutex<Vec<Vec<String>>>);

    impl Transport for Seen {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            let out = if argv.first().is_some_and(|a| a == "event-seq") { r#"{"seq":7,"keys":[]}"# } else { "[]" };
            Ok(Reply { code: 0, out: out.into(), err: String::new(), read_ended: false })
        }
    }

    fn sent_by(read: impl FnOnce(&RealLc)) -> Vec<Vec<String>> {
        let seen = std::sync::Arc::new(Seen(Default::default()));
        let lc = RealLc { bin: None, client: Some(LcClient::over(seen.clone(), true)) };
        read(&lc);
        let sent = seen.0.lock().unwrap().clone();
        sent
    }

    #[test_id(t0zrdje)]
    #[test]
    fn the_queue_reads_the_live_set_or_named_ids_never_every_row() {
        let sent = sent_by(|lc| {
            lc.bead_rows(None).unwrap();
            lc.bead_rows_of(&["sp-a".to_string(), "sp-b".to_string()]).unwrap();
        });
        assert_eq!(sent, [vec!["list", "--live"], vec!["list", "--ids", "sp-a,sp-b"]]);
    }

    #[test_id(ttvn2i1)]
    #[test]
    fn a_cut_and_a_stage_record_each_members_range_with_git_before_they_send() {
        let git = |dir: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let dir = testkit::TempDir::new("queue-record-ranges");
        let repo = dir.path().join("alpha");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "prereq"]);
        let prereq = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "dependent"]);
        let dependent = git(&repo, &["rev-parse", "HEAD"]);
        let env = spira_config::process::fixture_world_env(dir.path(), &dir.path().join("run"), &["spira", "alpha"]);
        let edits: Vec<(&str, Option<&str>)> = env.iter().map(|(k, v)| (*k, v.as_deref())).collect();
        let _env = testkit::env(&edits);
        let members = format!("sp-pre:{prereq},sp-dep:{dependent}");
        let sent = sent_by(|lc| {
            let _ = lc.cut("b-1", "alpha", "H", &base, &members, "queue", None);
            let _ = lc.stage("b-2", "alpha", "H", &base, &members, "b-1", "queue");
        });
        let verb_at = |verb: &str| sent.iter().position(|a| a[0] == verb).unwrap();
        let recorded: Vec<serde_json::Value> = sent.iter().filter(|a| a.starts_with(&["git-fact".to_string(), "put".to_string(), "range".to_string()])).map(|a| serde_json::from_str(&a[4]).unwrap()).collect();
        assert_eq!(recorded.len(), 4, "two members, for each of the cut and the stage: {sent:?}");
        let of_dep = recorded.iter().find(|r| r["tip"] == dependent.as_str()).unwrap();
        assert_eq!((of_dep["repo"].as_str(), of_dep["base"].as_str()), (Some("alpha"), Some(base.as_str())));
        assert_eq!(of_dep["commits"], serde_json::json!([dependent, prereq]));
        let of_pre = recorded.iter().find(|r| r["tip"] == prereq.as_str()).unwrap();
        assert_eq!(of_pre["commits"], serde_json::json!([prereq]));
        assert!(sent[..verb_at("cut")].iter().filter(|a| a[0] == "git-fact").count() == 2, "the ranges go before the cut: {sent:?}");
        assert!(sent[..verb_at("stage")].iter().filter(|a| a[0] == "git-fact").count() == 4, "and before the stage: {sent:?}");
    }

    #[test_id(tu0q60g)]
    #[test]
    fn probe_asks_for_the_head_revision_not_a_listing() {
        let mut result = Ok(());
        let sent = sent_by(|lc| result = lc.probe());
        assert_eq!(sent, [vec!["event-seq", "--head"]]);
        assert_eq!(result, Ok(()));
    }

    #[test_id(tkevcvp)]
    #[test]
    fn an_unregistered_key_reads_as_none_and_any_other_refusal_propagates() {
        let unregistered = "K is not a registered config key (spira/conf.d) — refusing".to_string();
        assert_eq!(unregistered_is_none::<u64>(Err(unregistered)), Ok(None));
        assert_eq!(unregistered_is_none::<u64>(Ok(7)), Ok(Some(7)));
        assert!(unregistered_is_none::<u64>(Err("K = \"x\" does not parse as a u64".into())).is_err());
    }

    #[test_id(thvqyx9)]
    #[test]
    fn every_config_key_is_registered_in_conf_d() {
        let conf_d = spira_config::resolve::locate_home_for_process().unwrap().join("conf.d");
        let missing = |keys: &[&str]| keys.iter().filter(|k| !conf_d.join(k).is_file()).map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(missing(&["SPIRA_PLANTED_UNREGISTERED"]).len(), 1, "positive control: an unregistered key is reported");
        assert!(missing(super::CONFIG_KEYS).is_empty(), "CONFIG_KEYS names keys spira/conf.d does not register: {:?}", missing(super::CONFIG_KEYS));
    }

    #[test_id(tno1423)]
    #[test]
    fn check_config_names_every_key_context_reads() {
        let src = include_str!("real.rs");
        let body = &src[src.find("fn context(").unwrap()..src.find("let s = Settings {").unwrap()];
        let mut read = Vec::new();
        for part in body.split("process::cfg").skip(1) {
            let after = &part[part.find("(\"").unwrap() + 2..];
            read.push(&after[..after.find('"').unwrap()]);
        }
        assert!(
            read.len() >= 8,
            "the scan must find the reads it is checking: {read:?}"
        );
        for k in &read {
            assert!(
                CONFIG_KEYS.contains(k),
                "{k} is read by context but not checked by check-config"
            );
        }
    }

    #[test_id(t1wn3w3)]
    #[test]
    fn a_resubmit_suite_check_passes_testenv_a_repeat_reason_flag() {
        let args = |first| -> Vec<String> {
            suite_run_command("testenv", "test-x.sh", "t", "r", first, &[], Path::new("d"))
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let first = args(true);
        let at = first.iter().position(|a| a == "--repeat-reason").unwrap();
        assert!(first[at + 1].chars().count() >= 10);
        assert!(!args(false).iter().any(|a| a == "--repeat-reason"));
    }

    #[test_id(tng8wsc)]
    #[test]
    fn a_resubmit_suite_check_hands_testenv_the_deny_list_snapshot() {
        let c = suite_run_command("testenv", "unit-tests", "t", "r", true, &[], Path::new("snap/deny"));
        let env = c.get_envs().find(|(k, _)| *k == "SPIRA_TEST_DENY_LIST").and_then(|(_, v)| v);
        assert_eq!(env, Some(std::ffi::OsStr::new("snap/deny")));
    }

    #[test_id(tvzrqr4)]
    #[test]
    fn the_on_red_hook_survives_a_replaced_binary_and_refuses_a_spaced_path() {
        assert_eq!(
            on_red_cmd("r/queue (deleted)", "b1", "repo").as_deref(),
            Some("r/queue round failed b1 repo --suite")
        );
        assert_eq!(
            on_red_cmd("r/queue", "b1", "repo").as_deref(),
            Some("r/queue round failed b1 repo --suite")
        );
        assert_eq!(on_red_cmd("my dir/queue", "b1", "repo"), None);
    }
    use super::*;

    #[test_id(tj82vcm)]
    #[test]
    fn utc_stamp_matches_date() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_790_000_000), "20260921T141320Z");
        assert_eq!(utc_stamp(951_782_400), "20000229T000000Z");
    }

    #[test_id(tfierok)]
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

    #[test_id(tm10b3t)]
    #[test]
    fn push_adds_app_credentials_only_when_configured() {
        let args = ["-q".to_string(), "origin".to_string(), "main".to_string()];
        let argv = |app| {
            crate::ops::helpers::push_command(Path::new("repo"), &args, app)
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
mod unit_of_cgroup_tests {
    use super::unit_of_cgroup;

    #[test_id(tqnwuxd)]
    #[test]
    fn the_unit_is_the_last_component_of_the_unified_line() {
        let text = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/spira-runtime-prod.service\n";
        assert_eq!(unit_of_cgroup(text).as_deref(), Some("spira-runtime-prod.service"));
        assert_eq!(unit_of_cgroup("0::/user.slice/session-3.scope\n").as_deref(), Some("session-3.scope"));
        assert_eq!(unit_of_cgroup("0::/\n"), None);
    }
}

#[cfg(test)]
mod vtime_tests {
    use super::*;
    use crate::ports::Clock;

    #[test_id(t58v9fb)]
    #[test]
    fn sys_clock_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, || SysClock.now());
        assert_eq!(got, 1_900_000_000);
    }

    #[test_id(t8dnjp6)]
    #[test]
    fn branches_under_a_refs_heads_prefix_are_read_from_loose_and_packed_ref_files() {
        let d = testkit::TempDir::new("real-branches-files");
        let sha = |c: char| c.to_string().repeat(40);
        let g = d.join(".git");
        fs::create_dir_all(g.join("refs/heads/spira")).unwrap();
        fs::write(g.join("refs/heads/spira/sp-loose"), format!("{}\n", sha('1'))).unwrap();
        fs::write(g.join("refs/heads/spira/sp-both"), format!("{}\n", sha('2'))).unwrap();
        fs::write(g.join("packed-refs"), format!("# pack-refs\n{} refs/heads/spira/sp-packed\n{} refs/heads/spira/sp-both\n{} refs/heads/main\n", sha('3'), sha('4'), sha('5'))).unwrap();
        let want = vec![("spira/sp-both".to_string(), sha('2')), ("spira/sp-loose".to_string(), sha('1')), ("spira/sp-packed".to_string(), sha('3'))];
        assert_eq!(RealGit.branches(d.path(), "refs/heads/spira/"), want);
        fs::write(g.join("refs/heads/spira/sp-loose"), format!("{}\n", sha('6'))).unwrap();
        assert_eq!(RealGit.branches(d.path(), "refs/heads/spira/")[1].1, sha('6'), "nothing is cached between reads");
    }
}

/// The service or scope a `/proc/<pid>/cgroup` text places its process in: the last path
/// component of the unified-hierarchy line that names one.
fn unit_of_cgroup(cgroup: &str) -> Option<String> {
    cgroup.lines().filter_map(|l| l.strip_prefix("0::")).flat_map(|p| p.rsplit('/').next()).find(|n| n.ends_with(".service") || n.ends_with(".scope")).map(str::to_string)
}
