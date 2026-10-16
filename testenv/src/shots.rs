//! `testenv shots <repo> <path>... [--widths 390,1280]` captures viewport PNGs of a tree's pages;
//! `testenv shots --batch <repo> <base> <head>` captures the pages a batch changed, before and after.
//!
//! The images are evidence for a reviewer, never a gate: nothing here opens or compares one.
//! The only failure is a capture that produced no file.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::foreign::{self, Decl, Shots};
use crate::runtime::{ContainerRuntime, RunRequest};

pub const USAGE: &str = "usage: testenv shots <repo> <path>... [--widths 390,1280]\n       testenv shots --batch <repo> <base> <head>";
pub const BEFORE: &str = "batch-before";
pub const AFTER: &str = "batch-after";
const RC_RED: i32 = 1;
const RC_FAULT: i32 = 2;
const FRONTEND: &str = "frontend/";
const SHOTS_MOUNT: &str = "/shots";
const SCRIPTS_MOUNT: &str = "/spira-shots";

pub fn shot_dir(run: &Path, bead: &str, tip: &str) -> PathBuf {
    run.join("shots").join(bead).join(tip)
}

/// A page's filename stem: `/` is `index`, `/a/b?x=1` is `a_b_x_1`.
pub fn slug(path: &str) -> String {
    let s: String = path.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "index".into()
    } else {
        s
    }
}

pub fn touches_frontend(changed: &str) -> bool {
    changed.lines().any(|l| l.starts_with(FRONTEND))
}

/// The pages a change under `frontend/` can alter: each changed `.html` file's own URL, or `/`
/// when only assets (styles, scripts) changed.
pub fn pages_for(changed: &str) -> Vec<String> {
    let mut pages: Vec<String> = changed
        .lines()
        .filter_map(|l| l.strip_prefix(FRONTEND))
        .filter(|p| p.ends_with(".html"))
        .map(|p| match p.strip_suffix("index.html") {
            Some(dir) => format!("/{dir}"),
            None => format!("/{p}"),
        })
        .collect();
    pages.sort();
    pages.dedup();
    if pages.is_empty() {
        pages.push("/".into());
    }
    pages
}

/// Clears the tip's directory, runs `capture` into it, and requires one non-empty file per page
/// and width. A failure leaves no directory behind, so a partial capture can never satisfy the guard.
pub fn capture(
    run: &Path,
    bead: &str,
    tip: &str,
    pages: &[String],
    widths: &[u32],
    capture: &dyn Fn(&Path) -> (i32, String),
) -> (i32, String) {
    let dir = shot_dir(run, bead, tip);
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return (RC_FAULT, format!("shots: cannot create {}: {e}", dir.display()));
    }
    let (rc, out) = capture(&dir);
    let mut failed: Vec<String> = Vec::new();
    for p in pages {
        for w in widths {
            let f = dir.join(format!("{}-{w}.png", slug(p)));
            if !std::fs::metadata(&f).map(|m| m.is_file() && m.len() > 0).unwrap_or(false) {
                failed.push(format!("{p} at {w}px"));
            }
        }
    }
    if rc != 0 || !failed.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        let named = if failed.is_empty() { format!("capture exited {rc}") } else { format!("no image for {}", failed.join(", ")) };
        return (RC_RED, format!("shots: {named}\n{out}"));
    }
    (0, format!("shots: {} images in {}", pages.len() * widths.len(), dir.display()))
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("timeout").arg("5").arg("git").arg("-C").arg(repo).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn changed_paths(repo: &Path, tip: &str, bases: &[&str]) -> Option<String> {
    let base = bases.iter().find(|b| !b.is_empty() && git(repo, &["rev-parse", "--verify", "--quiet", &format!("{b}^{{commit}}")]).is_some())?;
    git(repo, &["diff", "--name-only", &format!("{base}...{tip}")])
}

