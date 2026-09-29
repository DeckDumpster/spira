//! The ports against the host: git, bd, the lib.sh seam, the harness scripts, the forge,
//! spira-lc, the config (spira-config library only), the clock, the environment, stdio.

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::model::{BeadRow, LandMode, LcBeadRow, RangeCommit};
use crate::ports::*;
use crate::seam::{self, Op};

/// Run `cmd` with stdout and stderr interleaved into one capture (the `2>&1` of a
/// `$(…)`), through a temp file both descriptors share.
fn run_combined(cmd: &mut Command) -> (i32, String) {
    let path = std::env::temp_dir().join(format!("queue-out-{}-{}", std::process::id(), unique()));
    let Ok(f) = File::create(&path) else { return (127, String::new()) };
    let Ok(f2) = f.try_clone() else { return (127, String::new()) };
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
    cmd.stdin(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn stdout_of(cmd: &mut Command) -> Option<String> {
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

// ------------------------------------------------------------------------------- lib seam

pub struct RealLib {
    pub home: PathBuf,
}

impl RealLib {
    fn home_str(&self) -> String {
        self.home.display().to_string()
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

impl Lib for RealLib {
    fn context(&self, repo: Option<&str>) -> Result<(Settings, RepoCtx), String> {
        let (rc, ans) = self.answer(Op::Context, &[repo.unwrap_or("")]);
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
            batcher_bin: pb(&g("batcher_bin")),
            lc_bin: pb(&g("lc_bin")),
            submitted_label: g("submitted_label"),
            home_repo: g("home_repo"),
            db: g("db"),
            bd: g("bd"),
            transition_pollsec: n("pollsec", 5),
            transition_maxsec: n("maxsec", 1800),
            preflight_wall_secs: n("preflight", 240),
        };
        let publish = g("publish");
        let publish = publish.trim().split_once(' ').map(|(a, b)| (a.to_string(), b.trim().to_string()));
        let r = RepoCtx {
            name: g("name"),
            path: (g("path_ok") == "0").then(|| PathBuf::from(g("path"))).filter(|p| !p.as_os_str().is_empty()),
            mode: LandMode::parse(&g("mode")).unwrap_or(LandMode::Push),
            landref: Some(g("landref")).filter(|s| !s.is_empty()),
            map_land: g("map_land"),
            map_base: g("map_base"),
            publish,
            remotes: g("remotes").split_whitespace().map(String::from).collect(),
        };
        Ok((s, r))
    }
    fn toml_path(&self) -> Option<PathBuf> {
        let (_, ans) = self.answer(Op::TomlPath, &[]);
        pb(ans.trim())
    }
    fn readback(&self, name: &str) -> (String, String) {
        let (_, ans) = self.answer(Op::Readback, &[name]);
        let mut it = ans.split('\0');
        (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string())
    }
    fn land_mark(&self, id: &str, state: &str, tip: &str, reason: &str) {
        self.call(Op::LandMark, &[id, state, tip, reason], false);
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
    fn gh_issue_closeout(&self, id: &str, sha: &str, repo: &Path) {
        self.call(Op::GhCloseout, &[id, sha, &repo.display().to_string()], false);
    }
    fn comment(&self, id: &str, text: &str) {
        self.call(Op::Comment, &[id, text], false);
    }
    fn notify(&self, repo: &str, subject: &str, body: &str) {
        self.call(Op::Notify, &[repo, subject, body], false);
    }
    fn event(&self, kind: &str, title: &str, detail: &str) {
        self.call(Op::Event, &[kind, title, detail], false);
    }
    fn divergence(&self, repo: &str, path: &Path, forge: &str, local: &str) -> bool {
        self.call(Op::Divergence, &[repo, &path.display().to_string(), forge, local], false).0 == 0
    }
    fn push(&self, path: &Path, remote: &str, refspec: &str) -> bool {
        self.call(Op::Push, &[&path.display().to_string(), remote, refspec], false).0 == 0
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
        let (_, ans) = self.answer(Op::SortRows, &[&path.display().to_string(), base, prio_json, rows]);
        ans.lines().filter_map(|l| l.split_once(' ').map(|(a, b)| (a.to_string(), b.trim().to_string()))).collect()
    }
    fn cancel_runs(&self, forge: &Path, path: &Path, branch: &str) {
        self.call(Op::CancelRuns, &[&forge.display().to_string(), &path.display().to_string(), branch], false);
    }
    fn lc_returned(&self, id: &str, reason: &str) {
        self.call(Op::LcReturned, &[id, reason], false);
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
    fn settle_publish(&self, name: &str, path: &Path) -> i32 {
        self.call(Op::SettlePublish, &[name, &path.display().to_string()], false).0
    }
}

// -------------------------------------------------------------------------------- scripts

pub struct RealScripts {
    pub home: PathBuf,
}

impl Scripts for RealScripts {
    fn gate(&self, branch: &str, repo: &str, bead: &str, suites: &str) -> (i32, String) {
        run_combined(
            Command::new("bash")
                .arg(self.home.join("gate.sh"))
                .arg(branch)
                .arg(repo)
                .env("SPIRA_GATE_BEAD", bead)
                .env("SPIRA_GATE_SUITES", suites),
        )
    }
    fn batch_sweep(&self, repo: &str, wait_zero: bool) -> i32 {
        let mut c = Command::new("bash");
        c.arg(self.home.join("batch.sh")).arg(repo);
        if wait_zero {
            c.env("SPIRA_QUEUE_BATCH_WAIT", "0");
        }
        c.stdin(Stdio::null()).status().ok().and_then(|s| s.code()).unwrap_or(127)
    }
    fn verdict(&self, repo: &str) -> i32 {
        Command::new("bash").arg(self.home.join("verdict.sh")).arg(repo).stdin(Stdio::null()).status().ok().and_then(|s| s.code()).unwrap_or(127)
    }
    fn batcher_cut(&self, bin: &Path, repo: &str, wait_zero: bool) -> i32 {
        let mut c = Command::new(bin);
        c.arg("cut").arg(repo);
        if wait_zero {
            c.env("SPIRA_QUEUE_BATCH_WAIT", "0");
        }
        c.stdin(Stdio::null()).status().ok().and_then(|s| s.code()).unwrap_or(127)
    }
    fn czar_fence(&self, class: &str) -> bool {
        ok(Command::new("bash").arg(self.home.join("czar-fence.sh")).arg(class))
    }
    fn build_tarball(&self, bins: &Path, repo_name: &str, name: &str, out: &Path, head: &str, repo: &Path) -> Option<PathBuf> {
        let o = Command::new("bash")
            .arg(self.home.join("build-tarball.sh"))
            .arg("build")
            .arg("--bin-dir")
            .arg(bins)
            .arg("--repo-name")
            .arg(repo_name)
            .arg("--name")
            .arg(name)
            .arg("--output")
            .arg(out)
            .arg(head)
            .arg(repo)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&o.stdout);
        s.lines().rev().find(|l| !l.trim().is_empty()).map(|l| PathBuf::from(l.trim()))
    }
    fn activate(&self, tarball: &Path, land_local: bool) -> (i32, String) {
        let mut c = Command::new("bash");
        c.arg(self.home.join("activate.sh")).arg(tarball);
        if land_local {
            c.env("SPIRA_ACTIVATE_LAND_LOCAL", "1");
        }
        run_combined(&mut c)
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
}

// ------------------------------------------------------------------------------- spira-lc

pub struct RealLc {
    pub bin: Option<PathBuf>,
}

impl RealLc {
    fn run(&self, args: &[&str]) -> (i32, String) {
        let Some(bin) = &self.bin else { return (2, "SPIRA_LC_BIN not available".into()) };
        let timeout = std::env::var("SPIRA_LC_TIMEOUT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "30".into());
        run_combined(Command::new("timeout").arg(timeout).arg(bin).args(args))
    }
    fn stdout(&self, args: &[&str]) -> Result<String, String> {
        let bin = self.bin.as_ref().ok_or("SPIRA_LC_BIN not available")?;
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
        self.bin.as_deref().map(crate::ops::simple::is_executable).unwrap_or(false)
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
    fn stub_home() -> PathBuf {
        let d = crate::testutil::tmpdir("reallib");
        fs::write(
            d.join("lib.sh"),
            r#"SPIRA_RUN=/run/x; LANDSTATE=/run/x/landstate; SPIRA_RELEASES=; SPIRA_DB=/db
spira_home_repo() { printf spira; }
repo_root() { [ "$1" = spira ] && printf /repo || return 1; }
repo_land() { printf queue.local; }
repo_field() { case "$2" in land) echo queue.local;; base) echo local/main;; esac; }
spira_landref() { printf local/main; }
spira_publish_forge() { printf 'origin main\n'; }
git() { printf 'origin\nupstream\n'; }
land_subject() { echo "noise on stdout"; printf 'spira: land %s — T' "$1"; }
rebase_branch() { REBASE_FAILURE=conflict; return 1; }
queue_sort_rows() { cat >/dev/null; printf '1 000000009 1 0000000005 sp-b tb\n1 000000009 1 0000000006 sp-a ta\n'; }
"#,
        )
        .unwrap();
        fs::write(d.join("lc.sh"), "").unwrap();
        d
    }

    #[test]
    fn context_seam_round_trips_through_bash() {
        let lib = RealLib { home: stub_home() };
        let (s, r) = lib.context(Some("spira")).unwrap();
        assert_eq!(s.run, PathBuf::from("/run/x"));
        assert_eq!(s.queue_dir, PathBuf::from("/run/x/queue"));
        assert_eq!(s.releases, None);
        assert_eq!(s.submitted_label, "spira-submitted"); // literal-ok: conf.sh's own default
        assert_eq!(s.transition_maxsec, 1800);
        assert_eq!(r.path, Some(PathBuf::from("/repo")));
        assert_eq!(r.mode, LandMode::QueueLocal);
        assert_eq!(r.landref.as_deref(), Some("local/main"));
        assert_eq!(r.publish, Some(("origin".to_string(), "main".to_string())));
        assert_eq!(r.remotes, vec!["origin", "upstream"]);
        let (_, other) = lib.context(Some("nope")).unwrap();
        assert_eq!(other.path, None);
    }

    #[test]
    fn answer_seams_ignore_log_lines_and_carry_failures() {
        let lib = RealLib { home: stub_home() };
        assert_eq!(lib.land_subject("sp-a"), "spira: land sp-a — T");
        assert_eq!(lib.rebase("spira/sp-a", "origin/main", Path::new("/repo"), "spira"), Err("conflict".to_string()));
        assert_eq!(
            lib.sort_rows(Path::new("/repo"), "b0", "[]", "sp-a ta 6\nsp-b tb 5\n"),
            vec![("sp-b".to_string(), "tb".to_string()), ("sp-a".to_string(), "ta".to_string())]
        );
    }
}
