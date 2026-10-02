//! The ports against the host: git, bd, the lib.sh seam, the harness scripts, the forge,
//! spira-lc, the config (spira-config library only), the clock, the environment, stdio.

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::model::{BeadRow, LandMode, LcBeadRow, RangeCommit};
use crate::ports::*;
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
            let r = libc::syscall(libc::SYS_close_range, 3u32, libc::c_uint::MAX, CLOSE_RANGE_CLOEXEC);
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
    let Ok(f) = File::create(&path) else { return (127, String::new()) };
    let Ok(f2) = f.try_clone() else { return (127, String::new()) };
    close_inherited_fds(cmd);
    let rc = cmd.stdin(Stdio::null()).stdout(f).stderr(f2).status().ok().and_then(|s| s.code()).unwrap_or(127);
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
    cmd.stdin(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn stdout_of(cmd: &mut Command) -> Option<String> {
    close_inherited_fds(cmd);
    let o = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).to_string())
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
        stdout_of(git(repo).args(["rev-parse", "--verify", "-q", rev])).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }
    fn is_ancestor(&self, repo: &Path, a: &str, b: &str) -> bool {
        ok(git(repo).args(["merge-base", "--is-ancestor", a, b]).stderr(Stdio::null()))
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
        ok(git(repo).args(["show-ref", "--verify", "--quiet", refname]).stderr(Stdio::null()))
    }
    fn current_branch(&self, repo: &Path) -> Option<String> {
        stdout_of(git(repo).args(["symbolic-ref", "-q", "--short", "HEAD"])).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
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
        Ok(crate::publish_range::parse_log(&String::from_utf8_lossy(&o.stdout)))
    }
    fn commit_exists(&self, repo: &Path, sha: &str) -> bool {
        ok(git(repo).args(["cat-file", "-e", &format!("{sha}^{{commit}}")]).stderr(Stdio::null()))
    }
    fn branch_set(&self, repo: &Path, name: &str, sha: &str, force: bool) -> bool {
        let mut c = git(repo);
        c.arg("branch");
        if force {
            c.arg("-f");
        }
        ok(c.args([name, sha]).stdout(Stdio::null()).stderr(Stdio::null()))
    }
    fn branch_delete(&self, repo: &Path, name: &str) -> bool {
        ok(git(repo).args(["branch", "-D", name]).stdout(Stdio::null()).stderr(Stdio::null()))
    }
    fn branch_delete_sanctioned(&self, repo: &Path, name: &str) -> bool {
        ok(git(repo).args(["branch", "-D", name]).env("SPIRA_REF_SANCTIONED", "1").stdout(Stdio::null()).stderr(Stdio::null()))
    }
    fn branches(&self, repo: &Path, prefix: &str) -> Vec<(String, String)> {
        stdout_of(git(repo).args(["for-each-ref", "--format=%(refname:short) %(objectname)", prefix]))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())))
            .collect()
    }
    fn worktree_prune(&self, repo: &Path) {
        let _ = ok(git(repo).args(["worktree", "prune"]).stderr(Stdio::null()));
    }
    fn worktree_add_detached(&self, repo: &Path, path: &Path, sha: &str) -> bool {
        ok(git(repo).args(["worktree", "add", "-q", "--detach"]).arg(path).arg(sha).stdout(Stdio::null()).stderr(Stdio::null()))
    }
    fn worktree_remove(&self, repo: &Path, path: &Path) {
        let _ = ok(git(repo).args(["worktree", "remove", "-f"]).arg(path).stderr(Stdio::null()));
    }
    fn merge_no_ff(&self, wt: &Path, message: &str, tip: &str) -> bool {
        // The subject carries a bead title (free text): a file, never argv.
        let msg = std::env::temp_dir().join(format!("queue-merge-msg-{}-{}", std::process::id(), unique()));
        if fs::write(&msg, message).is_err() {
            return false;
        }
        let name = std::env::var("SPIRA_GIT_NAME").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "spira".into());
        let email = std::env::var("SPIRA_GIT_EMAIL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "spira@spira.invalid".into());
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
    fn is_clean(&self, wt: &Path) -> bool {
        stdout_of(git(wt).args(["status", "--porcelain"])).map(|s| s.trim().is_empty()).unwrap_or(false)
    }
}

// ------------------------------------------------------------------------------------- bd

pub struct RealBd {
    pub bd: String,
    pub db: String,
}

