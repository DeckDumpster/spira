//! The world scenarios of `spira/sim/scenarios/` whose property is the loop's own, ported to the
//! real-registration driver (ruling D6 of the scheduler consolidation design): every row of
//! `tasks::all()` runs on the `VirtualDriver` and every effect is answered by a double, as in
//! `whole_loop.rs`, whose world this file widens. The world scenarios ran the loop's units as
//! actor processes (`sentinel --call-only`, `landing-pass land`, `round-controller sifts`,
//! `batcher rounds`, `gate-worker run`); those units are tasks of the service now, so a scenario
//! that named them no longer loads. Each test below names the scenario it replaces and asserts
//! that scenario's property on the service's own tasks.
//!
//! The widening over `whole_loop.rs`: a dwarf session may submit more than once (a moved tip);
//! beads carry a priority and a file they touch, so two siblings can conflict and the dwarf
//! claims in priority order; REWORK is claimable again; and the scenario can write gate
//! verdicts into the store at fixed virtual times, as a gate outside the loop would.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use lifecycle::bead::{BeadEvent, BeadEventKind, BeadRow, BeadState};
use lifecycle::reason::GateRedReason;
use runtime::host::{EffectRunner, VirtualDriver};
use runtime::journal::Journal;
use runtime::ports::{Clock, CountingProc, Proc, ProcSpec, VirtualClock};
use runtime::sources::Sources;
use runtime::units::{self, Exit, ScopeSpec, ScriptedUnits, StartSpec, UnitSpec, UNIT_RUN_SCOPE, UNIT_START};
use runtime::{Engine, MemJournal};
use sim::doubles::Doubles;
use spira_runtime::tasks::a::{batcher_local as bl, queue_step as qs, sentinel_call as sc, sift};
use spira_runtime::{registrations, tasks};

const REPO: &str = "spira";
const RUNE: &str = "builder";
const BASE: &str = "ba5eba5eba5eba5eba5eba5eba5eba5eba5eba5e";
const MIN: u64 = 60_000;
const DWARF_MS: u64 = 10 * MIN;
const CORPUS_MS: u64 = 5 * MIN;
const CERTIFY_MS: u64 = 2 * MIN;

/// The `n`th tip a dwarf submits for `id` (from 0): full-length, distinct per submit.
fn tip_of(id: &str, n: usize) -> String {
    let hex: String = id.bytes().map(|b| format!("{b:02x}")).collect();
    format!("{n:x}{hex:0>39}")
}

/// One bead the scenario files: when, at what priority, the file its change touches, and when
/// in each dwarf session (ms after the claim) the dwarf submits; the tip moves at every submit.
#[derive(Clone)]
struct Bead {
    id: &'static str,
    at: u64,
    priority: u8,
    file: &'static str,
    submits: Vec<u64>,
}

impl Bead {
    fn new(id: &'static str, at: u64) -> Bead {
        Bead { id, at, priority: 2, file: id, submits: vec![DWARF_MS - 1] }
    }
}

/// A verdict a gate outside the loop writes into the store at a virtual time, at the bead's
/// tip as the store then holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Pass,
    Red,
}

#[derive(Clone)]
struct Script {
    beads: Vec<Bead>,
    max_concurrent: u32,
    stopped_until: u64,
    verdicts: Vec<(u64, &'static str, Verdict)>,
    until: u64,
}

enum Due {
    File(String),
    Submit { bead: String, actor: String, n: usize },
    Verdict { bead: String, verdict: Verdict },
}

/// A write the scenario made from outside the loop, and whether the store applied it.
#[derive(Clone, Debug)]
struct Outside {
    state_before: BeadState,
    applied: bool,
}

#[derive(Default)]
struct Lc {
    rows: BTreeMap<String, BeadRow>,
    due: Vec<(u64, Due)>,
    history: Vec<(u64, String, String, String)>,
    refused: Vec<String>,
    outside: Vec<Outside>,
    round: Option<(String, Vec<(String, String)>)>,
    rounds: u32,
    dwarves: BTreeMap<String, u64>,
    items: BTreeMap<String, String>,
    /// Per bead: the claims it has had (its sessions), so each session's tips are distinct.
    sessions: BTreeMap<String, usize>,
    /// The tips each bead was submitted at, in order.
    submitted: BTreeMap<String, Vec<String>>,
    /// Per submitted tip: how many landings had touched its file when it was built.
    built_on: BTreeMap<String, usize>,
    /// The beads landed, in order, with the file each touched.
    landed: Vec<(String, String)>,
    /// Every round opened, with its members as the batcher cut them.
    opened: Vec<Vec<(String, String)>>,
    /// Every round landed, with its members at their tips.
    lands: Vec<Vec<(String, String)>>,
    /// Every pass sift recorded, at its tip.
    sifted: Vec<(String, String)>,
}

impl Lc {
    fn file(&mut self, now: u64, id: &str) {
        self.rows.insert(id.into(), BeadRow::filed(id));
        self.items.insert(id.into(), format!("{id}: the scenario's work\n"));
        self.history.push((now, id.into(), "-".into(), BeadState::Ready.as_str().into()));
    }

