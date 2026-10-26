use crate::trace::{bead_row, Event, Snapshot, Trace};
use crate::world::run;
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
const REPO: &str = crate::world::REPO_NAME;
pub const SCENARIOS_DIR: &str = "spira/sim/scenarios";
pub const EXEC_LOG: &str = "exec.log";
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
    /// The `actor` of this step's event; `step<N>` by position when absent. A check step exits 0
    /// when it held, so an `expect` on its event with `exit = 0` is the assertion.
    pub name: Option<String>,
    pub command: Option<String>,
    /// File this bead: a READY lifecycle row the world's summon can claim.
    pub file: Option<String>,
    /// Script the stub agent for this bead (`script` lines are `agent::parse` steps); the
    /// world's summon claims the bead and plays them.
    pub claim: Option<String>,
    #[serde(default)]
    pub script: Vec<String>,
    /// With `file`: the bead's priority (bd's `-p`); 2 when absent.
    pub priority: Option<u8>,
    /// With `file`: the bead's description (bd's `-d`), e.g. the `## Build` section a builder needs.
    pub body: Option<String>,
    /// With `file` or `claim`: the world repository the bead belongs to; the world's own when absent.
    pub repo: Option<String>,
}

/// A bead filed through `bead.sh` when the scenario starts: its persona or kind, priority,
/// labels, dependencies on earlier seeds, and body. `handle` names it inside the scenario; the
/// id `bead.sh` allocates is read back from `$SPIRA_RUN/seeds/<handle>.id`, which a `command`
/// step reaches as `{bead:<handle>}`. Seeds with the same `at` run in file order in one step.
#[derive(Debug, Deserialize)]
pub struct SeedDef {
    #[serde(default)]
    pub at: u64,
    pub handle: String,
    pub title: String,
    #[serde(rename = "for")]
    pub persona: Option<String>,
    pub kind: Option<String>,
    pub repo: Option<String>,
    pub priority: Option<u8>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    pub body: Option<String>,
}

const SEEDS_DIR: &str = "$SPIRA_RUN/seeds";

