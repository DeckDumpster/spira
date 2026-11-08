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

/// The in-memory lifecycle store: rows, the writes due later (a dwarf submits when its session
/// ends), the round the batcher has open, and each row's history.
#[derive(Default)]
struct Lc {
    rows: BTreeMap<String, BeadRow>,
    due: Vec<(u64, Due)>,
    history: Vec<(u64, String, String, String)>,
    refused: Vec<String>,
    round: Option<(String, Vec<(String, String)>)>,
    rounds: u32,
    dwarves: BTreeMap<String, u64>,
    items: BTreeMap<String, String>,
}

impl Lc {
    fn file(&mut self, now: u64, id: &str) {
        self.rows.insert(id.into(), BeadRow::filed(id));
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

    fn in_state(&self, s: BeadState) -> Vec<&BeadRow> {
        self.rows.values().filter(|r| r.state == s).collect()
    }
}

/// What a scenario scripts: the beads it files (at what time), how many dwarves the rune may
/// run at once, and which member's change turns the corpus red (none: green).
#[derive(Clone)]
struct Script {
    file: Vec<(u64, &'static str)>,
    max_concurrent: u32,
    guilty: Option<&'static str>,
}

struct World {
    clock: VirtualClock,
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
    let ready = lc.in_state(BeadState::Ready).len() as u32;
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

/// The dwarf: a unit on the scripted manager. It claims the first ready bead now and submits
/// it as its session ends.
fn sentinel_unit_start(w: &World, input: &str) -> Result<String, String> {
    let start: sc::Start = parse(sc::UNIT_START, input)?;
    let now = w.now();
    let unit = units::unit_name(sc::NAME, &start.key).map_err(|e| e.0)?;
    let mut lc = w.lc();
    let bead = lc.in_state(BeadState::Ready).first().map(|r| r.bead_id.clone()).ok_or("the dwarf found nothing ready")?;
    lc.apply(now, &bead, &unit, BeadEventKind::Claim { holder: unit.clone(), lease_until: ((now + DWARF_MS) / 1000) as i64 + 600, stack: Default::default(), stack_depth: 0, stack_max_depth: 0, persona: None })?;
    lc.due.push((now + DWARF_MS - 1, Due::Event { bead: bead.clone(), actor: unit.clone(), kind: BeadEventKind::Submit { tip: tip_of(&bead) } }));
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

fn sift_observe(w: &World) -> Result<String, String> {
    let now = w.now();
    let lc = w.lc();
    let rows: Vec<&BeadRow> = lc.rows.values().filter(|r| matches!(r.state, BeadState::Submitted | BeadState::Certified)).collect();
    let mut repo = sift::Repo { rows: rows.iter().map(|r| pool_row(r)).collect(), fetched: true, read_at_ms: now, identity_pages: Some(vec![]), ..sift::Repo::default() };
    for r in &rows {
        let tip = r.tip.clone().unwrap_or_default();
        repo.branch_tips.insert(r.bead_id.clone(), tip.clone());
        repo.items.insert(r.bead_id.clone(), lc.items.get(&r.bead_id).cloned().unwrap_or_default());
        repo.candidates.insert(r.bead_id.clone(), sift::Candidate { base: BASE.into(), patch_id: Some(format!("patch-{}", r.bead_id)), ..sift::Candidate::default() });
    }
    if let Some((_, members)) = &lc.round {
        repo.open_round = members.iter().map(|(i, t)| format!("{i}:{t}")).collect();
    }
    json(&sift::Snapshot { repos: [(REPO.to_string(), repo)].into() })
}

fn sift_record_pass(w: &World, input: &str) -> Result<String, String> {
    let wr: sift::Write = parse(sift::RECORD_PASS, input)?;
    let now = w.now();
    w.lc().apply(now, &wr.id, sift::NAME, BeadEventKind::Sifted { tip: wr.tip })?;
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
    let open_round = lc.round.as_ref().map(|(id, m)| bl::Round { id: id.clone(), members: m.iter().map(|(i, _)| i.clone()).collect(), head: BASE.into(), worktree: format!("/sim/wt/{id}"), driven: held.as_ref() == Some(id) });
    let repo = bl::Repo { pool, open_round, base_sha: BASE.into(), ..bl::Repo::default() };
    json(&bl::Snapshot { repos: [(REPO.to_string(), repo)].into(), now_ms: w.now() })
}

/// The merge of every member on the base: clean, in the order given, with suites.
fn batcher_assemble(input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::ASSEMBLE, input)?;
    json(&bl::Assembled { merged: leg.members.clone(), wt: format!("/sim/wt/{}", bl::digest(&leg.round_key)), base: leg.base, suites: true, ..bl::Assembled::default() })
}

fn batcher_round_open(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::ROUND_OPEN, input)?;
    let mut lc = w.lc();
    if lc.round.is_some() {
        return Err("a round is already open".into());
    }
    lc.rounds += 1;
    let batch = format!("round-{}", lc.rounds);
    lc.round = Some((batch.clone(), leg.members));
    json(&bl::Opened { batch })
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
            w.lc().due.push((now + ATTRIBUTE_MS - 1, Due::Event { bead: g.into(), actor: "batcher attribute".into(), kind: BeadEventKind::GateRed { tip, reason: GateRedReason::SuitesFailed } }));
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
    let spec = UnitSpec { task: bl::NAME.into(), key: leg.unit.clone(), argv: vec!["queue".into(), "round".into(), "certify".into(), leg.batch, REPO.into()], env: vec![], cwd: None };
    w.units.script(bl::NAME, CERTIFY_MS, Exit::Code(0));
    w.units.script_output(bl::NAME, b"certified\n", b"");
    units::effect(&*w.units, UNIT_RUN_SCOPE, &json(&ScopeSpec { unit: spec, cpu_quota_pct: None, memory_max_bytes: None })?)
}

/// `round certify`'s store half: every member SUBMITTED → CERTIFIED at its tip.
fn batcher_lc_certify(w: &World, input: &str) -> Result<String, String> {
    let leg: bl::Leg = parse(bl::LC_CERTIFY, input)?;
    let now = w.now();
    let mut lc = w.lc();
    for (id, tip) in &leg.members {
        lc.apply(now, id, bl::NAME, BeadEventKind::GatePass { tip: tip.clone(), gate_key: format!("round:{}", leg.round_key) })?;
    }
    Ok("{}".into())
}

/// `queue round land`: the round's members delivered and landed, and the round closed.
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
    }
    lc.round = None;
    json(&bl::Landed { staged: None })
}