    fn event(&mut self, now: u64, id: &str, actor: &str, kind: BeadEventKind) -> Result<(), String> {
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
    fn apply(&mut self, now: u64, id: &str, actor: &str, kind: BeadEventKind) -> Result<(), String> {
        self.event(now, id, actor, kind).inspect_err(|why| self.refused.push(why.clone()))
    }

    fn landings_of(&self, file: &str) -> usize {
        self.landed.iter().filter(|(_, f)| f == file).count()
    }

    fn settle(&mut self, now: u64, script: &Script) {
        let (ready, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.due).into_iter().partition(|(at, _)| *at <= now);
        self.due = later;
        for (at, d) in ready {
            match d {
                Due::File(id) => self.file(at, &id),
                Due::Submit { bead, actor, n } => {
                    let session = self.sessions.get(&bead).copied().unwrap_or(1) - 1;
                    let per = script.beads.iter().find(|b| b.id == bead).map_or(1, |b| b.submits.len());
                    let tip = tip_of(&bead, session * per + n);
                    let file = script.beads.iter().find(|b| b.id == bead).map_or(bead.as_str(), |b| b.file).to_string();
                    if self.apply(at, &bead, &actor, BeadEventKind::Submit { tip: tip.clone() }).is_ok() {
                        let landings = self.landings_of(&file);
                        self.built_on.insert(tip.clone(), landings);
                        self.submitted.entry(bead).or_default().push(tip);
                    }
                }
                Due::Verdict { bead, verdict } => {
                    let Some(row) = self.rows.get(&bead).cloned() else { continue };
                    let tip = row.tip.clone().unwrap_or_default();
                    // `spira-lc certify <bead> <tip> pass`: a row already CERTIFIED at that tip
                    // answers APPLIED with no event (spira-lc's own `certify_voids_a_stale_
                    // certification_first_and_leaves_a_current_one_alone`).
                    let applied = match verdict {
                        Verdict::Pass if row.state == BeadState::Certified => true,
                        Verdict::Pass => self.event(at, &bead, "gate", BeadEventKind::GatePass { tip, gate_key: format!("gate:{bead}") }).is_ok(),
                        Verdict::Red => self.event(at, &bead, "gate", BeadEventKind::GateRed { tip, reason: GateRedReason::SuitesFailed }).is_ok(),
                    };
                    self.outside.push(Outside { state_before: row.state, applied });
                }
            }
        }
    }

    fn claimable(&self, script: &Script) -> Option<String> {
        let prio = |id: &str| script.beads.iter().find(|b| b.id == id).map_or(u8::MAX, |b| b.priority);
        let mut open: Vec<&BeadRow> = self.rows.values().filter(|r| matches!(r.state, BeadState::Ready | BeadState::Rework)).collect();
        open.sort_by_key(|r| (prio(&r.bead_id), r.bead_id.clone()));
        open.first().map(|r| r.bead_id.clone())
    }
}

struct World {
    clock: VirtualClock,
    units: Arc<ScriptedUnits>,
    lc: Mutex<Lc>,
    script: Script,
    proc: Arc<CountingProc>,
}

impl World {
    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    fn lc(&self) -> MutexGuard<'_, Lc> {
        let mut g = self.lc.lock().unwrap_or_else(|e| e.into_inner());
        g.settle(self.clock.now_ms(), &self.script);
        g
    }

