//! Family Y (wave 4.21, sp-07jcz): `format_rebased`, `rebase_branch`, `recut_onto` —
//! lib.sh's generic "move a submitted branch forward" helpers, called on every landing
//! pass by dwarf, landing-pass and queue. Ported byte-for-byte from lib.sh (`spira/lib.sh`
//! around line 5617-5880); the three `REBASE_*`/two `RECUT_*` bash globals become the typed
//! [`RebaseResult`] / [`RecutResult`] returned here. lib.sh's own copies become one-line
//! shims to this crate's `rebase-stale` binary (`rebase-branch`, `recut-onto` subcommands),
//! so the bash-seam callers (dwarf, landing-pass, queue) do not change at all.
//!
//! Distinct from this crate's own `engine`/`holder`/`resolve` (the sp-x1c6k mechanical
//! "stale branch" tool, a different program with its own scratch tree
//! `.rebase-stale.<repo>`). This family's scratch tree keeps lib.sh's own name,
//! `.rebase.<repo-basename>`, because a leftover tree from before this port must still be
//! found at the same path.
//!
//! Any worktree pruning or pre-destruction salvage in this path goes through `hel`'s
//! destruction chokepoint (wave 4.20, sp-9envm) in-process — never a raw `git worktree
//! remove` or `git branch -D` (there is none of either here: `rebase_branch` only adds a
//! scratch worktree and moves a branch ref by ordinary update, and the one leftover
//! worktree it ever touches is `reset --hard`, never removed).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::git::Git;

#[derive(Clone, Debug)]
pub struct BranchConfig {
    /// `$SPIRA_RUN` — the scratch tree lives at `<run>/worktree/.rebase.<repo-basename>`.
    pub run: PathBuf,
    /// `$SPIRA_REAPLOG` (or `<run>/reap.log`) — the chokepoint's own log, named here because
    /// `hel::reap::{prune_worktrees,salvage}` take it as an explicit argument.
    pub reaplog: PathBuf,
    pub git_name: String,
    pub git_email: String,
    /// `${SPIRA_FORMAT_TIMEOUT:-300}`.
    pub format_timeout: Duration,
}

/// `$REBASE_FAILURE`. `Display`/`as_str` render lib.sh's own words byte for byte — callers
/// (landing-pass's `model::Rebase.failure`, queue's `Err(String)`) still compare against
/// these exact strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebaseFailure {
    Conflict,
    RebaseRefused,
    NoBase,
    NoBranch,
    NoWorktree,
}

impl RebaseFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            RebaseFailure::Conflict => "conflict",
            RebaseFailure::RebaseRefused => "rebase-refused",
            RebaseFailure::NoBase => "no-base",
            RebaseFailure::NoBranch => "no-branch",
            RebaseFailure::NoWorktree => "no-worktree",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RebaseResult {
    pub ok: bool,
    pub failure: Option<RebaseFailure>,
    pub conflicts: String,
    pub refused_reason: String,
}

