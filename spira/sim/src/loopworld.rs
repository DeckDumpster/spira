//! The loop world: the service's own registrations (`spira_runtime::registrations`, every row of
//! `tasks::all()`) on the `VirtualDriver` under virtual time, every effect answered by a double
//! (ruling D6 of the scheduler consolidation design). The dwarf sessions and the round's legs are
//! units on the simulator's manager (`ScriptedUnits`); their ends reach the owning task's stream
//! through the service's own `unit.ran` source. Every other effect answers the quiet world
//! (`sim::doubles::service`) unless a scenario lays a double of its own over it ([`run_with`]).
//!
//! The world the doubles answer from is one in-memory lifecycle store whose rows move only
//! through the store's own machine (`lifecycle::bead::apply`). It widens `sim/tests/it/whole_loop.rs`
//! with what the world scenarios of `spira/sim/scenarios/` scripted: a dwarf session may submit
//! more than once (a moved tip); beads carry a priority and a file they touch, so two siblings
//! can conflict and the dwarf claims in priority order; REWORK is claimable again; a gate outside
//! the loop can write verdicts at fixed virtual times; and the repo can publish by PR.
//!
//! A scenario file with `driver = "loop"` is one of these ([`parse`]); `sim run` drives it and
//! writes `exec.log` in the kept run directory, one line per effect in the world's own format
//! (`=== t=<vt> <effect> exit status: <rc> in <ms>ms`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use lifecycle::bead::{BeadEvent, BeadEventKind, LifecycleRow, BeadState, HoldKind};
use lifecycle::reason::{GateRedReason, HoldCause};
use runtime::host::{EffectRunner, VirtualDriver};
use runtime::journal::Journal;
use runtime::ports::{Clock, CountingProc, Proc, ProcSpec, VirtualClock};
use runtime::sources::Sources;
use runtime::units::{self, Exit, ScopeSpec, ScriptedUnits, StartSpec, UnitSpec, UNIT_RUN_SCOPE, UNIT_START};
use runtime::{Engine, EffectSpec, MemJournal};
use serde::Deserialize;
use sim::doubles::Doubles;
use crate::registryworld::{self, Registry};
use spira_runtime::tasks::a::{batcher_local as bl, publish as pb, queue_step as qs, sentinel_call as sc, sift};
use spira_runtime::{registrations, tasks, Settings};

pub const REPO: &str = "spira";
pub const RUNE: &str = "builder";
pub const BASE: &str = "ba5eba5eba5eba5eba5eba5eba5eba5eba5eba5e";
pub const MIN: u64 = 60_000;
pub const DWARF_MS: u64 = 10 * MIN;
pub const CORPUS_MS: u64 = 5 * MIN;
pub const CERTIFY_MS: u64 = 2 * MIN;
/// The value of a scenario's `driver` key that makes it a loop world.
pub const DRIVER: &str = "loop";

/// The `n`th tip a dwarf submits for `id` (from 0): full-length, distinct per submit.
pub fn tip_of(id: &str, n: usize) -> String {
    let hex: String = id.bytes().map(|b| format!("{b:02x}")).collect();
    format!("{n:x}{hex:0>39}")
}

/// The local main after the `n`th landing (from 1): full-length, distinct per landing.
pub fn main_after(n: usize) -> String {
    format!("{n:0>40x}")
}

fn default_priority() -> u8 {
    2
}

fn default_submits() -> Vec<u64> {
    vec![DWARF_MS - 1]
}

/// One bead the scenario files: when, at what priority, the file its change touches (its own id
/// when empty), and when in each dwarf session (ms after the claim) the dwarf submits; the tip
/// moves at every submit.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bead {
    pub id: String,
    #[serde(default)]
    pub at: u64,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub file: String,
    #[serde(default = "default_submits")]
    pub submits: Vec<u64>,
    /// The beads that must finish, or stand certified and not known red, before this one is offered.
    #[serde(default)]
    pub blocks: Vec<String>,
}

impl Bead {
    pub fn new(id: &str, at: u64) -> Bead {
        Bead { id: id.into(), at, priority: default_priority(), file: String::new(), submits: default_submits(), blocks: Vec::new() }
    }

    pub fn file(&self) -> &str {
        if self.file.is_empty() {
            &self.id
        } else {
            &self.file
        }
    }
}

/// A verdict a gate outside the loop writes into the store at a virtual time, at the bead's
/// tip as the store then holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Red,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerdictAt {
    pub at: u64,
    pub bead: String,
    pub verdict: Verdict,
}

fn one() -> u32 {
    1
}

fn yes() -> bool {
    true
}

/// A test that is red on the combined tree of any round carrying one of `members` and green on
/// each member alone, until the registry holds it out of the run. The round's flip handling
/// disables it and files `follow_on`, which the world files READY, or held when it does not land.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flip {
    pub test: String,
    pub members: Vec<String>,
    pub follow_on: String,
    #[serde(default = "yes")]
    pub follow_on_lands: bool,
    /// From this virtual time (ms) a round carrying the test must find it red again.
    #[serde(default)]
    pub blocks_again_after: Option<u64>,
}

/// A registry row that names `owner`, a member of the round that lands the fix. The round lifts
/// it and runs `test` on the merged tree: green when `red` is false; red on the tree and the base
/// when true, which keeps the row, re-pointed at `fix`, the bead the round files.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lift {
    pub test: String,
    pub owner: String,
    pub fix: String,
    #[serde(default = "yes")]
    pub red: bool,
    /// From this virtual time (ms) a round must find the kept test red again.
    #[serde(default)]
    pub blocks_again_after: Option<u64>,
    /// A member that is not the owner and edits the test's body: its change is the row's claimed fix.
    #[serde(default)]
    pub claimed_by: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookRow {
    pub test: String,
    pub bead: String,
}

/// A read of the ops pane's queue section at a virtual time, which must list exactly `rows`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Look {
    pub at: u64,
    #[serde(default)]
    pub rows: Vec<LookRow>,
}

/// What a loop scenario scripts.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    /// `"loop"` in a scenario file; ignored by [`run`].
    #[serde(default)]
    pub driver: String,
    /// `<bead>:<STATE>` the run must end in; `sim run` fails when it does not.
    #[serde(default)]
    pub goal: Option<String>,
    /// The repo publishes by PR: `sim run` also fails unless a publish PR opened at the head of
    /// the last landing.
    #[serde(default)]
    pub publish: bool,
    #[serde(default, rename = "bead")]
    pub beads: Vec<Bead>,
    #[serde(default = "one")]
    pub max_concurrent: u32,
    /// Until when (virtual ms) the repo's `rounds` lane is stopped (a scoped stop).
    #[serde(default)]
    pub stopped_until: u64,
    #[serde(default, rename = "verdict")]
    pub verdicts: Vec<VerdictAt>,
    #[serde(default, rename = "flip")]
    pub flips: Vec<Flip>,
    #[serde(default, rename = "lift")]
    pub lifts: Vec<Lift>,
    #[serde(default, rename = "look")]
    pub looks: Vec<Look>,
    /// Where the run stops, in virtual ms.
    pub until: u64,
    /// Settings the service boots with over [`settings_with`]'s defaults, by key.
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
}