    fn bead(&self, id: &str) -> Option<&Bead> {
        self.script.beads.iter().find(|b| b.id == id)
    }
}

fn json<T: serde::Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(|e| e.to_string())
}

fn parse<T: serde::de::DeserializeOwned>(what: &str, s: &str) -> Result<T, String> {
    serde_json::from_str(s).map_err(|e| format!("{what}: {e}"))
}

// --- sentinel-call ------------------------------------------------------------------------------

fn sentinel_observe(w: &World) -> Result<String, String> {
    let now = w.now();
    let lc = w.lc();
    let live: Vec<String> = lc.dwarves.iter().filter(|(_, end)| **end > now).map(|(u, _)| u.clone()).collect();
    let ready = lc.rows.values().filter(|r| matches!(r.state, BeadState::Ready | BeadState::Rework)).count() as u32;
    let snap = sc::Snapshot {
        live_by_rune: [(RUNE.to_string(), live.len() as u32)].into(),
        ready_by_rune: [(RUNE.to_string(), Some(ready))].into(),
        urgent: Some(vec![]),
        express_ready: Some(0),
        max_live: Some(4),
        lanes_max_live: Some(4),
        max_dwarves: Some(4),
        roster: vec![RUNE.into()],
        runes: [(RUNE.to_string(), sc::RuneCfg { lane: false, max_concurrent: w.script.max_concurrent, elastic: false })].into(),
        live_units: live,
        now_ms: now,
        ..sc::Snapshot::default()
    };
    json(&snap)
}

/// The dwarf claims the first claimable bead in priority order (READY or REWORK, as
/// `spira-claim` ranks them) and submits at each of the bead's scripted offsets.
fn sentinel_unit_start(w: &World, input: &str) -> Result<String, String> {
    let start: sc::Start = parse(sc::UNIT_START, input)?;
    let now = w.now();
    let unit = units::unit_name(sc::NAME, &start.key).map_err(|e| e.0)?;
    let mut lc = w.lc();
    let bead = lc.claimable(&w.script).ok_or("the dwarf found nothing ready")?;
    lc.apply(now, &bead, &unit, BeadEventKind::Claim { holder: unit.clone(), lease_until: ((now + DWARF_MS) / 1000) as i64 + 600, stack: Default::default(), stack_depth: 0, stack_max_depth: 0, persona: None })?;
    *lc.sessions.entry(bead.clone()).or_default() += 1;
    let submits = w.bead(&bead).map_or(vec![DWARF_MS - 1], |b| b.submits.clone());
    for (n, off) in submits.iter().enumerate() {
        lc.due.push((now + off, Due::Submit { bead: bead.clone(), actor: unit.clone(), n }));
    }
    lc.dwarves.insert(unit, now + DWARF_MS);
    drop(lc);
    w.units.script(sc::NAME, DWARF_MS, Exit::Code(0));
    let spec = UnitSpec { task: sc::NAME.into(), key: start.key, argv: vec!["dwarf".into(), "--home".into(), "/sim/home".into(), start.rune], env: vec![], cwd: None };
    units::effect(&*w.units, UNIT_START, &json(&StartSpec { unit: spec, props: sc::dwarf_props("3600")? })?)
}

// --- sift ---------------------------------------------------------------------------------------

fn pool_row(r: &BeadRow) -> sift::PoolRow {
    sift::PoolRow { id: r.bead_id.clone(), state: r.state.as_str().into(), version: r.version, tip: r.tip.clone().unwrap_or_default(), holds: vec![], ejected_red_tip: r.ejected_red_tip.clone(), sifted_tip: r.sifted_tip.clone() }
}