fn batcher_abandon(w: &World) -> Result<String, String> {
    w.lc().round = None;
    Ok("{}".into())
}

// --- queue.step ---------------------------------------------------------------------------------

/// A `queue.local` repo has no forge batch: the batcher's own `land` lands its rounds.
fn queue_step_observe(_: &World) -> Result<String, String> {
    let repo = qs::Repo { mode: "queue.local".into(), lc_ok: true, open_batch: None, base_sha: BASE.into(), ..qs::Repo::default() };
    json(&qs::Snapshot { repos: [(REPO.to_string(), repo)].into(), world_running: true })
}

/// The quiet world with this world's doubles over it, and the unit effects on its manager.
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
}

fn run(seed: u64, script: Script) -> Run {
    let began = std::time::Instant::now();
    let clock = VirtualClock::new(0);
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let units = Arc::new(ScriptedUnits::new(c.clone()));
    let mut lc = Lc::default();
    for (at, id) in &script.file {
        lc.due.push((*at, Due::File(id.to_string())));
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
    driver.run_until(UNTIL_MS).expect("the loop settles");
    let lc = world.lc();
    Run {
        trace: runtime::conformance::trace(&*journal),
        history: lc.history.clone(),
        refused: lc.refused.clone(),
        asked: runner.asked(),
        unit_calls: units.calls(),
        states: lc.rows.iter().map(|(id, r)| (id.clone(), r.state)).collect(),
        spawned: world.proc.spawned(),
        wall: began.elapsed(),
    }
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

#[test]
fn one_bead_is_filed_claimed_submitted_cut_certified_and_landed_on_the_service_tasks() {
    let script = Script { file: vec![(30_000, "sp-w3a")], max_concurrent: 1, guilty: None };
    let a = run(11, script.clone());
    let b = run(11, script);
    println!("green scenario: wall {:?} and {:?} for {} virtual minutes", a.wall, b.wall, UNTIL_MS / MIN);
    show("green", &a);
    let lifecycle = lifecycle_of(&a, "sp-w3a");
    assert_eq!(lifecycle, ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", dump(&a));
    assert_eq!(a.states["sp-w3a"], BeadState::Landed);
    assert!(a.refused.is_empty(), "the store refused nothing: {:?}", a.refused);
    assert_eq!(batcher_states(&a.trace), ["Assembling", "Opened", "Corpus", "Certifying", "Landing", "Idle"], "{}", dump(&a));
    let call = tags_on(&a, sc::STREAM);
    assert!(call.contains(&runtime::units::UNIT_RAN.to_string()), "the dwarf's end reached sentinel-call's own stream: {call:?}");
    let sifted = a.trace.iter().any(|e| e.1 == bl::STREAM && e.2 == spira_runtime::tasks::a::kit::WORK_SUBMITTED);
    assert!(sifted, "sift's pass woke the batcher on its own stream (EmitTo work.submitted)");
    let legs: Vec<&String> = a.trace.iter().filter(|e| e.1 == bl::STREAM && e.2 == runtime::units::UNIT_RAN).map(|e| &e.3).collect();
    assert_eq!(legs.len(), 2, "the corpus leg and the certify leg each ended on the batcher's stream");
    assert!(a.asked.iter().any(|n| n == qs::OBSERVE), "queue.step observed the repo");
    assert!(!a.asked.iter().any(|n| n.starts_with("queue.step.") && n != qs::OBSERVE), "queue.step asked for nothing past its observe: a queue.local repo with no batch is silent");
    assert_no_process(&a);
    assert_eq!(a.trace, b.trace, "one seed, one trace");
    assert_eq!(a.history, b.history);
    assert_eq!(a.asked, b.asked, "one seed, the same effects in the same order");
}

/// Two members in one round, one of them red. Both are sifted in one pass, so the one cut takes
/// both: the corpus goes red, the attribution leg ejects the guilty member (GateRed: REWORK) and
/// names the survivor, which is certified and landed.
#[test]
fn a_red_round_attributes_ejects_the_guilty_member_and_lands_the_rest() {
    let script = Script { file: vec![(30_000, "sp-w3a"), (30_000, "sp-w3b")], max_concurrent: 2, guilty: Some("sp-w3b") };
    let a = run(12, script.clone());
    let b = run(12, script);
    println!("red scenario: wall {:?} and {:?} for {} virtual minutes", a.wall, b.wall, UNTIL_MS / MIN);
    show("red", &a);
    assert_eq!(lifecycle_of(&a, "sp-w3a"), ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"], "{}", dump(&a));
    assert_eq!(lifecycle_of(&a, "sp-w3b"), ["READY", "WORKING", "SUBMITTED", "REWORK"], "{}", dump(&a));
    assert!(a.refused.is_empty(), "the store refused nothing: {:?}", a.refused);
    assert_eq!(batcher_states(&a.trace), ["Assembling", "Opened", "Corpus", "Attributing", "Certifying", "Landing", "Idle"], "{}", dump(&a));
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
#[test]
fn two_members_sifted_in_one_pass_are_cut_into_one_round() {
    let script = Script { file: vec![(30_000, "sp-w3a"), (30_000, "sp-w3b")], max_concurrent: 2, guilty: None };
    let a = run(12, script);
    show("one-pass", &a);
    let opens = a.asked.iter().filter(|n| *n == bl::ROUND_OPEN).count();
    assert_eq!(opens, 1, "one cut, one round_open: {}", dump(&a));
    for id in ["sp-w3a", "sp-w3b"] {
        assert_eq!(a.states[id], BeadState::Landed, "{id}: {}", dump(&a));
    }
}
