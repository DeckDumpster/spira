//! The sim happy path measured against its design goal, outside any round. The goal is a
//! target the watcher reports against, never an assertion in the corpus: the clock and the sim
//! are traits so a test scripts both.

use crate::Clock;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub trait Sim {
    /// Whether a round VM pass is certifying right now.
    fn round_open(&mut self) -> Result<bool, String>;
    /// Run the scenario to its end and return the world's `exec.log`.
    fn run(&mut self) -> Result<String, String>;
    /// Wait before the next fixture start attempt.
    fn backoff(&mut self) {}
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Skipped,
    Measured { millis: u64, slowest: Option<(String, u64)> },
    Failed(String),
    FixtureDown(String),
}

pub const FIXTURE_ATTEMPTS: usize = 3;

/// A failure of the world's database fixture to start, not of the sim under test.
pub fn is_fixture_failure(err: &str) -> bool {
    err.contains("testdb up")
}

const PHASE_WIDTH: usize = 44;

/// The costliest command in an `exec.log` (`=== t=<n> <command> exit status: <rc> in <ms>ms`).
pub fn slowest_phase(exec_log: &str) -> Option<(String, u64)> {
    exec_log
        .lines()
        .filter_map(|l| {
            let body = l.strip_prefix("=== ")?.strip_suffix("ms")?;
            let (head, ms) = body.rsplit_once(" in ")?;
            let head = head.rsplit_once(" exit status: ")?.0;
            let cmd = head.strip_prefix("t=")?.split_once(' ')?.1;
            Some((cmd.chars().take(PHASE_WIDTH).collect::<String>(), ms.parse::<u64>().ok()?))
        })
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
}

pub fn measure(clock: &dyn Clock, sim: &mut dyn Sim) -> Outcome {
    match sim.round_open() {
        Ok(false) => {}
        Ok(true) => return Outcome::Skipped,
        Err(e) => return Outcome::Failed(format!("round state unknown, run withheld: {e}")),
    }
    let mut attempt = 1;
    loop {
        let t0 = clock.now_ms();
        let r = sim.run();
        let millis = clock.now_ms().saturating_sub(t0);
        match r {
            Ok(log) => return Outcome::Measured { millis, slowest: slowest_phase(&log) },
            Err(e) if is_fixture_failure(&e) && attempt < FIXTURE_ATTEMPTS => {
                attempt += 1;
                sim.backoff();
            }
            Err(e) if is_fixture_failure(&e) => return Outcome::FixtureDown(format!("{e} (after {attempt} attempts)")),
            Err(e) => return Outcome::Failed(e),
        }
    }
}

/// `<epoch>\t<ms>` lines, oldest first; the last `n` readings.
pub fn trend(history: &str, n: usize) -> Vec<u64> {
    let all: Vec<u64> = history.lines().filter_map(|l| l.split_once('\t')?.1.trim().parse().ok()).collect();
    all[all.len().saturating_sub(n)..].to_vec()
}

pub fn history_line(epoch: u64, millis: u64) -> String {
    format!("{epoch}\t{millis}\n")
}

/// The alarm for one pass, or nothing when it met the goal, was withheld, or the round was open.
pub fn alarm(outcome: &Outcome, goal_ms: u64, recent: &[u64]) -> Option<String> {
    match outcome {
        Outcome::Skipped => None,
        Outcome::Failed(e) => Some(format!("PERF UNRUNNABLE happy-path: {e}")),
        Outcome::FixtureDown(e) => Some(format!("PERF FIXTURE-DOWN happy-path: {e}")),
        Outcome::Measured { millis, slowest } if *millis > goal_ms => {
            let phase = slowest.as_ref().map_or("unknown".to_string(), |(c, ms)| format!("{c} {ms}ms"));
            let tr = recent.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
            Some(format!("PERF SLOW happy-path {millis}ms (goal {goal_ms}ms) slowest phase: {phase}; recent ms: {tr}"))
        }
        Outcome::Measured { .. } => None,
    }
}