/// A loop scenario file: `driver = "loop"` and the [`Script`] keys.
pub fn parse(text: &str) -> Result<Script, String> {
    let s: Script = spira_config::parse_foreign(text).map_err(|e| format!("loop scenario: {e}"))?;
    if s.driver != DRIVER {
        return Err(format!("loop scenario: driver is {:?}, not {DRIVER:?}", s.driver));
    }
    if s.until == 0 {
        return Err("loop scenario: until must be a positive virtual time".into());
    }
    if let Some(g) = &s.goal {
        let (b, _) = crate::drive::parse_bead_state(g)?;
        if !s.beads.iter().any(|x| x.id == b) && !s.flips.iter().any(|f| f.follow_on == b) {
            return Err(format!("loop scenario: goal {g:?} names no bead the scenario files or a flip files"));
        }
    }
    let mut seen: Vec<&str> = vec![];
    for b in &s.beads {
        if seen.contains(&b.id.as_str()) || b.submits.is_empty() || b.submits.iter().any(|o| *o >= DWARF_MS) {
            return Err(format!("loop scenario: bead {} repeats, or submits outside its {DWARF_MS} ms session", b.id));
        }
        seen.push(&b.id);
    }
    if let Some(v) = s.verdicts.iter().find(|v| !seen.contains(&v.bead.as_str())) {
        return Err(format!("loop scenario: a verdict names {}, which the scenario does not file", v.bead));
    }
    for f in &s.flips {
        if f.members.iter().any(|m| !seen.contains(&m.as_str())) || seen.contains(&f.follow_on.as_str()) {
            return Err(format!("loop scenario: flip {} names a member the scenario does not file, or a follow-on it files itself", f.test));
        }
    }
    for l in &s.lifts {
        if !seen.contains(&l.owner.as_str()) || seen.contains(&l.fix.as_str()) || l.claimed_by.as_ref().is_some_and(|c| !seen.contains(&c.as_str())) {
            return Err(format!("loop scenario: lift {} names an owner the scenario does not file, or a fix it files itself", l.test));
        }
    }
    Ok(s)
}

/// Whether a scenario file's text is a loop world's.
pub fn is_loop(text: &str) -> bool {
    #[derive(Deserialize)]
    struct Driver {
        #[serde(default)]
        driver: String,
    }
    spira_config::parse_foreign::<Driver>(text).is_ok_and(|d| d.driver == DRIVER)
}

pub enum Due {
    File(String),
    Submit { bead: String, actor: String, n: usize },
    Verdict { bead: String, verdict: Verdict },
    Look(usize),
}

/// A registry row a round's flip handling wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct Disabled {
    pub test: String,
    pub bead: String,
    pub since: i64,
    pub until: i64,
}

/// A registry row a round kept after its owner lifted it.
#[derive(Clone, Debug, PartialEq)]
pub struct Kept {
    pub test: String,
    pub bead: String,
    pub reason: String,
    pub since: i64,
    pub until: i64,
}

/// What the ops pane's queue section showed at a look.
#[derive(Clone, Debug)]
pub struct Looked {
    pub at: u64,
    pub held: Vec<registryworld::Held>,
    pub pane: String,
}

/// A write the scenario made from outside the loop, and whether the store applied it.
#[derive(Clone, Debug)]
pub struct Outside {
    pub state_before: BeadState,
    pub applied: bool,
}
#[derive(Default)]
pub struct Lc {
    pub rows: BTreeMap<String, LifecycleRow>,
    pub due: Vec<(u64, Due)>,
    pub history: Vec<(u64, String, String, String)>,
    pub refused: Vec<String>,
    pub outside: Vec<Outside>,
    pub round: Option<(String, Vec<(String, String)>)>,
    /// The tree `round open` was handed, which the queue records as the round's worktree.
    pub round_wt: String,
    /// The members of the open round whose own gate went red there, as `id -> (tip, the gate's
    /// mark)`: the row stays IN_DELIVERY and carries the mark, it does not leave the round.
    pub gate_red: BTreeMap<String, (String, String)>,
    pub rounds: u32,
    /// Every claim the loop made: the bead and the tips it stacked on.
    pub claims: Vec<(String, lifecycle::bead::Stack)>,
    pub dwarves: BTreeMap<String, u64>,
    pub items: BTreeMap<String, String>,
    /// Per bead: the claims it has had (its sessions), so each session's tips are distinct.
    pub sessions: BTreeMap<String, usize>,
    /// The tips each bead was submitted at, in order.
    pub submitted: BTreeMap<String, Vec<String>>,
    /// Per submitted tip: how many landings had touched its file when it was built.
    pub built_on: BTreeMap<String, usize>,
    /// The beads landed, in order, with the file each touched.
    pub landed: Vec<(String, String)>,
    /// Every round opened, with its members as the batcher cut them.
    pub opened: Vec<Vec<(String, String)>>,
    /// Every round landed, with its members at their tips.
    pub lands: Vec<Vec<(String, String)>>,
    /// Every pass sift recorded, at its tip.
    pub sifted: Vec<(String, String)>,
    /// The publish PRs the forge holds, each at the head it was opened for.
    pub prs: Vec<(u64, String)>,
    /// The publish records written, as `(pr, head)`.
    pub published: Vec<(u64, String)>,
    pub reg: Option<Arc<Registry>>,
    pub disabled: Vec<Disabled>,
    /// The rows a round kept: re-pointed at the fix it filed, with the reason the registry recorded.
    pub kept: Vec<Kept>,
    pub looks: Vec<(usize, Looked)>,
    /// Corpus passes the world scripted red: when, the round, the test.
    pub reds: Vec<(u64, String, String)>,
    /// The registry rows a landing cleared, with when.
    pub cleared: Vec<(u64, Vec<String>)>,
}

impl Lc {
    /// The local main: the base, moved by each landing.
    pub fn main(&self) -> String {
        if self.lands.is_empty() {
            BASE.into()
        } else {
            main_after(self.lands.len())
        }
    }
}

impl Lc {
    /// A bead filed READY at `now`, with no session of its own until a dwarf claims it.
    pub fn file(&mut self, now: u64, id: &str) {
        self.rows.insert(id.into(), LifecycleRow::filed(id));
        self.items.insert(id.into(), format!("{id}: the scenario's work\n"));
        self.history.push((now, id.into(), "-".into(), BeadState::Ready.as_str().into()));
    }

    pub fn event(&mut self, now: u64, id: &str, actor: &str, kind: BeadEventKind) -> Result<(), String> {
        let row = self.rows.get(id).cloned().ok_or_else(|| format!("no row {id}"))?;
        let ev = BeadEvent { expect: row.state, version: row.version, kind, actor: actor.into(), at: Some((now / 1000) as i64) };
        let out = lifecycle::bead::apply(&row, &ev);
        if !out.applied {
            return Err(format!("{id} {:?} refused {:?}: {:?}", row.state, ev.kind, out.refusal));
        }
        if out.row.state != row.state {
            self.history.push((now, id.into(), row.state.as_str().into(), out.row.state.as_str().into()));
        }
        self.rows.insert(id.into(), out.row);
        Ok(())
    }

    /// One event of the loop through the store's machine; a refusal is recorded and returned.
    pub fn apply(&mut self, now: u64, id: &str, actor: &str, kind: BeadEventKind) -> Result<(), String> {
        self.event(now, id, actor, kind).inspect_err(|why| self.refused.push(why.clone()))
    }

    fn landings_of(&self, file: &str) -> usize {
        self.landed.iter().filter(|(_, f)| f == file).count()
    }