/// A candidate conflicts with the base when a bead touching its file landed after its tip was
/// built: git's `merge-tree` of that tip on the moved base.
fn sift_observe(w: &World) -> Result<String, String> {
    let now = w.now();
    let lc = w.lc();
    let rows: Vec<&BeadRow> = lc.rows.values().filter(|r| matches!(r.state, BeadState::Submitted | BeadState::Certified)).collect();
    let mut repo = sift::Repo { rows: rows.iter().map(|r| pool_row(r)).collect(), fetched: true, read_at_ms: now, identity_pages: Some(vec![]), ..sift::Repo::default() };
    for r in &rows {
        let tip = r.tip.clone().unwrap_or_default();
        let file = w.bead(&r.bead_id).map_or(r.bead_id.as_str(), |b| b.file);
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
    let wr: sift::Write = parse(sift::RECORD_PASS, input)?;
    let now = w.now();
    let mut lc = w.lc();
    lc.apply(now, &wr.id, sift::NAME, BeadEventKind::Sifted { tip: wr.tip.clone() })?;
    lc.sifted.push((wr.id, wr.tip));
    Ok("{}".into())
}

/// `lc gate-red` at the screened tip: the bead goes back to its dwarf.
fn sift_send_back(w: &World, input: &str) -> Result<String, String> {
    let wr: sift::Write = parse(sift::SEND_BACK, input)?;
    let now = w.now();
    w.lc().apply(now, &wr.id, sift::NAME, BeadEventKind::GateRed { tip: wr.tip, reason: GateRedReason::SuitesFailed })?;
    Ok("{}".into())
}

// --- batcher.local ------------------------------------------------------------------------------

fn batcher_observe(w: &World, input: &str) -> Result<String, String> {
    let carry: bl::Carry = parse(bl::OBSERVE, input)?;
    let held = carry.repos.get(REPO).and_then(|rc| rc.batch.clone());
    let lc = w.lc();
    let pool = lc
        .rows
        .values()
        .filter(|r| matches!(r.state, BeadState::Submitted | BeadState::Certified))
        .map(|r| bl::Member { id: r.bead_id.clone(), state: r.state.as_str().into(), version: r.version, tip: r.tip.clone().unwrap_or_default(), sifted_tip: r.sifted_tip.clone(), title: r.bead_id.clone(), priority: w.bead(&r.bead_id).map_or(2, |b| b.priority), ..bl::Member::default() })
        .collect();
    let open_round = lc.round.as_ref().map(|(id, m)| bl::Round { id: id.clone(), members: m.iter().map(|(i, _)| i.clone()).collect(), head: BASE.into(), worktree: format!("/sim/wt/{id}"), driven: held.as_ref() == Some(id) });
    let repo = bl::Repo { pool, open_round, base_sha: BASE.into(), stopped: w.now() < w.script.stopped_until, ..bl::Repo::default() };
    json(&bl::Snapshot { repos: [(REPO.to_string(), repo)].into(), now_ms: w.now() })
}

/// The merge of each member on the base in the order the batcher gave (`topo_order` of the
/// leg's members, which carry no priority): a member whose tip is stale on the base (a bead
/// touching its file landed after it was built) conflicts with the base itself; one whose file
/// an earlier member of the round already touched conflicts with the round's accumulation.
fn batcher_assemble(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::ASSEMBLE, input)?;
    let lc = w.lc();
    let (mut merged, mut conflicts, mut base_conflicts, mut touched) = (vec![], vec![], vec![], vec![]);
    for (id, tip) in &leg.members {
        let file = w.bead(id).map_or(id.as_str(), |b| b.file);
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
    json(&bl::Assembled { merged, conflicts, base_conflicts, wt: format!("/sim/wt/{}", bl::digest(&leg.round_key)), base: leg.base, suites: true, ..bl::Assembled::default() })
}

fn batcher_round_open(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::ROUND_OPEN, input)?;
    let mut lc = w.lc();
    if lc.round.is_some() {
        return Err("a round is already open".into());
    }
    lc.rounds += 1;
    let batch = format!("round-{}", lc.rounds);
    lc.opened.push(leg.members.clone());
    lc.round = Some((batch.clone(), leg.members));
    json(&bl::Opened { batch })
}

/// The corpus is green: these ports turn on the store and the cut, not on attribution.
fn batcher_corpus(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::CORPUS, input)?;
    let spec = UnitSpec { task: bl::NAME.into(), key: leg.unit.clone(), argv: vec!["round-vm".into(), "run".into(), leg.wt.clone()], env: vec![], cwd: None };
    w.units.script(bl::NAME, CORPUS_MS, Exit::Code(0));
    units::effect(&*w.units, UNIT_START, &json(&StartSpec { unit: spec, props: Default::default() })?)
}