fn parse_widths(v: &str) -> Option<Vec<u32>> {
    let w: Option<Vec<u32>> = v.split(',').map(|x| x.parse::<u32>().ok().filter(|n| *n > 0)).collect();
    w.filter(|w| !w.is_empty())
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// `resolve` maps a repository name to its checkout and landing base.
pub fn main(
    args: &[String],
    run: &Path,
    resolve: &dyn Fn(&str) -> Option<(PathBuf, String)>,
    rt: Option<&dyn ContainerRuntime>,
    harness: Option<&Path>,
) -> i32 {
    let fault = |m: String| {
        eprintln!("{m}");
        RC_FAULT
    };
    if args.first().map(String::as_str) == Some("--batch") {
        let (Some(repo_arg), Some(base), Some(head)) = (args.get(1), args.get(2), args.get(3)) else { return fault(USAGE.into()) };
        let Some((repo, _)) = resolve(repo_arg) else { return fault(format!("shots: unknown repository {repo_arg}")) };
        let (rc, msg) = batch(&repo, run, base, head, rt, harness);
        if rc == 0 {
            println!("{msg}");
        } else {
            eprintln!("{msg}");
        }
        return rc;
    }

    let mut widths: Option<Vec<u32>> = None;
    let mut pos: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--widths" {
            match args.get(i + 1).and_then(|v| parse_widths(v)) {
                Some(w) => widths = Some(w),
                None => return fault("shots: --widths needs a comma list of positive pixel widths".into()),
            }
            i += 2;
        } else {
            pos.push(args[i].clone());
            i += 1;
        }
    }
    if pos.len() < 2 {
        return fault(USAGE.into());
    }
    let (repo_arg, pages) = (&pos[0], &pos[1..]);
    if let Some(p) = pages.iter().find(|p| !p.starts_with('/') || p.chars().any(char::is_whitespace)) {
        return fault(format!("shots: page {p:?} must be a path starting with / and holding no whitespace"));
    }
    let Ok(bead) = std::env::var("SPIRA_WORK_BEAD_ID") else { return fault("shots: SPIRA_WORK_BEAD_ID is not set; run from the bead's worktree".into()) };
    let Some(_) = resolve(repo_arg) else { return fault(format!("shots: unknown repository {repo_arg}")) };
    let tree = PathBuf::from(git(Path::new("."), &["rev-parse", "--show-toplevel"]).unwrap_or_default().trim());
    if tree.as_os_str().is_empty() {
        return fault("shots: not inside a git worktree".into());
    }
    let Some(tip) = git(&tree, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string()) else { return fault("shots: cannot read HEAD".into()) };
    if git(&tree, &["status", "--porcelain"]).is_none_or(|s| !s.trim().is_empty()) {
        return fault("shots: the worktree has uncommitted changes; commit first so the images are of the tip you submit".into());
    }
    let decl_text = match std::fs::read_to_string(tree.join(foreign::DECL)) {
        Ok(t) => t,
        Err(e) => return fault(format!("shots: {}: {e}", foreign::DECL)),
    };
    let decl = match Decl::parse(&decl_text) {
        Ok(d) => d,
        Err(e) => return fault(format!("shots: {e}")),
    };
    let Some(shots) = decl.shots.as_ref() else { return fault(format!("shots: {} declares no [shots]", foreign::DECL)) };
    let (Some(rt), Some(harness)) = (rt, harness) else { return fault("shots: cannot find the harness (SPIRA_TESTENV_HARNESS)".into()) };
    let widths = widths.unwrap_or_else(|| shots.widths.clone());
    let run_dir = run.to_path_buf();
    let log = run_dir.join("shots").join(&bead).join(format!("{tip}.log"));
    let (rc, msg) = capture(run, &bead, &tip, pages, &widths, &|out| run_container(rt, &decl, shots, &tree, out, harness, pages, &widths, &log));
    if rc == 0 {
        println!("{msg}");
    } else {
        eprintln!("{msg}");
    }
    rc
}

fn worktree_add(repo: &Path, tree: &Path, sha: &str) -> bool {
    // batch-job: a checkout takes as long as the tree is large
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args(["worktree", "add", "-q", "--detach"]).arg(tree).arg(sha);
    c.status().is_ok_and(|s| s.success())
}

fn worktree_remove(repo: &Path, tree: &Path) {
    // batch-job: removal takes as long as the tree is large
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args(["worktree", "remove", "-f"]).arg(tree);
    let _ = c.status();
}