    fn settle(&mut self, now: u64, script: &Script) {
        let (ready, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.due).into_iter().partition(|(at, _)| *at <= now);
        self.due = later;
        let mut ready = ready;
        ready.sort_by_key(|(at, _)| *at);
        for (at, d) in ready {
            match d {
                Due::File(id) => self.file(at, &id),
                Due::Submit { bead, actor, n } => {
                    let session = self.sessions.get(&bead).copied().unwrap_or(1) - 1;
                    let per = script.beads.iter().find(|b| b.id == bead).map_or(1, |b| b.submits.len());
                    let tip = tip_of(&bead, session * per + n);
                    let file = script.beads.iter().find(|b| b.id == bead).map_or(bead.as_str(), |b| b.file()).to_string();
                    if self.apply(at, &bead, &actor, BeadEventKind::Submit { tip: tip.clone() }).is_ok() {
                        let landings = self.landings_of(&file);
                        self.built_on.insert(tip.clone(), landings);
                        self.submitted.entry(bead).or_default().push(tip);
                    }
                }
                Due::Look(i) => {
                    if let Some(reg) = &self.reg {
                        let pane = reg.pane(at, self.rows.values().filter(|r| r.state == BeadState::Certified).count());
                        self.looks.push((i, Looked { at, held: pane.held, pane: pane.text }));
                    }
                }
                Due::Verdict { bead, verdict } => {
                    let Some(row) = self.rows.get(&bead).cloned() else { continue };
                    let tip = row.tip.clone().unwrap_or_default();
                    // `spira-lc certify <bead> <tip> pass`: a row already CERTIFIED at that tip
                    // answers APPLIED with no event (spira-lc's own `certify_voids_a_stale_
                    // certification_first_and_leaves_a_current_one_alone`).
                    let in_round = self.round.as_ref().is_some_and(|(_, m)| m.iter().any(|(id, t)| *id == bead && *t == tip));
                    let applied = match verdict {
                        Verdict::Red if in_round => {
                            self.gate_red.insert(bead.clone(), (tip, GateRedReason::SuitesFailed.as_str().to_string()));
                            true
                        }
                        Verdict::Pass if row.state == BeadState::Certified => true,
                        Verdict::Pass => self.event(at, &bead, "gate", BeadEventKind::GatePass { tip, gate_key: format!("gate:{bead}") }).is_ok(),
                        Verdict::Red => self.event(at, &bead, "gate", BeadEventKind::GateRed { tip, reason: GateRedReason::SuitesFailed, step: None, message: None }).is_ok(),
                    };
                    self.outside.push(Outside { state_before: row.state, applied });
                }
            }
        }
    }

    /// The rows `spira-claim` would offer a dwarf: READY or REWORK and under no hold.
    fn open_rows(&self) -> impl Iterator<Item = &LifecycleRow> {
        self.rows.values().filter(|r| r.holds.is_empty() && matches!(r.state, BeadState::Ready | BeadState::Rework))
    }

    /// The stack a claim of `id` would carry, or `None` while a blocker is neither finished nor
    /// stackable (certified or in a round, and not known red).
    fn stack_for(&self, script: &Script, id: &str) -> Option<lifecycle::bead::Stack> {
        let mut stack = lifecycle::bead::Stack::new();
        for blocker in script.beads.iter().find(|b| b.id == id).map(|b| b.blocks.as_slice()).unwrap_or_default() {
            let row = self.rows.get(blocker)?;
            let gate = spira_config::edges::gate(row.state, row.tip.as_deref(), lifecycle::bead::known_red(row));
            match gate.class {
                spira_config::edges::GateClass::Finished => {}
                spira_config::edges::GateClass::Stackable if !lifecycle::bead::provisional(row.state, row.reason.as_deref()) => {
                    stack.insert(blocker.clone(), gate.tip?);
                }
                _ => return None,
            }
        }
        Some(stack)
    }

    /// The bead a dwarf claims and the stack it builds on: REWORK before READY, then by priority.
    fn claimable(&self, script: &Script) -> Option<(String, lifecycle::bead::Stack)> {
        let prio = |id: &str| script.beads.iter().find(|b| b.id == id).map_or(u8::MAX, |b| b.priority);
        let mut open: Vec<&LifecycleRow> = self.open_rows().filter(|r| self.stack_for(script, &r.bead_id).is_some()).collect();
        open.sort_by_key(|r| (r.state != BeadState::Rework, prio(&r.bead_id), r.bead_id.clone()));
        let id = open.first()?.bead_id.clone();
        let stack = self.stack_for(script, &id)?;
        Some((id, stack))
    }
}

pub struct World {
    pub clock: VirtualClock,
    pub units: Arc<ScriptedUnits>,
    pub lc: Mutex<Lc>,
    pub script: Script,
    pub proc: Arc<CountingProc>,
    /// Round pass scopes a test keeps alive: (batch, virtual ms it ends).
    pub pass_scopes: Mutex<Vec<(String, u64)>>,
}

impl World {
    /// The batches whose pass scope is still alive now.
    pub fn live_pass_scopes(&self) -> Vec<String> {
        let now = self.now();
        self.pass_scopes.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, until)| *until > now).map(|(n, _)| n.clone()).collect()
    }

    pub fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    pub fn lc(&self) -> MutexGuard<'_, Lc> {
        let mut g = self.lc.lock().unwrap_or_else(|e| e.into_inner());
        g.settle(self.clock.now_ms(), &self.script);
        g
    }

    pub fn bead(&self, id: &str) -> Option<&Bead> {
        self.script.beads.iter().find(|b| b.id == id)
    }
}

fn json<T: serde::Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(|e| e.to_string())
}

fn parse_json<T: serde::de::DeserializeOwned>(what: &str, s: &str) -> Result<T, String> {
    serde_json::from_str(s).map_err(|e| format!("{what}: {e}"))
}

// --- sentinel-call ------------------------------------------------------------------------------

fn sentinel_observe(w: &World) -> Result<String, String> {
    let now = w.now();
    let lc = w.lc();
    let live: Vec<String> = lc.dwarves.iter().filter(|(_, end)| **end > now).map(|(u, _)| u.clone()).collect();
    let ready = lc.open_rows().filter(|r| lc.stack_for(&w.script, &r.bead_id).is_some()).count() as u32;
    let snap = sc::Snapshot {
        live_by_rune: [(RUNE.to_string(), live.len() as u32)].into(),
        ready_by_rune: [(RUNE.to_string(), Some(if w.script.max_concurrent == 0 { 0 } else { ready }))].into(),
        urgent: Some(vec![]),
        express_ready: Some(0),
        max_live: Some(4),
        max_dwarves: Some(4),
        roster: vec![RUNE.into()],
        runes: [(RUNE.to_string(), sc::RuneCfg { single_flight: w.script.max_concurrent == 1, elastic: false, uses: vec![], reactive: false })].into(),
        live_units: live,
        now_ms: now,
        ..sc::Snapshot::default()
    };
    json(&snap)
}

/// The dwarf claims the first claimable bead in priority order (READY or REWORK and unheld, as
/// `spira-claim` ranks them) and submits at each of the bead's scripted offsets.
fn sentinel_unit_start(w: &World, input: &str) -> Result<String, String> {
    let start: sc::Start = parse_json(sc::UNIT_START, input)?;
    let now = w.now();
    let unit = units::unit_name(sc::NAME, &start.key).map_err(|e| e.0)?;
    let mut lc = w.lc();
    let (bead, stack) = lc.claimable(&w.script).ok_or("the dwarf found nothing ready")?;
    let (stack_depth, stack_max_depth) = if stack.is_empty() { (0, 0) } else { (1, 4) };
    lc.apply(now, &bead, &unit, BeadEventKind::Claim { holder: unit.clone(), lease_until: ((now + DWARF_MS) / 1000) as i64 + 600, stack: stack.clone(), stack_depth, stack_max_depth, persona: None, directed: None })?;
    lc.claims.push((bead.clone(), stack));
    *lc.sessions.entry(bead.clone()).or_default() += 1;
    let submits = w.bead(&bead).map_or(vec![DWARF_MS - 1], |b| b.submits.clone());
    for (n, off) in submits.iter().enumerate() {
        lc.due.push((now + off, Due::Submit { bead: bead.clone(), actor: unit.clone(), n }));
    }
    lc.dwarves.insert(unit, now + DWARF_MS);
    drop(lc);
    w.units.script(sc::NAME, DWARF_MS, Exit::Code(0));
    let spec = UnitSpec { task: sc::NAME.into(), key: start.key, argv: vec!["dwarf".into(), "--home".into(), "sim/home".into(), start.rune], env: vec![], cwd: None };
    units::effect(&*w.units, UNIT_START, &json(&StartSpec { unit: spec, props: units::dwarf_props("3600")? })?)
}

// --- sift ---------------------------------------------------------------------------------------

fn pool_row(r: &LifecycleRow) -> sift::PoolRow {
    sift::PoolRow { id: r.bead_id.clone(), state: r.state.as_str().into(), version: r.version, tip: r.tip.clone().unwrap_or_default(), holds: vec![], ejected_red_tip: r.ejected_red_tip.clone(), sifted_tip: r.sifted_tip.clone() }
}

