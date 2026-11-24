//! W3 of the scheduler consolidation design (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`
//! §6): the whole loop, file → claim → submit → round → land, on the service's own registrations
//! (`spira_runtime::registrations`, every row of `tasks::all()`) under virtual time. Ruling D6:
//! the real registrations run, and every effect is answered by a double; no process is spawned.
//!
//! The world the doubles answer from is one in-memory lifecycle store whose rows move only
//! through the store's own machine (`lifecycle::bead::apply`), so an illegal move is refused
//! as the store refuses it. The dwarf sessions and the round's legs are units on the simulator's
//! manager (`ScriptedUnits`); their ends reach the owning task's stream through the service's
//! own `unit.ran` source (`spira_runtime::sources::units::scripted`). Every other effect of every
//! other task answers the quiet world (`sim::doubles::service`).
//!
//! The path is the contracts' local one (`wiki/projects/spira/designs/tasks/`): sentinel-call
//! calls a dwarf for the ready rune; the dwarf claims and submits; sift screens the submission,
//! records the pass and hands `work.submitted` to the batchers (an `EmitTo`); batcher.local cuts
//! a round, runs its corpus and certify legs, certifies the members and lands them with
//! `queue round land`. queue.step sees a `queue.local` repo with no batch and asks for nothing
//! (its contract: an idle local repo is silent); landing is the pr-mode pass and has no pr-mode
//! repo here.

use crate::common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lifecycle::bead::{BeadEvent, BeadEventKind, LifecycleRow, BeadState};
use lifecycle::reason::GateRedReason;
use runtime::host::{EffectRunner, Submit, VirtualDriver};
use runtime::EventKind;
use runtime::event::{Event, EFFECT_FAILED, EFFECT_REQUESTED, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT};
use runtime::journal::{Boot, FaultyJournal, Journal, Poison, Turn};
use runtime::ports::{Clock, CountingProc, Proc, ProcSpec, VirtualClock};
use runtime::sources::Sources;
use runtime::units::{self, Exit, ScopeSpec, ScriptedUnits, StartSpec, UnitSpec, UNIT_RUN_SCOPE, UNIT_START};
use runtime::event::StreamKey;
use runtime::{Engine, MemJournal};
use sim::doubles::Doubles;
use spira_runtime::tasks::a::{batcher_local as bl, queue_step as qs, sentinel_call as sc, sift};
use spira_runtime::{registrations, tasks};

const REPO: &str = "spira";
const RUNE: &str = "builder";
const BASE: &str = "ba5eba5eba5eba5eba5eba5eba5eba5eba5eba5e";
const MIN: u64 = 60_000;
/// Every dwarf session runs this long, then exits 0 having submitted.
const DWARF_MS: u64 = 10 * MIN;
const CORPUS_MS: u64 = 5 * MIN;
const ATTRIBUTE_MS: u64 = 3 * MIN;
const CERTIFY_MS: u64 = 2 * MIN;
/// Where the scenario stops: the loop has long since landed by then.
const UNTIL_MS: u64 = 50 * MIN;

/// A full-length tip for `id` (the store refuses a submit whose tip is not a 40-hex sha).
fn tip_of(id: &str) -> String {
    format!("{:0>40}", id.bytes().map(|b| format!("{b:02x}")).collect::<String>())
}

enum Due {
    File(String),
    Event { bead: String, actor: String, kind: BeadEventKind },
}

/// The round the queue holds open: its batch, members and the worktree it was opened with.
#[derive(Debug)]
struct OpenRound {
    batch: String,
    members: Vec<(String, String)>,
    wt: String,
}

/// The in-memory lifecycle store: rows, the writes due later (a dwarf submits when its session
/// ends), the round the batcher has open, and each row's history.
#[derive(Default)]
struct Lc {
    rows: BTreeMap<String, LifecycleRow>,
    due: Vec<(u64, Due)>,
    history: Vec<(u64, String, String, String)>,
    refused: Vec<String>,
    round: Option<OpenRound>,
    rounds: u32,
    landed: Vec<String>,
    dwarves: BTreeMap<String, u64>,
    items: BTreeMap<String, String>,
    released: std::collections::BTreeSet<String>,
    pages: Vec<String>,
}

impl Lc {
    fn file(&mut self, now: u64, id: &str) {
        self.rows.insert(id.into(), LifecycleRow::filed(id));
        self.items.insert(id.into(), format!("{id}: the scenario's work\n"));
        self.history.push((now, id.into(), "-".into(), BeadState::Ready.as_str().into()));
    }

    /// One event through the store's machine; a refusal is recorded and returned.
    fn apply(&mut self, now: u64, id: &str, actor: &str, kind: BeadEventKind) -> Result<(), String> {
        let row = self.rows.get(id).cloned().ok_or_else(|| format!("no row {id}"))?;
        let ev = BeadEvent { expect: row.state, version: row.version, kind, actor: actor.into(), at: Some((now / 1000) as i64) };
        let out = lifecycle::bead::apply(&row, &ev);
        if !out.applied {
            let why = format!("{id} {:?} refused {:?}: {:?}", row.state, ev.kind, out.refusal);
            self.refused.push(why.clone());
            return Err(why);
        }
        if out.row.state != row.state {
            self.history.push((now, id.into(), row.state.as_str().into(), out.row.state.as_str().into()));
        }
        self.rows.insert(id.into(), out.row);
        Ok(())
    }

    /// Every write due by `now`, in the order it was scheduled.
    fn settle(&mut self, now: u64) {
        let (ready, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.due).into_iter().partition(|(at, _)| *at <= now);
        self.due = later;
        for (at, d) in ready {
            match d {
                Due::File(id) => self.file(at, &id),
                Due::Event { bead, actor, kind } => {
                    let _ = self.apply(at, &bead, &actor, kind);
                }
            }
        }
    }

    fn in_state(&self, s: BeadState) -> Vec<&LifecycleRow> {
        self.rows.values().filter(|r| r.state == s).collect()
    }

    /// READY and not snoozed at `now_ms`: what a dwarf may claim.
    fn claimable(&self, now_ms: u64) -> Vec<&LifecycleRow> {
        let now = (now_ms / 1000) as i64;
        self.in_state(BeadState::Ready)
            .into_iter()
            .filter(|r| !(r.holds.contains(&lifecycle::bead::HoldKind::Wait) && r.reason.as_deref().is_some_and(|x| spira_store::lc_state::still_snoozed(x, now))))
            .collect()
    }
}

