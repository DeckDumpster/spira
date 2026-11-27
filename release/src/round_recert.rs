//! `round-recert --batch <id> --repo <repo>` (DESIGN.md "round-recert"): one certification pass
//! of an open round, certify then land, run in a transient user unit so the caller's deadline
//! covers starting it and a restart of the caller does not kill it.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use runtime::units::UnitState;

pub const USAGE: &str = "usage: round-recert --batch <id> --repo <repo>";

pub const UNIT_PREFIX: &str = "round-recert-";
pub const CERTIFY_DEADLINE_SECS: u64 = 7200;
pub const LAND_DEADLINE_SECS: u64 = 3600;
pub const QUERY_DEADLINE_SECS: u64 = 120;
pub const CONTENTION_WAIT_SECS: u64 = 15;
pub const CONTENTION_TRIES: u32 = 20;
pub const LAND_DEPLOY_FAULT_EXIT: i32 = 3;
const VM_NO_OUTPUT: i32 = 77;
const NO_OUTPUT_PARKED: &str = "VM produced no output on two fresh VMs running";

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
    /// The user manager's typed state of `unit` (its `ActiveState` property, over the unit
    /// port); `Err` when the manager gave no answer, which is "cannot tell", never "inactive".
    fn unit_state(&self, unit: &str) -> Result<UnitState, String>;
}

pub fn valid_token(s: &str) -> bool {
    !s.is_empty() && !s.starts_with(['-', '.']) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The driver's unit: one per batch, so a second start finds the first.
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
    queue_run(sys, args, &mut |argv| sys.exec("queue", argv, deadline))
}