/// A candidate conflicts with the base when a bead touching its file landed after its tip was
/// built: git's `merge-tree` of that tip on the moved base.
fn sift_observe(w: &World) -> Result<String, String> {
    let now = w.now();
    let lc = w.lc();
    let rows: Vec<&LifecycleRow> = lc.rows.values().filter(|r| matches!(r.state, BeadState::Submitted | BeadState::Certified)).collect();
    let mut repo = sift::Repo { rows: rows.iter().map(|r| pool_row(r)).collect(), fetched: true, read_at_ms: now, identity_pages: Some(vec![]), ..sift::Repo::default() };
    for r in &rows {
        let tip = r.tip.clone().unwrap_or_default();
        let file = w.bead(&r.bead_id).map_or(r.bead_id.as_str(), |b| b.file());
        let stale = lc.built_on.get(&tip).is_some_and(|n| lc.landings_of(file) > *n);
        let conflicts = if stale { vec![format!("out/{file}.txt")] } else { vec![] };
        repo.branch_tips.insert(r.bead_id.clone(), tip.clone());
        repo.items.insert(r.bead_id.clone(), lc.items.get(&r.bead_id).cloned().unwrap_or_default());
        repo.candidates.insert(r.bead_id.clone(), sift::Candidate { base: BASE.into(), patch_id: Some(format!("patch-{tip}")), conflicts, ..sift::Candidate::default() });
    }
    if let Some((_, members)) = &lc.round {
        repo.open_round = members.iter().map(|(i, t)| format!("{i}:{t}")).collect();
    }
    json(&sift::Snapshot { repos: [(REPO.to_string(), repo)].into() })
}

fn sift_record_pass(w: &World, input: &str) -> Result<String, String> {
    let wr: sift::Write = parse_json(sift::RECORD_PASS, input)?;
    let now = w.now();
    let mut lc = w.lc();
    lc.apply(now, &wr.id, sift::NAME, BeadEventKind::Sifted { tip: wr.tip.clone() })?;
    lc.sifted.push((wr.id, wr.tip));
    Ok("{}".into())
}

/// `lc gate-red` at the screened tip: the bead goes back to its dwarf.
fn sift_send_back(w: &World, input: &str) -> Result<String, String> {
    let wr: sift::Write = parse_json(sift::SEND_BACK, input)?;
    let now = w.now();
    w.lc().apply(now, &wr.id, sift::NAME, BeadEventKind::GateRed { tip: wr.tip, reason: GateRedReason::SuitesFailed, step: None, message: None })?;
    Ok("{}".into())
}

// --- batcher.local ------------------------------------------------------------------------------

fn batcher_observe(w: &World, input: &str) -> Result<String, String> {
    let carry: bl::Carry = parse_json(bl::OBSERVE, input)?;
    let held = carry.repos.get(REPO).and_then(|rc| rc.batch.clone());
    let lc = w.lc();
    let pool = lc
        .rows
        .values()
        .filter(|r| matches!(r.state, BeadState::Submitted | BeadState::Certified))
        .map(|r| bl::Member { id: r.bead_id.clone(), state: r.state.as_str().into(), version: r.version, tip: r.tip.clone().unwrap_or_default(), sifted_tip: r.sifted_tip.clone(), title: r.bead_id.clone(), priority: w.bead(&r.bead_id).map_or(2, |b| b.priority), ..bl::Member::default() })
        .collect();
    let open_round = lc.round.as_ref().map(|(id, m)| {
        let gate_red = m.iter().filter_map(|(i, _)| lc.gate_red.get(i).map(|(tip, mark)| (i.clone(), tip.clone(), mark.clone()))).collect();
        bl::Round { id: id.clone(), members: m.iter().map(|(i, _)| i.clone()).collect(), head: BASE.into(), worktree: lc.round_wt.clone(), driven: held.as_ref() == Some(id), gate_red, ..bl::Round::default() }
    });
    let live_legs = units::Units::list(&*w.units, &units::task_glob(bl::NAME)).ok().map(|all| bl::running_legs(&all));
    let repo = bl::Repo { pool, open_round, base_sha: BASE.into(), stopped: w.now() < w.script.stopped_until, live_legs, scoped_batches: Some(w.live_pass_scopes()), ..bl::Repo::default() };
    json(&bl::Snapshot { repos: [(REPO.to_string(), repo)].into(), now_ms: w.now(), ..Default::default() })
}

/// The merge of each member on the base in the order the batcher gave (`topo_order` of the
/// leg's members, which carry no priority): a member whose tip is stale on the base (a bead
/// touching its file landed after it was built) conflicts with the base itself; one whose file
/// an earlier member of the round already touched conflicts with the round's accumulation.
fn batcher_assemble(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse_json(bl::ASSEMBLE, input)?;
    let lc = w.lc();
    let (mut merged, mut conflicts, mut base_conflicts, mut touched) = (vec![], vec![], vec![], vec![]);
    for (id, tip) in &leg.members {
        let file = w.bead(id).map_or(id.as_str(), |b| b.file());
        if lc.built_on.get(tip).is_some_and(|n| lc.landings_of(file) > *n) {
            conflicts.push((id.clone(), tip.clone()));
            base_conflicts.push(id.clone());
        } else if touched.contains(&file) {
            conflicts.push((id.clone(), tip.clone()));
        } else {
            touched.push(file);
            merged.push((id.clone(), tip.clone()));
        }
    }
    json(&bl::Assembled { merged, conflicts, base_conflicts, wt: format!("sim/wt/{}", bl::digest(&leg.round_key)), base: leg.base, suites: true, ..bl::Assembled::default() })
}

pub fn batcher_round_open(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse_json(bl::ROUND_OPEN, input)?;
    let mut lc = w.lc();
    if lc.round.is_some() {
        return Err("a round is already open".into());
    }
    lc.rounds += 1;
    let batch = format!("round-{}", lc.rounds);
    lc.opened.push(leg.members.clone());
    lc.round_wt = leg.wt;
    lc.round = Some((batch.clone(), leg.members));
    json(&bl::Opened { batch, ..bl::Opened::default() })
}

/// The flips of `script` the round of `leg` carries that the registry does not hold out of the
/// run at `now`: red on the combined tree.
fn undenied(w: &World, leg: &bl::Leg) -> Result<Vec<Flip>, String> {
    let Some(reg) = w.lc().reg.clone() else { return Ok(vec![]) };
    let denied = reg.denied(w.now())?;
    Ok(w.script.flips.iter().filter(|f| f.members.iter().any(|m| leg.members.iter().any(|(id, _)| id == m)) && !denied.iter().map(String::as_str).any(|d| d == f.test)).cloned().collect())
}

fn note_reds(w: &World, leg: &bl::Leg, red: &[Flip]) {
    let now = w.now();
    w.lc().reds.extend(red.iter().map(|f| (now, leg.batch.clone(), f.test.clone())));
}

fn run_leg(w: &World, leg: &bl::Leg, argv: Vec<String>, code: i32, ms: u64, said: &str, scoped: bool) -> Result<String, String> {
    let spec = UnitSpec { task: bl::NAME.into(), key: leg.unit.clone(), argv, env: vec![], cwd: None };
    w.units.script(bl::NAME, ms, Exit::Code(code));
    if !said.is_empty() {
        w.units.script_output(bl::NAME, said.as_bytes(), b"");
    }
    if scoped {
        units::effect(&*w.units, UNIT_RUN_SCOPE, &json(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None })?)
    } else {
        units::effect(&*w.units, UNIT_START, &json(&StartSpec { unit: spec, props: Default::default() })?)
    }
}