/// Directories of a release tree a sim world reads from its checkout.
const WORLD_DIRS: [&str; 6] = [".github", "lifecycle", "out", "sim", "spira", "systemd"];

/// Top-level directories named by the workspace `members` of a Cargo.toml.
pub fn workspace_dirs(manifest: &str) -> Vec<String> {
    let Some(at) = manifest.find("members") else { return Vec::new() };
    let rest = &manifest[at..];
    let Some(open) = rest.find('[') else { return Vec::new() };
    let Some(close) = rest[open..].find(']') else { return Vec::new() };
    rest[open + 1..open + close]
        .split('"')
        .skip(1)
        .step_by(2)
        .filter_map(|m| m.split('/').next())
        .filter(|d| !d.is_empty() && *d != "*" && *d != "." && *d != "..")
        .map(str::to_string)
        .collect()
}

fn make_writable(path: &Path) -> Result<(), String> {
    let st = Command::new("chmod").arg("-R").arg("u+w").arg(path).status().map_err(|e| format!("chmod: {e}"))?;
    if st.success() { Ok(()) } else { Err(format!("chmod u+w {}: {st}", path.display())) }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    // batch-job: git add/commit of a whole release tree takes as long as the tree is large
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=perf-watch", "-c", "user.email=perf-watch@localhost"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("git {} {}: {}", args.join(" "), out.status, stderr_tail(&String::from_utf8_lossy(&out.stderr))))
    }
}

/// A git checkout the sim can run in: `repo` itself when it is one, else a repository kept
/// under `scratch` and committed from the release tree, so an unchanged tree keeps its hash.
pub fn checkout(repo: &Path, scratch: &Path) -> Result<PathBuf, String> {
    if repo.join(".git").exists() {
        return Ok(repo.to_path_buf());
    }
    let dir = scratch.join("tree");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !dir.join(".git").exists() {
        git(&dir, &["init", "-q", "--initial-branch=main"])?;
    }
    for e in std::fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
        if e.file_name() != ".git" {
            let p = e.path();
            make_writable(&p)?;
            if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) }.map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    let members = workspace_dirs(&std::fs::read_to_string(repo.join("Cargo.toml")).unwrap_or_default());
    for e in std::fs::read_dir(repo).map_err(|e| format!("{}: {e}", repo.display()))?.flatten() {
        let name = e.file_name();
        let wanted = if e.path().is_dir() {
            WORLD_DIRS.iter().any(|d| name == *d) || members.iter().any(|d| name == d.as_str())
        } else {
            name != ".git"
        };
        if wanted {
            // batch-job: copies a release directory, as long as it is large
            let st = Command::new("cp").arg("-a").arg(e.path()).arg(dir.join(&name)).status().map_err(|e| format!("cp: {e}"))?;
            if !st.success() {
                return Err(format!("cp {}: {st}", e.path().display()));
            }
            make_writable(&dir.join(&name))?;
        }
    }
    git(&dir, &["add", "-A", "-f"])?;
    if git(&dir, &["status", "--porcelain"])?.trim().is_empty() && git(&dir, &["rev-parse", "-q", "--verify", "HEAD"]).is_ok() {
        return Ok(dir);
    }
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "release tree"])?;
    Ok(dir)
}

const TAIL_BYTES: usize = 400;