fn batcher_certify(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::CERTIFY, input)?;
    let spec = UnitSpec { task: bl::NAME.into(), key: leg.unit.clone(), argv: vec!["queue".into(), "round".into(), "certify".into(), leg.batch, REPO.into()], env: vec![], cwd: None };
    w.units.script(bl::NAME, CERTIFY_MS, Exit::Code(0));
    w.units.script_output(bl::NAME, b"certified\n", b"");
    units::effect(&*w.units, UNIT_RUN_SCOPE, &json(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None })?)
}

/// `round certify`'s store half (`lc_certify_round`): every SUBMITTED member to CERTIFIED at its
/// tip; a member in any other state (already CERTIFIED by a gate) is left alone.
fn batcher_lc_certify(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::LC_CERTIFY, input)?;
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
    let leg: bl::Leg = parse(bl::LAND, input)?;
    let now = w.now();
    let mut lc = w.lc();
    match &lc.round {
        Some((batch, _)) if *batch == leg.batch => {}
        other => return Err(format!("land {}: the open round is {other:?}", leg.batch)),
    }
    for (id, _) in &leg.members {
        lc.apply(now, id, bl::NAME, BeadEventKind::Deliver)?;
        lc.apply(now, id, bl::NAME, BeadEventKind::Delivered { merge_sha: BASE.into(), proof: format!("round {}", leg.batch) })?;
        let file = w.bead(id).map_or(id.as_str(), |b| b.file).to_string();
        lc.landed.push((id.clone(), file));
    }
    lc.lands.push(leg.members.clone());
    lc.round = None;
    json(&bl::Landed { staged: None })
}

fn batcher_abandon(w: &World) -> Result<String, String> {
    w.lc().round = None;
    Ok("{}".into())
}

/// `round withdraw` (`lc_withdraw`): a CERTIFIED member that kept conflicting is delivered and
/// returned (`batch-ejected`) to REWORK; a member in any other state is left alone.
fn batcher_withdraw(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::WITHDRAW, input)?;
    let now = w.now();
    let mut lc = w.lc();
    if lc.rows.get(&leg.bead).is_some_and(|r| r.state == BeadState::Certified) {
        lc.apply(now, &leg.bead, bl::NAME, BeadEventKind::Deliver)?;
        lc.apply(now, &leg.bead, bl::NAME, BeadEventKind::Returned { reason: lifecycle::reason::ReturnedReason::BatchEjected })?;
    }
    Ok("{}".into())
}

fn queue_step_observe(_: &World) -> Result<String, String> {
    let repo = qs::Repo { mode: "queue.local".into(), lc_ok: true, open_batch: None, base_sha: BASE.into(), ..qs::Repo::default() };
    json(&qs::Snapshot { repos: [(REPO.to_string(), repo)].into(), world_running: true })
}

fn doubles(w: &Arc<World>) -> Doubles {
    let forks = w.proc.clone();
    let d = sim::units::with_units(sim::doubles::service(), w.units.clone()).double("fork", move |_| {
        let spec = ProcSpec { prog: "planted".into(), args: vec![], stdin: vec![], deadline: std::time::Duration::from_secs(1) };
        forks.run(&spec).map(|_| String::new())
    });
    let on = |d: Doubles, name: &str, f: fn(&World, &str) -> Result<String, String>| {
        let w = w.clone();
        d.double(name, move |input| f(&w, input))
    };
    let d = on(d, sc::OBSERVE, |w, _| sentinel_observe(w));
    let d = on(d, sc::UNIT_START, sentinel_unit_start);
    let d = on(d, sift::OBSERVE, |w, _| sift_observe(w));
    let d = on(d, sift::RECORD_PASS, sift_record_pass);
    let d = on(d, sift::SEND_BACK, sift_send_back);
    let d = on(d, bl::OBSERVE, batcher_observe);
    let d = on(d, bl::ASSEMBLE, batcher_assemble);
    let d = on(d, bl::ROUND_OPEN, batcher_round_open);
    let d = on(d, bl::CORPUS, batcher_corpus);
    let d = on(d, bl::CERTIFY, batcher_certify);
    let d = on(d, bl::LC_CERTIFY, batcher_lc_certify);
    let d = on(d, bl::LAND, batcher_land);
    let d = on(d, bl::ABANDON, |w, _| batcher_abandon(w));
    let d = on(d, bl::WITHDRAW, batcher_withdraw);
    on(d, qs::OBSERVE, |w, _| queue_step_observe(w))
}