fn batch(repo: &Path, run: &Path, base: &str, head: &str, rt: Option<&dyn ContainerRuntime>, harness: Option<&Path>) -> (i32, String) {
    let Some(decl_text) = git(repo, &["show", &format!("{head}:{}", foreign::DECL)]) else { return (0, "shots: its repository declares no shots capture".into()) };
    let decl = match Decl::parse(&decl_text) {
        Ok(d) => d,
        Err(e) => return (RC_FAULT, format!("shots: {e}")),
    };
    let Some(shots) = decl.shots.as_ref() else { return (0, "shots: its repository declares no shots capture".into()) };
    let Some(changed) = git(repo, &["diff", "--name-only", &format!("{base}...{head}")]) else { return (RC_FAULT, "shots: cannot tell what the batch changes".into()) };
    if !touches_frontend(&changed) {
        return (0, format!("shots: the batch changes nothing under {FRONTEND}"));
    }
    let (Some(rt), Some(harness)) = (rt, harness) else { return (RC_FAULT, "shots: cannot find the harness (SPIRA_TESTENV_HARNESS)".into()) };
    let pages = pages_for(&changed);
    let mut lines = Vec::new();
    for (label, sha) in [(BEFORE, base), (AFTER, head)] {
        let tree = run.join("shots-tree").join(format!("{label}-{}", std::process::id()));
        let added = std::fs::create_dir_all(tree.parent().unwrap_or(run)).is_ok()
            && worktree_add(repo, &tree, sha);
        if !added {
            return (RC_FAULT, format!("shots: cannot check out {sha} for the {label} capture"));
        }
        let log = run.join("shots").join(label).join(format!("{sha}.log"));
        let _ = std::fs::create_dir_all(log.parent().unwrap_or(run));
        let (rc, msg) = capture(run, label, sha, &pages, &shots.widths, &|out| run_container(rt, &decl, shots, &tree, out, harness, &pages, &shots.widths, &log));
        worktree_remove(repo, &tree);
        let _ = std::fs::remove_dir_all(&tree);
        if rc != 0 {
            return (rc, format!("{label}: {msg}"));
        }
        lines.push(msg);
    }
    (0, lines.join("\n"))
}

#[allow(clippy::too_many_arguments)]
fn run_container(rt: &dyn ContainerRuntime, decl: &Decl, shots: &Shots, tree: &Path, out: &Path, harness: &Path, pages: &[String], widths: &[u32], log: &Path) -> (i32, String) {
    let pairs: Vec<String> = pages.iter().map(|p| format!("{}={p}", slug(p))).collect();
    let req = RunRequest {
        name: format!("testenv-shots-{}-{}", std::process::id(), slug(&tree.to_string_lossy())),
        image: shots.image.clone().unwrap_or_else(|| decl.image.clone()),
        mount: tree.to_path_buf(),
        workdir: decl.workdir.clone(),
        env: vec![
            ("SHOTS_OUT".into(), SHOTS_MOUNT.into()),
            ("SHOTS_PAGES".into(), pairs.join(" ")),
            ("SHOTS_WIDTHS".into(), widths.iter().map(u32::to_string).collect::<Vec<_>>().join(",")),
            ("SHOTS_CAPTURE".into(), format!("{SCRIPTS_MOUNT}/shots-capture.mjs")),
        ],
        mounts: vec![(out.to_path_buf(), SHOTS_MOUNT.into()), (harness.join("spira").join("testenv"), SCRIPTS_MOUNT.into())],
        overlays: Vec::new(),
        argv: shots.argv.clone(),
        timeout: Some(Duration::from_secs(shots.timeout.or(decl.timeout).unwrap_or(600))),
        deadline: None,
        output: log.to_path_buf(),
        ..RunRequest::default()
    };
    let o = rt.run_once(&req);
    rt.purge(&req.name);
    let tail: Vec<String> = std::fs::read_to_string(log).unwrap_or_default().lines().rev().take(15).map(String::from).collect::<Vec<_>>().into_iter().rev().collect();
    (o.rc, tail.join("\n"))
}

#[cfg(test)]
mod tests;
