//! `release vm-repro <bead>` (DESIGN.md "vm-repro"): the round VM's build-then-test sequence
//! for one member, run on the host from a detached worktree, with its verdict and logs kept.

use crate::git::{Git, RealGit};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const USAGE: &str = "usage: release vm-repro <bead-id> [--suites a.sh,b.sh] [--repo R] [--deadline SECS] [--was VERDICT[:STEP]] [--release DIR]";

pub const DEFAULT_DEADLINE_SECS: u64 = 600;
pub const EXIT_RED: u8 = 1;
pub const EXIT_FAULT: u8 = 3;

#[derive(Clone)]
pub struct Opts {
    pub bead: String,
    pub suites: Option<String>,
    pub repo: PathBuf,
    pub run: PathBuf,
    pub release: PathBuf,
    pub home: PathBuf,
    pub toml: String,
    pub path_tail: String,
    pub dav_addr: String,
    pub deadline: u64,
    pub was: Option<String>,
}

pub fn branch(bead: &str) -> String {
    format!("spira/{bead}")
}

pub fn log_dir(run: &Path, bead: &str) -> PathBuf {
    run.join("vm-repro").join(bead)
}

pub fn tree_dir(run: &Path, bead: &str) -> PathBuf {
    run.join("worktree").join(format!("vm-repro-{bead}"))
}