impl RebaseResult {
    fn ok() -> RebaseResult {
        RebaseResult { ok: true, ..Default::default() }
    }
    fn fail(failure: RebaseFailure) -> RebaseResult {
        RebaseResult { ok: false, failure: Some(failure), ..Default::default() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecutResult {
    pub ok: bool,
    pub applied: u32,
    pub conflicts: String,
}

fn basename(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn scratch_path(run: &Path, repo: &Path) -> PathBuf {
    run.join("worktree").join(format!(".rebase.{}", basename(repo)))
}

/// `[ ! -e "$scratch/.git" ] && { mkdir -p …; spira_prune_worktrees …; git worktree add … }`.
/// The prune's own result is ignored, exactly as the bash `>/dev/null 2>&1` did — a prune
/// that refuses (sp-9envm's own "a live directory is still prunable" case) just means the
/// `worktree add` right after it is attempted against a repo that was not pruned, same as
/// before this port.
fn ensure_scratch(cfg: &BranchConfig, repo_git: &Git, repo: &Path, scratch: &Path, onto: &str) -> bool {
    if scratch.join(".git").exists() {
        return true;
    }
    if let Some(d) = scratch.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = hel::reap::prune_worktrees(&cfg.reaplog, repo);
    repo_git.ok(["worktree", "add", "-q", "--detach", &scratch.display().to_string(), onto])
}

/// lib.sh `rebase_branch <branch> <onto> <repo> <name>`. The caller must already have
/// established that no live dwarf holds `br` (lib.sh's own precondition; unchanged — this
/// port adds no liveness check of its own, exactly as the bash did not).
pub fn rebase_branch(cfg: &BranchConfig, repo: &Path, br: &str, onto: &str, name: &str, format_cmd: &str) -> RebaseResult {
    let repo_git = Git::new(repo, &cfg.git_name, &cfg.git_email);

    if repo_git.rev(onto).is_none() {
        return RebaseResult::fail(RebaseFailure::NoBase);
    }
    if !repo_git.ok(["show-ref", "--verify", "-q", &format!("refs/heads/{br}")]) {
        return RebaseResult::fail(RebaseFailure::NoBranch);
    }
    if repo_git.is_ancestor(onto, &format!("refs/heads/{br}")) {
        return RebaseResult::ok();
    }

    let scratch = scratch_path(&cfg.run, repo);
    let (wt, is_scratch) = match crate::git::worktree_of(&repo_git, br) {
        Some(wt) => (wt, false),
        None => {
            if !ensure_scratch(cfg, &repo_git, repo, &scratch, onto) {
                return RebaseResult::fail(RebaseFailure::NoWorktree);
            }
            let scratch_git = Git::new(&scratch, &cfg.git_name, &cfg.git_email);
            let _ = scratch_git.ok(["checkout", "-q", "--detach"]);
            if !scratch_git.ok(["checkout", "-q", "-B", br, &format!("refs/heads/{br}")]) {
                return RebaseResult::fail(if repo_git.ok(["show-ref", "--verify", "-q", &format!("refs/heads/{br}")]) {
                    RebaseFailure::NoWorktree
                } else {
                    RebaseFailure::NoBranch
                });
            }
            (scratch.clone(), true)
        }
    };

    let wt_git = Git::new(&wt, &cfg.git_name, &cfg.git_email);

    // `git diff --quiet HEAD` — dirty tracked state is routine (wiki/tasks.md, a generated
    // file, is rewritten by a timer) and is salvaged then hard-reset, never left in place.
    if !wt_git.ok(["diff", "--quiet", "HEAD"]) {
        let id = br.rsplit('/').next().unwrap_or(br);
        // Salvage's own result is not checked — matches the bash, which piped it to
        // `/dev/null` with no `||` chain and proceeded to the hard reset regardless.
        let _ = hel::reap::salvage(&cfg.run, &cfg.reaplog, &format!("{id}-prerebase"), &wt);
        let _ = wt_git.ok(["reset", "-q", "--hard", "HEAD"]);
    }

    
    let rebase_run = wt_git.run(["rebase", "-q", onto]);
    let result = if !rebase_run.ok {
        let conflicts = wt_git.unmerged_paths().join(" ");
        let _ = wt_git.ok(["rebase", "--abort"]);
        if !conflicts.is_empty() {
            RebaseResult { ok: false, failure: Some(RebaseFailure::Conflict), conflicts, refused_reason: String::new() }
        } else {
            let reason = rebase_run.stderr.lines().next().unwrap_or("").to_string();
            RebaseResult { ok: false, failure: Some(RebaseFailure::RebaseRefused), conflicts: String::new(), refused_reason: reason }
        }
    } else {
        // THE REBASE ACTUALLY REPLAYED COMMITS — format_rebased only runs on this path,
        // never on the already-an-ancestor no-op (which returned above) or on a failure.
        format_rebased(cfg, br, onto, &wt_git, name, format_cmd);
        RebaseResult::ok()
    };

    // Let go of the branch: a scratch tree still holding it would refuse a later
    // `git branch -D` the way holding it in any worktree does.
    if is_scratch {
        let _ = wt_git.ok(["checkout", "-q", "--detach"]);
    }
    result
}

/// lib.sh `format_rebased <branch> <onto> <worktree> <name>` — always a no-op on the
/// caller's result; a formatter failure changes nothing (`git checkout -q -- .` discards
/// whatever it touched) and the rebase it ran on top of stands either way.
fn format_rebased(cfg: &BranchConfig, br: &str, onto: &str, wt: &Git, name: &str, format_cmd: &str) {
    if format_cmd.is_empty() {
        return;
    }
    let cargo_bin = std::env::var("HOME").map(|h| format!("{h}/.cargo/bin")).unwrap_or_default();
    let path = format!("{cargo_bin}:{}", std::env::var("PATH").unwrap_or_default());
    let ran = std::process::Command::new("timeout")
        .arg(cfg.format_timeout.as_secs().to_string())
        .arg("bash")
        .arg("-c")
        .arg(format_cmd)
        .current_dir(&wt.dir)
        .env_clear()
        .env("PATH", &path)
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("TERM", "dumb")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ran {
        eprintln!("format: {name}'s formatter failed on {br} — leaving the rebase unformatted");
        let _ = wt.ok(["checkout", "-q", "--", "."]);
        return;
    }

    // The branch's own files, from history (onto..HEAD), filtered to paths that still exist
    // — a path the branch deleted cannot have been reformatted.
    let paths: Vec<String> = wt.diff_name_only_z(onto, "HEAD").into_iter().filter(|f| wt.dir.join(f).is_file()).collect();
    if !paths.is_empty() {
        let mut args: Vec<String> = vec!["add".into(), "--".into()];
        args.extend(paths);
        let _ = wt.ok(args);
    }

    // Everything the formatter touched outside the branch's own work goes back.
    let _ = wt.ok(["checkout", "-q", "--", "."]);
    if wt.ok(["diff", "--cached", "--quiet"]) {
        return; // nothing staged — no commit.
    }

    let tail = br.rsplit('/').next().unwrap_or(br);
    let msg = format!(
        "spira: re-format {tail} after rebase onto {onto}\n\nThe rebase replayed cleanly and nothing re-ran {name}'s formatter on the result, so\nthe tree its own check tests was machine-produced. Formatted with: {format_cmd}\n"
    );
    let _ = wt.run_stdin(["commit", "-q", "-F", "-"], &msg);
    eprintln!("format: re-formatted {br} after its rebase onto {onto}");
}

/// lib.sh `recut_onto <branch> <onto> <repo> <name>` — cherry-picks the branch's own commits
/// one by one onto `onto` in the shared scratch tree, advancing as far as they allow. The
/// branch ref moves only when at least one commit landed (moving it with zero would strip
/// all work and leave a trivially-clean branch the next pass certifies with no content).
pub fn recut_onto(cfg: &BranchConfig, repo: &Path, br: &str, onto: &str, _name: &str) -> RecutResult {
    let repo_git = Git::new(repo, &cfg.git_name, &cfg.git_email);
    let scratch = scratch_path(&cfg.run, repo);

    if !ensure_scratch(cfg, &repo_git, repo, &scratch, onto) {
        return RecutResult { ok: false, applied: 0, conflicts: "no-worktree".into() };
    }
    if repo_git.rev(onto).is_none() {
        return RecutResult { ok: false, applied: 0, conflicts: "no-base".into() };
    }
    if !repo_git.ok(["show-ref", "--verify", "-q", &format!("refs/heads/{br}")]) {
        return RecutResult { ok: false, applied: 0, conflicts: "no-branch".into() };
    }
    if repo_git.is_ancestor(onto, &format!("refs/heads/{br}")) {
        return RecutResult { ok: true, applied: 0, conflicts: String::new() };
    }
    let Some(old_base) = repo_git.out(["merge-base", onto, &format!("refs/heads/{br}")]) else {
        return RecutResult { ok: false, applied: 0, conflicts: "no-merge-base".into() };
    };

    let scratch_git = Git::new(&scratch, &cfg.git_name, &cfg.git_email);
    if !scratch_git.ok(["checkout", "-q", "--detach", onto]) {
        return RecutResult { ok: false, applied: 0, conflicts: "no-checkout".into() };
    }

    let commits: Vec<String> = repo_git
        .out(["rev-list", "--reverse", &format!("{old_base}..{br}")])
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default();

    let mut applied: u32 = 0;
    let mut conflicts = String::new();
    let mut ok = true;
    for commit in &commits {
        if commit.is_empty() {
            continue;
        }
        let cp = scratch_git.run(["cherry-pick", commit]);
        if !cp.ok {
            conflicts = scratch_git.unmerged_paths().join(" ");
            if conflicts.is_empty() {
                conflicts = cp.stderr.lines().next().unwrap_or("").to_string();
            }
            let _ = scratch_git.ok(["cherry-pick", "--abort"]);
            ok = false;
            break;
        }
        applied += 1;
    }

    let new_tip = scratch_git.rev("HEAD");
    if applied > 0 {
        if let Some(tip) = &new_tip {
            let _ = repo_git.ok(["update-ref", &format!("refs/heads/{br}"), tip]);
        }
    }
    let _ = scratch_git.ok(["checkout", "-q", "--detach"]);

    RecutResult { ok, applied, conflicts }
}