struct Run {
    trace: Vec<(String, String, String, String, u64)>,
    history: Vec<(u64, String, String, String)>,
    refused: Vec<String>,
    outside: Vec<Outside>,
    rows: BTreeMap<String, BeadRow>,
    submitted: BTreeMap<String, Vec<String>>,
    opened: Vec<Vec<(String, String)>>,
    lands: Vec<Vec<(String, String)>>,
    sifted: Vec<(String, String)>,
    spawned: u64,
    wall: std::time::Duration,
}

fn run(seed: u64, script: Script) -> Run {
    let began = std::time::Instant::now();
    let until = script.until;
    let clock = VirtualClock::new(0);
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let units = Arc::new(ScriptedUnits::new(c.clone()));
    let mut lc = Lc::default();
    for b in &script.beads {
        lc.due.push((b.at, Due::File(b.id.to_string())));
    }
    for (at, bead, verdict) in &script.verdicts {
        lc.due.push((*at, Due::Verdict { bead: bead.to_string(), verdict: *verdict }));
    }
    let world = Arc::new(World { clock: clock.clone(), units: units.clone(), lc: Mutex::new(lc), script, proc: CountingProc::refusing() });
    let runner = Arc::new(doubles(&world));
    let journal: Arc<dyn Journal> = MemJournal::new();
    let settings = common::settings_with(&[("SPIRA_BATCHER_WALL_SECS", "3600")]);
    let mut engine = Engine::open(journal.clone(), c.clone(), registrations(&settings), seed).expect("the service's registrations open");
    for (_, stream, kind) in tasks::boots() {
        engine.start(stream, kind);
    }
    let mut sources = Sources::default();
    sources.register(Box::new(spira_runtime::sources::units::scripted(units.clone(), c.clone()))).expect("the unit.ran source");
    let mut driver = VirtualDriver::new(engine, clock, runner.clone() as Arc<dyn EffectRunner>).with_sources(sources).expect("scripted sources only");
    driver.run_until(until).expect("the loop settles");
    let lc = world.lc();
    Run {
        trace: runtime::conformance::trace(&*journal),
        history: lc.history.clone(),
        refused: lc.refused.clone(),
        outside: lc.outside.clone(),
        rows: lc.rows.clone(),
        submitted: lc.submitted.clone(),
        opened: lc.opened.clone(),
        lands: lc.lands.clone(),
        sifted: lc.sifted.clone(),
        spawned: world.proc.spawned(),
        wall: began.elapsed(),
    }
}

fn lifecycle_of(r: &Run, bead: &str) -> Vec<String> {
    r.history.iter().filter(|(_, id, _, _)| id == bead).map(|(_, _, _, to)| to.clone()).collect()
}

fn landed_at(r: &Run, bead: &str) -> Option<u64> {
    r.history.iter().find(|(_, id, _, to)| id == bead && to == "LANDED").map(|(at, ..)| *at)
}

fn dump(r: &Run) -> String {
    let interesting: Vec<String> = r.trace.iter().filter(|e| [sc::STREAM, sift::STREAM, bl::STREAM].contains(&e.1.as_str()) && e.2 != "effect.succeeded").map(|e| format!("{:>8} {} {} {}", e.4, e.1, e.2, e.3.chars().take(160).collect::<String>())).collect();
    format!("history {:?}\nrefused {:?}\noutside {:?}\nopened {:?}\nlands {:?}\nsifted {:?}\n{}", r.history, r.refused, r.outside, r.opened, r.lands, r.sifted, interesting.join("\n"))
}

fn show(name: &str, r: &Run) {
    println!("{name}: wall {:?}", r.wall);
    if std::env::var_os("W3_DUMP").is_some() {
        println!("=== {name}\n{}", dump(r));
    }
}