impl Bd for RealBd {
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        if self.db.is_empty() {
            return Err("SPIRA_DB is empty".into());
        }
        let timeout = std::env::var("BD_TIMEOUT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "180".into());
        let mut out = Vec::new();
        for chunk in ids.chunks(100) {
            let o = Command::new("timeout")
                .arg(&timeout)
                .arg(&self.bd)
                .arg("-C")
                .arg(&self.db)
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
    let start = text.lines().position(|l| l.starts_with('[') || l.starts_with('{')).ok_or("bd answered no JSON")?;
    let body: String = text.lines().skip(start).collect::<Vec<_>>().join("\n");
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    Ok(items.into_iter().filter_map(|i| serde_json::from_value(i).ok()).collect())
}

/// Pin the lifecycle switch into a step child's environment (queue-step-all.md). OFF is
/// `SPIRA_LIFECYCLE_ENFORCE=0` and nothing else: spira-lc is invoked by name (sp-gypjk), so
/// there is no path to poison, and binary presence is never the switch.
fn lifecycle_env(c: &mut Command, lc_off: bool) {
    if lc_off {
        c.env("SPIRA_LIFECYCLE_ENFORCE", "0");
    } else {
        c.env("SPIRA_LIFECYCLE_ENFORCE", "1");
    }
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
        let mut cmd = Command::new("bash");
        cmd.stdin(Stdio::piped()).stderr(Stdio::inherit());
        cmd.stdout(if capture { Stdio::piped() } else { Stdio::inherit() });
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own release's
        // bin/+spira/ on the CHILD's PATH, never only inherited — see inbox-triage's scar.
        cmd.envs(spira_config::release_env::child_path_env_for_process());
        let Ok(mut child) = cmd.spawn() else { return (127, String::new()) };
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
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("remote")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().map(String::from).collect())
        .unwrap_or_default()
}

impl Lib for RealLib {
    fn context(&self, repo: Option<&str>) -> Result<(Settings, RepoCtx), String> {
        // spira_home_repo/repo_root/repo_land/repo_field (family U) and spira_landref/
        // spira_publish_forge (family W, sp-o88bx "wave 4.12") no longer a bash seam call
        // (sp-k6lku, "wave 4.13"): the name is resolved in-process first, then threaded
        // into the (slimmed) CONTEXT seam as $1 only so its CI-timeout per-repo override
        // lookup still has it, and every repo-registry field below reads the same
        // Registry the shims used to shell out to.
        let reg = self.repo_registry();
        let name = repo.map(str::to_string).unwrap_or_else(|| reg.home_repo().to_string());
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
        let s = Settings {
            home: PathBuf::from(g("home")),
            run: PathBuf::from(g("run")),
            queue_dir: PathBuf::from(g("queue_dir")),
            landstate: PathBuf::from(g("landstate")),
            releases: pb(&g("releases")),
            forge: PathBuf::from(g("forge")),
            repo_map: pb(&g("repo_map")),
            // Every harness tool by name, on the launcher's PATH (sp-gypjk).
            batcher_bin: Some(PathBuf::from("batcher")),
            batcher_off: g("batcher_enable").trim() == "0",
            lc_bin: Some(PathBuf::from("spira-lc")),
            submitted_label: g("submitted_label"),
            home_repo: reg.home_repo().to_string(),
            db: g("db"),
            bd: g("bd"),
            transition_pollsec: n("pollsec", 5),
            transition_maxsec: n("maxsec", 1800),
            preflight_wall_secs: n("preflight", 240),
            verdict: VerdictSettings {
                ci_maxsec: n("ci_maxsec", 3600),
                ci_idle_sec: n("ci_idle", 600),
                infra_retries: n("infra_retries", 2),
                lock_wait: n("lock_wait", 90),
                lock_starve_max: n("starve_max", 5),
                incident_priority: Some(g("incident_priority")).filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "1".into()),
            },
        };
        let path = reg.root(&name);
        let landref = spira_config::repos::landref(&reg, &name).filter(|s| !s.is_empty());
        let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let publish = spira_config::repos::publish_forge(&reg, &name, &env_map);
        let remotes = path.as_deref().map(git_remotes).unwrap_or_default();
        let path = path.map(PathBuf::from);
        let r = RepoCtx {
            mode: LandMode::parse(&reg.land(&name)).unwrap_or(LandMode::Push),
            map_land: reg.field(&name, spira_config::repos::Column::Land).unwrap_or_default(),
            map_base: reg.field(&name, spira_config::repos::Column::Base).unwrap_or_default(),
            name,
            path,
            landref,
            publish,
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
    /// landing-pass owns the landstate ledger's one writer now (sp-cnnt6, "wave 4.16"):
    /// `landing-pass mark`, reading `$SPIRA_RUN` from this process's own environment —
    /// never the lib.sh seam, which this family dropped.
    fn land_mark(&self, id: &str, state: &str, tip: &str, reason: &str) {
        let _ = Command::new("landing-pass").args(["mark", id, state, tip, reason]).status();
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
    fn bead_close_on_land(&self, id: &str, sha: &str) {
        self.call(Op::CloseOnLand, &[id, sha], false);
    }
    /// `gh-intake closeout <id> <sha> <repo>` (sp-j3fim, "wave 4.31"): gh_issue_closeout
    /// moved natively into gh-intake; this crate shells to the compiled binary by bare
    /// name now, the same way `land_mark` shells to `landing-pass` — no lib.sh snippet
    /// backs this any more. stdout/stderr inherit straight through, exactly as the
    /// dropped seam call's own `capture: false` did.
    fn gh_issue_closeout(&self, id: &str, sha: &str, repo: &Path) {
        let _ = Command::new("gh-intake").args(["closeout", id, sha, &repo.display().to_string()]).status();
    }
    fn comment(&self, id: &str, text: &str) {
        self.call(Op::Comment, &[id, text], false);
    }
    fn notify(&self, repo: &str, subject: &str, body: &str) {
        // In-process (sp-hwjsq, "wave 4.32"): queue owns this family now, so its own seam
        // call onto queue_notify_concierge is gone — lib.sh's copy is retired outright.
        crate::ops::helpers::notify(repo, subject, body);
    }
    fn event(&self, kind: &str, title: &str, detail: &str) {
        self.call(Op::Event, &[kind, title, detail], false);
    }
    fn divergence(&self, queue_dir: &Path, repo: &str, path: &Path, forge: &str, local: &str) -> Divergence {
        // In-process (sp-hwjsq, "wave 4.32"); lib.sh's queue_local_check_divergence is
        // retired outright — nothing else ever called it.
        crate::ops::helpers::check_divergence(queue_dir, repo, path, forge, local)
    }
    fn push(&self, path: &Path, remote: &str, refspec: &str) -> bool {
        // In-process (sp-hwjsq, "wave 4.32"). Stderr discarded here, as the old seam body
        // (`spira_git_push ... 2>/dev/null`) did for this call site specifically —
        // spira_git_push itself never redirected anything; that stays the caller's choice,
        // which is why the CLI shim for bash callers (below, in main.rs) does not.
        ok(crate::ops::helpers::git_push_cmd(path, &["-q".to_string(), remote.to_string(), refspec.to_string()]).stderr(Stdio::null()))
    }
    fn rebase(&self, branch: &str, onto: &str, path: &Path, name: &str) -> Result<(), String> {
        let (rc, why) = self.answer(Op::Rebase, &[branch, onto, &path.display().to_string(), name]);
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
        let ans = ans.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
        if ans.is_empty() {
            format!("spira: land {id}")
        } else {
            ans
        }
    }
    fn sort_rows(&self, path: &Path, base: &str, prio_json: &str, rows: &str) -> Vec<(String, String)> {
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
        let express_label = std::env::var("SPIRA_EXPRESS_LABEL").unwrap_or_else(|_| "express".into());
        let (ranked, warning) = crate::ops::helpers::sort_rows(&with_trans, prio_json, &express_label);
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
    /// `spira-lc returned` — the caller verb that replaced lc.sh's `lc_returned` (sp-arpjt).
    /// Best-effort and silent, as the seam call was; the machine reads its own switch.
    fn lc_returned(&self, id: &str, reason: &str) {
        let _ = Command::new("spira-lc")
            .args(["returned", id, reason])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    fn format_batch(&self, wt: &Path, base: &str, name: &str) {
        self.call(Op::FormatBatch, &[&wt.display().to_string(), base, name], false);
    }
    fn base_conflict(&self, path: &Path, base: &str, tip: &str) -> bool {
        self.call(Op::BaseConflict, &[&path.display().to_string(), base, tip], false).0 == 0
    }
    fn pf_gate(&self, branch: &str, name: &str, stamp: &str, wall_secs: u64) -> (i32, String) {
        self.answer(Op::PfGate, &[branch, name, stamp, &wall_secs.to_string()])
    }
    fn create_bug(&self, actor: &str, title: &str, priority: &str, labels: &str, body: &str) -> Option<String> {
        let (_, ans) = self.answer(Op::CreateBug, &[actor, title, priority, labels, body]);
        Some(ans.trim().to_string()).filter(|id| !id.is_empty())
    }
}

// -------------------------------------------------------------------------------- scripts

pub struct RealScripts {
    pub home: PathBuf,
}

impl Scripts for RealScripts {
    fn gate(&self, branch: &str, repo: &str, bead: &str, suites: &str) -> (i32, String) {
        run_combined(
            Command::new("gate.sh")
                .arg(branch)
                .arg(repo)
                .env("SPIRA_GATE_BEAD", bead)
                .env("SPIRA_GATE_SUITES", suites),
        )
    }
    fn judgement_ci(&self, bin: &Path, s: &Settings, repo: &str, suites: &str, members: &str, evidence: &str) -> RunOut {
        let (rc, out) = run_combined(
            Command::new(bin)
                .arg("judgement-ci")
                .arg(repo)
                .args(["--suites", suites, "--members", members, "--evidence", evidence])
                .arg("--home")
                .arg(&s.home)
                .arg("--run")
                .arg(&s.run)
                .arg("--db")
                .arg(&s.db),
        );
        RunOut { rc, out, err: String::new() }
    }
    fn observe_flake(&self, suite: &str, sha: &str) {
        let _ = ok(Command::new("testenv").args(["suites", "observe-flake", suite, sha]).stdout(Stdio::null()).stderr(Stdio::null()));
    }
    fn mail_operator(&self, subject: &str, body: &str) {
        let Ok(mut child) = Command::new("mail")
            .args(["send", "operator", "--from", "Spira Queue <queue@spira>", "--subject", subject])
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
    fn batcher_cut(&self, bin: &Path, repo: &str, wait_zero: bool, lc_off: bool) -> i32 {
        let mut c = Command::new(bin);
        c.arg("cut").arg(repo).arg("--home").arg(&self.home);
        if wait_zero {
            c.env("SPIRA_QUEUE_BATCH_WAIT", "0");
        }
        lifecycle_env(&mut c, lc_off);
        c.stdin(Stdio::null()).status().ok().and_then(|s| s.code()).unwrap_or(127)
    }
    fn czar_fence(&self, class: &str) -> bool {
        ok(Command::new("czar-fence.sh").arg(class))
    }
    fn release(&self, bin: &Path, args: &[String], db: &str) -> RunOut {
        match Command::new(bin).args(args).env("SPIRA_DB", db).stdin(Stdio::null()).output() {
            Ok(o) => RunOut {
                rc: o.status.code().unwrap_or(127),
                out: String::from_utf8_lossy(&o.stdout).to_string(),
                err: String::from_utf8_lossy(&o.stderr).to_string(),
            },
            Err(e) => RunOut { rc: 127, out: String::new(), err: format!("cannot run release: {e}") },
        }
    }
}

// ---------------------------------------------------------------------------------- forge

pub struct RealForge;

impl Forge for RealForge {
    fn pr_create(&self, forge: &Path, repo: &Path, head: &str, base: &str, title: &str, body: &str) -> Option<String> {
        let mut child = Command::new(forge)
            .arg("pr-create")
            .arg(repo)
            .arg(head)
            .arg(base)
            .arg(title)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(body.as_bytes());
        }
        let o = child.wait_with_output().ok()?;
        if !o.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
    }
    fn pr_close(&self, forge: &Path, repo: &Path, pr: &str) {
        let _ = ok(Command::new(forge).arg("pr-close").arg(repo).arg(pr).stderr(Stdio::null()));
    }
    fn pr_comment(&self, forge: &Path, repo: &Path, pr: &str, line: &str) {
        let _ = ok(Command::new(forge).arg("pr-comment").arg(repo).arg(pr).arg(line).stderr(Stdio::null()));
    }
    fn branch_protect(&self, forge: &Path, repo: &Path, branch: &str) -> bool {
        ok(Command::new(forge).arg("branch-protect").arg(repo).arg(branch))
    }
    fn pr_state(&self, forge: &Path, repo: &Path, pr: &str) -> Option<String> {
        stdout_of(Command::new(forge).arg("pr-state").arg(repo).arg(pr)).map(|s| s.trim().to_string())
    }
    fn check_status(&self, forge: &Path, repo: &Path, pr: &str, branch: &str) -> Option<String> {
        stdout_of(Command::new(forge).arg("check-status").arg(repo).arg(pr).arg(branch))
    }
    fn run_id(&self, forge: &Path, repo: &Path, branch: &str) -> Option<String> {
        stdout_of(Command::new(forge).arg("run-id").arg(repo).arg(branch)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }
    fn run_metadata(&self, forge: &Path, repo: &Path, run: &str) -> String {
        stdout_of(Command::new(forge).arg("run-metadata").arg(repo).arg(run)).unwrap_or_default()
    }
    fn run_cancel(&self, forge: &Path, repo: &Path, run: &str) {
        let _ = ok(Command::new(forge).arg("run-cancel").arg(repo).arg(run).stderr(Stdio::null()));
    }
    fn workflow_rerun(&self, forge: &Path, repo: &Path, run: &str) {
        let _ = ok(Command::new(forge).arg("workflow-rerun").arg(repo).arg(run));
    }
}

// ------------------------------------------------------------------------------- spira-lc

pub struct RealLc {
    pub bin: Option<PathBuf>,
}

impl RealLc {
    fn run(&self, args: &[&str]) -> (i32, String) {
        let Some(bin) = &self.bin else { return (2, "no spira-lc program".into()) };
        let timeout = std::env::var("SPIRA_LC_TIMEOUT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "30".into());
        run_combined(Command::new("timeout").arg(timeout).arg(bin).args(args))
    }
    fn stdout(&self, args: &[&str]) -> Result<String, String> {
        let bin = self.bin.as_ref().ok_or("no spira-lc program")?;
        let timeout = std::env::var("SPIRA_LC_TIMEOUT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "30".into());
        let o = Command::new("timeout").arg(timeout).arg(bin).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(format!("spira-lc {} exited {:?}", args.first().unwrap_or(&""), o.status.code()));
        }
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    }
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
    fn batch_state(&self, batch_id: &str) -> Option<(String, String)> {
        let out = self.stdout(&["show-batch", batch_id]).ok()?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        Some((json_str(&v, "state")?, json_str(&v, "version")?))
    }
    fn create_bead(&self, id: &str) {
        let _ = self.run(&["create-bead", id]);
    }
    fn cut(&self, batch_id: &str, repo: &str, head: &str, base: &str, members: &str, actor: &str) -> Result<String, (i32, String)> {
        let (rc, out) = self.run(&["cut", batch_id, "--repo", repo, "--head", head, "--base", base, "--members", members, "--actor", actor]);
        if rc != 0 {
            return Err((rc, out.trim().to_string()));
        }
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or(serde_json::Value::Null);
        Ok(json_str(&v, "version").unwrap_or_default())
    }
    fn abandon_batch(&self, batch_id: &str, state: &str, version: &str, actor: &str, reason: &str) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&["abandon-batch", batch_id, "--expect", state, "--version", version, "--actor", actor, "--reason", reason]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn eject_member(&self, batch_id: &str, bead: &str, state: &str, version: &str, actor: &str, reason: &str) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&["eject-member", batch_id, "--bead-id", bead, "--expect", state, "--version", version, "--actor", actor, "--reason", reason]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn batch_event(&self, batch_id: &str, state: &str, version: &str, actor: &str, kind: &str) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&["event", "batch", batch_id, "--expect", state, "--version", version, "--actor", actor, "--kind", kind]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn land_batch(&self, batch_id: &str, version: &str, actor: &str, sha: &str) -> Result<(), (i32, String)> {
        let (rc, out) = self.run(&["land", batch_id, "--expect", "GREEN", "--version", version, "--actor", actor, "--sha", sha]);
        if rc == 0 {
            Ok(())
        } else {
            Err((rc, out.trim().to_string()))
        }
    }
    fn probe(&self) -> Result<(), String> {
        let out = self.stdout(&["list", "--state", "IN_DELIVERY"])?;
        serde_json::from_str::<serde_json::Value>(&out).map(|_| ()).map_err(|e| format!("spira-lc list: {e}"))
    }
    fn in_delivery(&self) -> Result<Vec<LcBeadRow>, String> {
        let out = self.stdout(&["list", "--state", "IN_DELIVERY"])?;
        serde_json::from_str(&out).map_err(|e| format!("spira-lc list: {e}"))
    }
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
    fn lifecycle_enforce(&self, toml: Option<&Path>) -> bool {
        let Some(p) = toml.filter(|p| p.is_file()) else { return false };
        spira_config::load(p).ok().and_then(|d| d.spira).and_then(|s| s.lifecycle_enforce).unwrap_or(false)
    }
    fn set_legacy_map_row(&self, map: &Path, name: &str, land: &str, base: &str) -> Result<(), String> {
        spira_config::legacy_map::set_row_in_file(map, name, land, base)
    }
}

// ------------------------------------------------------------------ clock, env and stdio

pub struct SysClock;

impl Clock for SysClock {
    fn now(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
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
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", secs / 3600, (secs % 3600) / 60, secs % 60)
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
        std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
        Ok(s)
    }
    fn read_file(&self, p: &Path) -> Result<String, String> {
        fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
    }
}

/// stdout/stderr, flushed per write so interleaving with child processes stays in order.
pub struct StdEmit {
    _lock: RefCell<()>,
}

impl StdEmit {
    pub fn new() -> StdEmit {
        StdEmit { _lock: RefCell::new(()) }
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
    use super::*;

    #[test]
    fn utc_stamp_matches_date() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_790_000_000), "20260921T141320Z");
        assert_eq!(utc_stamp(951_782_400), "20000229T000000Z");
    }

    #[test]
    fn bd_json_skips_a_preamble_and_accepts_one_object() {
        let rows = parse_bd_json("warning: x\n[{\"id\":\"sp-a\",\"status\":\"closed\",\"labels\":[\"l\"]}]").unwrap();
        assert_eq!(rows[0].id, "sp-a");
        let one = parse_bd_json("{\"id\":\"sp-b\"}").unwrap();
        assert_eq!(one[0].id, "sp-b");
        assert!(parse_bd_json("nothing").is_err());
    }

    /// A stand-in lib.sh with the functions the seam calls, so RealLib's whole path —
    /// fixed script, stdin values, answer mark, record parse — runs for real.
    /// `spira_home_repo`/`spira_repos`/`repo_root`/`repo_land`/`repo_field` (family U) are
    /// NOT faked here (sp-k6lku, "wave 4.13"): they are no longer a bash seam call at all,
    /// so a test exercising them sets `SPIRA_REPO_MAP`/`SPIRA_HOME_REPO` and writes a real
    /// map file at that path instead — the same fixture `spira_config::repos` itself reads,
    /// not a stand-in for it.
    fn stub_home() -> testkit::TempDir {
        let d = crate::testutil::tmpdir("reallib");
        fs::write(
            d.join("lib.sh"),
            r#"SPIRA_RUN=/run/x; LANDSTATE=/run/x/landstate; SPIRA_RELEASES=; SPIRA_DB=/db
git() { printf 'origin\nupstream\n'; }
land_subject() { echo "noise on stdout"; printf 'spira: land %s — T' "$1"; }
rebase_branch() { REBASE_FAILURE=conflict; return 1; }
"#,
        )
        .unwrap();
        d
    }

    /// Sets `SPIRA_REPO_MAP` (and `SPIRA_HOME_REPO` when given) for the duration of `f`,
    /// restoring whatever was there before — the real-registry counterpart of `stub_home`'s
    /// bash fakes, for the family-U/W fields only `spira_config::repos` resolves now.
    fn with_repo_map<R>(map: &Path, home_repo: Option<&str>, f: impl FnOnce() -> R) -> R {
        let prev_map = std::env::var("SPIRA_REPO_MAP").ok();
        let prev_home_repo = std::env::var("SPIRA_HOME_REPO").ok();
        std::env::set_var("SPIRA_REPO_MAP", map);
        match home_repo {
            Some(h) => std::env::set_var("SPIRA_HOME_REPO", h),
            None => std::env::remove_var("SPIRA_HOME_REPO"),
        }
        let out = f();
        match prev_map {
            Some(v) => std::env::set_var("SPIRA_REPO_MAP", v),
            None => std::env::remove_var("SPIRA_REPO_MAP"),
        }
        match prev_home_repo {
            Some(v) => std::env::set_var("SPIRA_HOME_REPO", v),
            None => std::env::remove_var("SPIRA_HOME_REPO"),
        }
        out
    }

    #[test]
    fn context_seam_carries_the_verdict_thresholds_with_the_per_repo_override() {
        let _serial = crate::testutil::serial();
        let home = stub_home();
        let lib_sh = fs::read_to_string(home.join("lib.sh")).unwrap();
        fs::write(
            home.join("lib.sh"),
            format!("{lib_sh}SPIRA_QUEUE_CI_MAXSEC=100; SPIRA_QUEUE_CI_MAXSEC_SPIRA=77; SPIRA_QUEUE_CI_IDLE_SEC=12; SPIRA_QUEUE_INFRA_RETRIES=4; SPIRA_QUEUE_LOCK_WAIT=9; SPIRA_QUEUE_LOCK_STARVE_MAX=3; SPIRA_INCIDENT_PRIORITY=3\n"),
        )
        .unwrap();
        let lib = RealLib { home: home.to_path_buf() };
        let (s, _) = lib.context(Some("spira")).unwrap();
        assert_eq!(
            s.verdict,
            VerdictSettings { ci_maxsec: 77, ci_idle_sec: 12, infra_retries: 4, lock_wait: 9, lock_starve_max: 3, incident_priority: "3".into() }
        );
        // another repository gets the global value; a name that is not an identifier none
        let (s, _) = lib.context(Some("svc")).unwrap();
        assert_eq!(s.verdict.ci_maxsec, 100);
        let (s, _) = lib.context(Some("we.ird")).unwrap();
        assert_eq!(s.verdict.ci_maxsec, 100);
    }

    #[test]
    fn create_bug_seam_files_through_bdq_with_the_body_in_a_file() {
        let _serial = crate::testutil::serial();
        let home = stub_home();
        let rec = home.join("bdq-args");
        let lib_sh = fs::read_to_string(home.join("lib.sh")).unwrap();
        fs::write(
            home.join("lib.sh"),
            format!(
                "{lib_sh}bdq() {{ {{ printf '%s|' \"$BEADS_ACTOR\" \"$@\"; echo; while [ $# -gt 0 ]; do [ \"$1\" = --body-file ] && cat \"$2\"; shift; done; }} > {}; printf ' sp-new9 \\n'; }}\n",
                rec.display()
            ),
        )
        .unwrap();
        let lib = RealLib { home: home.to_path_buf() };
        let id = lib.create_bug("queue.sh", "publish PR 7 red for spira: a.sh", "3", "spira,plan,repo:spira", "line one\n$(two) `x`");
        assert_eq!(id.as_deref(), Some("sp-new9"));
        let seen = fs::read_to_string(&rec).unwrap();
        assert!(seen.starts_with("queue.sh|create|publish PR 7 red for spira: a.sh|--type|bug|--priority|3|--labels|spira,plan,repo:spira|--body-file|"), "{seen}");
        assert!(seen.ends_with("|--silent|\nline one\n$(two) `x`"), "{seen}");
        fs::write(home.join("lib.sh"), format!("{lib_sh}bdq() {{ return 1; }}\n")).unwrap();
        assert_eq!(lib.create_bug("a", "t", "1", "l", "b"), None);
    }

    #[test]
    fn context_seam_round_trips_through_bash() {
        let _serial = crate::testutil::serial();
        let home = stub_home();
        let lib = RealLib { home: home.to_path_buf() };

        // spira_home_repo/repo_root/repo_land/repo_field (family U) and spira_landref/
        // spira_publish_forge (family W) are all resolved in-process now (family U since
        // sp-k6lku "wave 4.13"; family W since sp-o88bx "wave 4.12"), through
        // spira_config::repos against THIS process's real environment and a real git
        // checkout — not through the stubbed lib.sh above, which no longer answers any of
        // them at all. A real registry file plus a real checkout stand in for them.
        let repo = home.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        git(&["init", "-q", "-b", "trunk"]);
        fs::write(repo.join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "x"]);
        git(&["branch", "-q", "local/main"]);
        // `remotes` (git_remotes, a real `git -C <repo> remote` now — sp-k6lku, "wave
        // 4.13" — not the stubbed lib.sh's fake `git()` shell function CONTEXT used to
        // shell out to) needs real remotes on this real checkout.
        git(&["remote", "add", "origin", "."]);
        git(&["remote", "add", "upstream", "."]);
        let map = home.join("repomap-fixture");
        fs::write(&map, format!("spira | {} | queue.local | local/main |  | \n", repo.display())).unwrap();

        let (s, r) = with_repo_map(&map, Some("spira"), || lib.context(Some("spira")).unwrap());

        assert_eq!(s.run, PathBuf::from("/run/x"));
        assert_eq!(s.queue_dir, PathBuf::from("/run/x/queue"));
        assert_eq!(s.releases, None);
        assert_eq!(s.submitted_label, "spira-submitted"); // literal-ok: conf.sh's own default
        assert_eq!(s.transition_maxsec, 1800);
        assert_eq!(s.home_repo, "spira");
        assert_eq!(r.path, Some(repo.clone()));
        assert_eq!(r.mode, LandMode::QueueLocal);
        assert_eq!(r.map_land, "queue.local");
        assert_eq!(r.map_base, "local/main");
        assert_eq!(r.landref.as_deref(), Some("local/main"));
        assert_eq!(r.publish, Some(("origin".to_string(), "main".to_string())));
        assert_eq!(r.remotes, vec!["origin", "upstream"]);
        let (_, other) = with_repo_map(&map, Some("spira"), || lib.context(Some("nope")).unwrap());
        assert_eq!(other.path, None);
    }

    #[test]
    fn repos_seam_lists_home_first_once_each() {
        let _serial = crate::testutil::serial();
        let home = stub_home();
        let lib = RealLib { home: home.to_path_buf() };
        let map = home.join("repomap-fixture");
        // Home repo first always, a repeated or blank row dropped, and a row sharing the
        // home repo's own name not duplicated — spira_config::repos::Registry::all()'s own
        // contract (spira_repos' bash original: `repo_names | grep -vx -- "$home"`).
        fs::write(&map, "svc | /nonexistent/svc | push |  |  | \nspira | /nonexistent/spira | push |  |  | \n\n").unwrap();
        let names = with_repo_map(&map, Some("spira"), || lib.repos().unwrap());
        assert_eq!(names, vec!["spira".to_string(), "svc".to_string()]);
    }

    #[test]
    fn repos_seam_is_never_empty_even_with_no_map() {
        // spira_repos (sp-k6lku, "wave 4.13") in-process via Registry::all(), which always
        // puts the home repo first: there is no bash seam left to fail or answer nothing,
        // so (unlike the retired seam call) this can no longer error.
        let _serial = crate::testutil::serial();
        let home = stub_home();
        let lib = RealLib { home: home.to_path_buf() };
        let prev_map = std::env::var("SPIRA_REPO_MAP").ok();
        let prev_home_repo = std::env::var("SPIRA_HOME_REPO").ok();
        std::env::remove_var("SPIRA_REPO_MAP");
        std::env::set_var("SPIRA_HOME_REPO", "spira");
        let names = lib.repos();
        match prev_map {
            Some(v) => std::env::set_var("SPIRA_REPO_MAP", v),
            None => std::env::remove_var("SPIRA_REPO_MAP"),
        }
        match prev_home_repo {
            Some(v) => std::env::set_var("SPIRA_HOME_REPO", v),
            None => std::env::remove_var("SPIRA_HOME_REPO"),
        }
        assert_eq!(names.unwrap(), vec!["spira".to_string()]);
    }

    #[test]
    fn batcher_cut_carries_the_lifecycle_switch() {
        // verdict runs in process now (DESIGN-verdict.md) and batch.sh is deleted
        // (sp-uwhx0) — batcher_cut is the only remaining Scripts method that both spawns a
        // child and takes lc_off, so it is the only one left to prove the switch on.
        let _serial = crate::testutil::serial();
        let d = crate::testutil::tmpdir("lc-env");
        let rec = d.join("seen");
        let bin = d.join("batcher");
        testkit::write_exe(&bin, &format!("#!/bin/sh\nprintf 'batcher %s %s\\n' \"$2\" \"${{SPIRA_LIFECYCLE_ENFORCE:-unset}}\" >> {}\n", rec.display()));
        let s = RealScripts { home: d.to_path_buf() };
        s.batcher_cut(&bin, "spira", false, true);
        s.batcher_cut(&bin, "svc", false, false);
        let seen = fs::read_to_string(&rec).unwrap();
        let lines: Vec<&str> = seen.lines().collect();
        assert_eq!(lines, ["batcher spira 0", "batcher svc 1"]);
    }

    #[test]
    fn batcher_cut_passes_home_explicitly() {
        let _serial = crate::testutil::serial();
        let d = crate::testutil::tmpdir("cut-home");
        let rec = d.join("seen");
        let bin = d.join("batcher");
        testkit::write_exe(&bin, &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n", rec.display()));
        let home = d.join("h");
        let s = RealScripts { home: home.clone() };
        s.batcher_cut(&bin, "spira", false, false);
        assert_eq!(fs::read_to_string(&rec).unwrap().trim(), format!("cut spira --home {}", home.display()));
    }

    #[test]
    fn answer_seams_ignore_log_lines_and_carry_failures() {
        let _serial = crate::testutil::serial();
        let home = stub_home();
        let lib = RealLib { home: home.to_path_buf() };
        assert_eq!(lib.land_subject("sp-a"), "spira: land sp-a — T");
        assert_eq!(lib.rebase("spira/sp-a", "origin/main", Path::new("/repo"), "spira"), Err("conflict".to_string()));
        // sort_rows runs in process now (sp-hwjsq, "wave 4.32"): "/repo" does not exist, so
        // the real `git diff` behind queue_is_suite_transition fails closed (not a
        // transition) for both rows, same as the bash stub it replaced always answered.
        assert_eq!(
            lib.sort_rows(Path::new("/repo"), "b0", "[]", "sp-a ta 6\nsp-b tb 5\n"),
            vec![("sp-b".to_string(), "tb".to_string()), ("sp-a".to_string(), "ta".to_string())]
        );
    }

    // -------------------------------------------------------------------------- sp-hwjsq
    // notify/divergence/push/sort_rows/cancel_runs run in process now ("wave 4.32"); these
    // prove RealLib's own implementation against real git, a fake `mail` and a fake forge
    // binary — not the TestLib fakes ops.rs tests use elsewhere.

    #[test]
    fn sort_rows_detects_a_real_suite_state_transition() {
        let _serial = crate::testutil::serial();
        let repo = crate::testutil::tmpdir("sort-trans-repo");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "a@b.c"]);
        git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("seed"), "x\n").unwrap();
        git(&repo, &["add", "seed"]);
        git(&repo, &["commit", "-q", "-m", "seed"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);

        fs::create_dir_all(repo.join("spira")).unwrap();
        fs::write(repo.join("spira/suite-state"), "state\n").unwrap();
        git(&repo, &["add", "spira/suite-state"]);
        git(&repo, &["commit", "-qm", "transition"]);
        let trans_tip = git(&repo, &["rev-parse", "HEAD"]);

        let home = minimal_home(":");
        let lib = RealLib { home: home.to_path_buf() };
        // sp-t carries the LATER epoch (2 vs. 1): the transition tiebreak must still put it
        // first, ahead of certification age — sp-ihxa0's "tiebreak only within a priority"
        // rule, not a rank of its own that could outrank arrival order the other way.
        let out = lib.sort_rows(&repo, &base, "[]", &format!("sp-t {trans_tip} 2\nsp-p {base} 1\n"));
        // sp-t (the transition) ranks first within the same (default) priority, despite
        // its later epoch.
        assert_eq!(out, vec![("sp-t".to_string(), trans_tip), ("sp-p".to_string(), base)]);
    }

    #[test]
    fn divergence_alarms_once_per_foreign_tip_and_clears_when_healthy() {
        let _serial = crate::testutil::serial();
        let repo = crate::testutil::tmpdir("div-repo");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "a@b.c"]);
        git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("f"), "base\n").unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let local = git(&repo, &["rev-parse", "HEAD"]);
        fs::write(repo.join("f"), "foreign\n").unwrap();
        git(&repo, &["commit", "-qam", "foreign push"]);
        let foreign = git(&repo, &["rev-parse", "HEAD"]);

