//! `round-recert --batch <id> --repo <repo>` (DESIGN.md "round-recert"): one certification pass
//! of an open round, certify then land, run in a transient user unit so the caller's deadline
//! covers starting it and a restart of the caller does not kill it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use round_controller::death::HAND_HOLDER;
use round_controller::lease::{acquire, token_word, Lease, LeaseError, Resource};
use round_controller::liveness::Phase;
use round_controller::ports::{Mark, Ports};
use round_controller::run::{phase_of, token_of_word};
use round_controller::slots::{grant_for_hand, Pool, Slot, SlotGrant};
use runtime::units::UnitState;
use state_store::Rev;

pub const USAGE: &str = "usage: round-recert --batch <id> --repo <repo>";

pub const UNIT_PREFIX: &str = "round-recert-";
pub const CERTIFY_DEADLINE_SECS: u64 = 7200;
pub const LAND_DEADLINE_SECS: u64 = 3600;
pub const QUERY_DEADLINE_SECS: u64 = 120;
pub const CONTENTION_WAIT_SECS: u64 = 15;
pub const CONTENTION_TRIES: u32 = 20;
pub const LAND_DEPLOY_FAULT_EXIT: i32 = 3;
use round_controller::judge::VM_NO_OUTPUT;
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
    /// [`Sys::exec`], calling `tick` about every `every_secs` while the program runs; once `tick`
    /// answers false the program is sent SIGTERM and waited for.
    fn exec_ticking(&self, program: &str, args: &[String], deadline_secs: u64, every_secs: u64, tick: &mut dyn FnMut() -> bool) -> Out;
    fn sleep(&self, secs: u64);
    fn now_ms(&self) -> u64;
    /// round-vm's latest progress mark, if it has written one.
    fn progress(&self) -> Option<Mark>;
    /// The user manager's typed state of `unit` (its `ActiveState` property, over the unit
    /// port); `Err` when the manager gave no answer, which is "cannot tell", never "inactive".
    fn unit_state(&self, unit: &str) -> Result<UnitState, String>;
}

