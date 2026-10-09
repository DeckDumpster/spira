//! The world's `round-vm`: a stub that answers `round-vm run <tree> --results-dir <dir>`
//! the way the real one does (round-vm/DESIGN.md), without a VM. Its verdict is scripted by
//! the scenario through `<world>/gate-verdict`; on a verdict it installs the world's release
//! executables into `<tree>/target/release`, where `queue round land` looks for the round's
//! own build, and writes one `<suite>.result` per suite into the results dir.

use std::path::{Path, PathBuf};

/// Names the world the stub belongs to; world-up writes it into `config/sim.env`.
pub const WORLD_ENV: &str = "SIM_WORLD";
/// A prebuilt release dir with `bin/`, used when the world has no `release` link.
pub const RELEASE_ENV: &str = "SPIRA_SIM_RELEASE";
/// The scripting file, one line: `green`, `red <suite>...`, or `fault [why]`.
pub const VERDICT_FILE: &str = "gate-verdict";
const PRODUCER: &str = "sim-round-vm";

/// round-vm's own failure: never a verdict, so `queue round certify` calls it a fault.
pub const FAULT: i32 = 2;

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Green,
    Red(Vec<String>),
    Fault(String),
}

pub fn parse_verdict(text: &str) -> Result<Verdict, String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).ok_or("the verdict is empty")?;
    let mut words = line.split_whitespace();
    match (words.next(), words.collect::<Vec<_>>()) {
        (Some("green"), rest) if rest.is_empty() => Ok(Verdict::Green),
        (Some("red"), suites) if !suites.is_empty() => {
            if let Some(bad) = suites.iter().find(|s| !is_suite_name(s)) {
                return Err(format!("{bad:?} is not a suite name"));
            }
            Ok(Verdict::Red(suites.iter().map(|s| s.to_string()).collect()))
        }
        (Some("fault"), why) => Ok(Verdict::Fault(if why.is_empty() { "scripted infra fault".into() } else { why.join(" ") })),
        _ => Err(format!("{line:?} is not `green`, `red <suite>...` or `fault [why]`")),
    }
}

fn is_suite_name(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

pub fn write_verdict(world: &Path, v: &Verdict) -> Result<(), String> {
    let line = match v {
        Verdict::Green => "green".to_string(),
        Verdict::Red(s) => format!("red {}", s.join(" ")),
        Verdict::Fault(why) => format!("fault {why}"),
    };
    std::fs::write(world.join(VERDICT_FILE), line + "\n").map_err(|e| e.to_string())
}

pub struct Run {
    pub tree: PathBuf,
    pub results: PathBuf,
    /// `--suites`: the suites the caller wants run, instead of the tree's whole corpus.
    pub suites: Option<Vec<String>>,
}

/// Flags the batcher passes that say how a real VM runs, and mean nothing to a stub.
const IGNORED_FLAGS: &[&str] = &["--maxpar", "--toolchain", "--attr-spool", "--base"];

pub fn parse_args(args: &[String]) -> Result<Run, String> {
    let Some((verb, rest)) = args.split_first() else { return Err("usage: round-vm run <tree> --results-dir <dir>".into()) };
    if verb != "run" {
        return Err(format!("the sim round-vm answers only `run`, not {verb:?}"));
    }
    let (mut tree, mut results, mut suites) = (None, None, None);
    let csv = |v: &str| v.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--results-dir=") {
            results = Some(PathBuf::from(v));
        } else if a == "--results-dir" {
            results = Some(PathBuf::from(it.next().ok_or("--results-dir needs a value")?));
        } else if let Some(v) = a.strip_prefix("--suites=") {
            suites = Some(csv(v));
        } else if a == "--suites" {
            suites = Some(csv(it.next().ok_or("--suites needs a value")?));
        } else if IGNORED_FLAGS.iter().any(|f| a == f) {
            it.next().ok_or_else(|| format!("{a} needs a value"))?;
        } else if IGNORED_FLAGS.iter().any(|f| a.strip_prefix(f).is_some_and(|r| r.starts_with('='))) {
        } else if a.starts_with("--") {
            return Err(format!("the sim round-vm does not take {a}"));
        } else if tree.replace(PathBuf::from(a)).is_some() {
            return Err("round-vm run takes one tree".into());
        }
    }
    Ok(Run { tree: tree.ok_or("round-vm run needs a tree")?, results: results.ok_or("the sim round-vm needs --results-dir")?, suites })
}