/// The end of a command's stderr as one line, for an alarm that names the cause.
pub fn stderr_tail(text: &str) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut start = one.len().saturating_sub(TAIL_BYTES);
    while !one.is_char_boundary(start) {
        start += 1;
    }
    one[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Stub(Cell<u64>);
    impl Clock for Stub {
        fn now_ms(&self) -> u64 {
            self.0.get()
        }
        fn epoch_secs(&self) -> u64 {
            0
        }
    }

    struct FakeSim<'a> {
        clock: &'a Stub,
        round: Result<bool, String>,
        cost: u64,
        log: Result<String, String>,
        ran: bool,
        runs: usize,
        flaky: usize,
    }
    impl Sim for FakeSim<'_> {
        fn round_open(&mut self) -> Result<bool, String> {
            self.round.clone()
        }
        fn run(&mut self) -> Result<String, String> {
            self.ran = true;
            self.runs += 1;
            if self.runs <= self.flaky {
                return Err("testdb up: i/o timeout".into());
            }
            self.clock.0.set(self.clock.0.get() + self.cost);
            self.log.clone()
        }
    }

    const LOG: &str = "=== t=1 file bead exit status: 0 in 900ms\n=== t=2 gate-worker pass --all exit status: 0 in 41000ms\n=== t=3 publish exit status: 0 in 2500ms\nnoise\n";

    fn sim<'a>(clock: &'a Stub, round: Result<bool, String>, cost: u64) -> FakeSim<'a> {
        FakeSim { clock, round, cost, log: Ok(LOG.to_string()), ran: false, runs: 0, flaky: 0 }
    }

    #[test]
    fn the_slowest_phase_is_named_with_its_time() {
        assert_eq!(slowest_phase(LOG), Some(("gate-worker pass --all".to_string(), 41000)));
        assert_eq!(slowest_phase("nothing here\n"), None);
    }

    #[test]
    fn a_run_over_the_goal_alarms_naming_the_slowest_phase_and_the_trend() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Ok(false), 73_400);
        let o = measure(&clock, &mut s);
        let line = alarm(&o, 60_000, &[52_000, 58_000, 73_400]).unwrap();
        assert_eq!(line, "PERF SLOW happy-path 73400ms (goal 60000ms) slowest phase: gate-worker pass --all 41000ms; recent ms: 52000 58000 73400");
    }

    #[test]
    fn a_run_at_or_under_the_goal_is_silent_but_still_measured() {
        let clock = Stub(Cell::new(0));
        for cost in [59_000, 60_000] {
            let o = measure(&clock, &mut sim(&clock, Ok(false), cost));
            assert!(matches!(o, Outcome::Measured { millis, .. } if millis == cost));
            assert_eq!(alarm(&o, 60_000, &[]), None);
        }
    }

    #[test]
    fn an_open_round_withholds_the_run_entirely() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Ok(true), 999_000);
        assert_eq!(measure(&clock, &mut s), Outcome::Skipped);
        assert!(!s.ran);
        assert_eq!(alarm(&Outcome::Skipped, 60_000, &[]), None);
    }

    #[test]
    fn an_unknowable_round_state_is_an_alarm_and_never_a_run() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Err("no answer".into()), 1);
        let o = measure(&clock, &mut s);
        assert!(!s.ran);
        assert!(alarm(&o, 60_000, &[]).unwrap().starts_with("PERF UNRUNNABLE happy-path: round state unknown"));
    }

    #[test]
    fn a_failed_sim_run_is_its_own_alarm_never_a_fast_pass() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Ok(false), 5);
        s.log = Err("sim exited 1".into());
        let o = measure(&clock, &mut s);
        assert_eq!(alarm(&o, 60_000, &[]), Some("PERF UNRUNNABLE happy-path: sim exited 1".to_string()));
    }

    #[test]
    fn a_fixture_that_cannot_start_is_retried_then_reported_as_its_own_state() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Ok(false), 5);
        s.log = Err("sim: testdb up: bd init failed".into());
        let o = measure(&clock, &mut s);
        assert_eq!(s.runs, FIXTURE_ATTEMPTS);
        assert!(alarm(&o, 60_000, &[]).unwrap().starts_with("PERF FIXTURE-DOWN happy-path: sim: testdb up"));
    }

    #[test]
    fn a_fixture_that_starts_on_a_retry_is_measured() {
        let clock = Stub(Cell::new(0));
        let mut s = sim(&clock, Ok(false), 5);
        s.flaky = 1;
        let o = measure(&clock, &mut s);
        assert!(matches!(o, Outcome::Measured { millis: 5, .. }), "{o:?}");
    }

    #[test]
    fn trend_keeps_the_last_readings_and_skips_junk() {
        let h = format!("{}{}junk\n{}", history_line(1, 10), history_line(2, 20), history_line(3, 30));
        assert_eq!(trend(&h, 2), vec![20, 30]);
        assert_eq!(trend("", 5), Vec::<u64>::new());
    }

    #[test]
    fn stderr_tail_is_one_line_and_bounded() {
        assert_eq!(stderr_tail("a\n  b\n"), "a b");
        let long = "é".repeat(1000);
        let t = stderr_tail(&long);
        assert!(t.len() <= TAIL_BYTES && t.chars().all(|c| c == 'é'));
        assert_eq!(stderr_tail(""), "");
    }

    #[test]
    fn a_release_tree_with_no_git_gets_a_checkout_the_sim_can_run_in() {
        let tmp = testkit::TempDir::new("perf-watch-checkout");
        let base = tmp.path().to_path_buf();
        let (release, scratch) = (base.join("release"), base.join("scratch"));
        std::fs::create_dir_all(release.join("spira/sim")).unwrap();
        std::fs::create_dir_all(release.join("bin")).unwrap();
        std::fs::write(release.join("Cargo.toml"), "x").unwrap();
        std::fs::write(release.join("spira/sim/a.toml"), "x").unwrap();
        std::fs::write(release.join("bin/big"), "x").unwrap();
        let dir = checkout(&release, &scratch).unwrap();
        assert!(git(&dir, &["rev-parse", "--show-toplevel"]).is_ok());
        let files = git(&dir, &["ls-files"]).unwrap();
        assert!(files.contains("spira/sim/a.toml") && files.contains("Cargo.toml") && !files.contains("bin/big"), "{files}");
        let first = git(&dir, &["rev-parse", "HEAD"]).unwrap();
        checkout(&release, &scratch).unwrap();
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]).unwrap(), first, "an unchanged tree is not recommitted");
        std::fs::write(release.join("spira/sim/b.toml"), "y").unwrap();
        checkout(&release, &scratch).unwrap();
        assert_ne!(git(&dir, &["rev-parse", "HEAD"]).unwrap(), first);
        assert_eq!(checkout(&base.join("nope"), &scratch).is_err(), true);
    }

    #[test]
    fn every_workspace_member_of_the_copied_manifest_exists_in_the_checkout() {
        let tmp = testkit::TempDir::new("perf-watch-members");
        let base = tmp.path().to_path_buf();
        let (release, scratch) = (base.join("release"), base.join("scratch"));
        let manifest = "[workspace]\nmembers = [\n  \"mail\",\n  \"crates/auron\",\n]\n";
        for d in ["mail", "crates/auron", "bin"] {
            std::fs::create_dir_all(release.join(d)).unwrap();
            std::fs::write(release.join(d).join("Cargo.toml"), "x").unwrap();
        }
        std::fs::write(release.join("mail/Cargo.toml"), "x").unwrap();
        std::fs::write(release.join("Cargo.toml"), manifest).unwrap();
        let mut perms = std::fs::metadata(release.join("mail/Cargo.toml")).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o444);
        std::fs::set_permissions(release.join("mail/Cargo.toml"), perms).unwrap();
        for _ in 0..2 {
            let dir = checkout(&release, &scratch).unwrap();
            let copied = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
            let members = workspace_dirs(&copied);
            assert_eq!(members, vec!["mail", "crates"]);
            for m in members {
                assert!(dir.join(&m).is_dir(), "member {m} missing");
            }
            assert!(dir.join("crates/auron/Cargo.toml").exists());
            assert!(!dir.join("bin").exists());
        }
    }
}