fn handle_ok(h: &str) -> bool {
    !h.is_empty() && h.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

fn id_file(handle: &str) -> String {
    format!("\"{SEEDS_DIR}/{handle}.id\"")
}

impl SeedDef {
    fn shell(&self) -> Result<String, String> {
        let h = &self.handle;
        if !handle_ok(h) {
            return Err(format!("scenario: seed handle {h:?} is not a name (lowercase letters, digits, _ and -)"));
        }
        if self.title.trim().is_empty() || self.priority.is_some_and(|p| p > 4) {
            return Err(format!("scenario: seed {h}: a title is required and priority is 0-4"));
        }
        let who = match (&self.persona, &self.kind) {
            (Some(p), None) => format!("--for {}", shell_quote(p)),
            (None, Some(k)) => format!("--kind {}", shell_quote(k)),
            _ => return Err(format!("scenario: seed {h}: exactly one of `for` and `kind`")),
        };
        let mut cmd = format!("mkdir -p \"{SEEDS_DIR}\"");
        let mut file = format!("bead.sh file {} {who} --repo {}", shell_quote(&self.title), shell_quote(self.repo.as_deref().unwrap_or(REPO)));
        if let Some(p) = self.priority {
            file += &format!(" --priority {p}");
        }
        if let Some(b) = &self.body {
            let body = format!("\"{SEEDS_DIR}/{h}.body\"");
            cmd += &format!(" && printf '%s\\n' {} > {body}", shell_quote(b));
            file += &format!(" --body-file {body}");
        }
        cmd += &format!(" && id=$({file}) && printf '%s\\n' \"$id\" > {}", id_file(h));
        for l in &self.labels {
            cmd += &format!(" && bead.sh label add \"$id\" {} >/dev/null", shell_quote(l));
        }
        for d in &self.deps {
            if !handle_ok(d) {
                return Err(format!("scenario: seed {h}: dep {d:?} is not a seed handle"));
            }
            cmd += &format!(" && bead.sh dep add \"$id\" \"$(cat {})\" >/dev/null", id_file(d));
        }
        Ok(cmd)
    }
}

/// `{bead:<handle>}` in a command step is that seed's allocated id.
pub fn expand_beads(command: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = command;
    while let Some(i) = rest.find("{bead:") {
        let tail = &rest[i + "{bead:".len()..];
        let end = tail.find('}').ok_or_else(|| format!("scenario: {command:?}: unclosed {{bead:"))?;
        let h = &tail[..end];
        if !handle_ok(h) {
            return Err(format!("scenario: {{bead:{h}}} is not a seed handle"));
        }
        out += &rest[..i];
        out += &format!("\"$(cat {})\"", id_file(h));
        rest = &tail[end + 1..];
    }
    out += rest;
    Ok(out)
}

fn command_handles(command: &str) -> Vec<String> {
    command.split("{bead:").skip(1).filter_map(|t| t.split_once('}')).map(|(h, _)| h.to_string()).collect()
}

/// The seeds that share an `at`, as one shell command, in time order.
pub fn seed_steps(seeds: &[SeedDef]) -> Result<Vec<(u64, String)>, String> {
    let mut times: Vec<u64> = seeds.iter().map(|s| s.at).collect();
    times.sort_unstable();
    times.dedup();
    times
        .into_iter()
        .map(|at| {
            let parts: Result<Vec<String>, String> = seeds.iter().filter(|s| s.at == at).map(SeedDef::shell).collect();
            parts.map(|p| (at, p.join(" && ")))
        })
        .collect()
}

/// A scenario's own SQL over the trace tables: an `invariant` is a query whose rows (each with a
/// `seq`) are violations; an `expect` is a query that must return a row by the end of the run.
#[derive(Debug, Deserialize)]
pub struct SqlDef {
    pub name: String,
    pub sql: String,
}

impl SqlDef {
    fn check(&self) -> Result<(), String> {
        if self.name.is_empty() || !self.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(format!("scenario: {:?} is not a view name (lowercase letters, digits, _)", self.name));
        }
        let q = self.sql.trim();
        if !q.get(..6).is_some_and(|h| h.eq_ignore_ascii_case("select")) || q.contains(';') {
            return Err(format!("scenario: {} must be one SELECT statement", self.name));
        }
        Ok(())
    }
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
        if self.priority.is_some_and(|p| p > 4) || (self.priority.is_some() && self.file.is_none()) {
            return Err(format!("scenario: step at {}: priority is 0-4 and belongs to a file step", self.at));
        }
        if self.body.is_some() && self.file.is_none() {
            return Err(format!("scenario: step at {}: body belongs to a file step", self.at));
        }
        if self.repo.is_some() && self.file.is_none() && self.claim.is_none() {
            return Err(format!("scenario: step at {}: repo belongs to a file or claim step", self.at));
        }
        let repo = self.repo.as_deref().unwrap_or(REPO);
        if !repo.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_')) {
            return Err(format!("scenario: {repo:?} is not a repository name"));
        }
        match (&self.command, &self.file, &self.claim) {
            (Some(c), None, None) if self.script.is_empty() => expand_beads(c),
            (None, Some(b), None) if self.script.is_empty() => {
                let b = bead_id(b)?;
                let named = self.repo.as_ref().map(|r| format!(" --repo {r}")).unwrap_or_default();
                Ok(format!("bd -C \"$SIM_BEADS_DB\" create {} --id {b} -t task -p {} -l spira -l plan -l repo:{repo}{} --silent >/dev/null && spira-lc create-bead {b}{named}", shell_quote(&format!("sim {b}")), self.priority.unwrap_or(2), self.body.as_ref().map(|d| format!(" -d {}", shell_quote(d))).unwrap_or_default()))
            }
            (None, None, Some(b)) if !self.script.is_empty() => {
                let b = bead_id(b)?;
                let routed = self.repo.as_ref().map(|r| format!("repo {r}"));
                let lines: Vec<String> = routed.iter().chain(&self.script).map(|l| shell_quote(&format!("{b} {l}"))).collect();
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
    /// With a goal: the run is done only once the bead is there and a tag with this prefix exists.
    pub goal_tag: Option<String>,
    /// Virtual milliseconds the run keeps going after the goal is reached, so what follows it is
    /// traced and held to the invariants too.
    #[serde(default)]
    pub settle: u64,
    #[serde(default, rename = "actor")]
    pub actors: Vec<ActorDef>,
    /// Actors taken by name from the tree's `sim/actors.toml`: cadence from its real timer
    /// units, durations from its fitted series.
    #[serde(default)]
    pub real_actors: Vec<String>,
    #[serde(default, rename = "seed")]
    pub seeds: Vec<SeedDef>,
    /// The world's repositories as `--repo` writes them (`<name>:<mode>[:<trunk>]`); the world's
    /// own is `sim:queue.local` unless named here.
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default, rename = "step")]
    pub steps: Vec<StepDef>,
    #[serde(default, rename = "invariant")]
    pub invariants: Vec<SqlDef>,
    #[serde(default, rename = "expect")]
    pub expects: Vec<SqlDef>,
    /// The world's summon runs the real `aeon` with the stub agent as its `SPIRA_AGENT`, and the
    /// brief the agent receives is the `briefs` table.
    #[serde(default)]
    pub real_aeon: bool,
    /// Read the bead store's rows (`bd`) into `bead_state` too; always so when there are seeds.
    #[serde(default)]
    pub bead_rows: bool,
    /// The tree under test `real_actors` resolve against: the world's `work` checkout.
    #[serde(skip)]
    pub tree: Option<PathBuf>,
}