        let run = crate::testutil::tmpdir("div-run");
        let queue_dir = run.join("queue");
        fs::create_dir_all(&queue_dir).unwrap();
        let mail_log = run.join("mail-log");
        let bindir = crate::testutil::tmpdir("div-bin");
        testkit::write_exe(bindir.join("mail"), &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncat >> {}\n", mail_log.display(), mail_log.display()));
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut p = std::ffi::OsString::from(bindir.path());
        p.push(":");
        p.push(&old_path);
        std::env::set_var("PATH", &p);

        let home = minimal_home(":");
        let lib = RealLib { home: home.to_path_buf() };

        // Local ahead of forge (healthy): true, no alarm, no statefile.
        assert_eq!(lib.divergence(&queue_dir, "fixq", &repo, &local, &foreign), Divergence::Ancestor);
        assert!(!queue_dir.join("fixq/divergence-alarmed").exists());

        // Foreign commit on top of local: false, alarmed once.
        assert!(matches!(lib.divergence(&queue_dir, "fixq", &repo, &foreign, &local), Divergence::Diverged(_)));
        let alarmed_after_first = fs::read_to_string(queue_dir.join("fixq/divergence-alarmed")).unwrap();
        assert_eq!(alarmed_after_first.trim(), foreign);
        let mail_after_first = fs::read_to_string(&mail_log).unwrap();
        assert!(mail_after_first.contains("fixq"), "{mail_after_first}");
        assert!(mail_after_first.contains("foreign push"), "{mail_after_first}");

        // Same foreign tip again: still false, but silent (no second mail).
        fs::write(&mail_log, "").unwrap();
        assert!(matches!(lib.divergence(&queue_dir, "fixq", &repo, &foreign, &local), Divergence::Diverged(_)));
        assert_eq!(fs::read_to_string(&mail_log).unwrap(), "", "a repeated call against the SAME foreign tip must not re-alarm");

        // Healthy again: the marker clears.
        assert_eq!(lib.divergence(&queue_dir, "fixq", &repo, &local, &foreign), Divergence::Ancestor);
        assert!(!queue_dir.join("fixq/divergence-alarmed").exists());

        std::env::set_var("PATH", &old_path);
    }

