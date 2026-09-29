//! Production implementations of the ports: bd, git, the lib.sh seam, the harness
//! programs, /proc and the clock.

use crate::model::{BeadRow, LandMode, Rebase, Recut, RepoRow, Settings};
use crate::ports::{Beads, Clock, Git, Lib, Procs, Tools};
use crate::report::Reporter;
use crate::seam::{self, Op, FIELD};
use crate::util::{command, run_capture, unix_now};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

// ──────────────────────────────────────────────────────────────────────────────
// The lib.sh seam
// ──────────────────────────────────────────────────────────────────────────────

pub struct SeamRunner<'a> {
    pub home: PathBuf,
    pub out: &'a Reporter,
}

impl<'a> SeamRunner<'a> {
    /// Run one seam op; lib.sh log lines are passed through to the pass's stdout and
    /// `progress` lines become movements. Returns (status, answer).
    pub fn call(&self, op: Op, values: &[&str]) -> (i32, String) {
        let home = self.home.to_string_lossy().into_owned();
        let mut all: Vec<&str> = vec![&home];
        all.extend_from_slice(values);
        let bytes = seam::stdin_bytes(op, &all);
        let mut c = command("bash");
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = match c.spawn() {
            Ok(ch) => ch,
            Err(e) => {
                self.out.log(&format!("landing: the lib.sh seam could not start ({e})"));
                return (-1, String::new());
            }
        };
        crate::util::CURRENT_CHILD.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(&bytes);
        }
        let o = child.wait_with_output();
        crate::util::CURRENT_CHILD.store(0, std::sync::atomic::Ordering::SeqCst);
        let Ok(o) = o else { return (-1, String::new()) };
        let printed = seam::split(&String::from_utf8_lossy(&o.stdout));
        for l in &printed.logs {
            self.out.raw(l);
        }
        for p in &printed.progress {
            self.out.progress(p);
        }
        (o.status.code().unwrap_or(-1), printed.answer)
    }
}

/// Resolve settings and every repository through lib.sh (seam S1): the one resolver.
pub fn load_context(home: &Path, out: &Reporter) -> Result<(Settings, Vec<RepoRow>), String> {
    let sr = SeamRunner { home: home.to_path_buf(), out };
    let (rc, answer) = sr.call(Op::Context, &[]);
    if rc != 0 {
        return Err(format!("the context seam exited {rc} (is {}/lib.sh loadable?)", home.display()));
    }
    parse_context(&answer, home)
}

pub fn parse_context(answer: &str, home: &Path) -> Result<(Settings, Vec<RepoRow>), String> {
    let mut kv: BTreeMap<String, String> = BTreeMap::new();
    let mut repos = Vec::new();
    for rec in answer.split('\0') {
        let Some((k, v)) = rec.split_once('=') else { continue };
        if k == "repo" {
            let f: Vec<&str> = v.split(FIELD).collect();
            if f.len() != 8 || f[0].is_empty() {
                continue;
            }
            let opt = |s: &str| if s.is_empty() { None } else { Some(s.to_string()) };
            repos.push(RepoRow {
                name: f[0].into(),
                path: PathBuf::from(f[1]),
                mode: LandMode::parse(f[2]),
                landref: opt(f[3]),
                base_fq: opt(f[4]),
                base_remote: opt(f[5]),
                base_branch: f[6].into(),
                forge_ref: opt(f[7]),
            });
        } else {
            kv.insert(k.into(), v.into());
        }
    }
    let run = kv.get("run").cloned().unwrap_or_default();
    if run.is_empty() {
        return Err("the context seam named no SPIRA_RUN".into());
    }
    let g = |k: &str| kv.get(k).cloned().unwrap_or_default();
    let num = |k: &str, d: i64| kv.get(k).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(d);
    let path_opt = |k: &str| kv.get(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let s = Settings {
        home: path_opt("home").unwrap_or_else(|| home.to_path_buf()),
        run: PathBuf::from(&run),
        repo: PathBuf::from(g("repo")),
        db: g("db"),
        bd: kv.get("bd").filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| "bd".into()),
        bd_timeout: num("bd_timeout", 180).max(1) as u64,
        home_repo: g("home_repo"),
        id_prefix: g("id_prefix"),
        land_maxsec: num("land_maxsec", 3600),
        gate_reserve: num("gate_reserve", 1200),
        gate_lock_wait: kv.get("gate_lock_wait").filter(|v| !v.is_empty()).cloned(),
        verdict_ttl: num("verdict_ttl", 0).max(0) as u64,
        verdicts: path_opt("verdicts").unwrap_or_else(|| PathBuf::from(&run).join("verdicts")),
        deferral_escalate_at: num("deferral_at", 5).max(0) as u32,
        express_label: g("express_label"),
        cutover_label: g("cutover_label"),
        submitted_label: g("submitted_label"),
        rebase_escalate_at: num("rebase_escalate_at", 3).max(0) as u32,
        git_name: g("git_name"),
        git_email: g("git_email"),
        incident: PathBuf::from(g("incident")),
        scope_label: g("scope_label"),
        queue_bin: path_opt("queue_bin"),
        queue_dir: path_opt("queue_dir").unwrap_or_else(|| PathBuf::from(&run).join("queue")),
        lc_bin: path_opt("lc_bin"),
        prod: path_opt("prod").unwrap_or_else(|| home.to_path_buf()),
        halt_grace: num("halt_grace", 30).max(0) as u64,
        path: kv.get("path").filter(|v| !v.is_empty()).cloned(),
        bdjson_fixture: path_opt("bdjson_fixture"),
        pr_pass_branch_sh: path_opt("pr_pass_branch_sh").unwrap_or_else(|| home.join("pr-pass-branch.sh")),
        toml: path_opt("toml"),
        lifecycle_enforce: false,
    };
    Ok((s, repos))
}