/// What a scenario scripts: the beads it files (at what time), whether the rune may
/// run only one at a time, and which member's change turns the certify red (none: green).
#[derive(Clone)]
struct Script {
    file: Vec<(u64, &'static str)>,
    single_flight: bool,
    guilty: Option<&'static str>,
    /// The first claim of each bead ends without progress: released, then held on a wait of this many ms.
    no_progress_snooze_ms: Option<u64>,
}

/// Where the scheduler dies: after `after` of the named double's store writes have reached the store.
#[derive(Clone, Copy)]
struct Crash {
    double: &'static str,
    after: u32,
}

#[derive(Default)]
struct Crashing {
    writes: u32,
    fired: bool,
    down: bool,
}

struct World {
    clock: VirtualClock,
    crash: Option<Crash>,
    crashing: Mutex<Crashing>,
    faulty: Arc<FaultyJournal>,
    /// Until this virtual time every store-class double answers `Err`.
    cold_until: AtomicU64,
    cold_refused: Mutex<Vec<String>>,
    units: Arc<ScriptedUnits>,
    lc: Mutex<Lc>,
    script: Script,
    /// Counts every process a double would start: the runner's `fork` double runs through it,
    /// as `runtime_tasks.rs` plants it (with its positive control there).
    proc: Arc<CountingProc>,
}

impl World {
    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    /// Called before each store write and once after the last: at the crash point the outcome's
    /// journal commit is made to fail, and from then on no double reaches the store.
    fn gate(&self, double: &str) -> Result<(), String> {
        let mut c = self.crashing.lock().unwrap_or_else(|e| e.into_inner());
        if c.down {
            return Err("the scheduler is down".into());
        }
        if self.crash.is_some_and(|t| t.double == double && !c.fired && c.writes == t.after) {
            c.fired = true;
            c.down = true;
            self.faulty.fail_next("append");
            return Err("the scheduler crashed here".into());
        }
        Ok(())
    }

    fn wrote(&self, double: &str) {
        if self.crash.is_some_and(|t| t.double == double) {
            self.crashing.lock().unwrap_or_else(|e| e.into_inner()).writes += 1;
        }
    }

    /// One counted write of `double` through the store's machine.
    fn write(&self, double: &str, lc: &mut Lc, id: &str, actor: &str, kind: BeadEventKind) -> Result<(), String> {
        self.gate(double)?;
        lc.apply(self.now(), id, actor, kind)?;
        self.wrote(double);
        Ok(())
    }

    fn lc(&self) -> MutexGuard<'_, Lc> {
        let mut g = self.lc.lock().unwrap_or_else(|e| e.into_inner());
        g.settle(self.clock.now_ms());
        g
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
    let ready = lc.claimable(now).len() as u32;
    let snap = sc::Snapshot {
        live_by_rune: [(RUNE.to_string(), live.len() as u32)].into(),
        ready_by_rune: [(RUNE.to_string(), Some(ready))].into(),
        urgent: Some(vec![]),
        express_ready: Some(0),
        max_live: Some(4),
        max_dwarves: Some(4),
        roster: vec![RUNE.into()],
        runes: [(RUNE.to_string(), sc::RuneCfg { single_flight: w.script.single_flight, elastic: false, uses: vec![], reactive: false })].into(),
        live_units: live,
        now_ms: now,
        ..sc::Snapshot::default()
    };
    json(&snap)
}

/// The dwarf: a unit on the scripted manager. It claims the first ready bead now and submits
/// it as its session ends.
fn sentinel_unit_start(w: &World, input: &str) -> Result<String, String> {
    let start: sc::Start = parse(sc::UNIT_START, input)?;
    let now = w.now();
    let unit = units::unit_name(sc::NAME, &start.key).map_err(|e| e.0)?;
    let mut lc = w.lc();
    let bead = lc.claimable(now).first().map(|r| r.bead_id.clone()).ok_or("the dwarf found nothing ready")?;
    w.write("sentinel_unit_start", &mut lc, &bead, &unit, BeadEventKind::Claim { holder: unit.clone(), lease_until: ((now + DWARF_MS) / 1000) as i64 + 600, stack: Default::default(), stack_depth: 0, stack_max_depth: 0, persona: None, directed: None })?;
    match w.script.no_progress_snooze_ms.filter(|_| lc.released.insert(bead.clone())) {
        Some(snooze) => {
            let at = now + MIN;
            let hold = BeadEventKind::Hold { kind: lifecycle::bead::HoldKind::Wait, cause: lifecycle::reason::HoldCause::UnlandedBlocker, detail: Some(spira_store::lc_state::snooze_reason(((at + snooze) / 1000) as i64)) };
            lc.due.push((at, Due::Event { bead: bead.clone(), actor: unit.clone(), kind: BeadEventKind::Release }));
            lc.due.push((at, Due::Event { bead: bead.clone(), actor: unit.clone(), kind: hold }));
        }
        None => lc.due.push((now + DWARF_MS - 1, Due::Event { bead: bead.clone(), actor: unit.clone(), kind: BeadEventKind::Submit { tip: tip_of(&bead) } })),
    }
    lc.dwarves.insert(unit, now + DWARF_MS);
    drop(lc);
    w.units.script(sc::NAME, DWARF_MS, Exit::Code(0));
    let spec = UnitSpec { task: sc::NAME.into(), key: start.key, argv: vec!["dwarf".into(), "--home".into(), "sim/home".into(), start.rune], env: vec![], cwd: None };
    let started = units::effect(&*w.units, UNIT_START, &json(&StartSpec { unit: spec, props: units::dwarf_props("3600")? })?)?;
    w.gate("sentinel_unit_start")?;
    Ok(started)
}

// --- sift ---------------------------------------------------------------------------------------

fn pool_row(r: &LifecycleRow) -> sift::PoolRow {
    sift::PoolRow { id: r.bead_id.clone(), state: r.state.as_str().into(), version: r.version, tip: r.tip.clone().unwrap_or_default(), holds: r.holds.iter().map(|h| h.as_str().to_string()).collect(), ejected_red_tip: r.ejected_red_tip.clone(), sifted_tip: r.sifted_tip.clone() }
}

fn sift_observe(w: &World) -> Result<String, String> {
    let now = w.now();
    let lc = w.lc();
    let rows: Vec<&LifecycleRow> = lc.rows.values().filter(|r| matches!(r.state, BeadState::Submitted | BeadState::Certified)).collect();
    let mut repo = sift::Repo { rows: rows.iter().map(|r| pool_row(r)).collect(), fetched: true, read_at_ms: now, identity_pages: Some(vec![]), ..sift::Repo::default() };
    for r in &rows {
        let tip = r.tip.clone().unwrap_or_default();
        repo.branch_tips.insert(r.bead_id.clone(), tip.clone());
        repo.items.insert(r.bead_id.clone(), lc.items.get(&r.bead_id).cloned().unwrap_or_default());
        repo.candidates.insert(r.bead_id.clone(), sift::Candidate { base: BASE.into(), patch_id: Some(format!("patch-{}", r.bead_id)), ..sift::Candidate::default() });
    }
    if let Some(OpenRound { members, .. }) = &lc.round {
        repo.open_round = members.iter().map(|(i, t)| format!("{i}:{t}")).collect();
    }
    json(&sift::Snapshot { repos: [(REPO.to_string(), repo)].into() })
}

fn sift_record_pass(w: &World, input: &str) -> Result<String, String> {
    let wr: sift::Write = parse(sift::RECORD_PASS, input)?;
    w.write("sift_record_pass", &mut w.lc(), &wr.id, sift::NAME, BeadEventKind::Sifted { tip: wr.tip })?;
    w.gate("sift_record_pass")?;
    Ok("{}".into())
}

// --- batcher.local ------------------------------------------------------------------------------

/// The probe's input is the carry it was asked with: an open round is driven when that carry's
/// machine holds it (`Round::driven`), as the real probe reads it.
fn batcher_observe(w: &World, input: &str) -> Result<String, String> {
    let carry: bl::Carry = parse(bl::OBSERVE, input)?;
    let held = carry.repos.get(REPO).and_then(|rc| rc.batch.clone());
    let lc = w.lc();
    let pool = lc
        .rows
        .values()
        .filter(|r| matches!(r.state, BeadState::Submitted | BeadState::Certified))
        .map(|r| bl::Member { id: r.bead_id.clone(), state: r.state.as_str().into(), version: r.version, tip: r.tip.clone().unwrap_or_default(), sifted_tip: r.sifted_tip.clone(), title: r.bead_id.clone(), priority: 2, ..bl::Member::default() })
        .collect();
    let open_round = lc.round.as_ref().map(|r| bl::Round { id: r.batch.clone(), members: r.members.iter().map(|(i, _)| i.clone()).collect(), head: BASE.into(), worktree: r.wt.clone(), driven: held.as_ref() == Some(&r.batch), ..bl::Round::default() });
    let repo = bl::Repo { pool, open_round, base_sha: BASE.into(), ..bl::Repo::default() };
    json(&bl::Snapshot { repos: [(REPO.to_string(), repo)].into(), now_ms: w.now(), ..Default::default() })
}

/// The merge of every member on the base: clean, in the order given, with suites.
fn batcher_assemble(input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::ASSEMBLE, input)?;
    json(&bl::Assembled { merged: leg.members.clone(), wt: format!("sim/wt/{}", bl::digest(&leg.round_key)), base: leg.base, suites: true, ..bl::Assembled::default() })
}

