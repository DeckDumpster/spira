//! `testenv lint [--verify] <tip> [<repo>]`: the fast compile-and-lint check a tip passes before
//! it may be submitted. A repository declares the tiers in `lint` of `.spira/testenv.toml`; a
//! pass is recorded per exact tip, and a tip whose repository declares none records `none`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::foreign;

pub const USAGE: &str = "usage: testenv lint [--verify] <40-hex tip> [<repo-name-or-path>]";
const RC_RED: i32 = 1;
const RC_FAULT: i32 = 2;

pub fn record_path(run: &Path, tip: &str) -> PathBuf {
    run.join("lint-pass").join(tip)
}

pub fn recorded(run: &Path, tip: &str) -> Option<String> {
    std::fs::read_to_string(record_path(run, tip)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn record(run: &Path, tip: &str, what: &str) -> Result<(), String> {
    let p = record_path(run, tip);
    let dir = p.parent().unwrap_or(run);
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{tip}.{}", std::process::id()));
    std::fs::write(&tmp, format!("{what}\n")).and_then(|_| std::fs::rename(&tmp, &p)).map_err(|e| format!("cannot record {}: {e}", p.display()))
}

fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The lint tiers the tip's own declaration names; empty when it declares none.
pub fn declared(decl_text: Option<&str>) -> Result<Vec<String>, String> {
    match decl_text {
        None => Ok(Vec::new()),
        Some(t) => foreign::Decl::parse(t).map(|d| d.lint),
    }
}

/// `verify`: only whether the tip's pass is on record. Otherwise run the tiers (through `run_tiers`,
/// which answers testenv's exit status and output) unless a pass is already recorded.
pub fn ensure(
    run: &Path,
    tip: &str,
    verify: bool,
    decl_text: Option<&str>,
    run_tiers: &dyn Fn(&[String]) -> (i32, String),
) -> (i32, String) {
    if !is_full_sha(tip) {
        return (RC_FAULT, format!("lint: {tip}: not a full 40-hex commit name"));
    }
    if let Some(r) = recorded(run, tip) {
        return (0, format!("lint: {tip} already passed ({r})"));
    }
    if verify {
        return (RC_RED, format!("refused: {tip} has not passed its repository's compile-and-lint check — run `testenv lint {tip}` first"));
    }
    let tiers = match declared(decl_text) {
        Ok(t) => t,
        Err(e) => return (RC_FAULT, format!("lint: {e}")),
    };
    if tiers.is_empty() {
        return match record(run, tip, "none") {
            Ok(()) => (0, format!("lint: {tip}: its repository declares no lint tier")),
            Err(e) => (RC_FAULT, format!("lint: {e}")),
        };
    }
    let (rc, out) = run_tiers(&tiers);
    match rc {
        0 => match record(run, tip, &format!("pass {}", tiers.join(","))) {
            Ok(()) => (0, format!("lint: {tip} passed {}", tiers.join(", "))),
            Err(e) => (RC_FAULT, format!("lint: {e}")),
        },
        1 => {
            let first = out
                .lines()
                .find_map(|l| l.trim_start().strip_prefix("first error ("))
                .map(|l| format!(" at {}", l.split_once("): ").map_or(l, |(_, e)| e)))
                .unwrap_or_default();
            (RC_RED, format!("refused: {tip} failed its compile-and-lint check ({}){first}\n{out}", tiers.join(", ")))
        }
        n => (RC_FAULT, format!("lint: the check could not run (testenv rc={n}) — nothing recorded\n{out}")),
    }
}

fn git_show(repo: &Path, tip: &str) -> Option<String> {
    let o = Command::new("git").arg("-C").arg(repo).args(["show", &format!("{tip}:{}", foreign::DECL)]).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn main(args: &[String], run: &Path, resolve: &dyn Fn(&str) -> Option<PathBuf>) -> i32 {
    let verify = args.iter().any(|a| a == "--verify");
    let pos: Vec<&String> = args.iter().filter(|a| *a != "--verify").collect();
    let Some(tip) = pos.first() else {
        eprintln!("{USAGE}");
        return RC_FAULT;
    };
    let repo_arg = pos.get(1).map(|s| s.as_str()).unwrap_or(".");
    let repo = if repo_arg.contains('/') || repo_arg == "." {
        PathBuf::from(repo_arg)
    } else {
        match resolve(repo_arg) {
            Some(p) => p,
            None => {
                eprintln!("lint: unknown repository {repo_arg}");
                return RC_FAULT;
            }
        }
    };
    let decl = git_show(&repo, tip);
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("testenv"));
    let run_tiers = |tiers: &[String]| {
        // batch-job: runs the repository's own declared lint tiers, bounded by the tier timeouts
        match Command::new(&exe).args(["--suites", &tiers.join(","), tip]).arg(&repo).output() {
            Ok(o) => (o.status.code().unwrap_or(RC_FAULT), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => (RC_FAULT, format!("cannot run testenv: {e}")),
        }
    };
    let (rc, msg) = ensure(run, tip, verify, decl.as_deref(), &run_tiers);
    if rc == 0 {
        println!("{msg}");
    } else {
        eprintln!("{msg}");
    }
    rc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
    const DECL: &str = "image = \"i\"\nlint = [\"lint\"]\n[tier.lint]\nargv = [\"x\"]\n";

    fn dir(n: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("lint-{n}"))
    }

    #[test]
    fn a_red_tip_is_refused_naming_the_first_error_and_records_nothing() {
        let tmp = dir("red");
        let run = tmp.path().to_path_buf();
        let red = |_: &[String]| (1, "  lint   RED  after 3s\n  first error (lint): src/binder.rs:222:9: error: needless mut\n".to_string());
        let (rc, msg) = ensure(&run, TIP, false, Some(DECL), &red);
        assert_eq!(rc, 1);
        assert!(msg.starts_with("refused:") && msg.contains("src/binder.rs:222:9"), "{msg}");
        assert_eq!(recorded(&run, TIP), None);
        assert_eq!(ensure(&run, TIP, true, Some(DECL), &red).0, 1);
    }

    #[test]
    fn a_clean_tip_records_its_pass_and_is_not_rerun() {
        let tmp = dir("green");
        let run = tmp.path().to_path_buf();
        let calls = Cell::new(0);
        let green = |_: &[String]| {
            calls.set(calls.get() + 1);
            (0, String::new())
        };
        assert_eq!(ensure(&run, TIP, false, Some(DECL), &green).0, 0);
        assert_eq!(ensure(&run, TIP, false, Some(DECL), &green).0, 0);
        assert_eq!(calls.get(), 1);
        assert_eq!(ensure(&run, TIP, true, Some(DECL), &green).0, 0);
        let other = "f".repeat(40);
        assert_eq!(ensure(&run, &other, true, Some(DECL), &green).0, 1);
    }

    #[test]
    fn a_fault_records_nothing_and_a_declaration_less_tip_records_none() {
        let tmp = dir("fault");
        let run = tmp.path().to_path_buf();
        let fault = |_: &[String]| (2, "FAULT".to_string());
        assert_eq!(ensure(&run, TIP, false, Some(DECL), &fault).0, 2);
        assert_eq!(recorded(&run, TIP), None);
        assert_eq!(ensure(&run, TIP, false, None, &fault).0, 0);
        assert_eq!(recorded(&run, TIP).as_deref(), Some("none"));
    }

    #[test]
    fn a_tip_that_is_not_a_full_sha_is_a_fault() {
        let tmp = dir("sha");
        let run = tmp.path().to_path_buf();
        assert_eq!(ensure(&run, "abc", false, None, &|_| (0, String::new())).0, 2);
    }
}