fn queue_run(sys: &dyn Sys, args: &[&str], exec: &mut dyn FnMut(&[String]) -> Out) -> Out {
    let argv: Vec<String> = ["round"].into_iter().chain(args.iter().copied()).map(String::from).collect();
    let mut last = Out { code: None, text: String::new() };
    for n in 1..=CONTENTION_TRIES {
        last = exec(&argv);
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

/// Whether a driver runs under `unit`, read from the unit's typed state (law-lifecycle-state-is-not-text).
#[derive(Debug, PartialEq, Eq)]
pub enum Driver {
    Running,
    NotRunning,
    /// The user manager did not answer: no second driver may be started on the guess.
    CannotTell(String),
}

pub fn driver(sys: &dyn Sys, unit: &str) -> Driver {
    match sys.unit_state(unit) {
        Ok(UnitState::Active | UnitState::Activating) => Driver::Running,
        Ok(UnitState::Deactivating | UnitState::Inactive | UnitState::Failed | UnitState::Absent) => Driver::NotRunning,
        Err(why) => Driver::CannotTell(why),
    }
}

/// [`driver`], with "cannot tell" as the refusal that starts nothing.
fn running(sys: &dyn Sys, unit: &str) -> Result<bool, String> {
    match driver(sys, unit) {
        Driver::Running => Ok(true),
        Driver::NotRunning => Ok(false),
        Driver::CannotTell(why) => Err(format!("cannot tell whether {unit} is running ({why}); nothing was started")),
    }
}

/// Refuses a round that is not open; finds a running driver or starts one.
pub fn start(sys: &dyn Sys, l: &Launch, batch: &str, repo: &str) -> Result<Started, String> {
    require_open(&status(sys, repo)?, repo, batch)?;
    let unit = unit_for(batch);
    if running(sys, &unit)? {
        return Ok(Started::Running(unit));
    }
    let vtime = spira_config::vtime::passthrough().into_iter().map(|(k, v)| format!("--setenv={k}={v}"));
    let args: Vec<String> = [
        "--user".to_string(),
        format!("--unit={unit}"),
        "--collect".into(),
        "--quiet".into(),
        format!("--setenv=SPIRA_TOML={}", l.toml),
    ]
    .into_iter()
    .chain(vtime)
    .chain([
        l.exe.display().to_string(),
        "round-recert".into(),
        "--drive".into(),
        "--batch".into(),
        batch.into(),
        "--repo".into(),
        repo.into(),
    ])
    .collect();
    let o = sys.exec("systemd-run", &args, QUERY_DEADLINE_SECS);
    if o.code == Some(0) {
        return Ok(Started::Started(unit));
    }
    if running(sys, &unit)? {
        return Ok(Started::Running(unit));
    }
    Err(format!("systemd-run could not start {unit}: {}", o.text.trim()))
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

const UNNAMED: &str = "incomplete, no red named";

/// What certify said about a pass that did not come out green: the red steps it named, the fault it named, or incomplete.
fn verdict(text: &str) -> String {
    let named = text.lines().find_map(|l| l.trim().strip_prefix("red=")).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).or_else(|| {
        text.lines().find_map(|l| {
            let rest = l.split_once(" is RED at ")?.1;
            let steps = rest.split_once(": ")?.1.split(" runner=").next()?.trim();
            Some(steps.to_string()).filter(|s| !s.is_empty())
        })
    });
    let faults = || text.lines().filter_map(|l| l.trim().strip_prefix("fault=")).map(str::trim);
    // A line that says what cut the pass outranks the over-cap handler's counts; the last of either is how certify ended.
    let fault = faults().filter(|s| s.starts_with("FAULT")).next_back().or_else(|| faults().filter(|s| s.starts_with("over-cap") || s.starts_with("capacity")).next_back());
    match (named, fault) {
        (Some(steps), _) => format!("RED: {steps}"),
        (None, _) if text.contains("red-output=") => "RED".to_string(),
        (None, Some(fault)) => fault.to_string(),
        (None, None) => UNNAMED.to_string(),
    }
}

fn not_green_line(batch: &str, cert: &Out) -> String {
    let named = verdict(&cert.text);
    let said = (named == UNNAMED).then(|| cert.text.lines().rev().find(|l| l.starts_with("queue.sh "))).flatten();
    format!(
        "round-recert: {batch} is not green: {named} (certify rc={}) — round left open{}",
        cert.code.map_or("signal".into(), |c| c.to_string()),
        said.map(|l| format!("; certify last said: {l}")).unwrap_or_default()
    )
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
    let verb = |verb: &str, deadline: u64| queue(sys, &[verb, batch, repo, "--by", RECERT_ACTOR], deadline);
    let mut cert = verb("certify", CERTIFY_DEADLINE_SECS);
    println!("{}", tail(&cert.text, 4));
    if cert.code == Some(VM_NO_OUTPUT) {
        println!("round-recert: {batch}: the round VM produced no output; certifying once more on a fresh VM");
        cert = verb("certify", CERTIFY_DEADLINE_SECS);
        println!("{}", tail(&cert.text, 4));
        if cert.code == Some(VM_NO_OUTPUT) {
            println!("round-recert: {batch} is parked: {NO_OUTPUT_PARKED} (certify rc={VM_NO_OUTPUT}) — round left open");
            return 1;
        }
    }
    if cert.code != Some(0) {
        println!("{}", not_green_line(batch, &cert));
        return 1;
    }
    let land = verb("land", LAND_DEADLINE_SECS);
    println!("{}", tail(&land.text, 4));
    // An unreadable status says nothing about the round: only a listing that still names it is "still open".
    let open = status(sys, repo).map(|s| open_rounds(&s).iter().any(|r| r.batch == batch));
    match (land.code, open) {
        (Some(0), Ok(false) | Err(_)) => println!("round-recert: {batch} landed at {}", round.head),
        (Some(LAND_DEPLOY_FAULT_EXIT), Ok(false) | Err(_)) => println!("round-recert: {batch} landed at {} but its release was not activated", round.head),
        (c, open) => {
            println!("round-recert: LAND FAILED for {batch} (land rc={}, round {}) — see above", c.map_or("signal".into(), |c| c.to_string()), match open { Ok(false) => "gone", Ok(true) => "still open", Err(_) => "unknown" });
            return 1;
        }
    }
    0
}

const RECERT_ACTOR: &str = "operator";

/// The manager knows a unit by its full name; asked for a bare one it answers InvalidArgs.
fn service_name(unit: &str) -> String {
    if unit.ends_with(".service") { unit.to_string() } else { format!("{unit}.service") }
}

fn state_in(units: &dyn runtime::units::Units, unit: &str) -> Result<UnitState, String> {
    units.facts(&runtime::units::UnitHandle::named(&service_name(unit))).map(|f| f.state).map_err(|e| format!("{unit}: {e}"))
}

struct Real {
    env: Vec<(String, String)>,
    path: String,
}

impl Real {
    fn command(&self, program: &str, args: &[String], deadline_secs: u64) -> Result<spira_config::bounded::Bounded, Out> {
        let Some(bin) = crate::vm_repro::which(program, &self.path) else {
            return Err(Out { code: None, text: format!("{program} is not on the launcher's PATH ({})\n", self.path) });
        };
        let mut c = spira_config::bounded::bounded_for(bin, &deadline_secs.to_string());
        c.args(args).env_clear().envs(self.env.iter().cloned()).stdin(Stdio::null());
        Ok(c)
    }
}

impl Sys for Real {
    fn exec(&self, program: &str, args: &[String], deadline_secs: u64) -> Out {
        let mut c = match self.command(program, args, deadline_secs) {
            Ok(c) => c,
            Err(o) => return o,
        };
        match c.output() {
            Ok(o) => Out { code: o.status.code(), text: format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)) },
            Err(e) => Out { code: None, text: format!("cannot run {program}: {e}\n") },
        }
    }

    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }

    fn unit_state(&self, unit: &str) -> Result<UnitState, String> {
        let clock: std::sync::Arc<dyn runtime::ports::Clock> = std::sync::Arc::new(runtime::ports::WallClock);
        let units = runtime::units::from_env(clock).map_err(|e| format!("the user manager: {e}"))?.units();
        state_in(&*units, unit)
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
    env.extend(spira_config::vtime::passthrough().into_iter().map(|(k, v)| (k.to_string(), v)));
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

    #[test_id(t9zoucp)]
    #[test]
    fn every_open_round_in_a_status_is_read_with_its_phase_and_head() {
        let r = open_rounds(STATUS);
        assert_eq!(r.len(), 2);
        assert_eq!((r[1].batch.as_str(), r[1].phase.as_str(), r[1].head.as_str()), ("r-2", "green", "bbb"));
        assert!(open_rounds("round=none\nrepo=spira\n").is_empty());
    }

    #[test_id(t9koa9z)]
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
        fn unit_state(&self, _: &str) -> Result<UnitState, String> {
            Ok(UnitState::Inactive)
        }
    }

    #[test_id(tsu9e9d)]
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

    #[test_id(tinx9ua)]
    #[test]
    fn a_red_certify_names_its_red_step_and_only_a_verdictless_one_is_incomplete() {
        let printed = "round r is RED at e198ebb: unit-tests lint\nred=unit-tests lint\nmender filed\n";
        assert_eq!(verdict(printed), "RED: unit-tests lint");
        assert_eq!(verdict("round r is RED at e198ebb: unit-tests\n"), "RED: unit-tests");
        assert_eq!(verdict("red=unit-tests\n"), "RED: unit-tests");
        assert_eq!(verdict("timed out waiting\n"), "incomplete, no red named");
        assert_eq!(
            verdict("queue.sh round certify: FAULT settings-refused phase=suites rc=2: batch: K is not set\nfault=FAULT settings-refused phase=suites rc=2: batch: K is not set\n"),
            "FAULT settings-refused phase=suites rc=2: batch: K is not set"
        );
        assert_eq!(verdict("fault=infra-fault\n"), "incomplete, no red named", "only a named fault line");
        assert_eq!(verdict("red=\nround r is RED at abc: lint\n"), "RED: lint");
        assert_eq!(verdict("red=\nround r is RED at abc: lint runner=ec2 (host io pressure full avg300 55.0 above 50.0) runner_changed=host->ec2\n"), "RED: lint");
        assert_eq!(verdict("red-output=/tmp/x\n"), "RED");
        let line = not_green_line("r-1", &Out { code: Some(1), text: "red=unit-tests\n".into() });
        assert!(line.contains("RED: unit-tests") && !line.contains("incomplete"), "{line}");
    }

    #[test_id(t8upayh)]
    #[test]
    fn a_cut_pass_is_named_by_its_over_cap_or_capacity_line() {
        let hung = "FAULT over-cap guard=hang-guard phase=entry elapsed=1806s guard=1800s cap=900s";
        assert_eq!(verdict(&format!("fault={hung}\n")), hung);
        assert_eq!(verdict("fault=over-cap unreached=3\n"), "over-cap unreached=3");
        assert_eq!(verdict("fault=capacity unreached=0,0\n"), "capacity unreached=0,0");
        assert_eq!(verdict("fault=over-cap unreached=3\nfault=capacity unreached=3,3\n"), "capacity unreached=3,3", "the last cut line is how the certify ended");
        let twice = format!("fault={hung}\nfault=over-cap unreached=0\nfault={hung}\nfault=capacity unreached=0,0\n");
        assert_eq!(verdict(&twice), hung, "a line that names the phase outranks the handler's counts");
    }

    #[test_id(tqf470o)]
    #[test]
    fn a_pass_with_nothing_named_carries_certifys_own_last_explanation() {
        let text = "queue.sh round certify: first\nqueue.sh round certify: the CI entry never ran; the round is not judged\nround-vm: stop VM\nround-vm: destroy VM\n";
        let line = not_green_line("r-1", &Out { code: Some(4), text: text.into() });
        assert!(line.contains("incomplete, no red named") && line.contains("queue.sh round certify: the CI entry never ran") && !line.contains("first"), "{line}");
        assert_eq!(line.lines().count(), 1, "{line}");
        let named = not_green_line("r-1", &Out { code: Some(4), text: format!("fault=capacity unreached=0,0\n{text}") });
        assert!(!named.contains("queue.sh"), "{named}");
    }

    #[test_id(tl047q9)]
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

    fn drive_answers(certifies: &[(i32, &str)]) -> Fake {
        let out = |code, text: &str| Out { code: Some(code), text: text.to_string() };
        let mut answers = vec![out(0, "batch_id=r-1\nphase=certifying\nhead=aaa\nround=open\n")];
        answers.extend(certifies.iter().map(|(c, t)| out(*c, t)));
        Fake { answers: RefCell::new(answers), slept: RefCell::new(vec![]) }
    }

    #[test_id(t8se8cm)]
    #[test]
    fn a_vm_that_produced_no_output_is_certified_once_more_and_a_second_parks_the_round() {
        let once = drive_answers(&[(VM_NO_OUTPUT, "fault=vm-no-output\n"), (1, "red=unit-tests\n")]);
        assert_eq!(drive(&once, "r-1", "spira"), 1);
        assert!(once.answers.borrow().is_empty(), "the re-drive ran certify again and judged what it said");
        let twice = drive_answers(&[(VM_NO_OUTPUT, "fault=vm-no-output\n"), (VM_NO_OUTPUT, "fault=vm-no-output\n")]);
        assert_eq!(drive(&twice, "r-1", "spira"), 1);
        assert!(twice.answers.borrow().is_empty(), "never a third certify");
    }

    #[test_id(txcr39d)]
    #[test]
    fn a_landed_round_whose_status_read_then_fails_is_still_landed_and_a_land_that_failed_is_not() {
        let landed = drive_answers(&[(0, ""), (0, "landed\n"), (2, "cannot tell: the lifecycle service took the request but its reply was lost (no reply)\n")]);
        assert_eq!(drive(&landed, "r-1", "spira"), 0, "the land verb said 0; a status that cannot be read does not undo it");
        let refused = drive_answers(&[(0, ""), (1, "refused\n"), (2, "no reply\n")]);
        assert_eq!(drive(&refused, "r-1", "spira"), 1);
        let open = drive_answers(&[(0, ""), (0, "landed\n"), (0, "batch_id=r-1\nphase=green\nhead=aaa\nround=open\n")]);
        assert_eq!(drive(&open, "r-1", "spira"), 1, "a listing that still names the round is a land that did not take");
    }

    #[test_id(t2jogib)]
    #[test]
    fn a_second_certify_after_a_no_output_vm_that_is_green_goes_on_to_land() {
        let fake = drive_answers(&[(VM_NO_OUTPUT, ""), (0, ""), (0, "landed\n"), (0, "round=none\n")]);
        assert_eq!(drive(&fake, "r-1", "spira"), 0);
    }

    /// Answers as the user manager does: a name without its type suffix is InvalidArgs.
    struct FullNamesOnly;

    impl runtime::units::Units for FullNamesOnly {
        fn start(&self, _: &runtime::units::UnitSpec) -> runtime::Result<runtime::units::UnitHandle> {
            unimplemented!()
        }
        fn stop(&self, _: &runtime::units::UnitHandle) -> runtime::Result<()> {
            unimplemented!()
        }
        fn restart(&self, _: &runtime::units::UnitHandle) -> runtime::Result<()> {
            unimplemented!()
        }
        fn facts(&self, h: &runtime::units::UnitHandle) -> runtime::Result<runtime::units::UnitFacts> {
            if h.name.ends_with(".service") {
                Ok(runtime::units::UnitFacts { state: UnitState::Absent, main_pid: None, exit: None, since_ms: None })
            } else {
                Err(runtime::RuntimeError::new(format!("{}: LoadUnit: org.freedesktop.DBus.Error.InvalidArgs", h.name)))
            }
        }
    }

    #[test_id(t7y5l4p)]
    #[test]
    fn a_driver_unit_is_asked_of_the_manager_by_its_service_name_and_an_absent_one_is_not_running() {
        assert_eq!(state_in(&FullNamesOnly, "round-recert-b"), Ok(UnitState::Absent));
        assert_eq!(state_in(&FullNamesOnly, "round-recert-b.service"), Ok(UnitState::Absent));
        assert_eq!(driver(&Fixed(state_in(&FullNamesOnly, "round-recert-b")), "round-recert-b"), Driver::NotRunning);
    }

    struct Fixed(Result<UnitState, String>);

    impl Sys for Fixed {
        fn exec(&self, _: &str, _: &[String], _: u64) -> Out {
            unimplemented!()
        }
        fn sleep(&self, _: u64) {}
        fn unit_state(&self, _: &str) -> Result<UnitState, String> {
            self.0.clone()
        }
    }

    struct Recorder(RefCell<Vec<(String, Vec<String>)>>);

    impl Sys for Recorder {
        fn exec(&self, prog: &str, args: &[String], _: u64) -> Out {
            self.0.borrow_mut().push((prog.to_string(), args.to_vec()));
            Out { code: Some(0), text: "batch_id=b\nphase=opened\n".into() }
        }
        fn sleep(&self, _: u64) {}
        fn unit_state(&self, _: &str) -> Result<UnitState, String> {
            Ok(UnitState::Inactive)
        }
    }

    #[test_id(tn506cy)]
    #[test]
    fn every_verb_the_pass_drives_names_the_operator_as_its_actor() {
        let rec = Recorder(RefCell::new(vec![]));
        drive(&rec, "b", "r");
        let calls = rec.0.borrow();
        for verb in ["certify", "land"] {
            let call = calls.iter().find(|(_, a)| a.get(1).map(String::as_str) == Some(verb)).unwrap_or_else(|| panic!("{verb} was not run"));
            assert_eq!(call.1, ["round", verb, "b", "r", "--by", "operator"]);
        }
    }

    #[test_id(tkaity9)]
    #[test]
    fn the_unit_runs_the_round_recert_subcommand_of_the_release_binary() {
        let rec = Recorder(RefCell::new(vec![]));
        let l = Launch { exe: Path::new("rel/bin/release"), toml: "t.toml" };
        assert!(matches!(start(&rec, &l, "b", "r"), Ok(Started::Started(_))));
        let calls = rec.0.borrow();
        let run = calls.iter().find(|(p, _)| p == "systemd-run").expect("systemd-run was called");
        assert!(run.1.join(" ").ends_with("rel/bin/release round-recert --drive --batch b --repo r"), "{:?}", run.1);
    }

    fn started_argv(rec: &Recorder) -> Vec<String> {
        let l = Launch { exe: Path::new("rel/bin/release"), toml: "t.toml" };
        assert!(matches!(start(rec, &l, "b", "r"), Ok(Started::Started(_))));
        let calls = rec.0.borrow();
        calls.iter().find(|(p, _)| p == "systemd-run").expect("systemd-run was called").1.clone()
    }

    #[test_id(tx5tg9z)]
    #[test]
    fn the_unit_carries_the_virtual_clock_before_the_command() {
        let _env = testkit::env(&[
            ("SPIRA_NOW", Some("1700000000")),
            ("GIT_AUTHOR_DATE", Some("1700000000 +0000")),
            ("GIT_COMMITTER_DATE", Some("1700000001 +0000")),
        ]);
        let argv = started_argv(&Recorder(RefCell::new(vec![])));
        let exe = argv.iter().position(|a| a == "rel/bin/release").unwrap();
        for want in ["SPIRA_NOW=1700000000", "GIT_AUTHOR_DATE=1700000000 +0000", "GIT_COMMITTER_DATE=1700000001 +0000"] {
            let at = argv.iter().position(|a| *a == format!("--setenv={want}")).unwrap_or_else(|| panic!("{want} is passed to the unit: {argv:?}"));
            assert!(at < exe, "{argv:?}");
        }
    }

    #[test_id(t9htx1v)]
    #[test]
    fn the_unit_carries_no_virtual_clock_when_none_is_set() {
        let _env = testkit::env(&[("SPIRA_NOW", None), ("GIT_AUTHOR_DATE", None), ("GIT_COMMITTER_DATE", None)]);
        let argv = started_argv(&Recorder(RefCell::new(vec![])));
        let want = ["--user", "--unit=round-recert-b", "--collect", "--quiet", "--setenv=SPIRA_TOML=t.toml", "rel/bin/release", "round-recert", "--drive", "--batch", "b", "--repo", "r"];
        assert_eq!(argv, want);
    }

    /// systemd answering nothing about the driver's unit is "cannot tell", never "not running":
    /// read as not running, a second driver is started on a batch that may already have one.
    struct Manager(RefCell<Vec<String>>, Result<UnitState, String>);

    impl Sys for Manager {
        fn exec(&self, prog: &str, _: &[String], _: u64) -> Out {
            self.0.borrow_mut().push(prog.to_string());
            Out { code: Some(0), text: "batch_id=b\nphase=opened\n".into() }
        }
        fn sleep(&self, _: u64) {}
        fn unit_state(&self, _: &str) -> Result<UnitState, String> {
            self.1.clone()
        }
    }

    #[test_id(td4ogdl)]
    #[test]
    fn a_driver_unit_systemd_cannot_answer_for_is_never_started_again() {
        let sys = Manager(RefCell::new(vec![]), Err("Failed to connect to bus: No medium found".into()));
        let l = Launch { exe: Path::new("rel/bin/release"), toml: "t.toml" };
        let e = start(&sys, &l, "b", "r").unwrap_err();
        assert!(e.contains("cannot tell whether round-recert-b is running"), "{e}");
        assert!(!sys.0.borrow().iter().any(|p| p == "systemd-run"), "nothing is started: {:?}", sys.0.borrow());
        let up = Manager(RefCell::new(vec![]), Ok(UnitState::Activating));
        assert_eq!(start(&up, &l, "b", "r"), Ok(Started::Running("round-recert-b".into())));
        assert!(!up.0.borrow().iter().any(|p| p == "systemd-run"), "a running driver is not started twice");
    }

    #[test_id(tmfv0ob)]
    #[test]
    fn a_unit_name_carries_the_batch_so_one_batch_has_one_driver() {
        assert_eq!(unit_for("r-auto-622"), "round-recert-r-auto-622");
        assert!(valid_token("r-auto-622") && !valid_token("a/b") && !valid_token("-x") && !valid_token(""));
    }
}
