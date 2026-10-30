//! `round-recert --batch <id> --repo <repo>` (DESIGN.md "round-recert"): one certification pass
//! of an open round, certify then land, run in a transient user unit so the caller's deadline
//! covers starting it and a restart of the caller does not kill it.

use std::path::{Path, PathBuf};
use std::process::Stdio;

pub const USAGE: &str = "usage: round-recert --batch <id> --repo <repo>";

pub const UNIT_PREFIX: &str = "round-recert-";
pub const CERTIFY_DEADLINE_SECS: u64 = 7200;
pub const LAND_DEADLINE_SECS: u64 = 3600;
pub const QUERY_DEADLINE_SECS: u64 = 120;
pub const CONTENTION_WAIT_SECS: u64 = 15;
pub const CONTENTION_TRIES: u32 = 20;
pub const LAND_DEPLOY_FAULT_EXIT: i32 = 3;

/// A round whose record is still listed but whose lifecycle has ended is not one to drive.
const ENDED: [&str; 4] = ["landed", "settled", "abandoned", "discarded"];

/// A held queue lock or an unreachable lifecycle is contention, not a verdict.
const CONTENTION: [&str; 3] = ["holds the lock", "cannot tell: deadline", "spira-lc is unreachable"];

pub struct Out {
    pub code: Option<i32>,
    pub text: String,
}

pub trait Sys {
    fn exec(&self, program: &str, args: &[String], deadline_secs: u64) -> Out;
    fn sleep(&self, secs: u64);
}