    #[test]
    fn divergence_cannot_check_without_a_queue_dir_or_a_resolvable_repo() {
        let _serial = crate::testutil::serial();
        let home = minimal_home(":");
        let lib = RealLib { home: home.to_path_buf() };
        let r = lib.divergence(Path::new(""), "fixq", Path::new("/repo"), "a", "b");
        assert!(matches!(r, Divergence::CannotCheck(_)), "{r:?}");
        let qd = crate::testutil::tmpdir("div-cc");
        let r = lib.divergence(&qd, "fixq", Path::new("/nonexistent-repo"), "a", "b");
        assert!(matches!(r, Divergence::CannotCheck(_)), "{r:?}");
    }

    #[test]
    fn push_adds_app_credentials_only_when_configured() {
        let _serial = crate::testutil::serial();
        let bindir = crate::testutil::tmpdir("push-bin");
        let args_file = bindir.join("git-args");
        testkit::write_exe(bindir.join("git"), &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n", args_file.display()));
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut p = std::ffi::OsString::from(bindir.path());
        p.push(":");
        p.push(&old_path);
        std::env::set_var("PATH", &p);

        let home = minimal_home(":");
        let lib = RealLib { home: home.to_path_buf() };

        std::env::remove_var("SPIRA_GH_APP_ID");
        std::env::remove_var("SPIRA_GH_APP_INSTALLATION_ID");
        assert!(lib.push(Path::new("/repo"), "origin", "main"));
        let plain = fs::read_to_string(&args_file).unwrap();
        assert!(!plain.contains("credential.helper"), "{plain}");
        assert!(plain.contains("push"), "{plain}");

        fs::remove_file(&args_file).ok();
        std::env::set_var("SPIRA_GH_APP_ID", "1");
        std::env::set_var("SPIRA_GH_APP_INSTALLATION_ID", "2");
        assert!(lib.push(Path::new("/repo"), "origin", "main"));
        let with_creds = fs::read_to_string(&args_file).unwrap();
        assert!(with_creds.contains("credential.helper"), "{with_creds}");
        assert!(with_creds.contains("insteadOf=git@github.com:"), "{with_creds}");

        std::env::remove_var("SPIRA_GH_APP_ID");
        std::env::remove_var("SPIRA_GH_APP_INSTALLATION_ID");
        std::env::set_var("PATH", &old_path);
    }

    #[test]
    fn cancel_runs_logs_each_attempt_and_warns_loudly_on_a_failed_cancel() {
        let _serial = crate::testutil::serial();
        let run = crate::testutil::tmpdir("cancel-run");
        std::env::set_var("SPIRA_RUN", run.path());
        let bindir = crate::testutil::tmpdir("cancel-bin");
        let forge = bindir.join("forge-fake.sh");
        testkit::write_exe(
            &forge,
            r#"#!/bin/sh
case "$1" in
    runs-for-branch) printf '101 queued\n202 in_progress\n' ;;
    run-cancel) [ "$3" = 202 ] && exit 1; exit 0 ;;
esac
"#,
        );
        let home = minimal_home(":");
        let lib = RealLib { home: home.to_path_buf() };
        lib.cancel_runs(&forge, Path::new("/repo"), "spira/queue/1");
        let log = fs::read_to_string(run.join("landing.log")).unwrap();
        assert!(log.contains("RUN_CANCEL ") && log.contains("run=101"), "{log}");
        assert!(log.contains("RUN_CANCEL_FAILED") && log.contains("run=202"), "{log}");
        std::env::remove_var("SPIRA_RUN");
    }