/// The corpus as testenv defines it: every `test-*.sh` regular file in `<tree>/spira`.
pub fn corpus(tree: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(tree.join("spira"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn release_bin(world: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    let linked = world.join("release");
    if linked.exists() {
        return Ok(linked.join("bin"));
    }
    env(RELEASE_ENV)
        .filter(|v| !v.is_empty())
        .map(|r| PathBuf::from(r).join("bin"))
        .ok_or_else(|| format!("the world has no release and {RELEASE_ENV} is unset"))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Links each executable of `bin` into `<tree>/target/release`; the count linked.
fn install_bins(bin: &Path, tree: &Path) -> Result<usize, String> {
    let rd = std::fs::read_dir(bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let dest = tree.join("target").join("release");
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let mut n = 0;
    for e in rd.flatten() {
        let src = e.path();
        if !is_executable(&src) {
            continue;
        }
        let src = src.canonicalize().map_err(|e| e.to_string())?;
        let at = dest.join(e.file_name());
        let _ = std::fs::remove_file(&at);
        std::os::unix::fs::symlink(&src, &at).map_err(|e| format!("{}: {e}", at.display()))?;
        n += 1;
    }
    Ok(n)
}

fn write_result(dir: &Path, suite: &str, line: &str) -> Result<(), String> {
    let tmp = dir.join(format!(".{suite}.result.tmp"));
    std::fs::write(&tmp, format!("{line}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(format!("{suite}.result"))).map_err(|e| e.to_string())
}

/// `round-vm run` in `world`: the exit code, and what went to stderr.
pub fn run(world: &Path, args: &[String], env: &dyn Fn(&str) -> Option<String>, now: u64) -> (i32, String) {
    match run_inner(world, args, env, now) {
        Ok((code, msg)) => (code, msg),
        Err(e) => (FAULT, format!("sim round-vm: {e}\n")),
    }
}

fn run_inner(world: &Path, args: &[String], env: &dyn Fn(&str) -> Option<String>, now: u64) -> Result<(i32, String), String> {
    if !world.join(crate::world::MARKER).is_file() {
        return Err(format!("{} is not a sim world", world.display()));
    }
    let r = parse_args(args)?;
    let path = world.join(VERDICT_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e} — no scripted verdict", path.display()))?;
    let verdict = parse_verdict(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let reds = match verdict {
        Verdict::Fault(why) => return Ok((FAULT, format!("sim round-vm: {why}\n"))),
        Verdict::Green => Vec::new(),
        Verdict::Red(s) => s,
    };
    let mut suites = r.suites.clone().unwrap_or_else(|| corpus(&r.tree));
    suites.extend(reds.iter().cloned());
    suites.sort();
    suites.dedup();
    if suites.is_empty() {
        return Err(format!("{} holds no test-*.sh suites — a green run with no results", r.tree.join("spira").display()));
    }
    let bin = release_bin(world, env)?;
    if install_bins(&bin, &r.tree)? == 0 {
        return Err(format!("{} holds no executables to install as the round's build", bin.display()));
    }
    std::fs::create_dir_all(&r.results).map_err(|e| e.to_string())?;
    for s in &suites {
        let line = if reds.contains(s) {
            format!("red {now} 0 fp serial {PRODUCER} 1")
        } else {
            format!("ok {now} 0 - serial {PRODUCER} 0")
        };
        write_result(&r.results, s, &line)?;
    }
    if reds.is_empty() {
        Ok((0, String::new()))
    } else {
        Ok((1, format!("sim round-vm: red: {}\n", reds.join(","))))
    }
}