pub fn valid_token(s: &str) -> bool {
    !s.is_empty() && !s.starts_with(['-', '.']) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The driver's unit: under a lease it carries the token, so a re-drive never shares a name with
/// the driver it replaces (`round_controller::death::hand_unit_name`).
pub fn unit_for(batch: &str, token: Option<Rev>) -> String {
    match token {
        Some(t) => format!("{UNIT_PREFIX}{batch}-{}", token_word(t)),
        None => format!("{UNIT_PREFIX}{batch}"),
    }
}

/// The round lease and runner slot a hand drive holds, from the round controller's own pools, so a
/// hand round and a controller round never run two VMs on one slot.
pub struct Hold {
    pub lease: Lease,
    pub slot: SlotGrant,
}

/// Why no hold was taken.
#[derive(Debug, PartialEq, Eq)]
pub enum NotTaken {
    /// Another round-recert holds the round: its unit.
    Driving(String),
    Refused(String),
}

impl Hold {
    pub fn take(p: &Ports, repo: &str, batch: &str) -> Result<Hold, NotTaken> {
        let lease = match acquire(p, &Resource::round(repo, batch), HAND_HOLDER) {
            Ok(l) => l,
            Err(LeaseError::Held(h)) if h.holder.as_deref() == Some(HAND_HOLDER) => return Err(NotTaken::Driving(unit_for(batch, Some(h.token)))),
            Err(LeaseError::Held(h)) => {
                return Err(NotTaken::Refused(format!(
                    "round {batch} is leased by {} under token {}, {}s left — only its holder drives it; it is free once the lease lapses, or once `round-controller lease clear --repo {repo} --batch {batch}` proves its holder dead",
                    h.holder.as_deref().unwrap_or("a holder no record names"),
                    token_word(h.token),
                    h.ttl_left_ms / 1000
                )))
            }
            Err(LeaseError::Terminal) => return Err(NotTaken::Refused(format!("round {batch} has ended"))),
            Err(e) => return Err(NotTaken::Refused(format!("cannot take the lease of round {batch}: {e:?}"))),
        };
        match grant_for_hand(p, HAND_HOLDER) {
            Ok(slot) => Ok(Hold { lease, slot }),
            Err(no) => {
                let _ = lease.release(p);
                Err(NotTaken::Refused(no.to_string()))
            }
        }
    }

    /// The hold `start` took, by the tokens it handed the driver's unit.
    pub fn handed(p: &Ports, repo: &str, batch: &str, lease: Rev, slot: Slot, slot_token: Rev) -> Hold {
        Hold { lease: Lease::handed(Resource::round(repo, batch), lease), slot: SlotGrant::handed(p, slot, slot_token) }
    }

    fn args(&self) -> Vec<String> {
        vec!["--lease-token".into(), token_word(self.lease.token()), "--slot".into(), self.slot.slot().to_string(), "--slot-token".into(), token_word(self.slot.token())]
    }

    pub fn release(self, p: &Ports) {
        let _ = self.lease.release(p);
        let _ = self.slot.release(p);
    }
}

/// Renews the hold while certify runs, under the controller's rule: only while round-vm's last
/// progress mark is inside its phase's deadline and the pass inside its maximum wall.
struct Keeper<'a, 'p> {
    p: &'a Ports<'p>,
    hold: &'a mut Hold,
    unit: String,
    batch: String,
    /// The round's head once status names it: round-vm's progress file is the run directory's,
    /// so a mark is this pass's only when it names this round, as the runner reads it.
    head: Option<String>,
    vm: Option<String>,
    started: u64,
    next_renew: u64,
    renewed: u64,
    lost: Option<String>,
}

impl Keeper<'_, '_> {
    /// Renews the hold when due; false once it is lost, so the verb running under it is stopped.
    fn tick(&mut self, now: u64, mark: Option<Mark>) -> bool {
        if self.lost.is_some() {
            return false;
        }
        let ours = |m: &Mark| m.at_ms >= self.started && (Some(&m.round) == self.head.as_ref() || m.round == self.batch);
        let mark = mark.filter(ours);
        if let Some(vm) = mark.as_ref().and_then(|m| m.vm.clone()).filter(|v| Some(v) != self.vm.as_ref()) {
            if self.hold.slot.record_holder(self.p, &self.unit, Some(&vm)).is_ok() {
                self.vm = Some(vm);
            }
        }
        if now < self.next_renew {
            return true;
        }
        self.next_renew = now + self.p.leases.renew_every_ms();
        // Until round-vm writes a mark for this pass, the pass is booting.
        let (last, phase) = mark.map_or((self.started, Phase::Boot), |m| (m.at_ms, phase_of(&m.phase)));
        if !self.p.leases.gate().may_renew(now, last, phase, self.started) {
            self.lost = Some(format!("no progress mark inside the {} deadline, or the pass is past its maximum wall", phase.word()));
            return false;
        }
        match (self.hold.lease.renew(self.p), self.hold.slot.renew(self.p)) {
            (Ok(()), Ok(())) => self.renewed = now,
            (Err(LeaseError::Lost), _) | (_, Err(LeaseError::Lost)) => self.lost = Some("the lease or the slot lapsed or was taken under another token".into()),
            // The store did not answer: the hold stands until its TTL from the last renewal.
            _ if now.saturating_sub(self.renewed) + self.p.leases.renew_every_ms() < self.p.leases.ttl_ms => {}
            (l, s) => self.lost = Some(format!("renewal failed past the TTL: lease {l:?}, slot {s:?}")),
        }
        self.lost.is_none()
    }
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

/// [`queue`] for a verb the hold is renewed across.
fn queue_ticking(sys: &dyn Sys, args: &[&str], deadline: u64, every_secs: u64, tick: &mut dyn FnMut() -> bool) -> Out {
    queue_run(sys, args, &mut |argv| sys.exec_ticking("queue", argv, deadline, every_secs, tick))
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

/// Refuses a round that is not open; finds a running driver or starts one. With `pools`, the
/// driver first takes the round's lease and a runner slot, and refuses naming their holders.
pub fn start(sys: &dyn Sys, l: &Launch, batch: &str, repo: &str, pools: Option<&Ports>) -> Result<Started, String> {
    require_open(&status(sys, repo)?, repo, batch)?;
    let hold = match pools.map(|p| Hold::take(p, repo, batch)) {
        None => None,
        Some(Ok(h)) => Some(h),
        Some(Err(NotTaken::Driving(unit))) => return Ok(Started::Running(unit)),
        Some(Err(NotTaken::Refused(why))) => return Err(why),
    };
    let mut hold = hold;
    let unit = unit_for(batch, hold.as_ref().map(|h| h.lease.token()));
    if hold.is_none() && running(sys, &unit)? {
        return Ok(Started::Running(unit));
    }
    // The slot names its unit before the unit exists: a holder that recorded none is taken to
    // have launched none, so its slot would be granted again without proof of death.
    if let (Some(h), Some(p)) = (hold.as_mut(), pools) {
        if let Err(e) = h.slot.record_holder(p, &unit, None) {
            let why = format!("cannot record {unit} as the holder of slot {}: {e:?}", h.slot.slot());
            if let Some(h) = hold {
                h.release(p);
            }
            return Err(why);
        }
    }
    let mut args: Vec<String> = [
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
    args.extend(hold.as_ref().map(Hold::args).unwrap_or_default());
    let o = sys.exec("systemd-run", &args, QUERY_DEADLINE_SECS);
    if o.code == Some(0) {
        return Ok(Started::Started(unit));
    }
    if hold.is_none() && running(sys, &unit)? {
        return Ok(Started::Running(unit));
    }
    if let (Some(h), Some(p)) = (hold, pools) {
        h.release(p);
    }
    Err(format!("systemd-run could not start {unit}: {}", o.text.trim()))
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// What certify said about a pass that did not come out green: the red steps it named, the fault it named, or incomplete.
fn verdict(text: &str) -> String {
    let named = text.lines().find_map(|l| l.trim().strip_prefix("red=")).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).or_else(|| {
        text.lines().find_map(|l| {
            let rest = l.split_once(" is RED at ")?.1;
            Some(rest.split_once(": ")?.1.trim().to_string()).filter(|s| !s.is_empty())
        })
    });
    let fault = text.lines().find_map(|l| l.trim().strip_prefix("fault=")).map(str::trim).filter(|s| s.starts_with("FAULT"));
    match (named, fault) {
        (Some(steps), _) => format!("RED: {steps}"),
        (None, _) if text.contains("red-output=") => "RED".to_string(),
        (None, Some(fault)) => fault.to_string(),
        (None, None) => "incomplete, no red named".to_string(),
    }
}

fn not_green_line(batch: &str, cert: &Out) -> String {
    format!("round-recert: {batch} is not green: {} (certify rc={}) — round left open", verdict(&cert.text), cert.code.map_or("signal".into(), |c| c.to_string()))
}

/// The pass itself: certify, and land only on green. 0 landed, 1 anything else. Under a hold,
/// every verb carries its lease token, the hold is renewed while each runs, and it is given back
/// at the end whatever the outcome.
pub fn drive(sys: &dyn Sys, batch: &str, repo: &str, held: Option<(&Ports, Hold)>) -> u8 {
    let Some((p, mut hold)) = held else {
        return drive_pass(sys, batch, repo, None);
    };
    let now = sys.now_ms();
    let mut keeper = Keeper {
        p,
        unit: unit_for(batch, Some(hold.lease.token())),
        hold: &mut hold,
        batch: batch.into(),
        head: None,
        vm: None,
        started: now,
        next_renew: 0,
        renewed: now,
        lost: None,
    };
    let rc = drive_pass(sys, batch, repo, Some(&mut keeper));
    let lost = keeper.lost.take();
    hold.release(p);
    if let Some(why) = lost {
        println!("round-recert: lost the hold on {batch}: {why}");
        if rc != 0 {
            return 1;
        }
    }
    rc
}

/// One verb of the pass: under a hold, with its token and the hold renewed while it runs.
fn verb(sys: &dyn Sys, keeper: &mut Option<&mut Keeper>, verb: &str, batch: &str, repo: &str, deadline: u64) -> Out {
    let Some(k) = keeper.as_deref_mut() else {
        return queue(sys, &[verb, batch, repo], deadline);
    };
    let token = token_word(k.hold.lease.token());
    let every = (k.p.leases.renew_every_ms() / 1000).max(1);
    let mut tick = || k.tick(sys.now_ms(), sys.progress());
    queue_ticking(sys, &[verb, batch, repo, "--lease-token", &token], deadline, every, &mut tick)
}

fn drive_pass(sys: &dyn Sys, batch: &str, repo: &str, mut keeper: Option<&mut Keeper>) -> u8 {
    if let Some(k) = keeper.as_deref_mut() {
        k.tick(sys.now_ms(), sys.progress());
    }
    let round = match status(sys, repo).and_then(|s| require_open(&s, repo, batch)) {
        Ok(r) => r,
        Err(e) => {
            println!("round-recert: {e}");
            return 1;
        }
    };
    if let Some(k) = keeper.as_deref_mut() {
        k.head = Some(round.head.clone());
    }
    println!("round-recert: driving {batch} ({}) at {}", round.phase, round.head);
    let mut cert = verb(sys, &mut keeper, "certify", batch, repo, CERTIFY_DEADLINE_SECS);
    println!("{}", tail(&cert.text, 4));
    if cert.code == Some(VM_NO_OUTPUT) {
        println!("round-recert: {batch}: the round VM produced no output; certifying once more on a fresh VM");
        cert = verb(sys, &mut keeper, "certify", batch, repo, CERTIFY_DEADLINE_SECS);
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
    if let Some(why) = keeper.as_ref().and_then(|k| k.lost.clone()) {
        println!("round-recert: {batch} is green but the hold on it was lost ({why}) — not landing it; round left open");
        return 1;
    }
    let land = verb(sys, &mut keeper, "land", batch, repo, LAND_DEADLINE_SECS);
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
    /// The run directory round-vm writes its progress file in.
    run: Option<PathBuf>,
}

impl Real {
    fn command(&self, program: &str, args: &[String], deadline_secs: u64) -> Result<std::process::Command, Out> {
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

    fn exec_ticking(&self, program: &str, args: &[String], deadline_secs: u64, every_secs: u64, tick: &mut dyn FnMut() -> bool) -> Out {
        let mut c = match self.command(program, args, deadline_secs) {
            Ok(c) => c,
            Err(o) => return o,
        };
        let mut child = match c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(ch) => ch,
            Err(e) => return Out { code: None, text: format!("cannot run {program}: {e}\n") },
        };
        // Both pipes are drained while it runs, so a chatty child never blocks on a full one.
        let drain = |r: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut s = Vec::new();
                if let Some(mut r) = r {
                    let _ = r.read_to_end(&mut s);
                }
                s
            })
        };
        let out = drain(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
        let err = drain(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
        let (mut waited, mut stopped) = (0, false);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Ok(s),
                Ok(None) => {}
                Err(e) => break Err(e),
            }
            // The bound is `timeout`, which passes the signal on to the verb it runs.
            if !stopped && waited % every_secs == 0 && !tick() {
                stopped = true;
                let _ = std::process::Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
            waited += 1;
        };
        let text = |h: std::thread::JoinHandle<Vec<u8>>| String::from_utf8_lossy(&h.join().unwrap_or_default()).into_owned();
        let (o, e) = (text(out), text(err));
        match status {
            Ok(s) => Out { code: s.code(), text: format!("{o}{e}") },
            Err(x) => Out { code: None, text: format!("cannot wait for {program}: {x}\n{o}{e}") },
        }
    }

    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }

    fn now_ms(&self) -> u64 {
        spira_config::vtime::now_epoch().saturating_mul(1000)
    }

    fn progress(&self) -> Option<Mark> {
        let file = self.run.as_ref()?.join(round_controller::real::child::PROGRESS_FILE);
        round_controller::real::child::mark_of(&std::fs::read_to_string(file).ok()?)
    }

    fn unit_state(&self, unit: &str) -> Result<UnitState, String> {
        let clock: std::sync::Arc<dyn runtime::ports::Clock> = std::sync::Arc::new(runtime::ports::WallClock);
        let units = runtime::units::from_env(clock).map_err(|e| format!("the user manager: {e}"))?.units();
        units.facts(&runtime::units::UnitHandle::named(unit)).map(|f| f.state).map_err(|e| format!("{unit}: {e}"))
    }
}

struct Args {
    batch: String,
    repo: String,
    drive: bool,
    /// The hold `start` handed the driver: lease token, slot and slot token.
    hold: Option<(Rev, Slot, Rev)>,
}

fn slot_of_word(w: &str) -> Option<Slot> {
    let (pool, n) = w.split_once('/')?;
    Some(Slot { pool: Pool::from_word(pool)?, n: n.parse().ok()? })
}

fn parse(args: &[String]) -> Result<Args, String> {
    let (mut batch, mut repo, mut drive) = (None, None, false);
    let (mut lease, mut slot, mut slot_token) = (None, None, None);
    let mut it = args.iter();
    while let Some(x) = it.next() {
        let mut val = |n: &str| it.next().cloned().ok_or_else(|| format!("{n} needs a value"));
        match x.as_str() {
            "--batch" => batch = Some(val(x)?),
            "--repo" => repo = Some(val(x)?),
            "--drive" => drive = true,
            "--lease-token" => lease = Some(val(x)?),
            "--slot" => slot = Some(val(x)?),
            "--slot-token" => slot_token = Some(val(x)?),
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
    let hold = match (lease, slot, slot_token) {
        (None, None, None) => None,
        (Some(l), Some(s), Some(t)) => Some((
            token_of_word(&l).ok_or_else(|| format!("--lease-token {l:?} is not <main>-<sub>"))?,
            slot_of_word(&s).ok_or_else(|| format!("--slot {s:?} is not <pool>/<n>"))?,
            token_of_word(&t).ok_or_else(|| format!("--slot-token {t:?} is not <main>-<sub>"))?,
        )),
        _ => return Err("--lease-token, --slot and --slot-token go together".into()),
    };
    Ok(Args { batch, repo, drive, hold })
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
    Ok((Real { env, path, run: spira_config::resolve::run_dir_for_process().ok() }, exe, toml))
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
    // The pools live in the round controller's store, which no binary is served yet
    // (`round_controller::real::Unserved`): until one is, a hand drive takes no hold, as before.
    let pools: Option<&Ports> = None;
    if a.drive {
        return match (a.hold, pools) {
            (None, _) => drive(&sys, &a.batch, &a.repo, None),
            (Some((lease, slot, slot_token)), Some(p)) => drive(&sys, &a.batch, &a.repo, Some((p, Hold::handed(p, &a.repo, &a.batch, lease, slot, slot_token)))),
            (Some(_), None) => {
                eprintln!("round-recert: refused: a hold was handed over, and no store serves the round controller's pools here");
                1
            }
        };
    }
    match start(&sys, &Launch { exe: &exe, toml: &toml }, &a.batch, &a.repo, pools) {
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
        fn exec_ticking(&self, p: &str, a: &[String], d: u64, _: u64, _: &mut dyn FnMut() -> bool) -> Out {
            self.exec(p, a, d)
        }
        fn sleep(&self, secs: u64) {
            self.slept.borrow_mut().push(secs);
        }
        fn now_ms(&self) -> u64 {
            0
        }
        fn progress(&self) -> Option<Mark> {
            None
        }
        fn unit_state(&self, _: &str) -> Result<UnitState, String> {
            Ok(UnitState::Inactive)
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
        assert_eq!(
            verdict("queue.sh round certify: FAULT settings-refused phase=suites rc=2: batch: K is not set\nfault=FAULT settings-refused phase=suites rc=2: batch: K is not set\n"),
            "FAULT settings-refused phase=suites rc=2: batch: K is not set"
        );
        assert_eq!(verdict("fault=over-cap unreached=3\n"), "incomplete, no red named", "only a named FAULT line");
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
        assert_eq!(drive(&fake, "r-1", "spira", None), 1);
        assert!(fake.answers.borrow().is_empty());
    }

    fn drive_answers(certifies: &[(i32, &str)]) -> Fake {
        let out = |code, text: &str| Out { code: Some(code), text: text.to_string() };
        let mut answers = vec![out(0, "batch_id=r-1\nphase=certifying\nhead=aaa\nround=open\n")];
        answers.extend(certifies.iter().map(|(c, t)| out(*c, t)));
        Fake { answers: RefCell::new(answers), slept: RefCell::new(vec![]) }
    }

    #[test]
    fn a_vm_that_produced_no_output_is_certified_once_more_and_a_second_parks_the_round() {
        let once = drive_answers(&[(VM_NO_OUTPUT, "fault=vm-no-output\n"), (1, "red=unit-tests\n")]);
        assert_eq!(drive(&once, "r-1", "spira", None), 1);
        assert!(once.answers.borrow().is_empty(), "the re-drive ran certify again and judged what it said");
        let twice = drive_answers(&[(VM_NO_OUTPUT, "fault=vm-no-output\n"), (VM_NO_OUTPUT, "fault=vm-no-output\n")]);
        assert_eq!(drive(&twice, "r-1", "spira", None), 1);
        assert!(twice.answers.borrow().is_empty(), "never a third certify");
    }

    #[test]
    fn a_second_certify_after_a_no_output_vm_that_is_green_goes_on_to_land() {
        let fake = drive_answers(&[(VM_NO_OUTPUT, ""), (0, ""), (0, "landed\n"), (0, "round=none\n")]);
        assert_eq!(drive(&fake, "r-1", "spira", None), 0);
    }

    struct Recorder(RefCell<Vec<(String, Vec<String>)>>);

    impl Sys for Recorder {
        fn exec(&self, prog: &str, args: &[String], _: u64) -> Out {
            self.0.borrow_mut().push((prog.to_string(), args.to_vec()));
            Out { code: Some(0), text: "batch_id=b\nphase=opened\n".into() }
        }
        fn exec_ticking(&self, p: &str, a: &[String], d: u64, _: u64, _: &mut dyn FnMut() -> bool) -> Out {
            self.exec(p, a, d)
        }
        fn sleep(&self, _: u64) {}
        fn now_ms(&self) -> u64 {
            0
        }
        fn progress(&self) -> Option<Mark> {
            None
        }
        fn unit_state(&self, _: &str) -> Result<UnitState, String> {
            Ok(UnitState::Inactive)
        }
    }

    #[test]
    fn the_unit_runs_the_round_recert_subcommand_of_the_release_binary() {
        let rec = Recorder(RefCell::new(vec![]));
        let l = Launch { exe: Path::new("/rel/bin/release"), toml: "t.toml" };
        assert!(matches!(start(&rec, &l, "b", "r", None), Ok(Started::Started(_))));
        let calls = rec.0.borrow();
        let run = calls.iter().find(|(p, _)| p == "systemd-run").expect("systemd-run was called");
        assert!(run.1.join(" ").ends_with("/rel/bin/release round-recert --drive --batch b --repo r"), "{:?}", run.1);
    }

    /// systemd answering nothing about the driver's unit is "cannot tell", never "not running":
    /// read as not running, a second driver is started on a batch that may already have one.
    struct Manager(RefCell<Vec<String>>, Result<UnitState, String>);

    impl Sys for Manager {
        fn exec(&self, prog: &str, _: &[String], _: u64) -> Out {
            self.0.borrow_mut().push(prog.to_string());
            Out { code: Some(0), text: "batch_id=b\nphase=opened\n".into() }
        }
        fn exec_ticking(&self, p: &str, a: &[String], d: u64, _: u64, _: &mut dyn FnMut() -> bool) -> Out {
            self.exec(p, a, d)
        }
        fn sleep(&self, _: u64) {}
        fn now_ms(&self) -> u64 {
            0
        }
        fn progress(&self) -> Option<Mark> {
            None
        }
        fn unit_state(&self, _: &str) -> Result<UnitState, String> {
            self.1.clone()
        }
    }

    #[test]
    fn a_driver_unit_systemd_cannot_answer_for_is_never_started_again() {
        let sys = Manager(RefCell::new(vec![]), Err("Failed to connect to bus: No medium found".into()));
        let l = Launch { exe: Path::new("/rel/bin/release"), toml: "t.toml" };
        let e = start(&sys, &l, "b", "r", None).unwrap_err();
        assert!(e.contains("cannot tell whether round-recert-b is running"), "{e}");
        assert!(!sys.0.borrow().iter().any(|p| p == "systemd-run"), "nothing is started: {:?}", sys.0.borrow());
        let up = Manager(RefCell::new(vec![]), Ok(UnitState::Activating));
        assert_eq!(start(&up, &l, "b", "r", None), Ok(Started::Running("round-recert-b".into())));
        assert!(!up.0.borrow().iter().any(|p| p == "systemd-run"), "a running driver is not started twice");
    }

    #[test]
    fn a_unit_name_carries_the_batch_so_one_batch_has_one_driver() {
        assert_eq!(unit_for("r-auto-622", None), "round-recert-r-auto-622");
        assert!(valid_token("r-auto-622") && !valid_token("a/b") && !valid_token("-x") && !valid_token(""));
    }

    use round_controller::lease::holder_of;
    use round_controller::sim::bench::Bench;
    use round_controller::slots::SlotSettings;
    use runtime::ports::Clock;

    const OPEN: &str = "batch_id=r-1\nphase=certifying\nhead=aaa\nround=open\n";

    /// queue and systemd as a hand drive meets them, on a bench's clock: certify runs `ticks`
    /// renewal intervals of `step_ms`, and round-vm's mark advances with it unless `stalled`.
    struct Driven<'b> {
        bench: &'b Bench,
        calls: RefCell<Vec<Vec<String>>>,
        ticks: u32,
        step_ms: u64,
        stalled: bool,
        /// Another writer takes the lease from it after certify's first renewal.
        revoked: bool,
        /// The round round-vm's progress file names.
        mark_round: &'static str,
        landed: std::cell::Cell<bool>,
        /// Whether the round's lease was live under the token land was handed.
        held_at_land: std::cell::Cell<Option<bool>>,
        /// The drive stopped a verb it had lost its hold under.
        stopped: std::cell::Cell<bool>,
        /// The VM the slot record named when land ran.
        vm_at_land: RefCell<Option<String>>,
    }

    impl<'b> Driven<'b> {
        fn new(bench: &'b Bench) -> Driven<'b> {
            Driven {
                bench,
                calls: RefCell::default(),
                ticks: 5,
                step_ms: 20_000,
                stalled: false,
                revoked: false,
                mark_round: "aaa",
                landed: Default::default(),
                held_at_land: Default::default(),
                stopped: Default::default(),
                vm_at_land: Default::default(),
            }
        }
    }

    impl Sys for Driven<'_> {
        fn exec(&self, prog: &str, args: &[String], _: u64) -> Out {
            self.calls.borrow_mut().push(args.to_vec());
            match (prog, args.get(1).map(String::as_str)) {
                ("queue", Some("status")) if self.landed.get() => Out { code: Some(0), text: "round=none\n".into() },
                _ => Out { code: Some(0), text: OPEN.into() },
            }
        }
        fn exec_ticking(&self, _: &str, args: &[String], _: u64, _: u64, tick: &mut dyn FnMut() -> bool) -> Out {
            self.calls.borrow_mut().push(args.to_vec());
            if args[1] == "land" {
                let token = args.iter().skip_while(|a| *a != "--lease-token").nth(1).cloned();
                let held = holder_of(&self.bench.ports(), &Resource::round("spira", "r-1")).unwrap();
                self.held_at_land.set(Some(held.is_some_and(|h| Some(token_word(h.token)) == token)));
                let slot = Slot { pool: Pool::Proxmox, n: 0 };
                *self.vm_at_land.borrow_mut() = slot_record(self.bench, slot)["vm"].as_str().map(str::to_string);
                self.landed.set(true);
                return Out { code: Some(0), text: "landed\n".into() };
            }
            for n in 0..self.ticks {
                self.bench.clock.advance(self.step_ms);
                if !tick() {
                    self.stopped.set(true);
                    return Out { code: Some(143), text: "terminated\n".into() };
                }
                if self.revoked && n == 0 {
                    let p = self.bench.ports();
                    let res = Resource::round("spira", "r-1");
                    let h = holder_of(&p, &res).unwrap().expect("held");
                    Lease::handed(res.clone(), h.token).release(&p).unwrap();
                    acquire(&p, &res, "round-controller").unwrap();
                }
            }
            Out { code: Some(0), text: "green\n".into() }
        }
        fn sleep(&self, _: u64) {}
        fn now_ms(&self) -> u64 {
            self.bench.clock.now_ms()
        }
        fn unit_state(&self, _: &str) -> Result<UnitState, String> {
            Ok(UnitState::Inactive)
        }
        fn progress(&self) -> Option<Mark> {
            let at_ms = if self.stalled { 0 } else { self.now_ms() };
            Some(Mark { round: self.mark_round.into(), phase: "suites".into(), phases: vec![], at_ms, vm: Some("vm-103".into()) })
        }
    }

    fn bench() -> Bench {
        let b = Bench::new();
        b.seed("batches/spira/r-1", serde_json::json!({ "state": "CI_RUNNING" }));
        b
    }

    fn slot_record(b: &Bench, slot: Slot) -> serde_json::Value {
        b.store.get(&slot.record_key()).unwrap().expect("a slot record").value.data
    }

    #[test]
    fn with_the_one_proxmox_slot_held_a_hand_drive_refuses_naming_its_holder_and_keeps_no_lease() {
        let b = bench();
        let no_spill = SlotSettings { ec2_slots: 0, spill: false };
        let p = Ports { slots: &no_spill, ..b.ports() };
        let mut held = grant_for_hand(&p, "round-controller").unwrap();
        held.record_holder(&p, "round-controller-run-r-0-3-0", None).unwrap();
        let sys = Driven::new(&b);
        let l = Launch { exe: Path::new("/rel/bin/release"), toml: "t.toml" };
        let e = start(&sys, &l, "r-1", "spira", Some(&p)).unwrap_err();
        assert!(e.contains("proxmox/0 is held by round-controller (unit round-controller-run-r-0-3-0)"), "{e}");
        assert!(!sys.calls.borrow().iter().any(|a| a.iter().any(|x| x == "--drive")), "nothing launched");
        assert_eq!(holder_of(&p, &Resource::round("spira", "r-1")).unwrap(), None, "the lease is given back");
    }

    #[test]
    fn a_leased_drives_unit_carries_its_token_as_the_controller_names_it() {
        let t = Rev::new(41, 2);
        assert_eq!(unit_for("r-auto-622", Some(t)), "round-recert-r-auto-622-41-2");
        assert_eq!(Ok(unit_for("r-auto-622", Some(t))), round_controller::death::hand_unit_name("r-auto-622", t));
    }
}