    #[test]
    fn notify_mails_the_concierge_mailbox_by_default() {
        let _serial = crate::testutil::serial();
        let run = crate::testutil::tmpdir("notify-run");
        let mail_log = run.join("mail-args");
        let bindir = crate::testutil::tmpdir("notify-bin");
        testkit::write_exe(&bindir.join("mail"), &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n", mail_log.display()));
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut p = std::ffi::OsString::from(bindir.path());
        p.push(":");
        p.push(&old_path);
        std::env::set_var("PATH", &p);
        std::env::remove_var("SPIRA_MAIL_SESSION_MAILBOX");

        let home = minimal_home(":");
        let lib = RealLib { home: home.to_path_buf() };
        lib.notify("fixq", "test subject", "test body");
        let seen = fs::read_to_string(&mail_log).unwrap();
        assert!(seen.contains("send concierge"), "{seen}");
        assert!(seen.contains("Merge queue: fixq test subject"), "{seen}");

        std::env::set_var("PATH", &old_path);
    }

    // ---------------------------------------------------------------------------- sp-uwhx0
    // format_batch/base_conflict/pf_gate used to source batch.sh (deleted); these run the
    // inlined bodies against a real git repo and real bash, not the FLib fakes ops.rs tests
    // use elsewhere — parity proof that the inlining kept batch.sh's own behaviour.

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A bare-bones lib.sh: none of format_batch/base_conflict/pf_gate call any lib.sh
    /// function except `repo_format` and `log` (FormatBatch), and PfGate calls `gate.sh` by
    /// name — supplied by the caller's PATH, not lib.sh.
    fn minimal_home(repo_format_body: &str) -> testkit::TempDir {
        let d = crate::testutil::tmpdir("batchfns");
        fs::write(d.join("lib.sh"), format!("log() {{ :; }}\nrepo_format() {{ {repo_format_body}\n}}\n")).unwrap();
        fs::write(d.join("lc.sh"), "").unwrap();
        d
    }

    #[test]
    fn base_conflict_is_true_only_when_the_tip_conflicts_with_base_itself() {
        let _serial = crate::testutil::serial();
        let run = crate::testutil::tmpdir("bc-run");
        fs::create_dir_all(run.join("worktree")).unwrap();
        std::env::set_var("SPIRA_RUN", run.path());
        let repo = crate::testutil::tmpdir("bc-repo");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "a@b.c"]);
        git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("f"), "base\n").unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);

        // A tip that edits the same line the new base also edits: conflicts with base itself.
        git(&repo, &["checkout", "-qb", "tipA"]);
        fs::write(repo.join("f"), "tipA\n").unwrap();
        git(&repo, &["commit", "-qam", "tipA"]);
        let tip_conflicts = git(&repo, &["rev-parse", "HEAD"]);

        git(&repo, &["checkout", "-q", &base]);
        git(&repo, &["checkout", "-qb", "advance-base"]);
        fs::write(repo.join("f"), "advanced\n").unwrap();
        git(&repo, &["commit", "-qam", "advance"]);
        let new_base = git(&repo, &["rev-parse", "HEAD"]);

        let home = minimal_home(":");
        let lib = RealLib { home: home.to_path_buf() };
        assert!(lib.base_conflict(&repo, &new_base, &tip_conflicts), "same-line edits on both sides must conflict with base");

        // A tip that touches an unrelated file: merges cleanly with base (no base conflict).
        git(&repo, &["checkout", "-q", &base]);
        git(&repo, &["checkout", "-qb", "tipB"]);
        fs::write(repo.join("g"), "tipB\n").unwrap();
        git(&repo, &["add", "g"]);
        git(&repo, &["commit", "-qm", "tipB"]);
        let tip_clean = git(&repo, &["rev-parse", "HEAD"]);
        assert!(!lib.base_conflict(&repo, &new_base, &tip_clean), "a disjoint-file tip must not conflict with base");

        // The scratch worktree it used is cleaned up either way.
        assert!(!run.join("worktree").read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".batch-ck-")));
        std::env::remove_var("SPIRA_RUN");
    }

    #[test]
    fn format_batch_commits_only_when_the_formatter_changes_something() {
        let _serial = crate::testutil::serial();
        let repo = crate::testutil::tmpdir("fb-repo");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "a@b.c"]);
        git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("f.txt"), "unformatted\n").unwrap();
        git(&repo, &["add", "f.txt"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        fs::write(repo.join("f.txt"), "member change\n").unwrap();
        git(&repo, &["commit", "-qam", "member"]);
        let before_head = git(&repo, &["rev-parse", "HEAD"]);

        // repo_format prints a shell command that rewrites the tracked file.
        let home = minimal_home("printf 'echo formatted > f.txt'");
        let lib = RealLib { home: home.to_path_buf() };
        lib.format_batch(&repo, &base, "spira");
        let after_head = git(&repo, &["rev-parse", "HEAD"]);
        assert_ne!(before_head, after_head, "a formatter that changes tracked content must produce a new commit");
        assert_eq!(fs::read_to_string(repo.join("f.txt")).unwrap(), "formatted\n");
        let msg = git(&repo, &["log", "-1", "--format=%s"]);
        assert_eq!(msg, "spira: format batch");
        assert!(git(&repo, &["status", "--porcelain"]).is_empty(), "the tree must be clean after the commit");

        // A formatter that changes nothing: no new commit.
        let home2 = minimal_home("printf 'true'");
        let lib2 = RealLib { home: home2.to_path_buf() };
        lib2.format_batch(&repo, &base, "spira");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), after_head, "a no-op formatter must not add a commit");

        // repo_format answering nothing: format_batch does not touch the tree at all.
        let home3 = minimal_home("printf ''");
        let lib3 = RealLib { home: home3.to_path_buf() };
        fs::write(repo.join("f.txt"), "dirty\n").unwrap();
        lib3.format_batch(&repo, &base, "spira");
        assert_eq!(fs::read_to_string(repo.join("f.txt")).unwrap(), "dirty\n", "no configured formatter must leave the working tree untouched");
        git(&repo, &["checkout", "-q", "--", "."]);
    }

    #[test]
    fn pf_gate_returns_gates_output_and_rc_and_enforces_its_own_wall() {
        let _serial = crate::testutil::serial();
        let bindir = crate::testutil::tmpdir("pfgate-bin");
        testkit::write_exe(&bindir.join("gate.sh"), "#!/bin/sh\nprintf 'gate ran for %s/%s bead=%s\\n' \"$1\" \"$2\" \"$SPIRA_GATE_BEAD\"\nexit 3\n");
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut p = std::ffi::OsString::from(bindir.path());
        p.push(":");
        p.push(&old_path);
        std::env::set_var("PATH", &p);
        let home = minimal_home(":");
        let lib = RealLib { home: home.to_path_buf() };
        let (rc, out) = lib.pf_gate("spira/queue/1", "spira", "1700000000", 10);
        assert_eq!(rc, 3);
        assert_eq!(out.trim(), "gate ran for spira/queue/1/spira bead=batch-1700000000");

        // A gate that outlives the wall is killed — the WHOLE process group, grandchild
        // included (test-batch-preflight.sh's own case: a test container the gate started
        // must not outlive the wall) — and reported as 124, not left running.
        let mark = crate::testutil::tmpdir("pfgate-mark");
        let grandchild_pid = mark.join("grandchild-pid");
        testkit::write_exe(
            &bindir.join("gate.sh"),
            &format!("#!/bin/sh\nsleep 30 & echo $! > {}\nsleep 30\n", grandchild_pid.display()),
        );
        let t0 = std::time::Instant::now();
        let (rc, _) = lib.pf_gate("spira/queue/2", "spira", "1700000001", 1);
        assert_eq!(rc, 124);
        assert!(t0.elapsed().as_secs() < 20, "the wall must cut the gate off well before its own sleep finishes");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let gc = fs::read_to_string(&grandchild_pid).unwrap_or_default();
        let gc = gc.trim();
        assert!(!gc.is_empty(), "the gate's grandchild must have started");
        assert!(
            Command::new("kill").arg("-0").arg(gc).status().map(|s| !s.success()).unwrap_or(true),
            "the whole process group must be killed, grandchild included — pid {gc} still alive"
        );
        std::env::set_var("PATH", &old_path);
    }
}