/// `batcher attribute` on a red round: a flip is disabled through batcher-cut's own flip handling
/// against the world's registry, and a test the registry already disabled once reproduces.
fn attribute_flips(w: &World, leg: &bl::Leg, flips: &[Flip]) -> Result<String, String> {
    let now = w.now();
    let reg = w.lc().reg.clone().ok_or("flips without a registry")?;
    for f in flips {
        if reg.ever_disabled(&f.test)? {
            let why = format!("{} reproduces on the rerun: red on the round tree and on the base", f.test);
            let said = format!("attribution\t{}\n", serde_json::json!({ "end": "blocked", "why": why }));
            return run_leg(w, leg, vec!["batcher".into(), "attribute".into()], 0, CORPUS_MS, &said, true);
        }
    }
    let pass = reg.pass_dir(now, &format!("pass-{}", leg.batch))?;
    let tests: Vec<String> = flips.iter().map(|f| f.test.clone()).collect();
    let rows = {
        let mut lc = w.lc();
        let mut file_bead = |test: &str| {
            let f = flips.iter().find(|f| f.test == test).ok_or_else(|| format!("{test} is no flip"))?;
            lc.file(now, &f.follow_on);
            if !f.follow_on_lands {
                lc.apply(now, &f.follow_on, bl::NAME, BeadEventKind::Hold { kind: HoldKind::Manual, cause: HoldCause::ManualHold, detail: Some("a follow-on the scenario never lands".into()) })?;
            }
            Ok(f.follow_on.clone())
        };
        batcher_cut::io::disable_flips_with(&mut file_bead, &mut |t, b, r| reg.disable(now, t, b, r), &pass, &leg.batch, &tests)?
    };
    let since = registryworld::epoch_at(now);
    w.lc().disabled.extend(rows.iter().map(|r| Disabled { test: r.test.clone(), bead: r.bead.clone(), since, until: r.until }));
    let said = format!("attribution\t{}\n", serde_json::json!({ "end": "land", "members": leg.members, "attribution_secs": 60, "mended": false }));
    run_leg(w, leg, vec!["batcher".into(), "attribute".into()], 0, CORPUS_MS, &said, true)
}

/// The corpus is green unless the scenario's flips make it red: these ports turn on the store and
/// the cut, not on attribution.
fn batcher_corpus(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse_json(bl::CORPUS, input)?;
    let red = undenied(w, &leg)?;
    let lifted = lifted_reds(w, &leg)?;
    let blocking = blocking_again(w);
    if leg.why == bl::ATTRIBUTE && !lifted.is_empty() {
        return attribute_lifts(w, &leg, &lifted);
    }
    if leg.why == bl::ATTRIBUTE && red.is_empty() && !blocking.is_empty() {
        let why = format!("{} reproduces on the rerun: red on the round tree and on the base", blocking[0].test);
        let said = format!("attribution\t{}\n", serde_json::json!({ "end": "blocked", "why": why }));
        return run_leg(w, &leg, vec!["batcher".into(), "attribute".into()], 0, CORPUS_MS, &said, true);
    }
    if leg.why == bl::ATTRIBUTE && !red.is_empty() {
        return attribute_flips(w, &leg, &red);
    }
    note_reds(w, &leg, &red);
    let now = w.now();
    w.lc().reds.extend(lifted.iter().map(|l| &l.0).chain(blocking.iter()).map(|l| (now, leg.batch.clone(), l.test.clone())));
    let any = !red.is_empty() || !lifted.is_empty() || !blocking.is_empty();
    run_leg(w, &leg, vec!["round-vm".into(), "run".into(), leg.wt.clone()], i32::from(any), CORPUS_MS, "", false)
}

/// The lifts whose row this round's members own, and whose test is red on the merged tree: the
/// round lifts the row through batcher-cut's own `DenySnapshot::lifting`.
fn lifted_reds(w: &World, leg: &bl::Leg) -> Result<Vec<(Lift, batcher_cut::vm::LiftedRow)>, String> {
    let Some(reg) = w.lc().reg.clone() else { return Ok(vec![]) };
    if w.script.lifts.is_empty() {
        return Ok(vec![]);
    }
    let dir = reg.pass_dir(w.now(), &format!("probe-{}", leg.batch))?;
    let ids: Vec<String> = leg.members.iter().map(|(id, _)| id.clone()).collect();
    let (_, rows) = batcher_cut::vm::DenySnapshot::from_pass(&dir).ok_or("no deny list")?.lifting(&ids, &claims_among(w, &ids));
    Ok(rows.into_iter().filter_map(|r| w.script.lifts.iter().find(|l| l.test == r.test && l.red).map(|l| (l.clone(), r))).collect())
}

/// (test, member) of each scripted claim whose member is among `ids`.
fn claims_among(w: &World, ids: &[String]) -> Vec<(String, String)> {
    w.script.lifts.iter().filter_map(|l| Some((l.test.clone(), l.claimed_by.clone().filter(|c| ids.contains(c))?))).collect()
}

fn names(list: &[String], test: &str) -> bool {
    list.iter().any(|t| t == test)
}

/// The kept tests whose row has expired while still red: they block any round again.
fn blocking_again(w: &World) -> Vec<Lift> {
    let Some(reg) = w.lc().reg.clone() else { return vec![] };
    let denied = reg.denied(w.now()).unwrap_or_default();
    let kept: Vec<String> = w.lc().kept.iter().map(|k| k.test.clone()).collect();
    w.script.lifts.iter().filter(|l| l.red && names(&kept, &l.test) && !names(&denied, &l.test)).cloned().collect()
}

/// `batcher attribute` on a round whose lifted test is red on the tree and the base: the row is
/// kept through batcher-cut's own `keep_row_with`, and the round lands every member.
fn attribute_lifts(w: &World, leg: &bl::Leg, lifted: &[(Lift, batcher_cut::vm::LiftedRow)]) -> Result<String, String> {
    let now = w.now();
    let reg = w.lc().reg.clone().ok_or("lifts without a registry")?;
    let ids: Vec<String> = leg.members.iter().map(|(id, _)| id.clone()).collect();
    let pass = reg.pass_dir(now, &format!("pass-{}", leg.batch))?;
    let (lifted_deny, _) = batcher_cut::vm::DenySnapshot::from_pass(&pass).ok_or("no deny list")?.lifting(&ids, &claims_among(w, &ids));
    std::fs::write(pass.join(batcher_cut::vm::DenySnapshot::FILE), &lifted_deny.text).map_err(|e| e.to_string())?;
    let (head, base) = {
        let lc = w.lc();
        (main_after(lc.lands.len() + 1), lc.main())
    };
    for (lift, row) in lifted {
        let kept = batcher_cut::io::KeptRow {
            test: lift.test.clone(),
            owner: lift.owner.clone(),
            old_bead: row.bead.clone(),
            round_branch: leg.batch.clone(),
            head: head.clone(),
            base: base.clone(),
            merged_lines: vec![format!("not ok {}", lift.test)],
            base_lines: vec![format!("not ok {}", lift.test)],
        };
        let mut reason = String::new();
        let disabled = {
            let mut lc = w.lc();
            let mut file = |_: &batcher_cut::io::KeptRow| {
                lc.file(now, &lift.fix);
                lc.apply(now, &lift.fix, bl::NAME, BeadEventKind::Hold { kind: HoldKind::Manual, cause: HoldCause::ManualHold, detail: Some("a fix the scenario never lands".into()) })?;
                Ok(lift.fix.clone())
            };
            batcher_cut::io::keep_row_with(
                &mut file,
                &mut |t, b, r| {
                    reason = r.to_string();
                    reg.disable(now, t, b, r)
                },
                &pass,
                &kept,
            )?
        };
        w.lc().kept.push(Kept { test: disabled.test, bead: disabled.bead, reason, since: registryworld::epoch_at(now), until: disabled.until });
    }
    let said = format!("attribution\t{}\n", serde_json::json!({ "end": "land", "members": leg.members, "attribution_secs": 60, "mended": false }));
    run_leg(w, leg, vec!["batcher".into(), "attribute".into()], 0, CORPUS_MS, &said, true)
}