pub struct RealLib<'a> {
    pub seam: SeamRunner<'a>,
    /// incident.sh — the intake the base's own red is filed through.
    pub incident: PathBuf,
}

fn p(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl<'a> Lib for RealLib<'a> {
    fn land_mark(&self, id: &str, state: &str, tip: &str, reason: &str) {
        self.seam.call(Op::LandMark, &[id, state, tip, reason]);
    }
    fn reopen(&self, id: &str, cause: &str, note: &str) {
        self.seam.call(Op::Reopen, &[id, cause, note]);
    }
    fn event(&self, kind: &str, id: &str, title: &str, detail: &str) {
        self.seam.call(Op::Event, &[kind, id, title, detail]);
    }
    fn noverdict(&self, id: &str, branch: &str, repo: &str, reason: &str, outcome: &str, out: &str) {
        self.seam.call(Op::NoVerdict, &[id, branch, repo, reason, outcome, out]);
    }
    fn incident(&self, labels: &str, repo: &str, ext_ref: &str, title: &str, payload: &str) -> Result<String, i32> {
        let intake = p(&self.incident);
        let (rc, ans) = self.seam.call(Op::Incident, &[&intake, labels, repo, ext_ref, title, payload]);
        if rc == 0 { Ok(ans) } else { Err(rc) }
    }
    fn ask_rebase_loop(&self, args: &[&str]) {
        self.seam.call(Op::AskRebaseLoop, args);
    }
    fn ask_red_recurring(&self, id: &str, branch: &str, repo: &str, class: &str, first_at: &str) {
        self.seam.call(Op::AskRedRecurring, &[id, branch, repo, class, first_at]);
    }
    fn ask_rebase_refused(&self, id: &str, branch: &str, repo: &str, reason: &str) {
        self.seam.call(Op::AskRebaseRefused, &[id, branch, repo, reason]);
    }
    fn ask_budget_deferred(&self, branch: &str, repo: &str, n: u32) {
        self.seam.call(Op::AskBudgetDeferred, &[branch, repo, &n.to_string()]);
    }
    fn rebase(&self, branch: &str, onto: &str, repo: &Path, name: &str) -> Rebase {
        let (rc, ans) = self.seam.call(Op::Rebase, &[branch, onto, &p(repo), name]);
        let f: Vec<&str> = ans.split(FIELD).collect();
        let get = |i: usize| f.get(i).map(|s| s.to_string()).unwrap_or_default();
        let mut failure = get(0);
        if rc != 0 && failure.is_empty() {
            failure = if rc == 96 { "no-lib".into() } else { "unknown".into() };
        }
        Rebase { ok: rc == 0, failure: if rc == 0 { String::new() } else { failure }, conflicts: get(1), refused_reason: get(2) }
    }
    fn recut(&self, branch: &str, onto: &str, repo: &Path, name: &str) -> Recut {
        let (rc, ans) = self.seam.call(Op::Recut, &[branch, onto, &p(repo), name]);
        let (a, c) = ans.split_once(FIELD).unwrap_or((ans.as_str(), ""));
        Recut { ok: rc == 0, applied: a.trim().parse().unwrap_or(0), conflicts: c.to_string() }
    }
    fn bump_requeue(&self, id: &str, reason: &str) {
        self.seam.call(Op::BumpRequeue, &[id, reason]);
    }
    fn requeues_of(&self, id: &str) -> u32 {
        self.seam.call(Op::RequeuesOf, &[id]).1.trim().parse().unwrap_or(0)
    }
    fn conflict_note(&self, args: &[&str]) -> String {
        self.seam.call(Op::ConflictNote, args).1
    }
    fn other_beads(&self, repo: &Path, branch: &str, base: &str, files: &str) -> String {
        self.seam.call(Op::OtherBeads, &[&p(repo), branch, base, files]).1
    }
    fn pr_merged(&self, repo: &Path, branch: &str) -> bool {
        self.seam.call(Op::PrMerged, &[&p(repo), branch]).0 == 0
    }
    fn note(&self, id: &str, text: &str) {
        self.seam.call(Op::Note, &[id, text]);
    }
    fn push(&self, tree: &Path, remote: &str, refspec: &str) -> Result<(), String> {
        let (rc, err) = self.seam.call(Op::Push, &[&p(tree), remote, refspec]);
        if rc == 0 { Ok(()) } else { Err(err) }
    }
    fn land_subject(&self, id: &str) -> String {
        let s = self.seam.call(Op::LandSubject, &[id]).1;
        if s.is_empty() { format!("spira: land {id}") } else { s }
    }
    fn deliver_delivered(&self, id: &str, sha: &str) {
        self.seam.call(Op::DeliverDelivered, &[id, sha]);
    }
    fn deliver_requeued(&self, id: &str, tip: &str) {
        self.seam.call(Op::DeliverRequeued, &[id, tip]);
    }
    fn deliver_returned(&self, id: &str, reason: &str) {
        self.seam.call(Op::DeliverReturned, &[id, reason]);
    }
    fn closeout(&self, id: &str, sha: &str, repo: &Path) {
        self.seam.call(Op::Closeout, &[id, sha, &p(repo)]);
    }
    fn close_on_land(&self, id: &str, sha: &str) {
        self.seam.call(Op::CloseOnLand, &[id, sha]);
    }
    fn prune_worktrees(&self, repo: &Path) {
        self.seam.call(Op::PruneWorktrees, &[&p(repo)]);
    }
    fn gh_unlanded_scan(&self) {
        self.seam.call(Op::GhUnlandedScan, &[]);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// The bead store (read only)
// ──────────────────────────────────────────────────────────────────────────────

pub struct RealBeads {
    pub home: PathBuf,
    pub db: String,
    pub bd: String,
    pub timeout: u64,
    pub home_repo: String,
    pub submitted_label: String,
    pub fixture: Option<PathBuf>,
}

/// `json_only`: bd can print warnings on stdout before the payload.
pub fn json_only(s: &str) -> &str {
    let mut off = 0;
    for line in s.split_inclusive('\n') {
        if line.starts_with('[') || line.starts_with('{') {
            return &s[off..];
        }
        off += line.len();
    }
    ""
}

impl RealBeads {
    fn show_raw(&self, ids: &[String]) -> Result<String, String> {
        if let Some(fx) = &self.fixture {
            let mut c = command("python3");
            c.arg(self.home.join("bdsim.py")).arg(fx).arg("show").args(ids).arg("--json").stdin(Stdio::null());
            let (rc, so, _) = run_capture(c);
            return if rc == 0 { Ok(String::from_utf8_lossy(&so).into_owned()) } else { Err(format!("bdsim exited {rc}")) };
        }
        // bdq's refusal: an empty SPIRA_DB would let bd discover whatever store the working
        // directory resolves to.
        if self.db.is_empty() {
            return Err("SPIRA_DB is empty — refusing to let bd auto-discover a store".into());
        }
        let mut last = String::new();
        for _try in 0..2 {
            let mut c = command("timeout");
            c.arg(self.timeout.to_string()).arg(&self.bd).arg("-C").arg(&self.db).arg("show").args(ids).arg("--json");
            c.stdin(Stdio::null());
            let (rc, so, se) = run_capture(c);
            if rc == 0 {
                return Ok(String::from_utf8_lossy(&so).into_owned());
            }
            last = String::from_utf8_lossy(&se).into_owned();
            // A pooled connection the server already dropped: the query never ran, retrying
            // is as safe as the first attempt (bdq's own rule).
            if !last.contains("invalid connection") {
                break;
            }
        }
        Err(format!("bd show failed: {}", last.lines().next().unwrap_or("")))
    }
}

impl Beads for RealBeads {
    fn show(&self, ids: &[String]) -> Result<Vec<BeadRow>, String> {
        let mut out = Vec::new();
        for chunk in ids.chunks(100) {
            let raw = self.show_raw(chunk)?;
            let js = json_only(&raw);
            if js.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(js).map_err(|e| format!("bd show: not JSON ({e})"))?;
            let items = match v {
                serde_json::Value::Array(a) => a,
                o @ serde_json::Value::Object(_) => vec![o],
                _ => Vec::new(),
            };
            out.extend(items.iter().filter_map(|i| BeadRow::from_json(i, &self.home_repo, &self.submitted_label)));
        }
        Ok(out)
    }
    fn land_status(&self, id: &str) -> String {
        match self.show(&[id.to_string()]) {
            Ok(rows) => rows.into_iter().next().map(|r| r.status).unwrap_or_else(|| "-".into()),
            Err(_) => "-".into(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// git
// ──────────────────────────────────────────────────────────────────────────────

pub struct RealGit;

fn git(repo: &Path) -> std::process::Command {
    let mut c = command("git");
    c.arg("-C").arg(repo).stdin(Stdio::null());
    c
}

fn git_ok(mut c: std::process::Command) -> bool {
    c.stdout(Stdio::null()).stderr(Stdio::null());
    c.status().map(|s| s.success()).unwrap_or(false)
}

fn git_out(c: std::process::Command) -> Option<String> {
    let (rc, so, _) = run_capture(c);
    if rc == 0 { Some(String::from_utf8_lossy(&so).trim_end_matches('\n').to_string()) } else { None }
}

impl Git for RealGit {
    fn spira_refs(&self, repo: &Path) -> Vec<(String, String)> {
        let mut c = git(repo);
        c.args(["for-each-ref", "--format=%(refname:short) %(objectname)", "refs/heads/spira/*"]);
        git_out(c)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())))
            .filter(|(a, b)| !a.is_empty() && !b.is_empty())
            .collect()
    }
    fn branch_exists(&self, repo: &Path, branch: &str) -> bool {
        let mut c = git(repo);
        c.args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]);
        git_ok(c)
    }
    fn rev_parse(&self, repo: &Path, rev: &str) -> Option<String> {
        let mut c = git(repo);
        c.args(["rev-parse", "--verify", "-q", rev]);
        git_out(c).filter(|s| !s.is_empty())
    }
    fn is_ancestor(&self, repo: &Path, a: &str, b: &str) -> bool {
        let mut c = git(repo);
        c.args(["merge-base", "--is-ancestor", a, b]);
        git_ok(c)
    }
    fn content_landed(&self, repo: &Path, branch: &str, base: &str) -> bool {
        let Some(ahead) = self.count(repo, &format!("{base}..{branch}")) else { return false };
        // An ancestor branch is landed: every commit is already reachable from the base.
        if self.is_ancestor(repo, branch, base) {
            return true;
        }
        if ahead == 0 {
            return false;
        }
        let mut c = git(repo);
        c.args(["merge-tree", "--write-tree", base, branch]);
        let Some(merged) = git_out(c) else { return false };
        let merged = merged.lines().next().unwrap_or("").to_string();
        if merged.is_empty() {
            return false;
        }
        self.rev_parse(repo, &format!("{base}^{{tree}}")).map(|t| t == merged).unwrap_or(false)
    }
    fn count(&self, repo: &Path, range: &str) -> Option<u64> {
        let mut c = git(repo);
        c.args(["rev-list", "--count", range]);
        git_out(c).and_then(|s| s.trim().parse().ok())
    }
    fn fetch(&self, repo: &Path, remote: &str) {
        let mut c = git(repo);
        c.args(["fetch", "-q", "--no-write-fetch-head", remote]);
        let _ = git_ok(c);
    }
    fn tree_ok(&self, tree: &Path) -> bool {
        tree.join(".git").exists()
    }
    fn tree_add_detached(&self, repo: &Path, tree: &Path, at: &str) {
        if let Some(d) = tree.parent() {
            let _ = fs::create_dir_all(d);
        }
        let mut c = git(repo);
        c.args(["worktree", "add", "-q", "--detach"]).arg(tree).arg(at);
        let _ = git_ok(c);
    }
    fn tree_checkout_landing(&self, tree: &Path, base: &str) -> bool {
        let mut c = git(tree);
        c.args(["checkout", "-q", "-B", "landing", base]);
        git_ok(c)
    }
    fn tree_merge(&self, tree: &Path, subject: &str, branch: &str, name: &str, email: &str) -> Result<(), String> {
        let mut c = git(tree);
        c.arg("-c").arg(format!("user.name={name}")).arg("-c").arg(format!("user.email={email}"));
        c.args(["merge", "--no-edit", "-q", "-m", subject, branch]);
        if git_ok(c) {
            return Ok(());
        }
        let mut d = git(tree);
        d.args(["diff", "--name-only", "--diff-filter=U"]);
        let files = git_out(d).unwrap_or_default().split('\n').filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        self.tree_merge_abort(tree);
        Err(files)
    }
    fn tree_head(&self, tree: &Path) -> Option<String> {
        self.rev_parse(tree, "HEAD")
    }
    fn tree_reset_hard(&self, tree: &Path, to: &str) {
        let mut c = git(tree);
        c.args(["reset", "-q", "--hard", to]);
        let _ = git_ok(c);
    }
    fn tree_merge_abort(&self, tree: &Path) {
        let mut c = git(tree);
        c.args(["merge", "--abort"]);
        let _ = git_ok(c);
    }
    fn delete_branch(&self, repo: &Path, branch: &str) -> bool {
        let mut c = git(repo);
        c.env("SPIRA_REF_SANCTIONED", "1").args(["branch", "-D", branch]);
        git_ok(c)
    }
    fn refs_matching(&self, repo: &Path, pattern: &str) -> Vec<String> {
        let mut c = git(repo);
        c.args(["for-each-ref", "--format=%(refname:short)", pattern]);
        git_out(c).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(String::from).collect()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Harness programs
// ──────────────────────────────────────────────────────────────────────────────

pub struct RealTools {
    pub home: PathBuf,
    pub queue_bin: Option<PathBuf>,
    /// Exported to every child: testenv appends its container names here for `halt`.
    pub containers: Option<PathBuf>,
}

fn combined(c: std::process::Command) -> (i32, String) {
    // stdout and stderr interleaved into one capture, as `2>&1` did.
    let mut c = c;
    let (r, w) = match os_pipe() {
        Some(p) => p,
        None => {
            let (rc, so, se) = run_capture(c);
            let mut s = String::from_utf8_lossy(&so).into_owned();
            s.push_str(&String::from_utf8_lossy(&se));
            return (rc, s);
        }
    };
    let w2 = match w.try_clone() {
        Ok(w2) => w2,
        Err(_) => return (-1, String::new()),
    };
    c.stdout(Stdio::from(w)).stderr(Stdio::from(w2)).stdin(Stdio::null());
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return (-1, e.to_string()),
    };
    drop(c);
    crate::util::CURRENT_CHILD.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    let mut buf = Vec::new();
    let mut r = r;
    let _ = std::io::Read::read_to_end(&mut r, &mut buf);
    let st = child.wait();
    crate::util::CURRENT_CHILD.store(0, std::sync::atomic::Ordering::SeqCst);
    let rc = st.ok().and_then(|s| s.code()).unwrap_or(-1);
    (rc, String::from_utf8_lossy(&buf).into_owned())
}

fn os_pipe() -> Option<(fs::File, fs::File)> {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    Some(unsafe { (fs::File::from_raw_fd(fds[0]), fs::File::from_raw_fd(fds[1])) })
}

impl RealTools {
    fn with_env(&self, c: &mut std::process::Command) {
        if let Some(p) = &self.containers {
            c.env("SPIRA_LANDING_CONTAINERS", p);
        }
    }
}

pub fn executable(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

impl Tools for RealTools {
    fn gate(&self, branch: &str, repo: &str, lock_wait: &str, bead: &str) -> (i32, String) {
        let mut c = command(self.home.join("gate.sh"));
        c.arg(branch).arg(repo).env("SPIRA_GATE_LOCK_WAIT", lock_wait).env("SPIRA_GATE_BEAD", bead);
        self.with_env(&mut c);
        combined(c)
    }
    fn gate_status(&self, branch: &str, repo: &str) -> Option<String> {
        let mut c = command("bash");
        c.arg(self.home.join("gate-run.sh")).arg("--status").arg(branch).arg(repo).stdin(Stdio::null());
        let (rc, so, _) = run_capture(c);
        if rc == 0 { Some(String::from_utf8_lossy(&so).trim_end_matches('\n').to_string()) } else { None }
    }
    fn confine(&self, id: &str, branch: &str, repo: &Path, base: &str, labels: &str) -> (i32, String) {
        let mut c = command(self.home.join("confine.sh"));
        c.arg(id).arg(branch).arg(repo).arg(base).arg(labels);
        let (rc, s) = combined(c);
        (rc, s.trim_end_matches('\n').to_string())
    }
    fn queue_step(&self, repo: &str) -> Result<Vec<String>, String> {
        let Some(bin) = self.queue_bin.as_ref().filter(|b| executable(b)) else {
            return Err("no queue binary (SPIRA_QUEUE_BIN) — the queue step did not run".into());
        };
        let mut c = command(bin);
        c.arg("step").arg(repo);
        self.with_env(&mut c);
        let (_, s) = combined(c);
        Ok(s.lines().map(String::from).collect())
    }
    fn skew_refresh(&self, repo: &Path) -> String {
        let mut c = command(self.home.join("skew.sh"));
        c.arg("refresh").arg(repo);
        combined(c).1.trim_end_matches('\n').to_string()
    }
    fn ensure(&self, script: &Path) -> Vec<String> {
        if !executable(script) {
            return Vec::new();
        }
        let mut c = command("bash");
        c.arg(script);
        combined(c).1.lines().map(String::from).collect()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Liveness and time
// ──────────────────────────────────────────────────────────────────────────────

pub struct RealProcs {
    pub run: PathBuf,
}

pub fn pid_alive(pid: &str) -> bool {
    let pid = pid.trim();
    !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) && Path::new(&format!("/proc/{pid}")).is_dir()
}

impl Procs for RealProcs {
    fn holder_alive(&self, id: &str) -> bool {
        if let Ok(pid) = fs::read_to_string(self.run.join(format!("hold-{id}.pid"))) {
            if pid_alive(&pid) {
                return true;
            }
        }
        let suffix = format!("-{id}.pid");
        let Ok(rd) = fs::read_dir(&self.run) else { return false };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if !(n.starts_with("aeon-") && n.ends_with(&suffix)) {
                continue;
            }
            let Ok(pid) = fs::read_to_string(e.path()) else { continue };
            if !pid_alive(&pid) {
                continue;
            }
            // argv must be our runner, not a recycled pid.
            let cmd = fs::read(format!("/proc/{}/cmdline", pid.trim())).unwrap_or_default();
            if is_aeon_cmdline(&String::from_utf8_lossy(&cmd).replace('\0', " ")) {
                return true;
            }
        }
        false
    }
}

/// Whether a space-joined cmdline is an aeon: the Rust binary (argv[0] `aeon` or `…/aeon`)
/// or the retired `aeon.sh` — the rule lib.sh aeon_alive, the sentinel, strand and
/// rebase-stale share.
pub fn is_aeon_cmdline(cmd: &str) -> bool {
    let argv0 = cmd.split(' ').next().unwrap_or("");
    cmd.contains("aeon.sh") || argv0 == "aeon" || argv0.ends_with("/aeon")
}

#[cfg(test)]
mod aeon_cmdline_tests {
    #[test]
    fn the_binary_and_the_retired_script_are_both_aeons() {
        assert!(super::is_aeon_cmdline("/r/current/bin/aeon --home /r/current/spira builder "));
        assert!(super::is_aeon_cmdline("bash /h/spira/aeon.sh builder "));
        assert!(!super::is_aeon_cmdline("sleep 30 "));
        assert!(!super::is_aeon_cmdline("/usr/bin/aeonic --x "));
    }
}

pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> u64 {
        unix_now()
    }
    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}