fn batcher_round_open(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::ROUND_OPEN, input)?;
    let mut lc = w.lc();
    w.gate("batcher_round_open")?;
    if lc.round.is_some() {
        return Err("a round is already open".into());
    }
    lc.rounds += 1;
    let batch = format!("round-{}", lc.rounds);
    lc.round = Some(OpenRound { batch: batch.clone(), members: leg.members, wt: leg.wt });
    w.wrote("batcher_round_open");
    w.gate("batcher_round_open")?;
    json(&bl::Opened { batch, ..bl::Opened::default() })
}

/// The corpus leg and the attribution leg both run under `batcher.local.corpus`: the corpus is
/// the round VM's pass (a unit, red when a guilty member is in the round); the attribution is a
/// captured scope that ejects the guilty member (GateRed in the store, as `round eject` writes it)
/// and names the survivors.
fn batcher_corpus(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::CORPUS, input)?;
    let spec = UnitSpec { task: bl::NAME.into(), key: leg.unit.clone(), argv: vec!["round-vm".into(), "run".into(), leg.wt.clone()], env: vec![], cwd: None };
    let guilty = w.script.guilty.filter(|g| leg.members.iter().any(|(id, _)| id == g));
    if leg.why == bl::ATTRIBUTE {
        let survivors: Vec<(String, String)> = leg.members.iter().filter(|(id, _)| Some(id.as_str()) != guilty).cloned().collect();
        if let Some(g) = guilty {
            let now = w.now();
            let tip = tip_of(g);
            w.lc().due.push((now + ATTRIBUTE_MS - 1, Due::Event { bead: g.into(), actor: "batcher attribute".into(), kind: BeadEventKind::GateRed { tip, reason: GateRedReason::SuitesFailed, step: None, message: None } }));
        }
        let line = format!("attribution\t{}\n", serde_json::json!({ "end": "land", "members": survivors, "attribution_secs": ATTRIBUTE_MS / 1000, "mended": false }));
        w.units.script(bl::NAME, ATTRIBUTE_MS, Exit::Code(0));
        w.units.script_output(bl::NAME, line.as_bytes(), b"");
        return units::effect(&*w.units, UNIT_RUN_SCOPE, &json(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None })?);
    }
    w.units.script(bl::NAME, CORPUS_MS, if guilty.is_some() { Exit::Code(1) } else { Exit::Code(0) });
    units::effect(&*w.units, UNIT_START, &json(&StartSpec { unit: spec, props: Default::default() })?)
}

fn batcher_certify(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::CERTIFY, input)?;
    let spec = UnitSpec { task: bl::NAME.into(), key: leg.unit.clone(), argv: vec!["queue".into(), "round".into(), "certify".into(), leg.batch.clone(), REPO.into()], env: vec![], cwd: None };
    let red = w.script.guilty.is_some_and(|g| leg.members.iter().any(|(id, _)| id == g));
    w.units.script(bl::NAME, CERTIFY_MS, Exit::Code(if red { 1 } else { 0 }));
    w.units.script_output(bl::NAME, if red { b"red=test-a.sh\n" } else { b"certified\n" }, b"");
    units::effect(&*w.units, UNIT_RUN_SCOPE, &json(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None })?)
}

/// `round certify`'s store half: every member SUBMITTED → CERTIFIED at its tip.
fn batcher_lc_certify(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::LC_CERTIFY, input)?;
    let mut lc = w.lc();
    for (id, tip) in &leg.members {
        if lc.rows.get(id).is_none_or(|r| r.state != BeadState::Submitted) {
            continue;
        }
        w.write("batcher_lc_certify", &mut lc, id, bl::NAME, BeadEventKind::GatePass { tip: tip.clone(), gate_key: format!("round:{}", leg.round_key) })?;
    }
    w.gate("batcher_lc_certify")?;
    Ok("{}".into())
}

/// `queue round land`: the round's members delivered and landed, and the round closed.
fn batcher_land(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::LAND, input)?;
    let mut lc = w.lc();
    if lc.landed.contains(&leg.batch) {
        return json(&bl::Landed::default());
    }
    match &lc.round {
        Some(r) if r.batch == leg.batch => {}
        other => return Err(format!("land {}: the open round is {other:?}", leg.batch)),
    }
    for (id, _) in &leg.members {
        if lc.rows.get(id).is_some_and(|r| r.state == BeadState::Certified) {
            w.write("batcher_land", &mut lc, id, bl::NAME, BeadEventKind::Deliver)?;
        }
        if lc.rows.get(id).is_some_and(|r| r.state == BeadState::InDelivery) {
            w.write("batcher_land", &mut lc, id, bl::NAME, BeadEventKind::Delivered { merge_sha: BASE.into(), proof: format!("round {}", leg.batch) })?;
        }
    }
    w.gate("batcher_land")?;
    lc.round = None;
    lc.landed.push(leg.batch.clone());
    w.wrote("batcher_land");
    w.gate("batcher_land")?;
    json(&bl::Landed::default())
}