impl Scenario {
    pub fn reads_bead_rows(&self) -> bool {
        self.bead_rows || !self.seeds.is_empty()
    }
}

/// A scenario argument: a path, or a bare name of a scenario shipped under `spira/sim/scenarios/`
/// in the checkout `cwd` is in.
pub fn scenario_path(cwd: &Path, arg: &str) -> PathBuf {
    let named = !arg.contains('/') && !arg.ends_with(".toml");
    if named {
        for dir in cwd.ancestors() {
            let shipped = dir.join(SCENARIOS_DIR).join(format!("{arg}.toml"));
            if shipped.is_file() {
                return shipped;
            }
        }
    }
    PathBuf::from(arg)
}

pub fn parse_scenario(text: &str) -> Result<Scenario, String> {
    let sc: Scenario = toml::from_str(text).map_err(|e| format!("scenario: {e}"))?;
    if let Some(g) = &sc.goal {
        parse_bead_state(g)?;
    }
    if sc.goal_tag.is_some() && sc.goal.is_none() {
        return Err("scenario: goal_tag needs a goal".to_string());
    }
    seed_steps(&sc.seeds)?;
    let named = scenario_repos(&sc, &[])?;
    let mut seen: Vec<&SeedDef> = Vec::new();
    for sd in &sc.seeds {
        if seen.iter().any(|p| p.handle == sd.handle || p.title == sd.title) {
            return Err(format!("scenario: seed {} repeats a handle or a title", sd.handle));
        }
        if let Some(d) = sd.deps.iter().find(|d| !seen.iter().any(|p| &p.handle == *d && p.at <= sd.at)) {
            return Err(format!("scenario: seed {} depends on {d:?}, which is not an earlier seed at or before its time", sd.handle));
        }
        seen.push(sd);
    }
    for (i, st) in sc.steps.iter().enumerate() {
        if let Some(n) = &st.name {
            if !handle_ok(n) || n.starts_with("seed") || n.starts_with("step") || sc.steps.iter().take(i).any(|p| p.name.as_ref() == Some(n)) {
                return Err(format!("scenario: step name {n:?} is not a unique name (lowercase letters, digits, _ and -; not starting `seed` or `step`)"));
            }
        }
        st.shell()?;
        if let Some(h) = st.command.iter().flat_map(|c| command_handles(c)).find(|h| !sc.seeds.iter().any(|s| &s.handle == h)) {
            return Err(format!("scenario: step at {} names {{bead:{h}}}, which no seed declares", st.at));
        }
        if let Some(r) = &st.repo {
            if !named.iter().any(|n| &n.name == r) {
                return Err(format!("scenario: step at {} names repository {r}, which the scenario does not declare", st.at));
            }
        }
    }
    for d in sc.invariants.iter().chain(&sc.expects) {
        d.check()?;
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

/// The scenario's repositories with `cli` laid over them (a name given on the command line
/// replaces the scenario's).
pub fn scenario_repos(sc: &Scenario, cli: &[String]) -> Result<Vec<crate::world::RepoSpec>, String> {
    let parsed = |v: &[String]| v.iter().map(|r| crate::world::RepoSpec::parse(r)).collect::<Result<Vec<_>, _>>();
    let (declared, given) = (parsed(&sc.repos)?, parsed(cli)?);
    let mut all: Vec<_> = declared.into_iter().filter(|d| !given.iter().any(|g| g.name == d.name)).collect();
    all.extend(given);
    crate::world::repo_set(&all)
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
            None => Err(format!("scenario: no actor {n:?} in the tree's sim/actors.toml")),
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
        self.command_with(shell, now_ms, true)
    }

    pub fn command_with(&self, shell: &str, now_ms: u64, resolved: bool) -> Result<Command, String> {
        let mut cmd = sim::actor_command("sh", self.epoch + now_ms / 1000);
        let path = format!(
            "{}:{}:{}:{}",
            self.world.join("bin").display(),
            self.world.join("release/bin").display(),
            self.world.join("release/spira").display(),
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
        if resolved {
            for (k, v) in read_conf_env(&self.world)? {
                cmd.env(k, v);
            }
        }
        Ok(cmd)
    }
}

pub const CONF_ENV: &str = "config/conf.env";
const SHELL_NOISE: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

fn read_conf_env(world: &Path) -> Result<Vec<(String, String)>, String> {
    match std::fs::read(world.join(CONF_ENV)) {
        Ok(b) => Ok(parse_env0(&b).into_iter().collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{CONF_ENV}: {e}")),
    }
}

fn parse_env0(bytes: &[u8]) -> std::collections::BTreeMap<String, String> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !SHELL_NOISE.contains(k))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Resolves the world's configuration through the real `conf.sh` once and writes what it exports
/// to `config/conf.env`, with `SPIRA_CONF_LOADED` set so every later script skips the
/// re-resolution. Fails if sourcing `conf.sh` again on top of the resolved environment changes
/// anything: the exported env must equal what `conf.sh` resolves.
pub fn resolve_conf(world: &Path, epoch: u64) -> Result<(), String> {
    let file = world.join(CONF_ENV);
    if file.exists() {
        return Ok(());
    }
    let exec = ProcessExec { world: world.to_path_buf(), epoch };
    let env0 = |shell: &str, resolved: bool| -> Result<std::collections::BTreeMap<String, String>, String> {
        let mut cmd = exec.command_with(shell, 0, resolved)?;
        let o = cmd.stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(format!("{shell}: {}: {}", o.status, String::from_utf8_lossy(&o.stderr)));
        }
        Ok(parse_env0(&o.stdout))
    };
    let home = read_env_file(&world.join("config/sim.env"))?
        .into_iter()
        .find(|(k, _)| k == "SPIRA_HOME")
        .map(|(_, v)| v)
        .ok_or("sim.env has no SPIRA_HOME")?;
    let source = format!("bash -c \". '{home}/conf.sh' && env -0\"");
    let base = env0("env -0", false)?;
    let once = env0(&source, false)?;
    let mut bytes = Vec::new();
    for (k, v) in once.iter().filter(|(k, v)| base.get(*k) != Some(*v)) {
        bytes.extend_from_slice(format!("{k}={v}\0").as_bytes());
    }
    std::fs::write(&file, &bytes).map_err(|e| e.to_string())?;
    let again = env0(&source, true)?;
    let differ: Vec<&String> = once.keys().chain(again.keys()).filter(|k| once.get(*k) != again.get(*k)).collect();
    if !differ.is_empty() {
        let _ = std::fs::remove_file(&file);
        return Err(format!("the exported env differs from what conf.sh resolves: {differ:?}"));
    }
    bytes.extend_from_slice(b"SPIRA_CONF_LOADED=1\0");
    std::fs::write(&file, &bytes).map_err(|e| e.to_string())
}

impl ProcessExec {
    /// Appends one run to `exec.log` in the world: what each command printed is not in the trace.
    fn log(&self, command: &str, now_ms: u64, status: &str, out: &str, err: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.world.join(EXEC_LOG)) {
            let _ = writeln!(f, "=== t={now_ms} {command}: {status}\n{}{}", out, err);
        }
    }
}