fn batcher_certify(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse_json(bl::CERTIFY, input)?;
    let red = undenied(w, &leg)?;
    note_reds(w, &leg, &red);
    let lifted = lifted_reds(w, &leg)?;
    let again = blocking_again(w);
    if let Some(l) = lifted.first().map(|(l, _)| l).or(again.first()) {
        let now = w.now();
        w.lc().reds.push((now, leg.batch.clone(), l.test.clone()));
        let argv = vec!["queue".into(), "round".into(), "certify".into(), leg.batch.clone(), REPO.into()];
        return run_leg(w, &leg, argv, 1, CERTIFY_MS, &format!("red={}\n", l.test.replace(' ', "_")), true);
    }
    let said = if red.is_empty() { "certified\n".to_string() } else { format!("red={}\n", red[0].test.replace(' ', "_")) };
    let argv = vec!["queue".into(), "round".into(), "certify".into(), leg.batch.clone(), REPO.into()];
    run_leg(w, &leg, argv, i32::from(!red.is_empty()), CERTIFY_MS, &said, true)
}

/// `round certify`'s store half (`lc_certify_round`): every SUBMITTED member to CERTIFIED at its
/// tip; a member in any other state (already CERTIFIED by a gate) is left alone.
fn batcher_lc_certify(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse_json(bl::LC_CERTIFY, input)?;
    let now = w.now();
    let mut lc = w.lc();
    for (id, tip) in &leg.members {
        if lc.rows.get(id).is_some_and(|r| r.state == BeadState::Submitted) {
            lc.apply(now, id, bl::NAME, BeadEventKind::GatePass { tip: tip.clone(), gate_key: format!("round:{}", leg.round_key) })?;
        }
    }
    Ok("{}".into())
}

fn batcher_land(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse_json(bl::LAND, input)?;
    let now = w.now();
    let mut lc = w.lc();
    match &lc.round {
        Some((batch, _)) if *batch == leg.batch => {}
        other => return Err(format!("land {}: the open round is {other:?}", leg.batch)),
    }
    for (id, _) in &leg.members {
        lc.apply(now, id, bl::NAME, BeadEventKind::Deliver)?;
        let merge_sha = main_after(lc.lands.len() + 1);
        lc.apply(now, id, bl::NAME, BeadEventKind::Delivered { merge_sha, proof: format!("round {}", leg.batch) })?;
        let file = w.bead(id).map_or(id.as_str(), |b| b.file()).to_string();
        lc.landed.push((id.clone(), file));
    }
    if let Some(reg) = lc.reg.clone() {
        let ids: Vec<String> = leg.members.iter().map(|(id, _)| id.clone()).collect();
        match reg.clear_landed(now, &ids) {
            Ok(cleared) if !cleared.is_empty() => lc.cleared.push((now, cleared)),
            Ok(_) => {}
            Err(e) => lc.refused.push(e),
        }
        let kept: Vec<String> = lc.kept.iter().map(|k| k.test.clone()).collect();
        let standing: Vec<(String, String)> = claims_among(w, &ids).into_iter().filter(|(t, _)| !names(&kept, t)).collect();
        let mut activated = vec![];
        for failed in batcher_cut::io::activate_claimed_with(
            &mut |t, reason| {
                activated.push(t.to_string());
                reg.activate(now, t, reason)
            },
            &leg.batch,
            &standing,
        ) {
            lc.refused.push(format!("{}: {}", failed.0, failed.1));
        }
        if !activated.is_empty() {
            lc.cleared.push((now, activated));
        }
    }
    lc.lands.push(leg.members.clone());
    lc.round = None;
    json(&bl::Landed { staged: None, base_moved: None })
}

/// `queue round eject` for a gate-red member: it leaves the open round and goes back to its dwarf.
fn batcher_eject_gate_red(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse_json(bl::EJECT_GATE_RED, input)?;
    let now = w.now();
    let mut lc = w.lc();
    let ids: Vec<&str> = leg.bead.split(',').filter(|s| !s.is_empty()).collect();
    match &mut lc.round {
        Some((batch, members)) if *batch == leg.batch => members.retain(|(id, _)| !ids.iter().any(|i| i == id)),
        other => return Err(format!("eject {}: the open round is {other:?}", leg.batch)),
    }
    for id in ids {
        lc.gate_red.remove(id);
        lc.apply(now, id, bl::NAME, BeadEventKind::Deliver)?;
        lc.apply(now, id, bl::NAME, BeadEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BatchEjectedRed })?;
    }
    Ok("{}".into())
}

fn batcher_abandon(w: &World) -> Result<String, String> {
    w.lc().round = None;
    Ok("{}".into())
}

/// `round withdraw` (`lc_withdraw`): a CERTIFIED member that kept conflicting is delivered and
/// returned (`batch-ejected`) to REWORK; a member in any other state is left alone.
fn batcher_withdraw(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse_json(bl::WITHDRAW, input)?;
    let now = w.now();
    let mut lc = w.lc();
    if lc.rows.get(&leg.bead).is_some_and(|r| r.state == BeadState::Certified) {
        lc.apply(now, &leg.bead, bl::NAME, BeadEventKind::Deliver)?;
        lc.apply(now, &leg.bead, bl::NAME, BeadEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BatchEjected })?;
    }
    Ok("{}".into())
}

fn queue_step_observe(w: &World) -> Result<String, String> {
    let repo = qs::Repo { mode: "queue.local".into(), lc_ok: true, open_batch: None, base_sha: w.lc().main(), ..qs::Repo::default() };
    json(&qs::Snapshot { repos: [(REPO.to_string(), repo)].into(), world_running: true })
}

// --- publish ------------------------------------------------------------------------------------

/// A `queue.local` repo that publishes by PR to a forge still at the base: its local main is
/// ahead (a fast-forward) once a round lands, and the forge's PR for a head is open, untested.
fn publish_observe(w: &World) -> Result<String, String> {
    let lc = w.lc();
    let head = lc.main();
    let rec = lc.published.last().map(|(pr, h)| pb::PublishRec { pr: *pr, head: h.clone(), branch: format!("publish/{}", &h[..12]), rerun: false, alarmed: false, ..Default::default() });
    let pr = match lc.prs.last() {
        Some(_) => pb::Pr::Open { ci: "pending".into(), behind: lc.prs.last().is_some_and(|(_, h)| *h != head), tries: 0 },
        None => pb::Pr::Gone,
    };
    let repo = pb::Repo {
        mode: "queue.local".into(),
        publish_mode: "pr".into(),
        unpublished: !w.script.publish,
        publish_rec: rec,
        pr,
        open_publish_prs: lc.prs.iter().map(|(n, _)| *n).collect(),
        forge_sha: Some(BASE.into()),
        head_sha: head,
        diverged: Some(false),
        ..pb::Repo::default()
    };
    json(&pb::Snapshot { repos: [(REPO.to_string(), repo)].into(), world_running: true })
}

fn publish_push_branch(_: &World, input: &str) -> Result<String, String> {
    let p: pb::Pub = parse_json(pb::PUSH_BRANCH, input)?;
    json(&pb::Pub { branch: format!("publish/{}", &p.head[..12.min(p.head.len())]), ..p })
}

fn publish_pr_create(w: &World, input: &str) -> Result<String, String> {
    let p: pb::Pub = parse_json(pb::PR_CREATE, input)?;
    let mut lc = w.lc();
    let pr = 100 + lc.prs.len() as u64;
    lc.prs.push((pr, p.head));
    json(&pb::Created { pr })
}

/// A new PR's record: refused, as queue's `settle_publish` would refuse the record, unless the
/// act carries the PR, its branch, its head and the base its members are read from.
fn publish_record(w: &World, input: &str) -> Result<String, String> {
    let p: pb::Pub = parse_json(pb::RECORD, input)?;
    if p.record.is_empty() && (p.pr == 0 || p.branch.is_empty() || p.head.is_empty() || p.base.is_empty()) {
        return Err(format!("{}: a new publish record without its PR, branch, head or base: {p:?}", pb::RECORD));
    }
    w.lc().published.push((p.pr, p.head));
    Ok("{}".into())
}

/// A double over the world: what [`run_with`]'s `extra` lays over the loop's own.
pub type WorldDouble = fn(&World, &str) -> Result<String, String>;