pub fn valid_token(s: &str) -> bool {
    !s.is_empty() && !s.starts_with(['-', '.']) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn unit_for(batch: &str) -> String {
    format!("{UNIT_PREFIX}{batch}")
}

#[derive(Debug, PartialEq, Eq)]
pub struct Round {
    pub batch: String,
    pub phase: String,
    pub head: String,
}

/// The open rounds `queue round status` lists: the first unprefixed, the rest `round<n>_`.
pub fn open_rounds(status: &str) -> Vec<Round> {
    let value = |key: &str| status.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(str::trim);
    let prefixes = std::iter::once(String::new()).chain((2..).map(|n| format!("round{n}_")));
    let mut out = Vec::new();
    for p in prefixes {
        let Some(batch) = value(&format!("{p}batch_id")).filter(|b| !b.is_empty()) else { break };
        out.push(Round { batch: batch.to_string(), phase: value(&format!("{p}phase")).unwrap_or_default().to_string(), head: value(&format!("{p}head")).unwrap_or_default().to_string() });
    }
    out
}

/// The round `batch`, or the refusal naming what state it is in.
pub fn require_open(status: &str, repo: &str, batch: &str) -> Result<Round, String> {
    let rounds = open_rounds(status);
    match rounds.into_iter().find(|r| r.batch == batch) {
        Some(r) if ENDED.contains(&r.phase.as_str()) => Err(format!("round {batch} is {}, not open", r.phase)),
        Some(r) => Ok(r),
        None => {
            let open: Vec<String> = open_rounds(status).into_iter().map(|r| r.batch).collect();
            let held = if open.is_empty() { "no round is open".to_string() } else { format!("open: {}", open.join(" ")) };
            Err(format!("round {batch} is not open in {repo} ({held})"))
        }
    }
}

fn queue(sys: &dyn Sys, args: &[&str], deadline: u64) -> Out {
    let argv: Vec<String> = ["round"].into_iter().chain(args.iter().copied()).map(String::from).collect();
    let mut last = Out { code: None, text: String::new() };
    for n in 1..=CONTENTION_TRIES {
        last = sys.exec("queue", &argv, deadline);
        if !CONTENTION.iter().any(|m| last.text.contains(m)) || n == CONTENTION_TRIES {
            break;
        }
        sys.sleep(CONTENTION_WAIT_SECS);
    }
    last
}

fn status(sys: &dyn Sys, repo: &str) -> Result<String, String> {
    let o = queue(sys, &["status", repo], QUERY_DEADLINE_SECS);
    match o.code {
        Some(0) => Ok(o.text),
        c => Err(format!("queue round status {repo} failed (rc={}): {}", c.map_or("signal".into(), |c| c.to_string()), o.text.trim())),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Started {
    Started(String),
    Running(String),
}

pub struct Launch<'a> {
    pub exe: &'a Path,
    pub toml: &'a str,
}

fn active(sys: &dyn Sys, unit: &str) -> bool {
    let o = sys.exec("systemctl", &["--user".into(), "is-active".into(), unit.into()], 20);
    matches!(o.text.lines().next().map(str::trim), Some("active" | "activating" | "reloading"))
}

/// Refuses a round that is not open; finds a running driver or starts one.
pub fn start(sys: &dyn Sys, l: &Launch, batch: &str, repo: &str) -> Result<Started, String> {
    require_open(&status(sys, repo)?, repo, batch)?;
    let unit = unit_for(batch);
    if active(sys, &unit) {
        return Ok(Started::Running(unit));
    }
    let args: Vec<String> = [
        "--user".to_string(),
        format!("--unit={unit}"),
        "--collect".into(),
        "--quiet".into(),
        format!("--setenv=SPIRA_TOML={}", l.toml),
        l.exe.display().to_string(),
        "round-recert".into(),
        "--drive".into(),
        "--batch".into(),
        batch.into(),
        "--repo".into(),
        repo.into(),
    ]
    .to_vec();
    let o = sys.exec("systemd-run", &args, QUERY_DEADLINE_SECS);
    if o.code == Some(0) {
        return Ok(Started::Started(unit));
    }
    if active(sys, &unit) {
        return Ok(Started::Running(unit));
    }
    Err(format!("systemd-run could not start {unit}: {}", o.text.trim()))
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// What certify said about a pass that did not come out green: the red steps it named, or incomplete.
fn verdict(text: &str) -> String {
    let named = text.lines().find_map(|l| l.trim().strip_prefix("red=")).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).or_else(|| {
        text.lines().find_map(|l| {
            let rest = l.split_once(" is RED at ")?.1;
            Some(rest.split_once(": ")?.1.trim().to_string()).filter(|s| !s.is_empty())
        })
    });
    match named {
        Some(steps) => format!("RED: {steps}"),
        None if text.contains("red-output=") => "RED".to_string(),
        None => "incomplete, no red named".to_string(),
    }
}

fn not_green_line(batch: &str, cert: &Out) -> String {
    format!("round-recert: {batch} is not green: {} (certify rc={}) — round left open", verdict(&cert.text), cert.code.map_or("signal".into(), |c| c.to_string()))
}

/// The pass itself: certify, and land only on green. 0 landed, 1 anything else.
pub fn drive(sys: &dyn Sys, batch: &str, repo: &str) -> u8 {
    let round = match status(sys, repo).and_then(|s| require_open(&s, repo, batch)) {
        Ok(r) => r,
        Err(e) => {
            println!("round-recert: {e}");
            return 1;
        }
    };
    println!("round-recert: driving {batch} ({}) at {}", round.phase, round.head);
    let cert = queue(sys, &["certify", batch, repo], CERTIFY_DEADLINE_SECS);
    println!("{}", tail(&cert.text, 4));
    if cert.code != Some(0) {
        println!("{}", not_green_line(batch, &cert));
        return 1;
    }
    let land = queue(sys, &["land", batch, repo], LAND_DEADLINE_SECS);
    println!("{}", tail(&land.text, 4));
    let gone = status(sys, repo).map(|s| !open_rounds(&s).iter().any(|r| r.batch == batch)).unwrap_or(false);
    match land.code {
        Some(0) if gone => println!("round-recert: {batch} landed at {}", round.head),
        Some(LAND_DEPLOY_FAULT_EXIT) if gone => println!("round-recert: {batch} landed at {} but its release was not activated", round.head),
        c => {
            println!("round-recert: LAND FAILED for {batch} (land rc={}, round {}) — see above", c.map_or("signal".into(), |c| c.to_string()), if gone { "gone" } else { "still open" });
            return 1;
        }
    }
    0
}

struct Real {
    env: Vec<(String, String)>,
    path: String,
}

impl Sys for Real {
    fn exec(&self, program: &str, args: &[String], deadline_secs: u64) -> Out {
        let Some(bin) = crate::vm_repro::which(program, &self.path) else {
            return Out { code: None, text: format!("{program} is not on the launcher's PATH ({})\n", self.path) };
        };
        let run = spira_config::bounded::bounded_for(bin, &deadline_secs.to_string()).args(args).env_clear().envs(self.env.iter().cloned()).stdin(Stdio::null()).output();
        match run {
            Ok(o) => Out { code: o.status.code(), text: format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)) },
            Err(e) => Out { code: None, text: format!("cannot run {program}: {e}\n") },
        }
    }

    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

struct Args {
    batch: String,
    repo: String,
    drive: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let (mut batch, mut repo, mut drive) = (None, None, false);
    let mut it = args.iter();
    while let Some(x) = it.next() {
        let mut val = |n: &str| it.next().cloned().ok_or_else(|| format!("{n} needs a value"));
        match x.as_str() {
            "--batch" => batch = Some(val(x)?),
            "--repo" => repo = Some(val(x)?),
            "--drive" => drive = true,
            f => return Err(format!("unknown argument {f}")),
        }
    }
    let batch = batch.ok_or("--batch is required")?;
    let repo = repo.ok_or("--repo is required")?;
    for (n, v) in [("--batch", &batch), ("--repo", &repo)] {
        if !valid_token(v) {
            return Err(format!("{n} {v:?} is not a plain name"));
        }
    }
    Ok(Args { batch, repo, drive })
}

fn real() -> Result<(Real, PathBuf, String), String> {
    let exe = std::env::current_exe().and_then(|e| e.canonicalize()).map_err(|e| format!("cannot find this binary: {e}"))?;
    let release = exe.parent().and_then(Path::parent).ok_or("cannot find the release this binary runs from")?.to_path_buf();
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty()).ok_or("HOME is not set")?;
    let toml = spira_config::process::spec()?;
    std::env::set_var(spira_config::RELEASE_ENV, &release);
    let path = spira_config::release_path_with_tail(&release.display().to_string(), &spira_config::process::cfg("SPIRA_PATH")?)?;
    let mut env = vec![
        ("HOME".to_string(), home),
        ("PATH".to_string(), path.clone()),
        (spira_config::RELEASE_ENV.to_string(), release.display().to_string()),
        ("SPIRA_TOML".to_string(), toml.clone()),
    ];
    for k in ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "USER", "LOGNAME"] {
        if let Some(v) = std::env::var(k).ok().filter(|v| !v.is_empty()) {
            env.push((k.to_string(), v));
        }
    }
    Ok((Real { env, path }, exe, toml))
}