pub fn valid_bead(bead: &str) -> bool {
    !bead.is_empty() && !bead.starts_with(['-', '.']) && bead.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The build the VM script runs, argv after `cargo`.
pub fn build_args() -> Vec<String> {
    let mut a: Vec<String> = ["build", "--profile", "release", "--workspace"].map(String::from).to_vec();
    a.extend(spira_config::build::one_shot("release"));
    a
}

/// The suites a repro runs. The sim lane's suites install in a second container, so a run with
/// none of them misses that install; a run of only allowlisted suites proves nothing, since those
/// skip in the container. A tree with no suite scripts (its tiers are declared elsewhere) is
/// left to testenv. Returns the list to pass, `None` leaving testenv to select, and what was added.
pub fn suites_for(tree: &Path, given: Option<&str>) -> (Option<String>, Vec<String>) {
    let mut found: Vec<String> = fs::read_dir(tree.join("spira"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
        .collect();
    found.sort();
    if found.is_empty() {
        return (given.map(String::from), Vec::new());
    }
    let allow: Vec<String> = fs::read_to_string(tree.join("spira/skip-allowlist.tsv"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').next().map(String::from))
        .collect();
    let sim = |n: &str| is_sim_lane(&tree.join("spira").join(n));
    let mut list: Vec<String> = given.map(|g| g.split(',').filter(|x| !x.is_empty()).map(String::from).collect()).unwrap_or_default();
    let mut added = Vec::new();
    if !list.iter().any(|n| sim(n)) {
        if let Some(n) = found.iter().find(|n| sim(n)) {
            added.push(format!("added {n}: the sim lane installs in a second container"));
            list.push(n.clone());
        }
    }
    if !list.iter().any(|n| !allow.contains(n) && !sim(n)) {
        if let Some(n) = found.iter().find(|n| !allow.contains(n) && !sim(n)) {
            added.push(format!("added {n}: a suite not on the skip allowlist runs in the container"));
            list.push(n.clone());
        }
    }
    (Some(list.join(",")), added)
}

fn is_sim_lane(script: &Path) -> bool {
    fs::read_to_string(script).unwrap_or_default().lines().take_while(|l| !l.starts_with("set ")).any(|l| l.trim_end() == "# lane: sim" || l.starts_with("# lane: sim "))
}

/// The suite run, argv after `testenv`.
pub fn testenv_args(o: &Opts, tree: &Path) -> Vec<String> {
    let mut a: Vec<String> = ["--mode", "parallel", "--artifacts"].map(String::from).to_vec();
    a.push(tree.join("target/release").display().to_string());
    if let Some(s) = &o.suites {
        a.extend(["--suites".to_string(), s.clone()]);
    }
    a.extend(["--deadline".to_string(), o.deadline.to_string(), branch(&o.bead), o.repo.display().to_string()]);
    a
}

/// The PATH of a launcher with no inherited environment: the release's own directories first.
pub fn path_for(o: &Opts) -> Result<String, String> {
    let base = spira_config::release_path_with_tail(&o.release.display().to_string(), &o.path_tail)?;
    let home = o.home.display().to_string();
    let mut path = base;
    for extra in [spira_config::spira_env_bin(&home), format!("{home}/.cargo/bin")] {
        if !path.split(':').any(|d| d == extra) {
            path = format!("{path}:{extra}");
        }
    }
    Ok(path)
}

/// The whole environment both steps run under, built from nothing.
pub fn base_env(o: &Opts, ambient: &dyn Fn(&str) -> Option<String>) -> Result<Vec<(String, String)>, String> {
    let mut env = vec![
        ("HOME".to_string(), o.home.display().to_string()),
        ("PATH".to_string(), path_for(o)?),
        (spira_config::RELEASE_ENV.to_string(), o.release.display().to_string()),
        ("SPIRA_TOML".to_string(), o.toml.clone()),
        ("SPIRA_RUN".to_string(), o.run.display().to_string()),
        ("SPIRA_REPO".to_string(), o.repo.display().to_string()),
    ];
    for k in ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "USER", "LOGNAME"] {
        if let Some(v) = ambient(k).filter(|v| !v.is_empty()) {
            env.push((k.to_string(), v));
        }
    }
    Ok(env)
}

/// The compile pool and shared cache for the build, exactly as a dwarf's cargo gets them. A
/// missing piece is said in the build log and the build goes on uncached or unadmitted.
pub fn cache_env(path: &str, o: &Opts, notes: &mut Vec<String>) -> Vec<(String, String)> {
    let store = spira_config::build::Store::from_values(|k| (k == spira_config::build::STORE_ADDR_ENV && !o.dav_addr.is_empty()).then(|| o.dav_addr.clone()));
    let w = match spira_config::build::wrapper(path, None) {
        Ok(w) => w,
        Err(e) => {
            notes.push(format!("{e} — this build is UNCACHED"));
            return Vec::new();
        }
    };
    notes.push(w.describe());
    match spira_config::build::find_on(path, spira_config::admission::BIN) {
        Some(a) => w.admitted_env(&a, &o.run.display().to_string(), &format!("vm-repro-{}", o.bead), store.as_ref()),
        None => {
            notes.push("spira-admit not on PATH — this build is not admitted".to_string());
            w.env(store.as_ref())
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    pub line: String,
    pub code: u8,
    pub report: Vec<String>,
}

/// The `VERDICT` line testenv ended its log with, else one made from its exit code.
pub fn verdict_of(rc: Option<i32>, log: &str) -> Verdict {
    let from_log = log.lines().rev().find(|l| l.starts_with("VERDICT "));
    let code = match rc {
        Some(0) => 0,
        Some(1) => EXIT_RED,
        _ => EXIT_FAULT,
    };
    let line = match (from_log, rc) {
        (Some(l), _) => l.to_string(),
        (None, Some(0)) => "VERDICT GREEN".to_string(),
        (None, Some(1)) => "VERDICT RED".to_string(),
        (None, Some(c)) => format!("VERDICT FAULT reason=testenv-rc-{c}"),
        (None, None) => "VERDICT FAULT reason=testenv-signalled".to_string(),
    };
    // A GREEN line over a failing exit, or the reverse, is the runner contradicting itself.
    let code = if line.starts_with("VERDICT GREEN") && code != 0 || !line.starts_with("VERDICT GREEN") && code == 0 { EXIT_FAULT } else { code };
    Verdict { line, code, report: Vec::new() }
}

struct Tree<'a> {
    repo: &'a Path,
    dir: PathBuf,
}

impl Tree<'_> {
    fn git(&self, args: &[&str]) -> Result<(), String> {
        let out = spira_config::bounded::bounded_for("git", "120").arg("-C").arg(self.repo).args(args).output().map_err(|e| format!("cannot run git: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }
}

impl Drop for Tree<'_> {
    fn drop(&mut self) {
        let _ = self.git(&["worktree", "remove", "--force", &self.dir.display().to_string()]);
        let _ = fs::remove_dir_all(&self.dir);
        let _ = self.git(&["worktree", "prune"]);
    }
}

fn step(program: &Path, args: &[String], cwd: &Path, env: &[(String, String)], log: &Path, header: &[String]) -> Result<Option<i32>, String> {
    let mut f = File::create(log).map_err(|e| format!("cannot write {}: {e}", log.display()))?;
    for h in header {
        let _ = writeln!(f, "vm-repro: {h}");
    }
    let _ = writeln!(f, "vm-repro: {} {}", program.display(), args.join(" "));
    let err = f.try_clone().map_err(|e| e.to_string())?;
    // batch-job: the build and the suites are bounded by the admission pool and testenv's own --deadline
    let st = Command::new(program).args(args).current_dir(cwd).env_clear().envs(env.iter().cloned()).stdin(Stdio::null()).stdout(f).stderr(err).status().map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    Ok(st.code())
}

pub fn which(prog: &str, path: &str) -> Option<PathBuf> {
    path.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join(prog)).find(|p| p.is_file())
}

/// The sequence. `Ok` carries the verdict; `Err` is a refusal before any verdict was possible.
pub fn run(o: &Opts, ambient: &dyn Fn(&str) -> Option<String>) -> Result<Verdict, String> {
    if !valid_bead(&o.bead) {
        return Err(format!("{:?} is not a bead id", o.bead));
    }
    let sha = RealGit.resolve(&o.repo, &branch(&o.bead))?;
    let logs = log_dir(&o.run, &o.bead);
    let _ = fs::remove_dir_all(&logs);
    fs::create_dir_all(&logs).map_err(|e| format!("cannot create {}: {e}", logs.display()))?;

    let tree = Tree { repo: &o.repo, dir: tree_dir(&o.run, &o.bead) };
    if tree.dir.exists() {
        let _ = tree.git(&["worktree", "remove", "--force", &tree.dir.display().to_string()]);
        let _ = fs::remove_dir_all(&tree.dir);
    }
    fs::create_dir_all(tree.dir.parent().unwrap_or(&o.run)).map_err(|e| e.to_string())?;
    tree.git(&["worktree", "add", "--detach", &tree.dir.display().to_string(), &sha])?;
    println!("vm-repro: {} at {sha}, worktree {}", branch(&o.bead), tree.dir.display());

    let env = base_env(o, ambient)?;
    let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()).unwrap_or_default();
    let cargo = which("cargo", &path).ok_or_else(|| format!("cargo is not on the launcher's PATH ({path})"))?;

    let mut notes = Vec::new();
    let mut build_env = env.clone();
    build_env.extend(cache_env(&path, o, &mut notes));
    let build_log = logs.join("build.log");
    let rc = step(&cargo, &build_args(), &tree.dir, &build_env, &build_log, &notes)?;
    if rc != Some(0) {
        let line = format!("VERDICT FAULT reason=build rc={}", rc.map_or("signal".to_string(), |c| c.to_string()));
        let report = outcome(&line, "", &[], o.was.as_deref());
        return Ok(Verdict { line, code: EXIT_FAULT, report });
    }

    let (suites, added) = suites_for(&tree.dir, o.suites.as_deref());
    for a in &added {
        println!("vm-repro: {a}");
    }
    let o = &Opts { suites, ..o.clone() };
    let testenv = tree.dir.join("target/release/testenv");
    let test_log = logs.join("testenv.log");
    let rc = step(&testenv, &testenv_args(o, &tree.dir), &tree.dir, &env, &test_log, &added)?;
    let text = fs::read_to_string(&test_log).unwrap_or_default();
    let sim: Vec<String> = fs::read_dir(tree.dir.join("spira")).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| is_sim_lane(&tree.dir.join("spira").join(n))).collect();
    let mut v = verdict_of(rc, &text);
    v.report = outcome(&v.line, &text, &sim, o.was.as_deref());
    Ok(v)
}

/// The lines closing a repro: which lanes and how many suites executed, then one of
/// `REPRODUCED`, `NOT REPRODUCED` or `NOTHING TESTED`. Only the second makes "the environment
/// differs" a hypothesis. With no `was`, any failure counts as the failure being reproduced.
pub fn outcome(line: &str, log: &str, sim: &[String], was: Option<&str>) -> Vec<String> {
    let executed: Vec<&str> = log
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (name, status) = (w.next()?, w.next()?);
            (name.starts_with("test-") && name.ends_with(".sh") && !matches!(status, "SKIPPED" | "DISABLED") && !status.starts_with("SKIP-REQ")).then_some(name)
        })
        .collect();
    let in_sim = executed.iter().filter(|n| sim.iter().any(|s| s == *n)).count();
    let lanes = match (executed.len() - in_sim, in_sim) {
        (0, 0) => "none".to_string(),
        (c, 0) => format!("container({c})"),
        (0, m) => format!("sim({m})"),
        (c, m) => format!("container({c}) sim({m})"),
    };
    let mut out = vec![format!("vm-repro: executed {} suite(s), lanes: {lanes}", executed.len())];
    let failed = !line.starts_with("VERDICT GREEN");
    let same = |w: &str| {
        let (word, step) = w.split_once(':').unwrap_or((w, ""));
        line.starts_with(&format!("VERDICT {word}")) && (step.is_empty() || line.contains(&format!("reason={step}")) || line.contains(&format!("red={step}")))
    };
    out.push(
        match (failed, executed.is_empty(), was) {
            (true, _, Some(w)) if !same(w) => format!("NOT REPRODUCED: ran the same selection and got {line}, not {w}"),
            (true, _, _) => "REPRODUCED".to_string(),
            (false, true, _) => "NOTHING TESTED: every suite skipped".to_string(),
            (false, false, _) => "NOT REPRODUCED: ran the same selection and passed".to_string(),
        },
    );
    out
}

fn resolve(args: &[String]) -> Result<Opts, (u8, String)> {
    let usage = |m: &str| (2u8, format!("{m}\n{USAGE}"));
    let (mut bead, mut suites, mut repo, mut release, mut deadline, mut was) = (None, None, None, None, DEFAULT_DEADLINE_SECS, None);
    let mut it = args.iter();
    while let Some(x) = it.next() {
        let mut val = |n: &str| it.next().cloned().ok_or_else(|| usage(&format!("{n} needs a value")));
        match x.as_str() {
            "--suites" => suites = Some(val(x)?),
            "--repo" => repo = Some(PathBuf::from(val(x)?)),
            "--release" => release = Some(PathBuf::from(val(x)?)),
            "--was" => was = Some(val(x)?),
            "--deadline" => deadline = val(x)?.parse().ok().filter(|d| *d > 0).ok_or_else(|| usage("--deadline needs whole seconds above 0"))?,
            "-h" | "--help" => return Err((2, USAGE.to_string())),
            f if f.starts_with('-') => return Err(usage(&format!("unknown flag {f}"))),
            b if bead.is_none() => bead = Some(b.to_string()),
            _ => return Err(usage("one bead id")),
        }
    }
    let bead = bead.ok_or_else(|| usage("vm-repro needs a bead id"))?;
    let fail = |e: String| (1u8, e);
    let release = match release {
        Some(r) => r,
        None => std::env::current_exe().ok().and_then(|e| e.canonicalize().ok()).and_then(|e| e.parent()?.parent().map(Path::to_path_buf)).ok_or_else(|| fail("cannot find the release this binary runs from".into()))?,
    };
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty()).map(PathBuf::from).ok_or_else(|| fail("HOME is not set".into()))?;
    let toml = spira_config::process::spec().map_err(fail)?;
    std::env::set_var(spira_config::RELEASE_ENV, &release);
    let cfg = spira_config::process::cfg;
    let run = PathBuf::from(cfg("SPIRA_RUN").map_err(fail)?);
    if run.as_os_str().is_empty() {
        return Err(fail("no run directory: set spira.run (SPIRA_RUN) in the config file".into()));
    }
    let env: crate::config::Env = [(spira_config::RELEASE_ENV.to_string(), release.display().to_string())].into();
    let repo = repo
        .map(|r| crate::repo::resolve(Some(&r), &env).unwrap_or(r))
        .or_else(|| crate::repo::resolve(None, &env))
        .ok_or_else(|| fail("no repository: pass --repo".into()))?;
    Ok(Opts { bead, suites, repo, run, release, home, toml, path_tail: cfg("SPIRA_PATH").map_err(fail)?, dav_addr: cfg("SPIRA_SCCACHE_DAV_ADDR").map_err(fail)?, deadline, was })
}