/// `d` with `name` answered by `f` over `w`.
pub fn on(d: Doubles, w: &Arc<World>, name: &str, f: WorldDouble) -> Doubles {
    let w = w.clone();
    d.double(name, move |input| f(&w, input))
}

fn doubles(w: &Arc<World>) -> Doubles {
    let forks = w.proc.clone();
    let d = sim::units::with_units(sim::doubles::service(), w.units.clone()).double("fork", move |_| {
        let spec = ProcSpec { prog: "planted".into(), args: vec![], stdin: vec![], deadline: std::time::Duration::from_secs(1) };
        forks.run(&spec).map(|_| String::new())
    });
    let d = on(d, w, sc::OBSERVE, |w, _| sentinel_observe(w));
    let d = on(d, w, sc::UNIT_START, sentinel_unit_start);
    let d = on(d, w, sift::OBSERVE, |w, _| sift_observe(w));
    let d = on(d, w, sift::RECORD_PASS, sift_record_pass);
    let d = on(d, w, sift::SEND_BACK, sift_send_back);
    let d = on(d, w, bl::OBSERVE, batcher_observe);
    let d = on(d, w, bl::ASSEMBLE, batcher_assemble);
    let d = on(d, w, bl::ROUND_OPEN, batcher_round_open);
    let d = on(d, w, bl::CORPUS, batcher_corpus);
    let d = on(d, w, bl::CERTIFY, batcher_certify);
    let d = on(d, w, bl::LC_CERTIFY, batcher_lc_certify);
    let d = on(d, w, bl::LAND, batcher_land);
    let d = on(d, w, bl::ABANDON, |w, _| batcher_abandon(w));
    let d = on(d, w, bl::WITHDRAW, batcher_withdraw);
    let d = on(d, w, bl::EJECT_GATE_RED, batcher_eject_gate_red);
    let d = on(d, w, qs::OBSERVE, |w, _| queue_step_observe(w));
    let d = on(d, w, pb::OBSERVE, |w, _| publish_observe(w));
    let d = on(d, w, pb::PUSH_BRANCH, publish_push_branch);
    let d = on(d, w, pb::PR_CREATE, publish_pr_create);
    on(d, w, pb::RECORD, publish_record)
}

/// The doubles, each answer timed: one `exec.log` line per effect.
struct Timed {
    inner: Arc<Doubles>,
    clock: VirtualClock,
    log: Mutex<Vec<String>>,
}

impl EffectRunner for Timed {
    fn run(&self, key: &str, spec: &EffectSpec) -> Result<String, String> {
        let t = std::time::Instant::now();
        let out = self.inner.run(key, spec);
        let line = format!("=== t={} {} exit status: {} in {}ms", self.clock.now_ms(), spec.name, i32::from(out.is_err()), t.elapsed().as_millis());
        self.log.lock().unwrap_or_else(|e| e.into_inner()).push(line);
        out
    }
}

/// The complete fixture (`testlib/complete.toml`) under the harness home: the sim is a binary,
/// and the tree it was compiled in is not where it runs.
fn complete_fixture_text() -> Option<String> {
    let path = spira_config::resolve::locate_home_for_process().ok().map(|h| h.join("testlib/complete.toml"))?;
    std::fs::read_to_string(path).ok()
}

/// `key`'s value under `[spira]` in the complete fixture (`spira/testlib/complete.toml`).
fn complete_fixture(key: &str) -> Option<String> {
    let doc: toml::Value = spira_config::parse_foreign(&complete_fixture_text()?).ok()?;
    let v = doc.get("spira")?.get(spira_config::registry::field_of(key))?;
    Some(v.as_str().map_or_else(|| v.to_string(), str::to_string))
}

/// The settings the loop boots the service's task table with: every task acts, so its effects
/// reach the doubles; the container cap is the host run ceiling; the store flag is off; a calendar key holds the
/// complete fixture's spec; `set` overrides, its last entry for a key winning; every other key is 300.
pub fn settings_with(set: &[(&str, &str)]) -> Result<Settings, String> {
    let modes: Vec<String> = tasks::all().map(|t| format!("{}=act", t.name)).collect();
    Settings::from_lookup(&|k| {
        if let Some((_, v)) = set.iter().rev().find(|(s, _)| *s == k) {
            return Ok(v.to_string());
        }
        Ok(match k {
            "SPIRA_TASK_MODES" => modes.join(","),
            "SPIRA_POOL_CAP_CONTAINER" => "2".into(),
            "SPIRA_LC_LIFECYCLE_ON_STORE" => "0".into(),
            k if k.ends_with("_CALENDAR") => complete_fixture(k).ok_or_else(|| format!("{k} has no value in the complete fixture"))?,
            _ => "300".into(),
        })
    })
}

/// What one run left: the journal's trace, the store's history and rows, and what the doubles
/// saw.
pub struct Run {
    pub trace: Vec<(String, String, String, String, u64)>,
    pub history: Vec<(u64, String, String, String)>,
    pub refused: Vec<String>,
    pub outside: Vec<Outside>,
    pub rows: BTreeMap<String, LifecycleRow>,
    pub submitted: BTreeMap<String, Vec<String>>,
    pub claims: Vec<(String, lifecycle::bead::Stack)>,
    pub opened: Vec<Vec<(String, String)>>,
    pub lands: Vec<Vec<(String, String)>>,
    pub sifted: Vec<(String, String)>,
    pub prs: Vec<(u64, String)>,
    pub published: Vec<(u64, String)>,
    pub disabled: Vec<Disabled>,
    pub kept: Vec<Kept>,
    pub looks: Vec<(usize, Looked)>,
    pub reds: Vec<(u64, String, String)>,
    pub cleared: Vec<(u64, Vec<String>)>,
    pub hold_secs: i64,
    pub asked: Vec<String>,
    pub exec_log: Vec<String>,
    pub spawned: u64,
    pub wall: std::time::Duration,
}