/// The verb: prints the unit it started or found, returns the exit code.
pub fn main(args: &[String]) -> u8 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("round-recert: {e}\n{USAGE}");
            return 2;
        }
    };
    let (sys, exe, toml) = match real() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("round-recert: {e}");
            return 1;
        }
    };
    if a.drive {
        return drive(&sys, &a.batch, &a.repo);
    }
    match start(&sys, &Launch { exe: &exe, toml: &toml }, &a.batch, &a.repo) {
        Ok(Started::Started(u)) => {
            println!("round-recert: started {u}");
            0
        }
        Ok(Started::Running(u)) => {
            println!("round-recert: driver already running {u}");
            0
        }
        Err(e) => {
            eprintln!("round-recert: refused: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const STATUS: &str = "rounds=2\nopen_batches=r-1 r-2\nround=open\nphase=certifying\nbatch_id=r-1\nhead=aaa\nround2=open\nround2_phase=green\nround2_batch_id=r-2\nround2_head=bbb\n";

    #[test]
    fn every_open_round_in_a_status_is_read_with_its_phase_and_head() {
        let r = open_rounds(STATUS);
        assert_eq!(r.len(), 2);
        assert_eq!((r[1].batch.as_str(), r[1].phase.as_str(), r[1].head.as_str()), ("r-2", "green", "bbb"));
        assert!(open_rounds("round=none\nrepo=spira\n").is_empty());
    }

    #[test]
    fn a_listed_round_is_open_unless_its_phase_has_ended() {
        assert_eq!(require_open(STATUS, "spira", "r-2").unwrap().phase, "green");
        assert_eq!(require_open("batch_id=r-1\nphase=landed\n", "spira", "r-1").unwrap_err(), "round r-1 is landed, not open");
        assert!(require_open(STATUS, "spira", "r-3").unwrap_err().contains("open: r-1 r-2"));
    }

    struct Fake {
        answers: RefCell<Vec<Out>>,
        slept: RefCell<Vec<u64>>,
    }

    impl Sys for Fake {
        fn exec(&self, _: &str, _: &[String], _: u64) -> Out {
            self.answers.borrow_mut().remove(0)
        }
        fn sleep(&self, secs: u64) {
            self.slept.borrow_mut().push(secs);
        }
    }

    #[test]
    fn a_held_queue_lock_is_waited_out_and_any_other_failure_is_final() {
        let out = |code, text: &str| Out { code, text: text.to_string() };
        let held = Fake { answers: RefCell::new(vec![out(Some(1), "another queue operation holds the lock"), out(Some(0), "round=none")]), slept: RefCell::new(vec![]) };
        assert_eq!(queue(&held, &["status", "spira"], 1).code, Some(0));
        assert_eq!(*held.slept.borrow(), [CONTENTION_WAIT_SECS]);
        let red = Fake { answers: RefCell::new(vec![out(Some(1), "round is RED")]), slept: RefCell::new(vec![]) };
        assert_eq!(queue(&red, &["certify", "r", "spira"], 1).code, Some(1));
        assert!(red.slept.borrow().is_empty());
    }

    #[test]
    fn a_red_certify_names_its_red_step_and_only_a_verdictless_one_is_incomplete() {
        let printed = "round r is RED at e198ebb: unit-tests lint\nred=unit-tests lint\nmender filed\n";
        assert_eq!(verdict(printed), "RED: unit-tests lint");
        assert_eq!(verdict("round r is RED at e198ebb: unit-tests\n"), "RED: unit-tests");
        assert_eq!(verdict("red=unit-tests\n"), "RED: unit-tests");
        assert_eq!(verdict("timed out waiting\n"), "incomplete, no red named");
        assert_eq!(verdict("red=\nround r is RED at abc: lint\n"), "RED: lint");
        assert_eq!(verdict("red-output=/tmp/x\n"), "RED");
        let line = not_green_line("r-1", &Out { code: Some(1), text: "red=unit-tests\n".into() });
        assert!(line.contains("RED: unit-tests") && !line.contains("incomplete"), "{line}");
    }

    #[test]
    fn drive_leaves_a_round_with_a_named_red_open_and_fails() {
        let out = |code, text: &str| Out { code, text: text.to_string() };
        let fake = Fake {
            answers: RefCell::new(vec![out(Some(0), "batch_id=r-1\nphase=certifying\nhead=aaa\nround=open\n"), out(Some(1), "red=unit-tests\n")]),
            slept: RefCell::new(vec![]),
        };
        assert_eq!(drive(&fake, "r-1", "spira"), 1);
        assert!(fake.answers.borrow().is_empty());
    }

    struct Recorder(RefCell<Vec<(String, Vec<String>)>>);

    impl Sys for Recorder {
        fn exec(&self, prog: &str, args: &[String], _: u64) -> Out {
            self.0.borrow_mut().push((prog.to_string(), args.to_vec()));
            match prog {
                "systemctl" => Out { code: Some(3), text: "inactive".into() },
                _ => Out { code: Some(0), text: "batch_id=b\nphase=opened\n".into() },
            }
        }
        fn sleep(&self, _: u64) {}
    }

    #[test]
    fn the_unit_runs_the_round_recert_subcommand_of_the_release_binary() {
        let rec = Recorder(RefCell::new(vec![]));
        let l = Launch { exe: Path::new("/rel/bin/release"), toml: "t.toml" };
        assert!(matches!(start(&rec, &l, "b", "r"), Ok(Started::Started(_))));
        let calls = rec.0.borrow();
        let run = calls.iter().find(|(p, _)| p == "systemd-run").expect("systemd-run was called");
        assert!(run.1.join(" ").ends_with("/rel/bin/release round-recert --drive --batch b --repo r"), "{:?}", run.1);
    }

    #[test]
    fn a_unit_name_carries_the_batch_so_one_batch_has_one_driver() {
        assert_eq!(unit_for("r-auto-622"), "round-recert-r-auto-622");
        assert!(valid_token("r-auto-622") && !valid_token("a/b") && !valid_token("-x") && !valid_token(""));
    }
}