/// The subcommand: prints the verdict and returns its exit code.
pub fn main(args: &[String]) -> u8 {
    let o = match resolve(args) {
        Ok(o) => o,
        Err((code, m)) => {
            eprintln!("release: {m}");
            return code;
        }
    };
    let ambient = |k: &str| std::env::var(k).ok();
    match run(&o, &ambient) {
        Ok(v) => {
            println!("{}", v.line);
            for r in &v.report {
                println!("{r}");
            }
            println!("vm-repro: logs in {}", log_dir(&o.run, &o.bead).display());
            v.code
        }
        Err(e) => {
            eprintln!("release: vm-repro: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Opts {
        Opts { bead: "sp-x".into(), suites: None, repo: "/r".into(), run: "/run".into(), release: "/rel/abc".into(), home: "/h".into(), toml: "/t.toml".into(), path_tail: String::new(), dav_addr: String::new(), deadline: 600, was: None }
    }

    #[test]
    fn the_build_is_the_vm_scripts_build() {
        let vm = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../round-vm/src/run.rs")).unwrap();
        let line = format!("cargo build -q {}", build_args().split_first().map(|(_, rest)| rest.join(" ")).unwrap());
        assert!(vm.contains(&line), "round-vm no longer builds with: {line}");
    }

    #[test]
    fn the_suite_run_names_the_branch_and_repo_explicitly() {
        let mut o = opts();
        o.suites = Some("a.sh,b.sh".into());
        assert_eq!(testenv_args(&o, Path::new("/t")).join(" "), "--mode parallel --artifacts /t/target/release --suites a.sh,b.sh --deadline 600 spira/sp-x /r");
    }

    #[test]
    fn the_path_puts_the_release_first_and_adds_cargo() {
        let p = path_for(&opts()).unwrap();
        assert!(p.starts_with("/rel/abc/bin:/rel/abc/spira:"), "{p}");
        assert!(p.ends_with(":/h/.cargo/bin"), "{p}");
    }

    #[test]
    fn the_environment_is_built_from_nothing_but_what_the_verb_names() {
        let env = base_env(&opts(), &|k| (k == "XDG_RUNTIME_DIR").then(|| "/run/user/1".to_string()).or_else(|| (k == "SECRET").then(|| "s".to_string()))).unwrap();
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"SPIRA_RELEASE") && keys.contains(&"SPIRA_TOML") && keys.contains(&"PATH") && keys.contains(&"XDG_RUNTIME_DIR"));
        assert!(!keys.contains(&"SECRET"));
    }

    #[test]
    fn a_verdict_line_in_the_log_wins_and_a_missing_one_is_made_from_the_exit() {
        assert_eq!(verdict_of(Some(0), "x\nVERDICT GREEN rc=0\n").code, 0);
        assert_eq!(verdict_of(Some(1), "VERDICT RED rc=1 red=a.sh\n").code, EXIT_RED);
        assert_eq!(verdict_of(Some(2), "VERDICT FAULT rc=2 reason=install\n").code, EXIT_FAULT);
        assert_eq!(verdict_of(Some(1), "").line, "VERDICT RED");
        assert_eq!(verdict_of(None, "").code, EXIT_FAULT);
    }

    #[test]
    fn a_green_line_over_a_failing_exit_is_a_fault() {
        assert_eq!(verdict_of(Some(1), "VERDICT GREEN\n").code, EXIT_FAULT);
        assert_eq!(verdict_of(Some(0), "no verdict line but rc 0 and a fault line\nVERDICT FAULT\n").code, EXIT_FAULT);
    }

    const LOG: &str = "  test-a.sh                        ok      3s\n  test-b.sh                        SKIPPED\n  test-s.sh                        ok      4s\n";

    #[test]
    fn a_pass_of_the_same_selection_is_not_reproduced_and_says_which_lanes_ran() {
        let r = outcome("VERDICT GREEN ran=2", LOG, &["test-s.sh".to_string()], None);
        assert_eq!(r, ["vm-repro: executed 2 suite(s), lanes: container(1) sim(1)", "NOT REPRODUCED: ran the same selection and passed"]);
    }

    #[test]
    fn a_run_where_every_suite_skipped_tested_nothing() {
        let r = outcome("VERDICT GREEN ran=0", "  test-b.sh   SKIPPED\n  test-c.sh   SKIP-REQ requires:jq\n", &[], None);
        assert_eq!(r[0], "vm-repro: executed 0 suite(s), lanes: none");
        assert!(r[1].starts_with("NOTHING TESTED"), "{r:?}");
    }

    #[test]
    fn a_failure_matching_the_named_verdict_and_step_is_reproduced_and_another_is_not() {
        let line = "VERDICT FAULT rc=2 ran=1 reason=install";
        assert_eq!(outcome(line, LOG, &[], Some("FAULT:install"))[1], "REPRODUCED");
        assert_eq!(outcome(line, LOG, &[], None)[1], "REPRODUCED");
        assert!(outcome(line, LOG, &[], Some("RED"))[1].starts_with("NOT REPRODUCED"));
        assert!(outcome(line, LOG, &[], Some("FAULT:build"))[1].starts_with("NOT REPRODUCED"));
    }

    #[test]
    fn a_bead_id_cannot_climb_out_of_the_run_directory() {
        assert!(valid_bead("sp-5vww8l") && valid_bead("sp-1.2"));
        assert!(!valid_bead("../x") && !valid_bead("") && !valid_bead("-rf") && !valid_bead("a/b"));
    }

    fn tree(files: &[(&str, &str)]) -> testkit::TempDir {
        let t = testkit::TempDir::new("vm-repro-suites");
        for (n, b) in files {
            fs::create_dir_all(t.path().join(n).parent().unwrap()).unwrap();
            fs::write(t.path().join(n), b).unwrap();
        }
        t
    }

    #[test]
    fn a_given_list_is_topped_up_with_a_sim_suite_and_one_off_the_allowlist() {
        let t = tree(&[("spira/test-a.sh", ""), ("spira/test-b.sh", "# lane: sim\nset -u\n"), ("spira/test-c.sh", ""), ("spira/skip-allowlist.tsv", "# x\ntest-a.sh\tr\tw\n")]);
        assert_eq!(suites_for(t.path(), Some("test-a.sh")).0.as_deref(), Some("test-a.sh,test-b.sh,test-c.sh"));
        assert_eq!(suites_for(t.path(), None).0.as_deref(), Some("test-b.sh,test-c.sh"));
        assert!(suites_for(t.path(), Some("test-b.sh,test-c.sh")).1.is_empty());
    }

    #[test]
    fn a_lane_header_after_the_script_starts_is_not_a_lane() {
        let t = tree(&[("spira/test-a.sh", "set -u\n# lane: sim\n"), ("spira/test-b.sh", "")]);
        assert_eq!(suites_for(t.path(), None).0.as_deref(), Some("test-a.sh"));
    }

    #[test]
    fn a_tree_without_suite_scripts_is_left_to_testenv() {
        let t = tree(&[("README", "")]);
        assert_eq!(suites_for(t.path(), None), (None, vec![]));
        assert_eq!(suites_for(t.path(), Some("tier")).0.as_deref(), Some("tier"));
    }

    #[test]
    fn the_build_goes_through_the_compile_pool_and_the_cache_when_both_are_on_the_path() {
        let bin = testkit::TempDir::new("vm-repro-bin");
        testkit::write_exe(bin.path().join("sccache"), "#!/bin/sh\n");
        testkit::write_exe(bin.path().join("spira-admit"), "#!/bin/sh\n");
        let path = bin.path().display().to_string();
        let mut notes = Vec::new();
        let env = cache_env(&path, &opts(), &mut notes);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("RUSTC_WRAPPER"), Some(format!("{path}/spira-admit")));
        assert_eq!(get(spira_config::admission::INNER_ENV), Some(format!("{path}/sccache")));
        assert_eq!(get(spira_config::admission::WHO_ENV).as_deref(), Some("vm-repro-sp-x"));
        assert_eq!(get("SPIRA_RUN").as_deref(), Some("/run"));
    }

    #[test]
    fn a_missing_pool_or_cache_is_said_and_never_stops_the_build() {
        let bin = testkit::TempDir::new("vm-repro-bin");
        let mut notes = Vec::new();
        assert!(cache_env(&bin.path().display().to_string(), &opts(), &mut notes).is_empty());
        assert!(notes[0].contains("UNCACHED"), "{notes:?}");
        testkit::write_exe(bin.path().join("sccache"), "#!/bin/sh\n");
        let mut notes = Vec::new();
        let env = cache_env(&bin.path().display().to_string(), &opts(), &mut notes);
        assert!(notes.iter().any(|n| n.contains("not admitted")), "{notes:?}");
        assert!(env.iter().any(|(k, _)| k == "RUSTC_WRAPPER"));
    }
}