/// The settings [`run_with`] boots the service with: the loop's batcher wall, then the script's own.
pub fn settings_for(script: &Script) -> Result<Settings, String> {
    let mut set = vec![("SPIRA_BATCHER_WALL_SECS", "3600")];
    set.extend(script.settings.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    settings_with(&set)
}

/// One run of `script` under `seed` on the loop's doubles.
pub fn run(seed: u64, script: Script) -> Result<Run, String> {
    run_with(seed, script, |d, _| d)
}

/// One run, with `extra` laying the scenario's own doubles over the loop's.
pub fn run_with(seed: u64, script: Script, extra: impl FnOnce(Doubles, &Arc<World>) -> Doubles) -> Result<Run, String> {
    let began = std::time::Instant::now();
    let until = script.until;
    let clock = VirtualClock::new(0);
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let units = Arc::new(ScriptedUnits::new(c.clone()));
    let mut lc = Lc::default();
    for b in &script.beads {
        lc.due.push((b.at, Due::File(b.id.clone())));
    }
    for v in &script.verdicts {
        lc.due.push((v.at, Due::Verdict { bead: v.bead.clone(), verdict: v.verdict }));
    }
    for (i, l) in script.looks.iter().enumerate() {
        lc.due.push((l.at, Due::Look(i)));
    }
    if !script.flips.is_empty() || !script.looks.is_empty() || !script.lifts.is_empty() {
        let hold = script.settings.get(test_registry::cli::HOLD_KEY).cloned().unwrap_or_else(|| registryworld::DEFAULT_HOLD_SECS.to_string());
        let reg = Arc::new(Registry::new(&hold)?);
        for l in &script.lifts {
            reg.disable(0, &l.test, &l.owner, "the owner's flip, disabled until its fix lands")?;
        }
        lc.reg = Some(reg);
    }
    let settings = settings_for(&script)?;
    let world = Arc::new(World { clock: clock.clone(), units: units.clone(), lc: Mutex::new(lc), script, proc: CountingProc::refusing(), pass_scopes: Mutex::default() });
    let inner = Arc::new(extra(doubles(&world), &world));
    let runner = Arc::new(Timed { inner: inner.clone(), clock: clock.clone(), log: Mutex::new(vec![]) });
    let journal: Arc<dyn Journal> = MemJournal::new();
    let mut engine = Engine::open(journal.clone(), c.clone(), registrations(&settings), seed).map_err(|e| format!("the service's registrations: {e:?}"))?;
    for (_, stream, kind) in tasks::boots() {
        engine.start(stream, kind);
    }
    let mut sources = Sources::default();
    sources.register(Box::new(spira_runtime::sources::units::scripted(units.clone(), c.clone()))).map_err(|e| format!("the unit.ran source: {e:?}"))?;
    let mut driver = VirtualDriver::new(engine, clock, runner.clone() as Arc<dyn EffectRunner>).with_sources(sources).map_err(|e| format!("the driver: {e:?}"))?;
    driver.run_until(until).map_err(|e| format!("the loop: {e:?}"))?;
    let lc = world.lc();
    let exec_log = runner.log.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Ok(Run {
        trace: runtime::conformance::trace(&*journal),
        history: lc.history.clone(),
        refused: lc.refused.clone(),
        outside: lc.outside.clone(),
        rows: lc.rows.clone(),
        submitted: lc.submitted.clone(),
        claims: lc.claims.clone(),
        opened: lc.opened.clone(),
        lands: lc.lands.clone(),
        sifted: lc.sifted.clone(),
        prs: lc.prs.clone(),
        published: lc.published.clone(),
        disabled: lc.disabled.clone(),
        kept: lc.kept.clone(),
        looks: lc.looks.clone(),
        reds: lc.reds.clone(),
        cleared: lc.cleared.clone(),
        hold_secs: world.script.settings.get(test_registry::cli::HOLD_KEY).and_then(|v| v.parse().ok()).unwrap_or(registryworld::DEFAULT_HOLD_SECS),
        asked: inner.asked(),
        exec_log,
        spawned: world.proc.spawned(),
        wall: began.elapsed(),
    })
}

impl Run {
    /// `bead`'s lifecycle, as the store recorded it.
    pub fn lifecycle_of(&self, bead: &str) -> Vec<String> {
        self.history.iter().filter(|(_, id, _, _)| id == bead).map(|(_, _, _, to)| to.clone()).collect()
    }

    pub fn landed_at(&self, bead: &str) -> Option<u64> {
        self.history.iter().find(|(_, id, _, to)| id == bead && to == "LANDED").map(|(at, ..)| *at)
    }

    /// Effects some task asked for that no double answers.
    pub fn unanswered(&self) -> Vec<String> {
        self.trace.iter().filter(|e| e.2 == runtime::EFFECT_FAILED && e.3.contains("no runner for effect")).map(|e| e.3.clone()).collect()
    }

    /// The store's history, its refusals and the loop's own streams, for a failure message.
    pub fn dump(&self) -> String {
        let streams = [sc::STREAM, sift::STREAM, bl::STREAM, pb::STREAM];
        let interesting: Vec<String> = self.trace.iter().filter(|e| streams.contains(&e.1.as_str()) && e.2 != "effect.succeeded").map(|e| format!("{:>8} {} {} {}", e.4, e.1, e.2, e.3.chars().take(160).collect::<String>())).collect();
        format!("history {:?}\nrefused {:?}\noutside {:?}\nopened {:?}\nlands {:?}\nsifted {:?}\nprs {:?}\n{}", self.history, self.refused, self.outside, self.opened, self.lands, self.sifted, self.prs, interesting.join("\n"))
    }

    /// Why the registry's walk missed what `script` expects of it, or `None`.
    fn registry_missed(&self, script: &Script) -> Option<String> {
        for f in &script.flips {
            if !self.disabled.iter().any(|d| d.test == f.test && d.bead == f.follow_on) {
                return Some(format!("flip {} was never disabled with follow-on {}: disabled {:?}, reds {:?}", f.test, f.follow_on, self.disabled, self.reds));
            }
            if let Some(after) = f.blocks_again_after {
                if !self.reds.iter().any(|(at, _, t)| *t == f.test && *at >= after) {
                    return Some(format!("{} did not block a round again after {after} ms: reds {:?}", f.test, self.reds));
                }
            }
        }
        for l in script.lifts.iter().filter(|l| l.red) {
            if !self.kept.iter().any(|k| k.test == l.test && k.bead == l.fix && k.reason.starts_with("kept:")) {
                return Some(format!("lift {} was never kept with fix {}: kept {:?}", l.test, l.fix, self.kept));
            }
            if let Some(after) = l.blocks_again_after {
                if !self.reds.iter().any(|(at, _, t)| *t == l.test && *at >= after) {
                    return Some(format!("{} did not block a round again after {after} ms: reds {:?}", l.test, self.reds));
                }
            }
        }
        if let Some(d) = self.disabled.iter().find(|d| d.until - d.since != self.hold_secs) {
            return Some(format!("{} was disabled for {} s, not the registry's hold of {} s", d.test, d.until - d.since, self.hold_secs));
        }
        for (i, look) in script.looks.iter().enumerate() {
            let Some((_, got)) = self.looks.iter().find(|(n, _)| *n == i) else { return Some(format!("the look at {} ms was never taken", look.at)) };
            let mut want: Vec<(String, String)> = look.rows.iter().map(|r| (r.test.clone(), r.bead.clone())).collect();
            let mut shown: Vec<(String, String)> = got.held.iter().map(|(t, b, _)| (t.clone(), b.clone())).collect();
            want.sort();
            shown.sort();
            if want != shown {
                return Some(format!("the ops pane at {} ms held {shown:?}, not {want:?}:\n{}", look.at, got.pane));
            }
            for (t, b, until) in &got.held {
                let drawn = got.pane.lines().any(|l| l.contains("disabled") && l.contains(t.as_str()) && l.contains(b.as_str()) && l.contains("until "));
                if !drawn || *until <= registryworld::epoch_at(look.at) {
                    return Some(format!("the pane at {} ms does not show {t} with {b} and a future expiry:\n{}", look.at, got.pane));
                }
            }
        }
        None
    }

    /// Why this run missed `script`'s goal and publish expectation, or `None` when it met both.
    pub fn missed(&self, script: &Script) -> Option<String> {
        if self.spawned != 0 {
            return Some(format!("{} process(es) spawned: every effect must be a double", self.spawned));
        }
        if let Some(e) = self.unanswered().first() {
            return Some(format!("an effect had no double: {e}"));
        }
        if let Some(g) = &script.goal {
            let (bead, state) = crate::drive::parse_bead_state(g).ok()?;
            let at = self.rows.get(&bead).map(|r| r.state.as_str().to_string());
            if at.as_deref() != Some(state.as_str()) {
                return Some(format!("goal {g} unmet: {bead} is {at:?}"));
            }
        }
        if let Some(why) = self.registry_missed(script) {
            return Some(why);
        }
        if script.publish {
            let head = main_after(self.lands.len());
            if self.lands.is_empty() || !self.published.iter().any(|(_, h)| *h == head) {
                return Some(format!("no publish PR opened at the last landing's head {head}: prs {:?}, records {:?}", self.prs, self.published));
            }
        }
        None
    }
}

#[cfg(test)]
mod complete_fixture_home_tests {
    use super::*;

    #[test_id(t36nfbl)]
    #[test]
    fn the_complete_fixture_is_read_from_spira_home_and_absent_without_one() {
        let h = testkit::TempDir::new("complete-home");
        std::fs::create_dir_all(h.path().join("testlib")).unwrap();
        std::fs::write(h.path().join("testlib/complete.toml"), "marker").unwrap();
        let home = h.path().to_str().unwrap();
        {
            let _env = testkit::env(&[("SPIRA_HOME", Some(home))]);
            assert_eq!(complete_fixture_text().as_deref(), Some("marker"));
        }
        let _env = testkit::env(&[("SPIRA_HOME", None), ("SPIRA_RELEASE", None)]);
        assert_eq!(complete_fixture_text(), None);
    }
}