fn batcher_abandon(w: &World) -> Result<String, String> {
    w.gate("batcher_abandon")?;
    w.lc().round = None;
    Ok("{}".into())
}

// --- queue.step ---------------------------------------------------------------------------------

/// A `queue.local` repo has no forge batch: the batcher's own `land` lands its rounds.
fn queue_step_observe(_: &World) -> Result<String, String> {
    let repo = qs::Repo { mode: "queue.local".into(), lc_ok: true, open_batch: None, base_sha: BASE.into(), ..qs::Repo::default() };
    json(&qs::Snapshot { repos: [(REPO.to_string(), repo)].into(), world_running: true })
}

/// The effects that read or write the lifecycle store.
const STORE_CLASS: &[&str] = &[sc::OBSERVE, sc::UNIT_START, sift::OBSERVE, sift::RECORD_PASS, bl::OBSERVE, bl::ROUND_OPEN, bl::LC_CERTIFY, bl::LAND, bl::ABANDON];

/// The quiet world with this world's doubles over it, and the unit effects on its manager.
fn doubles(w: &Arc<World>) -> Doubles {
    let forks = w.proc.clone();
    let d = sim::units::with_units(sim::doubles::service(), w.units.clone()).double("fork", move |_| {
        let spec = ProcSpec { prog: "planted".into(), args: vec![], stdin: vec![], deadline: std::time::Duration::from_secs(1) };
        forks.run(&spec).map(|_| String::new())
    });
    let on = |d: Doubles, name: &str, f: fn(&World, &str) -> Result<String, String>| {
        let w = w.clone();
        let store = STORE_CLASS.contains(&name);
        let name = name.to_string();
        d.double(&name.clone(), move |input| {
            if store && w.now() < w.cold_until.load(Ordering::SeqCst) {
                w.cold_refused.lock().unwrap_or_else(|e| e.into_inner()).push(name.clone());
                return Err("the lifecycle store is cold".into());
            }
            f(&w, input)
        })
    };
    let d = d.double(bl::INBOX_APPEND, {
        let w = w.clone();
        move |line| {
            if let Some((_, page)) = line.split_once("[watch:rounds] ") {
                w.lc.lock().unwrap_or_else(|e| e.into_inner()).pages.push(page.to_string());
            }
            Ok(String::new())
        }
    });
    let d = on(d, sc::OBSERVE, |w, _| sentinel_observe(w));
    let d = on(d, sc::UNIT_START, sentinel_unit_start);
    let d = on(d, sift::OBSERVE, |w, _| sift_observe(w));
    let d = on(d, sift::RECORD_PASS, sift_record_pass);
    let d = on(d, bl::OBSERVE, batcher_observe);
    let d = on(d, bl::ASSEMBLE, |_, i| batcher_assemble(i));
    let d = on(d, bl::ROUND_OPEN, batcher_round_open);
    let d = on(d, bl::CORPUS, batcher_corpus);
    let d = on(d, bl::CERTIFY, batcher_certify);
    let d = on(d, bl::LC_CERTIFY, batcher_lc_certify);
    let d = on(d, bl::LAND, batcher_land);
    let d = on(d, bl::ABANDON, |w, _| batcher_abandon(w));
    on(d, qs::OBSERVE, |w, _| queue_step_observe(w))
}

struct Run {
    trace: Vec<(String, String, String, String, u64)>,
    history: Vec<(u64, String, String, String)>,
    refused: Vec<String>,
    asked: Vec<String>,
    unit_calls: Vec<String>,
    states: BTreeMap<String, BeadState>,
    spawned: u64,
    wall: std::time::Duration,
    /// The error the first scheduler died with, when a crash was planned.
    crashed: Option<String>,
    rounds: u32,
    round_open: bool,
    /// Each requested effect and the ids of the outcome events shipped for it.
    effects: Vec<(String, Vec<String>)>,
    /// Every request id a committed turn resolved, in order.
    resolved: Vec<String>,
    /// The batcher's pages, without their stamp.
    pages: Vec<String>,
    /// The store-class effects the cold store refused, by name.
    cold_refused: Vec<String>,
}

/// How a run is interrupted: where the scheduler dies, and whether the restarted one is told
/// nothing about the effects the first left open (the planted fault that shows the checks can see a lost effect).
#[derive(Clone, Copy, Default)]
struct Interrupt {
    crash: Option<Crash>,
    blind: bool,
    /// The scheduler is killed at this virtual instant, wherever its effects stand.
    kill_at: Option<u64>,
    /// After the restart the store answers `Err` for this long.
    cold_ms: u64,
}

/// A journal that boots with no unresolved effect and delegates everything else.
struct Blind(Arc<dyn Journal>);

impl Journal for Blind {
    fn record(&self, ev: &Event) {
        self.0.record(ev)
    }
    fn boot(&self) -> runtime::Result<Boot> {
        self.0.boot().map(|b| Boot { effects: vec![], ..b })
    }
    fn commit(&self, turn: &Turn) -> runtime::Result<()> {
        self.0.commit(turn)
    }
    fn poison(&self, p: &Poison) -> runtime::Result<()> {
        self.0.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> runtime::Result<Option<Poison>> {
        self.0.poisoned(stream)
    }
    fn history(&self) -> Vec<Event> {
        self.0.history()
    }
}

fn effects_of(history: &[Event]) -> Vec<(String, Vec<String>)> {
    let outcome = |e: &Event| [EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT].contains(&e.kind.tag.as_str());
    history
        .iter()
        .filter(|e| e.kind.tag == EFFECT_REQUESTED)
        .map(|r| (r.id.0.clone(), history.iter().filter(|o| outcome(o) && o.cause.as_ref() == Some(&r.id)).map(|o| o.id.0.clone()).collect()))
        .collect()
}

fn run(seed: u64, script: Script) -> Run {
    let r = run_interrupted(seed, script, Interrupt::default());
    assert!(r.crashed.is_none(), "the loop settles: {:?}", r.crashed);
    r
}

/// The processes a crash leaves standing (the journal, the store, the units, the clock) and the
/// way to boot a scheduler over them.
struct Rig {
    seed: u64,
    clock: VirtualClock,
    c: Arc<dyn Clock>,
    units: Arc<ScriptedUnits>,
    mem: Arc<MemJournal>,
    faulty: Arc<FaultyJournal>,
    world: Arc<World>,
    runner: Arc<Doubles>,
}

impl Rig {
    fn new(seed: u64, script: Script, crash: Option<Crash>) -> Rig {
        let clock = VirtualClock::new(0);
        let c: Arc<dyn Clock> = Arc::new(clock.clone());
        let units = Arc::new(ScriptedUnits::new(c.clone()));
        let mut lc = Lc::default();
        for (at, id) in &script.file {
            lc.due.push((*at, Due::File(id.to_string())));
        }
        let mem = MemJournal::new();
        let faulty = FaultyJournal::new(mem.clone());
        let world = Arc::new(World { clock: clock.clone(), crash, crashing: Mutex::default(), faulty: faulty.clone(), cold_until: AtomicU64::new(0), cold_refused: Mutex::default(), units: units.clone(), lc: Mutex::new(lc), script, proc: CountingProc::refusing() });
        let runner = Arc::new(doubles(&world));
        Rig { seed, clock, c, units, mem, faulty, world, runner }
    }