impl Exec for ProcessExec {
    fn run(&mut self, command: &str, now_ms: u64) -> Result<i32, String> {
        let started = std::time::Instant::now();
        let trace = std::env::var_os(crate::proctrace::TRACE_ENV).is_some().then(|| (self.world.as_path(), command));
        let (status, out, err) = crate::world::run_capture_traced(&mut self.command(command, now_ms)?, COMMAND_DEADLINE, trace)?;
        self.log(command, now_ms, &format!("{status} in {}ms", started.elapsed().as_millis()), &out, &err);
        // conf.env freezes the config; a step that changes it sends later commands back through conf.sh
        if status.success() && (command.contains("spira-config set") || command.contains("spira-config unset")) {
            let _ = std::fs::remove_file(self.world.join(CONF_ENV));
        }
        status.code().ok_or_else(|| format!("{command}: {status}"))
    }
}

/// CPU (user and system) of every process this one has run and reaped, from `/proc/self/stat`:
/// the part of a world's cost that does not depend on how loaded the box was.
pub fn children_cpu_ms() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<u64> = stat.rsplit_once(')').map(|(_, rest)| rest.split_whitespace().skip(13).take(2).filter_map(|f| f.parse().ok()).collect()).unwrap_or_default();
    fields.iter().sum::<u64>() * 10
}