fn assert_no_process(r: &Run) {
    assert_eq!(r.spawned, 0, "no process is spawned: every effect is a double");
    let unanswered: Vec<&String> = r.trace.iter().filter(|e| e.2 == runtime::EFFECT_FAILED && e.3.contains("no runner for effect")).map(|e| &e.3).collect();
    assert!(unanswered.is_empty(), "every effect asked for had a double: {unanswered:?}");
    assert!(r.wall.as_secs() < 30, "the scenario ran in {:?}", r.wall);
}

/// Replaces `resubmit-moved-tip` (sp-2adk7): a dwarf that submits, commits again and submits
/// once more has the moved tip recorded on its SUBMITTED row (`lc_tip = branch_tip`). On the
/// service's tasks the moved tip is also the one sift screens and the round cuts, certifies and
/// lands; the first tip never reaches a round. The `rounds` lane is stopped until both submits
/// are in, as the world's cut waited for its sift pass.
#[test]
fn resubmit_moved_tip_the_moved_tip_is_recorded_screened_cut_and_landed() {
    let bead = Bead { submits: vec![4 * MIN, 9 * MIN], ..Bead::new("sp-rm01", 30_000) };
    let script = Script { beads: vec![bead], max_concurrent: 1, stopped_until: 14 * MIN, verdicts: vec![], until: 40 * MIN };
    let r = run(21, script);
    show("resubmit-moved-tip", &r);
    let tips = &r.submitted["sp-rm01"];
    assert_eq!(tips.len(), 2, "both submits reached the store: {}", dump(&r));
    let moved = tips[1].clone();
    assert_ne!(tips[0], moved, "the second submit is at a moved tip");
    assert!(r.refused.is_empty(), "the store refused nothing, the second submit on SUBMITTED included: {:?}", r.refused);
    assert_eq!(lifecycle_of(&r, "sp-rm01"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", dump(&r));
    assert_eq!(r.rows["sp-rm01"].tip.as_deref(), Some(moved.as_str()), "the row carries the moved tip (moved_tip_was_recorded)");
    assert!(r.sifted.iter().any(|(id, t)| id == "sp-rm01" && *t == moved), "sift screened the moved tip: {:?}", r.sifted);
    assert_eq!(r.opened, [vec![("sp-rm01".to_string(), moved.clone())]], "one round, cut at the moved tip");
    assert_eq!(r.lands, [vec![("sp-rm01".to_string(), moved)]], "the moved tip is what landed");
    assert_no_process(&r);
}

/// Replaces `certify-twice` (sp-2adk7): with the cut stood down, a gate certifies the bead at
/// its tip, and the same verdict delivered twice more (the gate's repeat runs) is answered
/// without moving the row — a CERTIFIED bead stays CERTIFIED at its tip. When the cut resumes
/// it takes the gate-certified bead as it stands (its own certify is one more repeat verdict at
/// the same tip) and lands it: the row never falls back to SUBMITTED.
#[test]
fn certify_twice_a_repeat_verdict_keeps_the_row_certified_and_the_round_lands_it() {
    let at = |m: u64| 12 * MIN + m * 30_000;
    let verdicts = vec![(at(0), "sp-ct01", Verdict::Pass), (at(1), "sp-ct01", Verdict::Pass), (at(2), "sp-ct01", Verdict::Pass)];
    let script = Script { beads: vec![Bead::new("sp-ct01", 30_000)], max_concurrent: 1, stopped_until: 15 * MIN, verdicts, until: 40 * MIN };
    let r = run(22, script);
    show("certify-twice", &r);
    let states: Vec<BeadState> = r.outside.iter().map(|o| o.state_before).collect();
    assert_eq!(states, [BeadState::Submitted, BeadState::Certified, BeadState::Certified], "the gate's three verdicts met the row SUBMITTED, then CERTIFIED twice: {}", dump(&r));
    assert!(r.outside.iter().all(|o| o.applied), "the first verdict certified the bead and both repeats were accepted (repeat_verdict_accepted): {}", dump(&r));
    assert!(r.refused.is_empty(), "the loop's own writes were all applied, the cut's certify of a CERTIFIED member included: {:?}", r.refused);
    assert_eq!(lifecycle_of(&r, "sp-ct01"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "repeat verdicts never moved the row back: {}", dump(&r));
    let tip = r.submitted["sp-ct01"][0].clone();
    assert_eq!(r.lands, [vec![("sp-ct01".to_string(), tip)]], "the gate-certified tip is what landed");
    assert_no_process(&r);
}

/// Replaces `long-gate-interleaving` (sp-2adk7): a slow gate whose verdict arrives after the
/// round landed the bead must not move it. LANDED is final (`landed_is_final`), and nothing
/// readmits the landed bead to a cut (`no_stale_admission`): one round, opened once.
#[test]
fn long_gate_interleaving_a_verdict_after_the_landing_leaves_the_bead_landed() {
    let verdicts = vec![(30 * MIN, "sp-lg01", Verdict::Pass), (31 * MIN, "sp-lg01", Verdict::Red)];
    let script = Script { beads: vec![Bead::new("sp-lg01", 30_000)], max_concurrent: 1, stopped_until: 0, verdicts, until: 36 * MIN };
    let r = run(23, script);
    show("long-gate-interleaving", &r);
    let landed = landed_at(&r, "sp-lg01").unwrap_or_else(|| panic!("the bead landed: {}", dump(&r)));
    assert!(landed < 30 * MIN, "the round landed before the slow gate finished: {}", dump(&r));
    assert_eq!(r.outside.len(), 2, "{}", dump(&r));
    for o in &r.outside {
        assert_eq!(o.state_before, BeadState::Landed, "{o:?}");
        assert!(!o.applied, "a gate verdict on a LANDED row is refused: {o:?}");
    }
    let after: Vec<&(u64, String, String, String)> = r.history.iter().filter(|(at, id, ..)| id == "sp-lg01" && *at > landed).collect();
    assert!(after.is_empty(), "LANDED is final: {after:?}");
    assert_eq!(r.rows["sp-lg01"].state, BeadState::Landed);
    assert_eq!(r.opened.len(), 1, "no landed bead is admitted to another cut: {:?}", r.opened);
    assert_no_process(&r);
}

/// Replaces `conflicting-siblings` (sp-2adk7): two siblings change the same file, the
/// higher-priority one (priority 0) filed alongside a priority-3 one. Both are sifted before the
/// cut (the `rounds` lane is stopped until then), so one cut sees both: the round takes the
/// priority-0 sibling and the other conflicts with it. The priority-0 sibling lands first; the
/// loser, now stale on the moved base, is returned to its dwarf (REWORK, `loser_was_returned`),
/// is claimed again, resubmits a tip built on the new base and lands after the winner
/// (`priority_won_the_conflict`).
#[test]
fn conflicting_siblings_priority_wins_the_conflict_and_the_loser_is_returned_and_lands_after() {
    let winner = Bead { priority: 0, file: "shared", ..Bead::new("sp-cs02", 30_000) };
    let loser = Bead { priority: 3, file: "shared", ..Bead::new("sp-cs01", 30_000) };
    let script = Script { beads: vec![loser, winner], max_concurrent: 2, stopped_until: 15 * MIN, verdicts: vec![], until: 70 * MIN };
    let r = run(24, script);
    show("conflicting-siblings", &r);
    assert!(r.refused.is_empty(), "the store refused nothing: {:?}", r.refused);
    let first = r.lands.first().unwrap_or_else(|| panic!("a round landed: {}", dump(&r)));
    assert_eq!(first.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["sp-cs02"], "the first round landed the priority-0 sibling alone: {}", dump(&r));
    let loser = lifecycle_of(&r, "sp-cs01");
    assert!(loser.iter().any(|s| s == "REWORK"), "the loser was returned (loser_was_returned): {}", dump(&r));
    let (w, l) = (landed_at(&r, "sp-cs02"), landed_at(&r, "sp-cs01"));
    assert!(w.is_some() && l.is_some() && w < l, "priority won the conflict: the winner landed at {w:?}, before the loser at {l:?}: {}", dump(&r));
    let last = r.submitted["sp-cs01"].last().cloned();
    assert_eq!(r.rows["sp-cs01"].tip, last, "the loser landed its reworked tip");
    assert_no_process(&r);
}