    /// The way `serve` boots over `over`: every task's tick started, and a new driver with a new unit source.
    fn boot(&self, over: Arc<dyn Journal>) -> VirtualDriver {
        let settings = common::settings_with(&[("SPIRA_BATCHER_WALL_SECS", "3600"), ("SPIRA_ROUND_MAX_OPEN", "1")]);
        let mut engine = Engine::open(over, self.c.clone(), registrations(&settings), self.seed).expect("the service's registrations open");
        for (_, stream, kind) in tasks::boots() {
            engine.start(stream, kind);
        }
        let mut sources = Sources::default();
        sources.register(Box::new(spira_runtime::sources::units::scripted(self.units.clone(), self.c.clone()))).expect("the unit.ran source");
        VirtualDriver::new(engine, self.clock.clone(), self.runner.clone() as Arc<dyn EffectRunner>).with_sources(sources).expect("scripted sources only")
    }

    fn journal(&self) -> Arc<dyn Journal> {
        self.mem.clone()
    }

    fn finish(&self, began: std::time::Instant, crashed: Option<String>) -> Run {
        let lc = self.world.lc();
        let journal = self.journal();
        Run {
            trace: runtime::conformance::trace(&*journal),
            history: lc.history.clone(),
            refused: lc.refused.clone(),
            asked: self.runner.asked(),
            unit_calls: self.units.calls(),
            states: lc.rows.iter().map(|(id, r)| (id.clone(), r.state)).collect(),
            spawned: self.world.proc.spawned(),
            wall: began.elapsed(),
            crashed,
            rounds: lc.rounds,
            round_open: lc.round.is_some(),
            effects: effects_of(&journal.history()),
            resolved: self.mem.commits().into_iter().flat_map(|t| t.resolved).collect(),
            pages: lc.pages.clone(),
            cold_refused: self.world.cold_refused.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        }
    }
}

fn run_interrupted(seed: u64, script: Script, interrupt: Interrupt) -> Run {
    let began = std::time::Instant::now();
    let rig = Rig::new(seed, script, interrupt.crash);
    let mut crashed = None;
    let mut driver = rig.boot(rig.faulty.clone());
    let outcome = match interrupt.kill_at {
        Some(at) => driver.run_until(at).map(|()| {
            crashed = Some(format!("killed at {at} ms"));
        }),
        None => driver.run_until(UNTIL_MS),
    };
    if let Err(e) = outcome {
        crashed = Some(e.to_string());
    }
    if crashed.is_some() {
        drop(driver);
        rig.world.crashing.lock().unwrap_or_else(|e| e.into_inner()).down = false;
        rig.world.cold_until.store(rig.clock.now_ms() + interrupt.cold_ms, Ordering::SeqCst);
        let over: Arc<dyn Journal> = if interrupt.blind { Arc::new(Blind(rig.journal())) } else { rig.journal() };
        driver = rig.boot(over);
        driver.run_until(UNTIL_MS).expect("the restarted loop settles");
    }
    rig.finish(began, crashed)
}

/// The batcher's carried machine state for the repo, each change once, in order; `Idle` when
/// the repo carries nothing (an idle repo's carry is removed).
fn batcher_states(trace: &[(String, String, String, String, u64)]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (_, stream, tag, body, _) in trace {
        if stream != bl::STREAM || tag != bl::STATE {
            continue;
        }
        let carry: bl::Carry = serde_json::from_str(body).unwrap_or_default();
        let s = carry.repos.get(REPO).map_or("Idle".to_string(), |rc| format!("{:?}", rc.state));
        if out.last() != Some(&s) {
            out.push(s);
        }
    }
    out
}

/// `bead`'s lifecycle, as the store recorded it.
fn lifecycle_of(r: &Run, bead: &str) -> Vec<String> {
    r.history.iter().filter(|(_, id, _, _)| id == bead).map(|(_, _, _, to)| to.clone()).collect()
}

fn tags_on(r: &Run, stream: &str) -> Vec<String> {
    r.trace.iter().filter(|e| e.1 == stream).map(|e| e.2.clone()).collect()
}

/// `W3_DUMP=1` prints the loop's own streams and the store's history.
fn show(name: &str, r: &Run) {
    if std::env::var_os("W3_DUMP").is_some() {
        println!("=== {name}\n{}", dump(r));
    }
}

fn assert_no_process(r: &Run) {
    assert_eq!(r.spawned, 0, "no process is spawned: every effect is a double");
    let unanswered: Vec<&String> = r.trace.iter().filter(|e| e.2 == runtime::EFFECT_FAILED && e.3.contains("no runner for effect")).map(|e| &e.3).collect();
    assert!(unanswered.is_empty(), "every effect asked for had a double: {unanswered:?}");
    let started: Vec<&String> = r.unit_calls.iter().filter(|c| c.starts_with("start") || c.starts_with("scope") || c.starts_with("run")).collect();
    assert!(!started.is_empty(), "the dwarf and the round's legs ran as units on the simulator's manager: {:?}", r.unit_calls);
}

fn dump(r: &Run) -> String {
    let interesting: Vec<String> = r.trace.iter().filter(|e| [sc::STREAM, sift::STREAM, bl::STREAM, qs::STREAM].contains(&e.1.as_str()) && e.2 != "effect.succeeded").map(|e| format!("{:>8} {} {} {}", e.4, e.1, e.2, e.3.chars().take(160).collect::<String>())).collect();
    format!("history {:?}\nrefused {:?}\nunits {:?}\n{}", r.history, r.refused, r.unit_calls, interesting.join("\n"))
}

#[test_id(tdls98x)]
#[test]
fn one_bead_is_filed_claimed_submitted_cut_certified_and_landed_on_the_service_tasks() {
    let script = Script { file: vec![(30_000, "sp-w3a")], single_flight: true, guilty: None, no_progress_snooze_ms: None };
    let a = run(11, script.clone());
    let b = run(11, script);
    println!("green scenario: wall {:?} and {:?} for {} virtual minutes", a.wall, b.wall, UNTIL_MS / MIN);
    show("green", &a);
    let lifecycle = lifecycle_of(&a, "sp-w3a");
    assert_eq!(lifecycle, ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", dump(&a));
    assert_eq!(a.states["sp-w3a"], BeadState::Landed);
    assert!(a.refused.is_empty(), "the store refused nothing: {:?}", a.refused);
    assert_eq!(batcher_states(&a.trace), ["Idle", "Assembling", "Opened", "Certifying", "Landing", "Idle"], "the first Idle records the named wait for sift (sp-8nlvra): {}", dump(&a));
    let call = tags_on(&a, sc::STREAM);
    assert!(call.contains(&runtime::units::UNIT_RAN.to_string()), "the dwarf's end reached sentinel-call's own stream: {call:?}");
    let sifted = a.trace.iter().any(|e| e.1 == bl::STREAM && e.2 == spira_runtime::tasks::a::kit::WORK_SUBMITTED);
    assert!(sifted, "sift's pass woke the batcher on its own stream (EmitTo work.submitted)");
    let legs: Vec<&String> = a.trace.iter().filter(|e| e.1 == bl::STREAM && e.2 == runtime::units::UNIT_RAN).map(|e| &e.3).collect();
    assert_eq!(legs.len(), 1, "the one certify leg ended on the batcher's stream");
    assert!(a.asked.iter().any(|n| n == qs::OBSERVE), "queue.step observed the repo");
    assert!(!a.asked.iter().any(|n| n.starts_with("queue.step.") && n != qs::OBSERVE), "queue.step asked for nothing past its observe: a queue.local repo with no batch is silent");
    assert_no_process(&a);
    assert_eq!(a.trace, b.trace, "one seed, one trace");
    assert_eq!(a.history, b.history);
    assert_eq!(a.asked, b.asked, "one seed, the same effects in the same order");
}

/// A dwarf that made no progress releases the bead and holds it on a timed wait. Its next claim
/// is after the wait ran out, with no sweep between: the claim ends the hold, so the submit that
/// follows is screened by the next sift and lands.
#[test_id(tdn4w1x)]
#[test]
fn a_bead_released_on_a_timed_wait_is_reclaimed_after_expiry_and_lands() {
    let script = Script { file: vec![(30_000, "sp-w3a")], single_flight: true, guilty: None, no_progress_snooze_ms: Some(3 * MIN) };
    let r = run(14, script);
    show("no-progress", &r);
    assert_eq!(lifecycle_of(&r, "sp-w3a"), ["READY", "WORKING", "READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", dump(&r));
    let claims: Vec<u64> = r.history.iter().filter(|(_, id, _, to)| id == "sp-w3a" && to == "WORKING").map(|(at, ..)| *at).collect();
    assert!(claims[1] >= claims[0] + MIN + 3 * MIN, "the second claim waited out the snooze: {claims:?}");
    assert!(r.refused.is_empty(), "{:?}", r.refused);
}

/// Two members in one round, one of them red. Both are sifted in one pass, so the one cut takes
/// both: the corpus goes red, the attribution leg ejects the guilty member (GateRed: REWORK) and
/// names the survivor, which is certified and landed.
#[test_id(tg6jir3)]
#[test]
fn a_red_round_attributes_ejects_the_guilty_member_and_lands_the_rest() {
    let script = Script { file: vec![(30_000, "sp-w3a"), (30_000, "sp-w3b")], single_flight: false, guilty: Some("sp-w3b"), no_progress_snooze_ms: None };
    let a = run(12, script.clone());
    let b = run(12, script);
    println!("red scenario: wall {:?} and {:?} for {} virtual minutes", a.wall, b.wall, UNTIL_MS / MIN);
    show("red", &a);
    assert_eq!(lifecycle_of(&a, "sp-w3a"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", dump(&a));
    assert_eq!(lifecycle_of(&a, "sp-w3b"), ["READY", "WORKING", "SUBMITTED", "REWORK"], "{}", dump(&a));
    assert!(a.refused.is_empty(), "the store refused nothing: {:?}", a.refused);
    assert_eq!(batcher_states(&a.trace), ["Idle", "Assembling", "Opened", "Certifying", "Attributing", "Certifying", "Landing", "Idle"], "the first Idle records the named wait for sift (sp-8nlvra): {}", dump(&a));
    let opens = a.asked.iter().filter(|n| *n == bl::ROUND_OPEN).count();
    assert_eq!(opens, 1, "one round was opened for the two members");
    assert_no_process(&a);
    assert_eq!(a.trace, b.trace, "one seed, one trace");
    assert_eq!(a.history, b.history);
}

/// The contract's single-flight per repo, when two members are sifted in one pass: sift hands
/// `work.submitted` to the batcher once per member, both wakes ask for an observe at one
/// instant, and the machine must cut once. Both observe answers are memory-only and queued
/// together; the second is decided against the carry the first answer emitted (Assembling, not
/// Idle), because the engine places a handler's writes in the projection before its stream's
/// next event (`runtime::conformance::a_handlers_writes_are_seen_before_its_streams_next_event`).
#[test_id(tnw9o1v)]
#[test]
fn two_members_sifted_in_one_pass_are_cut_into_one_round() {
    let script = Script { file: vec![(30_000, "sp-w3a"), (30_000, "sp-w3b")], single_flight: false, guilty: None, no_progress_snooze_ms: None };
    let a = run(12, script);
    show("one-pass", &a);
    let opens = a.asked.iter().filter(|n| *n == bl::ROUND_OPEN).count();
    assert_eq!(opens, 1, "one cut, one round_open: {}", dump(&a));
    for id in ["sp-w3a", "sp-w3b"] {
        assert_eq!(a.states[id], BeadState::Landed, "{id}: {}", dump(&a));
    }
}

// --- a crash between a store write and its journaled outcome --------------------------------------

const GREEN2: fn() -> Script = || Script { file: vec![(30_000, "sp-w3a"), (30_000, "sp-w3b")], single_flight: false, guilty: None, no_progress_snooze_ms: None };
const RED2: fn() -> Script = || Script { file: vec![(30_000, "sp-w3a"), (30_000, "sp-w3b")], single_flight: false, guilty: Some("sp-w3b"), no_progress_snooze_ms: None };
const BEADS: [&str; 2] = ["sp-w3a", "sp-w3b"];
/// The store writes one landing of `BEADS` makes: deliver and delivered per member, then the round's close.
const LAND_WRITES: u32 = 2 * BEADS.len() as u32 + 1;

/// Every requested effect has an outcome, and no outcome was handled twice. An outcome
/// shipped by a turn that never committed is shipped again as the restart's own, under the
/// same event id: one outcome event, resolved once.
fn effects_resolved(r: &Run) -> Result<(), String> {
    let mut ids: Vec<(&String, std::collections::BTreeSet<&String>)> = r.effects.iter().map(|(k, o)| (k, o.iter().collect())).collect();
    ids.retain(|(_, o)| o.len() != 1);
    if !ids.is_empty() {
        return Err(format!("effects without exactly one outcome: {ids:?}"));
    }
    let mut seen = std::collections::BTreeSet::new();
    let twice: Vec<&String> = r.resolved.iter().filter(|id| !seen.insert(*id)).collect();
    if !twice.is_empty() {
        return Err(format!("outcomes handled twice: {twice:?}"));
    }
    Ok(())
}

/// What the interrupted run must equal the uninterrupted one on, as the first difference.
fn converged(crashed: &Run, clean: &Run) -> Result<(), String> {
    for b in BEADS {
        if lifecycle_of(crashed, b) != lifecycle_of(clean, b) {
            return Err(format!("{b} lifecycle {:?}, uninterrupted {:?}\n{}", lifecycle_of(crashed, b), lifecycle_of(clean, b), dump(crashed)));
        }
    }
    if crashed.states != clean.states {
        return Err(format!("final states {:?}, uninterrupted {:?}", crashed.states, clean.states));
    }
    if crashed.rounds != clean.rounds || crashed.round_open {
        return Err(format!("rounds opened {} (uninterrupted {}), a round still open: {}\n{}", crashed.rounds, clean.rounds, crashed.round_open, dump(crashed)));
    }
    effects_resolved(crashed)?;
    let not_landed: Vec<&String> = crashed.trace.iter().filter(|e| e.1 == bl::STREAM && e.3.contains("NOT LANDED")).map(|e| &e.3).collect();
    if !not_landed.is_empty() {
        return Err(format!("the batcher journaled a verdict the store contradicts: {not_landed:?}"));
    }
    if batcher_states(&crashed.trace).last().map(String::as_str) != Some("Idle") {
        return Err(format!("the batcher ended {:?}\n{}", batcher_states(&crashed.trace), dump(crashed)));
    }
    Ok(())
}

fn crash_at(name: &str, script: fn() -> Script, seed: u64, double: &'static str, after: u32) {
    crash_at_cutting(name, script, seed, double, after, None);
}

/// [`crash_at`], for a crash after which the cut is made from what the store shows sifted so
/// far: `rounds` is how many rounds that takes, where the uninterrupted run took the clean count.
fn crash_at_cutting(name: &str, script: fn() -> Script, seed: u64, double: &'static str, after: u32, rounds: Option<u32>) {
    let interrupt = Interrupt { crash: Some(Crash { double, after }), ..Interrupt::default() };
    let clean = run(seed, script());
    let a = run_interrupted(seed, script(), interrupt);
    let b = run_interrupted(seed, script(), interrupt);
    show(name, &a);
    assert_eq!(a.crashed.as_deref(), Some("injected fault in append"), "the crash was seen at {double} after {after} writes");
    let mut want = clean;
    want.rounds = rounds.unwrap_or(want.rounds);
    let clean = want;
    if let Err(why) = converged(&a, &clean) {
        panic!("{name}: {why}\n{}", dump(&a));
    }
    assert_no_process(&a);
    assert_eq!(a.trace, b.trace, "one seed, one trace");
}

#[test_id(tlo6qiv)]
#[test]
fn a_crash_after_the_dwarfs_claim_converges_on_the_store() {
    crash_at("claim", GREEN2, 12, "sentinel_unit_start", 1);
}

#[test_id(t9kbzbc)]
#[test]
fn a_crash_after_sift_recorded_both_passes_converges_on_the_store() {
    crash_at("sifted_both", GREEN2, 12, "sift_record_pass", 2);
}

/// Sift's first pass reached the store and the second did not: the batcher's first look at the
/// restarted store finds one member sifted and cuts it alone, and the other lands in the next round.
#[test_id(tn7py3n)]
#[test]
fn a_crash_after_sift_recorded_one_pass_cuts_what_the_store_shows_sifted() {
    crash_at_cutting("sifted_one", GREEN2, 12, "sift_record_pass", 1, Some(2));
}

#[test_id(tl650s3)]
#[test]
fn a_crash_after_the_round_was_recorded_converges_on_the_store() {
    crash_at("round_open", GREEN2, 12, "batcher_round_open", 1);
}

#[test_id(tbzns1e)]
#[test]
fn a_crash_after_the_first_member_was_certified_converges_on_the_store() {
    crash_at("lc_certify", GREEN2, 12, "batcher_lc_certify", 1);
}

#[test_id(tkzafc4)]
#[test]
fn a_crash_after_the_first_members_deliver_converges_on_the_store() {
    crash_at("land_1_deliver", GREEN2, 12, "batcher_land", 1);
}

#[test_id(t0h2dv7)]
#[test]
fn a_crash_after_the_first_member_landed_converges_on_the_store() {
    crash_at("land_1_delivered", GREEN2, 12, "batcher_land", 2);
}

#[test_id(tkbzbjj)]
#[test]
fn a_crash_after_the_last_member_landed_converges_on_the_store() {
    crash_at("land_2_delivered", GREEN2, 12, "batcher_land", 4);
}

#[test_id(t5clgcl)]
#[test]
fn a_crash_after_the_round_was_closed_converges_on_the_store() {
    crash_at("land_closed", GREEN2, 12, "batcher_land", 5);
}

#[test_id(t4yjl2t)]
#[test]
fn a_crash_after_the_last_member_was_certified_converges_on_the_store() {
    crash_at("lc_certify_both", GREEN2, 12, "batcher_lc_certify", 2);
}

#[test_id(tvmd5kp)]
#[test]
fn a_crash_after_the_round_was_recorded_converges_when_one_member_is_red() {
    crash_at("red_round_open", RED2, 12, "batcher_round_open", 1);
}

#[test_id(tfhu3y0)]
#[test]
fn a_crash_after_the_survivor_was_delivered_converges_when_one_member_is_red() {
    crash_at("red_land", RED2, 12, "batcher_land", 1);
}

/// The checks above must be able to see a lost effect: a restart that is told nothing about
/// the effects the first scheduler left open never answers the round open, and they fail.
#[test_id(t52flua)]
#[test]
fn the_checks_see_an_effect_lost_at_a_restart() {
    let blind = Interrupt { crash: Some(Crash { double: "batcher_round_open", after: 1 }), blind: true, ..Interrupt::default() };
    let a = run_interrupted(12, GREEN2(), blind);
    assert_eq!(a.crashed.as_deref(), Some("injected fault in append"));
    let pending = effects_resolved(&a).expect_err("the open round_open has no outcome");
    assert!(pending.contains("without exactly one outcome"), "{pending}");
    assert!(converged(&a, &run(12, GREEN2())).is_err(), "and the run does not converge");
}

// --- the restart every landing performs: scheduler and lifecycle store restarted together ---------

/// What the loop does once recovered: where the uninterrupted run ends, no round abandoned or
/// re-driven, no page the uninterrupted run did not raise, and no effect left without an outcome.
fn assert_recovers(base: &Run, r: &Run, at: &str) {
    if let Err(why) = converged(r, base) {
        panic!("{at}: {why}");
    }
    assert_eq!(r.pages, base.pages, "{at}: no page the uninterrupted run did not raise");
    for name in [bl::WITHDRAW, bl::ABANDON, bl::PAUSE, bl::REDRIVE] {
        assert!(!r.asked.iter().any(|n| n == name), "{at}: {name} was asked: {}", dump(r));
    }
    assert_no_process(r);
}

/// J1: the store applied `land` and the scheduler died before it heard: the round ends landed with no page.
#[test_id(tn8ed8r)]
#[test]
fn a_restart_after_land_applied_ends_the_round_landed_without_a_landing_refused_page() {
    let base = run(12, GREEN2());
    let r = run_interrupted(12, GREEN2(), Interrupt { crash: Some(Crash { double: "batcher_land", after: LAND_WRITES }), ..Interrupt::default() });
    assert!(r.crashed.is_some(), "the scheduler died after land applied");
    let lost = r.trace.iter().filter(|e| e.1 == bl::STREAM && e.2 == runtime::EFFECT_TIMED_OUT && e.3 == runtime::engine::LOST_AT_RESTART).count();
    assert!(lost >= 1, "the restart closed the unanswered land as lost at restart: {}", dump(&r));
    assert!(!r.pages.iter().any(|p| p.contains("LANDING REFUSED")), "{:?}", r.pages);
    assert_recovers(&base, &r, "crash after land applied");
    for b in BEADS {
        assert_eq!(r.states[b], BeadState::Landed, "{b}: {}", dump(&r));
    }
}

/// J2: the same restart with the store cold for two virtual minutes.
#[test_id(thyb643)]
#[test]
fn a_restart_at_landing_with_the_store_cold_for_two_minutes_raises_no_withdraw_abandon_pause_or_page() {
    let base = run(12, GREEN2());
    let cold = Interrupt { crash: Some(Crash { double: "batcher_land", after: LAND_WRITES }), cold_ms: 2 * MIN, ..Interrupt::default() };
    let r = run_interrupted(12, GREEN2(), cold);
    assert!(r.cold_refused.iter().any(|n| n.starts_with(bl::NAME)), "the cold store refused a batcher effect after the restart: {:?}", r.cold_refused);
    assert_recovers(&base, &r, "restart at landing, store cold");
}

/// J3: the scheduler dies while the certify leg runs; the leg's `unit.ran` reaches the restarted batcher and the round lands.
#[test_id(tqjbbga)]
#[test]
fn a_crash_mid_certify_lands_the_round_from_the_units_end_without_a_redrive() {
    let base = run(12, GREEN2());
    let entered = base.trace.iter().filter(|e| e.1 == bl::STREAM && e.2 == bl::STATE).find_map(|e| {
        let carry: bl::Carry = serde_json::from_str(&e.3).unwrap_or_default();
        (carry.repos.get(REPO).map(|rc| format!("{:?}", rc.state)).as_deref() == Some("Certifying")).then_some(e.4)
    });
    let at = entered.expect("the batcher certified") + CERTIFY_MS / 2;
    let r = run_interrupted(12, GREEN2(), Interrupt { kill_at: Some(at), ..Interrupt::default() });
    assert!(r.crashed.is_some(), "the scheduler was killed mid-certify");
    let ends = r.trace.iter().filter(|e| e.1 == bl::STREAM && e.2 == units::UNIT_RAN && e.4 > at).count();
    assert!(ends >= 1, "the certify leg's end reached the restarted batcher: {}", dump(&r));
    let certifies = |run: &Run| run.unit_calls.iter().filter(|c| c.contains("certify")).count();
    assert_eq!(certifies(&r), certifies(&base), "no second certify leg was started: {:?}", r.unit_calls);
    assert_recovers(&base, &r, "crash mid certify");
}

fn held_ids(j: &dyn Journal) -> Vec<String> {
    j.history().into_iter().filter(|e| e.kind.tag == runtime::EFFECT_HELD).map(|e| e.id.0).collect()
}

/// How many times each held effect was requested on its release.
fn releases_of(j: &dyn Journal, held: &[String]) -> Vec<usize> {
    let requested: Vec<_> = j.history().into_iter().filter(|e| e.kind.tag == runtime::EFFECT_REQUESTED).collect();
    held.iter().map(|h| requested.iter().filter(|e| e.cause.as_ref().is_some_and(|c| &c.0 == h)).count()).collect()
}

/// J4: a halt survives the restart; each effect held before it is released once on resume.
#[test_id(t6456v9)]
#[test]
fn a_halt_holds_across_a_joint_crash_and_the_resume_releases_each_held_effect_once() {
    let rig = Rig::new(13, Script { file: vec![(30_000, "sp-w3a")], single_flight: true, guilty: None, no_progress_snooze_ms: None }, None);
    let journal = rig.journal();
    let finish = || rig.finish(std::time::Instant::now(), None);
    let mut driver = rig.boot(journal.clone());
    driver.submit(Submit::new("halt-1", runtime::HALT_STREAM, EventKind::new(runtime::SCHEDULER_HALTED, "the operator"))).unwrap();
    driver.run_until(12 * MIN).unwrap();
    let held = held_ids(&*journal);
    assert!(!held.is_empty(), "work was held while halted: {}", dump(&finish()));
    assert_eq!(releases_of(&*journal, &held), vec![0; held.len()], "nothing held is released while halted");

    drop(driver);
    driver = rig.boot(journal.clone());
    assert!(driver.engine().is_halted(), "the halt survives the restart");
    driver.run_until(20 * MIN).unwrap();
    let after = held_ids(&*journal);
    assert!(after.starts_with(&held), "every effect held before the crash is still held, in order: {held:?} then {after:?}");
    let mut unique = after.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), after.len(), "no held effect is held twice");
    assert_eq!(releases_of(&*journal, &after), vec![0; after.len()], "the restart released nothing");
    assert_eq!(finish().states["sp-w3a"], BeadState::Ready, "the bead waits out the halt");

    driver.submit(Submit::new("resume-1", runtime::HALT_STREAM, EventKind::new(runtime::SCHEDULER_RESUMED, "the operator"))).unwrap();
    driver.run_until(UNTIL_MS).unwrap();
    let r = finish();
    let all = held_ids(&*journal);
    assert!(releases_of(&*journal, &all).iter().all(|n| *n <= 1), "each held effect is released at most once");
    assert!(releases_of(&*journal, &held).iter().all(|n| *n == 1), "each effect held before the crash was released once");
    assert_eq!(r.states["sp-w3a"], BeadState::Landed, "{}", dump(&r));
    assert_eq!(lifecycle_of(&r, "sp-w3a"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"]);
}