/// One line of where a run's wall time went: world-up phases from `profile.tsv`, and the
/// count and total of command runs and probes from `exec.log`.
pub fn profile_line(world: &Path) -> String {
    let phases: Vec<String> = std::fs::read_to_string(world.join(crate::world::PROFILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(k, ms)| format!("{k}={ms}ms"))
        .collect();
    let (mut cmds, mut cmd_ms, mut probes, mut probe_ms) = (0u64, 0u64, 0u64, 0u64);
    let mut timed_cmds: Vec<(u64, String)> = Vec::new();
    for l in std::fs::read_to_string(world.join(EXEC_LOG)).unwrap_or_default().lines() {
        let timed = l.contains("(probe): in ") || l.contains(": exit status: ");
        let Some(ms) = Some(l).filter(|_| timed).and_then(|l| l.rsplit_once(" in ")).and_then(|(_, t)| t.strip_suffix("ms")).and_then(|t| t.parse::<u64>().ok()) else { continue };
        if l.contains("(probe): in ") {
            probes += 1;
            probe_ms += ms;
        } else {
            cmds += 1;
            cmd_ms += ms;
            timed_cmds.push((ms, l.chars().take(70).collect()));
        }
    }
    timed_cmds.sort_by(|a, b| b.0.cmp(&a.0));
    let slowest: Vec<String> = timed_cmds.iter().take(4).map(|(ms, c)| format!("{ms}ms {c}")).collect();
    format!("{} cpu={}ms commands={cmds}/{cmd_ms}ms probes={probes}/{probe_ms}ms slowest: {}", phases.join(" "), children_cpu_ms(), slowest.join(" | "))
}

pub struct ProcessProbe {
    pub exec: ProcessExec,
    pub probe: Option<String>,
    /// Set when the world's `SIM_PROBE` is this binary's own `sim probe`: read in-process, with
    /// the world's locators, instead of through a shell and a second `sim`.
    observer: Option<crate::probe::Observer>,
    /// Ask the bead store (`bd list`) for every bead's row on each snapshot.
    bead_rows: bool,
}

impl ProcessProbe {
    pub fn for_world(world: &Path, epoch: u64, bead_rows: bool) -> Result<Self, String> {
        let sim_env = read_env_file(&world.join("config/sim.env"))?;
        let probe = sim_env.iter().find(|(k, _)| k == PROBE_KEY).map(|(_, v)| v.clone());
        let own = world.canonicalize().ok().zip(std::env::current_exe().ok()).map(|(w, exe)| crate::world::probe_command(&exe, &w));
        let observer = match (&probe, own) {
            (Some(p), Some(own)) if *p == own => {
                let locator = |k: &str| sim_env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
                Some(crate::probe::Observer::new(world, &locator)?)
            }
            _ => None,
        };
        Ok(ProcessProbe { exec: ProcessExec { world: world.to_path_buf(), epoch }, probe, observer, bead_rows })
    }
}

impl Probe for ProcessProbe {
    fn snapshot(&mut self) -> Result<Snapshot, String> {
        let started = std::time::Instant::now();
        let snap = self.take();
        self.exec.log("(probe)", 0, &format!("in {}ms", started.elapsed().as_millis()), "", "");
        snap
    }
}

const BD_LIST: &str = "bd -C \"$SIM_BEADS_DB\" list --all --limit 0 --json";

impl ProcessProbe {
    fn take(&mut self) -> Result<Snapshot, String> {
        let mut snap = self.observe()?;
        if self.bead_rows {
            let out = run(&mut self.exec.command_with(BD_LIST, 0, false)?, COMMAND_DEADLINE).map_err(|e| format!("bd list: {e}"))?;
            crate::probe::merge_bd(&mut snap.beads, &out)?;
        }
        Ok(snap)
    }

    fn observe(&mut self) -> Result<Snapshot, String> {
        if let Some(o) = self.observer.as_mut() {
            let seen = o.observe()?;
            let mut snap = Snapshot { refs: seen.refs, mail: seen.mail, run: crate::runstate::observe(&self.exec.world), ..Snapshot::default() };
            for t in seen.tags.into_iter().chain(crate::gh::tag_names(&self.exec.world.join("gh"))?) {
                if !snap.tags.contains(&t) {
                    snap.tags.push(t);
                }
            }
            for row in &seen.rows {
                snap.beads.push(bead_row(row)?);
            }
            return Ok(snap);
        }
        let work = self.exec.world.join("work");
        let git = |args: &[&str]| run(Command::new("git").arg("-C").arg(&work).args(args), COMMAND_DEADLINE);
        let mut snap = Snapshot { mail: crate::probe::mail_rows(&crate::world::mail_root(&self.exec.world)), run: crate::runstate::observe(&self.exec.world), ..Snapshot::default() };
        for l in git(&["for-each-ref", "--format=%(refname) %(objectname)"])?.lines() {
            if let Some((r, sha)) = l.split_once(' ') {
                snap.refs.push((r.to_string(), sha.to_string()));
            }
        }
        snap.tags = git(&["tag"])?.lines().map(str::to_string).collect();
        let origin = self.exec.world.join("origin.git");
        let origin_tags = run(Command::new("git").arg("-C").arg(&origin).arg("tag"), COMMAND_DEADLINE)?;
        for t in origin_tags.lines().map(str::to_string).chain(crate::gh::tag_names(&self.exec.world.join("gh"))?) {
            if !snap.tags.contains(&t) {
                snap.tags.push(t);
            }
        }
        if let Some(p) = &self.probe {
            let out = run(&mut self.exec.command_with(p, 0, false)?, COMMAND_DEADLINE)?;
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
    let ran: Rc<Cell<bool>> = Rc::default();

    let mut sim = Sim::new(seed);
    let call = {
        let (exec, clock, last_exit, fatal, ran) = (exec.clone(), clock.clone(), last_exit.clone(), fatal.clone(), ran.clone());
        move |command: &str| {
            ran.set(true);
            match exec.borrow_mut().run(command, clock.get()) {
                Ok(code) => last_exit.set(Some(code)),
                Err(e) => *fatal.borrow_mut() = Some(format!("{command}: {e}")),
            }
        }
    };
    if sc.real_aeon {
        let command = format!("printf '%s\\n' '{}' >> \"${}\"", crate::summon::REAL_AEON, crate::agent::SCENARIO_VAR);
        let call = call.clone();
        sim.add_scenario("real-aeon", Box::new(move |_| call(&command)));
        sim.schedule_step(0, "real-aeon");
    }
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
        let release = sc.tree.as_deref().ok_or("scenario names real_actors but no tree to read them from")?;
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
    for (i, (at, command)) in seed_steps(&sc.seeds)?.into_iter().enumerate() {
        let (call, last_exit, fatal) = (call.clone(), last_exit.clone(), fatal.clone());
        let name = format!("seed{}", i + 1);
        let label = name.clone();
        sim.add_scenario(&name, Box::new(move |_| {
            call(&command);
            if last_exit.get().is_some_and(|c| c != 0) {
                *fatal.borrow_mut() = Some(format!("{label} failed (exit {}): a seed that is not filed leaves the scenario meaning nothing", last_exit.get().unwrap_or(-1)));
            }
        }));
        sim.schedule_step(at, &name);
    }
    for (i, s) in sc.steps.iter().enumerate() {
        let (command, call) = (s.shell()?, call.clone());
        let name = s.name.clone().unwrap_or_else(|| format!("step{}", i + 1));
        sim.add_scenario(&name, Box::new(move |_| call(&command)));
        sim.schedule_step(s.at, &name);
    }

    let goal = match (&sc.goal, stop) {
        (_, Stop::Bead(b, s)) => Some((b.clone(), s.clone())),
        (Some(g), Stop::Goal) => Some(parse_bead_state(g)?),
        _ => None,
    };
    let tag_prefix = sc.goal_tag.clone().filter(|_| matches!(stop, Stop::Goal));
    let mut events: Vec<Value> = Vec::new();
    let mut open: BTreeMap<String, (u64, i32)> = BTreeMap::new();
    let mut reached = false;
    let mut last_snap: Option<Snapshot> = None;
    let mut settle_until: Option<u64> = None;
    let mut budget = if let Stop::Events(n) = stop { Some(*n) } else { None };
    while let Some(next) = sim.next_time() {
        let live = events.len() >= resume.len();
        if settle_until.is_some_and(|u| next > u) {
            break;
        }
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
        // An event that ran no command cannot have changed what the probe reads.
        let snap = match (&last_snap, ran.take()) {
            (Some(prev), false) => prev.clone(),
            _ => probe.snapshot().map_err(|e| format!("seed {seed} seq {seq}: probe: {e}"))?,
        };
        last_snap = Some(snap.clone());
        if let Some(tr) = trace {
            tr.snapshot(seq, &snap)?;
        }
        if let Some((b, s)) = &goal {
            let tagged = tag_prefix.as_deref().is_none_or(|p| snap.tags.iter().any(|t| t.starts_with(p)));
            if !reached && bead_in_state(&snap, b, s) && tagged {
                reached = true;
                if sc.settle == 0 {
                    break;
                }
                settle_until = Some(t.time + sc.settle);
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
