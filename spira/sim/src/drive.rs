use crate::trace::{bead_row, Event, Snapshot, Trace};
use crate::world::{run, run_capture};
use serde::Deserialize;
use serde_json::Value;
use sim::{EventKind, Outcome, Sim};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

const COMMAND_DEADLINE: Duration = Duration::from_secs(120); // batch-job: one actor run or scenario step against a world
const PROBE_KEY: &str = "SIM_PROBE";

#[derive(Debug, Deserialize)]
pub struct ActorDef {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub first: u64,
    pub every: Option<u64>,
    pub duration_min: u64,
    pub duration_max: u64,
    #[serde(default)]
    pub staged: bool,
}

#[derive(Debug, Deserialize)]
pub struct StepDef {
    pub at: u64,
    pub command: Option<String>,
    /// File this bead: a READY lifecycle row the world's summon can claim.
    pub file: Option<String>,
    /// Script the stub agent for this bead (`script` lines are `agent::parse` steps); the
    /// world's summon claims the bead and plays them.
    pub claim: Option<String>,
    #[serde(default)]
    pub script: Vec<String>,
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn bead_id(s: &str) -> Result<&str, String> {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        Ok(s)
    } else {
        Err(format!("scenario: {s:?} is not a bead id"))
    }
}

impl StepDef {
    /// The shell command this step runs in the world.
    pub fn shell(&self) -> Result<String, String> {
        match (&self.command, &self.file, &self.claim) {
            (Some(c), None, None) if self.script.is_empty() => Ok(c.clone()),
            (None, Some(b), None) if self.script.is_empty() => Ok(format!("spira-lc create-bead {}", bead_id(b)?)),
            (None, None, Some(b)) if !self.script.is_empty() => {
                let b = bead_id(b)?;
                let lines: Vec<String> = self.script.iter().map(|l| shell_quote(&format!("{b} {l}"))).collect();
                Ok(format!("printf '%s\\n' {} >> \"${}\"", lines.join(" "), crate::agent::SCENARIO_VAR))
            }
            _ => Err(format!("scenario: step at {} needs exactly one of command, file, or claim with a script", self.at)),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub epoch: u64,
    pub horizon: u64,
    pub goal: Option<String>,
    #[serde(default, rename = "actor")]
    pub actors: Vec<ActorDef>,
    /// Actors taken by name from the release's `sim/actors.toml`: cadence from its real timer
    /// units, durations from its fitted series.
    #[serde(default)]
    pub real_actors: Vec<String>,
    #[serde(default, rename = "step")]
    pub steps: Vec<StepDef>,
    /// The release `real_actors` resolve against; the world's `release` link.
    #[serde(skip)]
    pub release: Option<PathBuf>,
}

pub fn parse_scenario(text: &str) -> Result<Scenario, String> {
    let sc: Scenario = toml::from_str(text).map_err(|e| format!("scenario: {e}"))?;
    if let Some(g) = &sc.goal {
        parse_bead_state(g)?;
    }
    for st in &sc.steps {
        st.shell()?;
    }
    for a in &sc.actors {
        if a.duration_min > a.duration_max {
            return Err(format!("scenario: actor {} has duration_min above duration_max", a.name));
        }
        if a.every == Some(0) {
            return Err(format!("scenario: actor {} has every = 0", a.name));
        }
    }
    Ok(sc)
}

pub fn parse_bead_state(spec: &str) -> Result<(String, String), String> {
    match spec.split_once(':') {
        Some((b, s)) if !b.is_empty() && !s.is_empty() => Ok((b.to_string(), s.to_string())),
        _ => Err(format!("{spec:?} is not <bead>:<STATE>")),
    }
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn load_specs(release: &Path, names: &[String]) -> Result<Vec<sim::actors::ActorSpec>, String> {
    let mut all = sim::actors::parse_specs(&read(&release.join("sim/actors.toml"))?)?;
    names
        .iter()
        .map(|n| match all.iter().position(|a| &a.name == n) {
            Some(i) => Ok(all.remove(i)),
            None => Err(format!("scenario: no actor {n:?} in the release's sim/actors.toml")),
        })
        .collect()
}

pub trait Exec {
    fn run(&mut self, command: &str, now_ms: u64) -> Result<i32, String>;
}

pub trait Probe {
    fn snapshot(&mut self) -> Result<Snapshot, String>;
}

pub struct NoProbe;

impl Probe for NoProbe {
    fn snapshot(&mut self) -> Result<Snapshot, String> {
        Ok(Snapshot::default())
    }
}

/// Answers from a recorded run while it lasts, then from the live executor.
pub struct Resumed {
    pub recorded: VecDeque<i32>,
    pub live: Option<Box<dyn Exec>>,
}

impl Exec for Resumed {
    fn run(&mut self, command: &str, now_ms: u64) -> Result<i32, String> {
        if let Some(code) = self.recorded.pop_front() {
            return Ok(code);
        }
        match self.live.as_mut() {
            Some(l) => l.run(command, now_ms),
            None => Err("the recorded run has fewer executions than the replay".to_string()),
        }
    }
}

pub fn read_env_file(path: &Path) -> Result<Vec<(String, String)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect())
}

pub struct ProcessExec {
    pub world: PathBuf,
    pub epoch: u64,
}

impl ProcessExec {
    fn command(&self, shell: &str, now_ms: u64) -> Result<Command, String> {
        let mut cmd = sim::actor_command("sh", self.epoch + now_ms / 1000);
        let path = format!(
            "{}:{}:{}",
            self.world.join("bin").display(),
            self.world.join("release/bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.arg("-c").arg(shell).current_dir(self.world.join("work")).env("PATH", path);
        if let Some(h) = std::env::var_os("HOME") {
            cmd.env("HOME", h);
        }
        cmd.env("SPIRA_TOML", self.world.join("config/sim.toml"));
        for (k, v) in read_env_file(&self.world.join("config/sim.env"))? {
            cmd.env(k, v);
        }
        Ok(cmd)
    }
}

impl Exec for ProcessExec {
    fn run(&mut self, command: &str, now_ms: u64) -> Result<i32, String> {
        let (status, _, _) = run_capture(&mut self.command(command, now_ms)?, COMMAND_DEADLINE)?;
        status.code().ok_or_else(|| format!("{command}: {status}"))
    }
}

pub struct ProcessProbe {
    pub exec: ProcessExec,
    pub probe: Option<String>,
}

impl ProcessProbe {
    pub fn for_world(world: &Path, epoch: u64) -> Result<Self, String> {
        let probe = read_env_file(&world.join("config/sim.env"))?.into_iter().find(|(k, _)| k == PROBE_KEY).map(|(_, v)| v);
        Ok(ProcessProbe { exec: ProcessExec { world: world.to_path_buf(), epoch }, probe })
    }
}

impl Probe for ProcessProbe {
    fn snapshot(&mut self) -> Result<Snapshot, String> {
        let work = self.exec.world.join("work");
        let git = |args: &[&str]| run(Command::new("git").arg("-C").arg(&work).args(args), COMMAND_DEADLINE);
        let mut snap = Snapshot::default();
        for l in git(&["for-each-ref", "--format=%(refname) %(objectname)"])?.lines() {
            if let Some((r, sha)) = l.split_once(' ') {
                snap.refs.push((r.to_string(), sha.to_string()));
            }
        }
        snap.tags = git(&["tag"])?.lines().map(str::to_string).collect();
        if let Some(p) = &self.probe {
            let out = run(&mut self.exec.command(p, 0)?, COMMAND_DEADLINE)?;
            for l in out.lines().filter(|l| !l.trim().is_empty()) {
                snap.beads.push(bead_row(&serde_json::from_str(l).map_err(|e| format!("{PROBE_KEY}: {e}: {l}"))?)?);
            }
        }
        Ok(snap)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    /// Until the scenario's goal is met or events run out.
    Goal,
    VTime(u64),
    Bead(String, String),
    Events(u64),
}

pub struct Report {
    pub events: Vec<Value>,
    pub goal_reached: bool,
}

fn event_row(kind: &EventKind, coalesced: bool) -> (&'static str, Option<String>) {
    match kind {
        EventKind::Due(a) if coalesced => ("due", Some(a.clone())),
        EventKind::Due(a) => ("start", Some(a.clone())),
        EventKind::Complete(a) => ("complete", Some(a.clone())),
        EventKind::Step(s) => ("step", Some(s.clone())),
        EventKind::Fault => ("fault", None),
    }
}

fn bead_in_state(snap: &Snapshot, bead: &str, state: &str) -> bool {
    snap.beads.iter().any(|b| b["bead"] == bead && b["lc_state"] == state)
}

/// Runs the scenario's event loop from the seed. The first `resume.len()` events are
/// re-derived and checked against `resume` without being executed or recorded again.
pub fn drive(
    sc: &Scenario,
    seed: u64,
    exec: Box<dyn Exec>,
    mut probe: Box<dyn Probe>,
    trace: Option<&Trace>,
    resume: &[Value],
    stop: &Stop,
) -> Result<Report, String> {
    let recorded = resume.iter().filter(|e| matches!(e["kind"].as_str(), Some("start" | "step"))).map(|e| e["exit"].as_i64().unwrap_or(0) as i32).collect();
    let exec = Rc::new(RefCell::new(Resumed { recorded, live: Some(exec) }));
    let clock = Rc::new(Cell::new(0u64));
    let last_exit: Rc<Cell<Option<i32>>> = Rc::default();
    let fatal: Rc<RefCell<Option<String>>> = Rc::default();

    let mut sim = Sim::new(seed);
    let call = {
        let (exec, clock, last_exit, fatal) = (exec.clone(), clock.clone(), last_exit.clone(), fatal.clone());
        move |command: &str| match exec.borrow_mut().run(command, clock.get()) {
            Ok(code) => last_exit.set(Some(code)),
            Err(e) => *fatal.borrow_mut() = Some(format!("{command}: {e}")),
        }
    };
    for a in &sc.actors {
        let (command, lo, hi, call) = (a.command.clone(), a.duration_min, a.duration_max, call.clone());
        sim.add_actor(&a.name, a.staged, Box::new(move |_, rng| {
            call(&command);
            Outcome { writes: Vec::new(), duration: rng.range(lo, hi) }
        }));
        let mut at = a.first;
        while at <= sc.horizon {
            sim.schedule_due(at, &a.name);
            match a.every {
                Some(e) => at += e,
                None => break,
            }
        }
    }
    if !sc.real_actors.is_empty() {
        let release = sc.release.as_deref().ok_or("scenario names real_actors but no release to read them from")?;
        let specs = load_specs(release, &sc.real_actors)?;
        let durations = sim::fit::Durations::parse(&read(&release.join("sim/durations.toml"))?)?;
        for spec in &specs {
            let timer = read(&release.join("systemd").join(&spec.timer))?;
            let service = sim::units::timer_target(&spec.timer, &timer);
            sim::units::require_oneshot(&service, &read(&release.join("systemd").join(&service))?)?;
            let schedule = sim::units::parse_timer(&timer).map_err(|e| format!("{}: {e}", spec.timer))?;
            let model = durations.series.get(&spec.duration).ok_or_else(|| format!("{}: no fitted series {:?}", spec.name, spec.duration))?.clone();
            let (command, call) = (spec.command.clone(), call.clone());
            sim.add_actor(&spec.name, spec.staged, Box::new(move |_, rng| {
                call(&command);
                Outcome { writes: Vec::new(), duration: model.sample_ms(rng) }
            }));
            for at in schedule.fire_times(sc.epoch, sc.horizon, &mut sim.rng) {
                sim.schedule_due(at, &spec.name);
            }
        }
    }
    for (i, s) in sc.steps.iter().enumerate() {
        let (command, call) = (s.shell()?, call.clone());
        let name = format!("step{}", i + 1);
        sim.add_scenario(&name, Box::new(move |_| call(&command)));
        sim.schedule_step(s.at, &name);
    }

    let goal = match (&sc.goal, stop) {
        (_, Stop::Bead(b, s)) => Some((b.clone(), s.clone())),
        (Some(g), Stop::Goal) => Some(parse_bead_state(g)?),
        _ => None,
    };
    let mut events: Vec<Value> = Vec::new();
    let mut open: BTreeMap<String, (u64, i32)> = BTreeMap::new();
    let mut reached = false;
    let mut budget = if let Stop::Events(n) = stop { Some(*n) } else { None };
    while let Some(next) = sim.next_time() {
        let live = events.len() >= resume.len();
        if live {
            if budget == Some(0) {
                break;
            }
            if matches!(stop, Stop::VTime(t) if next > *t) {
                break;
            }
        }
        clock.set(next);
        if !sim.step() {
            break;
        }
        if let Some(e) = fatal.borrow_mut().take() {
            return Err(format!("seed {seed} seq {}: {e}", events.len() + 1));
        }
        let t = sim.trace.last().ok_or("step left no trace")?;
        let (kind, actor) = event_row(&t.kind, t.coalesced);
        let seq = events.len() as u64 + 1;
        let (mut started, mut completed, mut exit) = (None, None, last_exit.take());
        match kind {
            "start" => {
                open.insert(actor.clone().unwrap_or_default(), (t.time, exit.unwrap_or(0)));
            }
            "complete" => {
                let (s, x) = open.remove(actor.as_deref().unwrap_or("")).unwrap_or((t.time, 0));
                (started, completed, exit) = (Some(s), Some(t.time), Some(x));
            }
            _ => {}
        }
        let ev = Event { seq, vtime: t.time, kind, actor, started, completed, exit, coalesced: t.coalesced, seed };
        events.push(ev.to_json());
        if seq as usize <= resume.len() {
            if seq as usize == resume.len() {
                check_resumed(seed, resume, &events)?;
            }
            continue;
        }
        if let Some(n) = budget.as_mut() {
            *n -= 1;
        }
        if let Some(tr) = trace {
            tr.event(&ev)?;
        }
        let snap = probe.snapshot().map_err(|e| format!("seed {seed} seq {seq}: probe: {e}"))?;
        if let Some(tr) = trace {
            tr.snapshot(seq, &snap)?;
        }
        if let Some((b, s)) = &goal {
            if bead_in_state(&snap, b, s) {
                reached = true;
                break;
            }
        }
    }
    check_resumed(seed, resume, &events[..resume.len().min(events.len())])?;
    Ok(Report { events, goal_reached: reached })
}

fn check_resumed(seed: u64, resume: &[Value], events: &[Value]) -> Result<(), String> {
    match crate::trace::first_divergence(resume, &events[..resume.len().min(events.len())]) {
        Some(d) => Err(format!("seed {seed}: the recorded trace diverges at seq {}", d.seq)),
        None => Ok(()),
    }
}
