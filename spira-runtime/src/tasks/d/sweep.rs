//! The certification sweep's machine, shared by [`super::cert_full`] and
//! [`super::cert_sample`]: one pass is a leg on the tip, then for each red the reruns, the probe
//! at the last green commit and the bisect over the window's members, each its own leg, then
//! the filings and one record. It is `cert-sweep pass` (`cert-sweep/src/main.rs:239-475`) as
//! a P2 machine over `cert_sweep`'s pure core: every leg is a transient unit (`unit.start`)
//! whose `unit.ran` moves the machine on, so no effect blocks for the minutes a leg takes.
//!
//! The two modes differ only in what a leg is ([`Sweep`]): full mode checks a tree out and runs
//! `round-vm run` there; subset mode points a branch at the commit and runs `testenv` on the
//! host. Everything after a leg's results are read is one code path, as it was one `pass`.

use std::collections::BTreeMap;

use cert_sweep::{bead_body, bead_title, culprit_body, culprit_title, filing_kind, first_fail_line, judge, record, red_body, runner_declined, runner_verdict, Culprit, Event, FilingKind, Judgement, Member, Outcome, Row, Verdict};
use runtime::event::{Command, EffectSpec, EventKind, EventKindTag, Projection, EFFECT_FAILED, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT};
use runtime::units::{RanOutput, UnitHandle, UnitRan, UnitSpec, UNIT_START, UNIT_STOP};
use serde::{Deserialize, Serialize};

use super::pattern::{self, Table};

/// One `cert-history` row as a snapshot carries it: a suite's verdict at a round.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRow {
    pub round: String,
    pub commit: String,
    pub tree: String,
    pub suite: String,
    pub verdict: String,
}

/// The history's verdict word: its own (`ok`, `red`, `fault`, `skip`), testenv's status words,
/// or `pass`/`fail`.
pub fn verdict(word: &str) -> Option<Verdict> {
    Verdict::parse(word).or_else(|| Verdict::from_status(word)).or(match word {
        "pass" => Some(Verdict::Ok),
        "fail" => Some(Verdict::Red),
        _ => None,
    })
}

/// A history row as `cert_sweep` reads it. The snapshot's history is the full and seed rows
/// whose commit has a `same_tree` answer plus every other row, so `mode` is `full` throughout:
/// certification reads `same_tree`, which only a full or seed row's commit is entered in.
pub fn row_of(h: &HistoryRow) -> Option<Row> {
    Some(Row { commit: h.commit.clone(), round: h.round.clone(), suite: h.suite.clone(), verdict: verdict(&h.verdict)?, secs: None, mode: "full".into() })
}

pub fn history_row(r: &Row, tree: &str) -> HistoryRow {
    HistoryRow { round: r.round.clone(), commit: r.commit.clone(), tree: tree.into(), suite: r.suite.clone(), verdict: r.verdict.as_str().into() }
}

pub fn outcomes(results: &[HistoryRow]) -> Vec<Outcome> {
    results.iter().filter_map(|h| Some(Outcome { suite: h.suite.clone(), verdict: verdict(&h.verdict)?, secs: None })).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Idle,
    WaitRound,
    Running,
    Collect,
    Attribute,
    File,
    Record,
}

/// What a leg is for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Purpose {
    /// The pass itself, over every pick at the tip.
    Pass,
    /// One rerun of the red at the tip.
    Rerun,
    /// The red suite at the last green commit.
    Green,
    /// The red suite at window member `mid`.
    Bisect(usize),
}

/// The leg in flight: its unit's key, what it runs, and where its results land.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leg {
    pub purpose: Purpose,
    pub commit: String,
    pub suites: Vec<String>,
    pub seq: u32,
    pub key: String,
    pub results_dir: String,
    /// The run's stdout tail, from its `unit.ran`.
    #[serde(default)]
    pub tail: String,
    /// The window to list with the prepare step, for the probe at the last green commit.
    #[serde(default)]
    pub window: Option<(String, String)>,
}

/// A window member and the first round that contains it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAt {
    pub commit: String,
    pub bead: Option<String>,
    pub round: Option<String>,
}

/// What the prepare step (checkout or branch) answers: the program the leg runs, the tree it
/// runs in, and the window's members when one was asked for. Empty fields take the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepared {
    #[serde(default)]
    pub program: String,
    #[serde(default)]
    pub tree: String,
    #[serde(default)]
    pub members: Vec<MemberAt>,
}

/// The prepare step's input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepare {
    pub commit: String,
    pub window: Option<(String, String)>,
}

/// The observe probe's input: none for a tick, the leg's results otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Look {
    #[serde(default)]
    pub results_dir: Option<String>,
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub round: String,
}

/// One red's attribution in progress.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribution {
    pub tries: u32,
    pub verdicts: Vec<Verdict>,
    /// The first red rerun's output, for its first FAIL line.
    pub text: String,
    pub members: Vec<MemberAt>,
    pub lo: usize,
    pub hi: usize,
}

/// A filing request: `bead.sh file <title> --for builder --repo spira --priority <p>`, after
/// the open-duplicate check on `suite` and `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReq {
    pub title: String,
    pub body: String,
    pub suite: String,
    pub kind: FilingKind,
    pub priority: u32,
}

/// The answer a filing runner gives for an open duplicate, before the id.
pub const DUPLICATE: &str = "duplicate ";

/// What one record appends: history rows, event rows in order, then the summary. `lines` is
/// each event's rendered text, the lines `cert-sweep` printed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recording {
    pub rows: Vec<Row>,
    pub events: Vec<Event>,
    pub lines: Vec<String>,
}

/// One pass in flight.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    pub start_s: u64,
    pub leg: Option<Leg>,
    pub rows: Vec<Row>,
    pub extra: Vec<Row>,
    pub events: Vec<Event>,
    pub cursor: usize,
    /// The event at `cursor` as attribution leaves it, and its filing.
    pub event: Option<Event>,
    pub filing: Option<(String, String)>,
    pub attribution: Option<Attribution>,
    pub emitted: Vec<Event>,
    pub ran: usize,
    pub red: usize,
    pub fault: usize,
    /// Full mode: a leg left no results, so no later leg of this pass waits on the round VM.
    pub vm_cut: bool,
}

/// `{ phase, tip, round, picks, seq, pending, reruns, bisect, held, filed }` and the pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub phase: Phase,
    pub tip: String,
    pub round: String,
    pub picks: Vec<String>,
    pub seq: u32,
    pub pending: Vec<String>,
    pub reruns: BTreeMap<String, Vec<String>>,
    pub bisect: BTreeMap<String, (String, String)>,
    pub held: Vec<String>,
    pub filed: u32,
    /// When `WaitRound` began, for the round wait.
    pub waiting_since: Option<u64>,
    #[serde(default)]
    pub work: Work,
}

/// What moved the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input<S> {
    Observed(S),
    /// The observe probe failed.
    Unobserved(String),
    Ran(UnitRan, RanOutput),
    Prepared(Result<Prepared, String>),
    Filed(Result<String, String>),
    /// The silence watch for leg `seq` fired.
    Silence(u32),
}

/// The mode a pass runs in: the names of its effects and what one leg runs.
pub struct Sweep {
    pub task: &'static str,
    /// `full` or `subset`, as the history's `mode` column and the summary read it.
    pub mode: &'static str,
    pub table: Table<Phase>,
    pub observe: &'static str,
    pub prepare: &'static str,
    pub file_bead: &'static str,
    pub record: &'static str,
    pub reruns: u32,
    pub max_beads: u32,
    pub priority: u32,
    /// The unit a leg runs, from the leg and what its prepare step answered.
    pub unit: fn(&Leg, &Prepared) -> UnitSpec,
    /// How long a leg may stay silent before it is stopped (full mode).
    pub silence_ms: Option<u64>,
    pub watch: EventKindTag,
    pub run_dir: String,
}

const PREPARE_DEADLINE_MS: u64 = 120_000;
const UNIT_DEADLINE_MS: u64 = 30_000;
const FILE_DEADLINE_MS: u64 = 60_000;
const RECORD_DEADLINE_MS: u64 = 10_000;
const OBSERVE_DEADLINE_MS: u64 = 30_000;

/// One move's output: the carry after it and the commands besides the carry's own `Emit`.
pub type Step = Result<(Carry, Vec<Command>), String>;

impl Sweep {
    fn go(&self, c: &mut Carry, on: &str) -> Result<(), String> {
        c.phase = pattern::transition(self.table, self.task, c.phase, on)?;
        Ok(())
    }

    fn effect(name: &str, input: String, deadline_ms: u64) -> Command {
        Command::Effect(EffectSpec { name: name.into(), input, deadline_ms })
    }

    fn json<T: Serialize>(v: &T) -> String {
        serde_json::to_string(v).unwrap_or_default()
    }

    /// Starts a pass over `picks` at `tip`: the pass leg's prepare step.
    pub fn begin(&self, c: &mut Carry, now_ms: u64, tip: &str, round: &str, picks: Vec<String>, on: &str) -> Step {
        self.go(c, on)?;
        c.tip = tip.into();
        c.round = round.into();
        c.picks = picks.clone();
        c.seq = 0;
        c.held.clear();
        c.filed = 0;
        c.pending.clear();
        c.reruns.clear();
        c.bisect.clear();
        c.waiting_since = None;
        c.work = Work { start_s: now_ms / 1000, ..Work::default() };
        let cmds = self.leg(c, Purpose::Pass, tip.to_string(), picks, None);
        Ok((c.clone(), cmds))
    }

    /// The next leg's prepare step; the unit starts when it answers.
    fn leg(&self, c: &mut Carry, purpose: Purpose, commit: String, suites: Vec<String>, window: Option<(String, String)>) -> Vec<Command> {
        let seq = c.seq;
        c.seq += 1;
        let short = &commit[..commit.len().min(12)];
        let key = format!("{short}:{}:{seq}", c.work.start_s);
        let results_dir = format!("{}/cert-sweep/{}-{}-{seq}", self.run_dir, c.work.start_s, self.mode);
        c.work.leg = Some(Leg { purpose, commit: commit.clone(), suites, seq, key, results_dir, tail: String::new(), window: window.clone() });
        vec![Self::effect(self.prepare, Self::json(&Prepare { commit, window }), PREPARE_DEADLINE_MS)]
    }

    /// The current leg's unit, once its prepare step answered.
    fn start_unit(&self, c: &mut Carry, now_ms: u64, prepared: Prepared) -> Step {
        self.go(c, "prepared")?;
        let leg = c.work.leg.clone().ok_or_else(|| format!("{}: a prepare answer with no leg in flight", self.task))?;
        if let Some(a) = c.work.attribution.as_mut() {
            if !prepared.members.is_empty() {
                a.members = prepared.members.clone();
            }
        }
        let spec = (self.unit)(&leg, &prepared);
        let mut cmds = vec![Self::effect(UNIT_START, Self::json(&spec), UNIT_DEADLINE_MS)];
        if let Some(ms) = self.silence_ms {
            cmds.push(Command::Timer { at: now_ms + ms, kind: EventKind::new(self.watch, &leg.seq.to_string()) });
        }
        Ok((c.clone(), cmds))
    }

    /// One move on `input`, the snapshot's history and results already read by the caller.
    pub fn step(&self, carry: &Carry, input: &Input<(Vec<HistoryRow>, Option<Vec<HistoryRow>>)>, now_ms: u64) -> Step {
        let mut c = carry.clone();
        match input {
            Input::Prepared(Ok(p)) if c.phase == Phase::Running => self.start_unit(&mut c, now_ms, p.clone()),
            Input::Prepared(Err(why)) if c.phase == Phase::Running => {
                self.go(&mut c, "prepare failed")?;
                self.leg_done(&mut c, now_ms, &[], None, &format!("the leg could not be prepared: {why}"))
            }
            Input::Ran(ran, out) => {
                let key = ran.key_for(self.task).unwrap_or_default();
                let mine = c.work.leg.as_ref().is_some_and(|l| l.key == key);
                if !mine || c.phase != Phase::Running {
                    return Err(pattern::transition(self.table, self.task, c.phase, "unit.ran").err().unwrap_or_else(|| format!("{}: unit.ran for {key}, not the leg in flight, refused in state {:?}", self.task, c.phase)));
                }
                self.go(&mut c, "unit.ran")?;
                let leg = c.work.leg.as_mut().ok_or("no leg")?;
                leg.tail = out.stdout_tail.clone().unwrap_or_default();
                let look = Look { results_dir: Some(leg.results_dir.clone()), stdout: Some(leg.tail.clone()), commit: leg.commit.clone(), round: c.round.clone() };
                Ok((c.clone(), vec![Command::Probe(EffectSpec { name: self.observe.into(), input: Self::json(&look), deadline_ms: OBSERVE_DEADLINE_MS })]))
            }
            Input::Silence(seq) => {
                let live = c.phase == Phase::Running && c.work.leg.as_ref().is_some_and(|l| l.seq == *seq);
                if !live {
                    return Ok((c, vec![]));
                }
                let leg = c.work.leg.as_ref().ok_or("no leg")?;
                let look = Look { results_dir: Some(leg.results_dir.clone()), stdout: None, commit: leg.commit.clone(), round: c.round.clone() };
                Ok((c.clone(), vec![Command::Probe(EffectSpec { name: self.observe.into(), input: Self::json(&look), deadline_ms: OBSERVE_DEADLINE_MS })]))
            }
            // The silence watch's answer: a leg that wrote nothing is stopped, as no verdict.
            Input::Observed((_, results)) if c.phase == Phase::Running => {
                if results.as_ref().is_some_and(|r| !r.is_empty()) {
                    return Ok((c, vec![]));
                }
                self.go(&mut c, "silent")?;
                let leg = c.work.leg.clone().ok_or("no leg")?;
                let handle = UnitHandle::of(self.task, &leg.key).map_err(|e| e.0)?;
                let secs = self.silence_ms.unwrap_or_default() / 1000;
                let why = format!("VERDICT no-verdict: round-vm produced no output in {secs}s (likely queued in host-wide admission)");
                let (c2, mut cmds) = self.leg_done(&mut c, now_ms, &[], None, &why)?;
                cmds.insert(0, Self::effect(UNIT_STOP, Self::json(&handle), UNIT_DEADLINE_MS));
                Ok((c2, cmds))
            }
            Input::Observed((history, results)) if c.phase == Phase::Collect => {
                let history: Vec<Row> = history.iter().filter_map(row_of).collect();
                let got = results.as_deref().map(outcomes).unwrap_or_default();
                self.leg_done(&mut c, now_ms, &got, Some(&history), "")
            }
            Input::Unobserved(why) if c.phase == Phase::Collect => self.leg_done(&mut c, now_ms, &[], None, why),
            Input::Filed(r) if c.phase == Phase::File => {
                let suite = c.work.event.as_ref().and_then(|e| e.suite().map(str::to_string)).unwrap_or_default();
                match r {
                    Ok(out) => {
                        if !out.starts_with(DUPLICATE) {
                            c.filed += 1;
                        }
                        self.go(&mut c, "filed")?;
                        if let Some(e) = c.work.event.take() {
                            c.work.emitted.push(e);
                        }
                    }
                    Err(_) => {
                        self.go(&mut c, "held")?;
                        c.held.push(suite);
                        c.work.event = None;
                    }
                }
                c.work.cursor += 1;
                self.advance(&mut c, now_ms)
            }
            _ => Ok((c, vec![])),
        }
    }

    /// A leg ended with `got` (empty: no results). `history` is read on the pass leg only.
    fn leg_done(&self, c: &mut Carry, now_ms: u64, got: &[Outcome], history: Option<&[Row]>, why: &str) -> Step {
        let leg = c.work.leg.take().ok_or_else(|| format!("{}: a leg ended with none in flight", self.task))?;
        if leg.purpose == Purpose::Pass {
            if got.is_empty() {
                if runner_declined(&leg.tail) {
                    self.go(c, "no results declined")?;
                    c.work = Work::default();
                    return Ok((c.clone(), vec![]));
                }
                self.go(c, "no results")?;
                let why = match (runner_verdict(&leg.tail), why) {
                    (Some(v), _) => format!("the runner returned no results: {v}"),
                    (None, "") => "the runner returned no results".into(),
                    (None, w) => w.to_string(),
                };
                let e = Event::SweepFault { mode: self.mode.into(), round: c.round.clone(), why };
                let rec = Recording { rows: vec![], lines: vec![e.render()], events: vec![e] };
                c.work = Work::default();
                return Ok((c.clone(), vec![Self::effect(self.record, Self::json(&rec), RECORD_DEADLINE_MS)]));
            }
            self.go(c, "results")?;
            let picked: Vec<Outcome> = got.iter().filter(|o| c.picks.contains(&o.suite)).cloned().collect();
            let (rows, events) = record(history.unwrap_or_default(), &picked, &c.tip, &c.round, self.mode);
            let count = |v| picked.iter().filter(|o| o.verdict == v).count();
            c.work.ran = picked.len();
            c.work.red = count(Verdict::Red);
            c.work.fault = count(Verdict::Fault);
            c.work.rows = rows;
            c.work.events = events;
            c.work.cursor = 0;
            c.pending = c.work.events.iter().filter(|e| matches!(e, Event::NewRed { .. } | Event::RedUnbounded { .. })).filter_map(|e| e.suite().map(str::to_string)).collect();
            return self.advance(c, now_ms);
        }
        self.go(c, "verdict")?;
        if got.is_empty() && self.silence_ms.is_some() {
            c.work.vm_cut = true;
        }
        let e = c.work.events.get(c.work.cursor).cloned().ok_or("no event under attribution")?;
        let suite = e.suite().unwrap_or_default().to_string();
        let v = got.iter().find(|o| o.suite == suite).map(|o| o.verdict).filter(|v| matches!(v, Verdict::Ok | Verdict::Red));
        let mut a = c.work.attribution.take().unwrap_or_default();
        match leg.purpose {
            Purpose::Pass => unreachable!("handled above"),
            Purpose::Rerun => {
                a.tries += 1;
                if let Some(v) = v {
                    if v == Verdict::Red && a.text.is_empty() {
                        a.text = leg.tail.clone();
                    }
                    a.verdicts.push(v);
                    c.reruns.entry(suite.clone()).or_default().push(v.as_str().into());
                }
                if a.tries < self.reruns {
                    return self.attribute_leg(c, now_ms, a, Purpose::Rerun, c.tip.clone(), None);
                }
                self.judged(c, now_ms, &e, a)
            }
            Purpose::Green => match v {
                Some(Verdict::Ok) => {
                    if a.members.is_empty() {
                        return self.plain(c, now_ms, &e, &a, "Reproducible; the bisect over the window's members could not finish.");
                    }
                    a.lo = 0;
                    a.hi = a.members.len() - 1;
                    self.bisect_next(c, now_ms, &e, a)
                }
                Some(_) => self.plain(c, now_ms, &e, &a, "Also red at the last green round when rerun: the history's green does not hold here, so no window can be named."),
                None => self.hold(c, now_ms, &suite),
            },
            Purpose::Bisect(mid) => match v {
                Some(Verdict::Red) => {
                    a.hi = mid;
                    self.bisect_next(c, now_ms, &e, a)
                }
                Some(_) => {
                    a.lo = mid + 1;
                    self.bisect_next(c, now_ms, &e, a)
                }
                None => self.plain(c, now_ms, &e, &a, "Reproducible; the bisect over the window's members could not finish."),
            },
        }
    }

    /// The next attribution leg, unless an earlier leg of this full pass left no results.
    fn attribute_leg(&self, c: &mut Carry, now_ms: u64, a: Attribution, purpose: Purpose, commit: String, window: Option<(String, String)>) -> Step {
        let suite = c.work.events.get(c.work.cursor).and_then(|e| e.suite().map(str::to_string)).unwrap_or_default();
        c.work.attribution = Some(a);
        if c.work.vm_cut {
            // `Rt::run`: the round VM gave no verdict earlier in this pass; not waiting on it again.
            c.work.leg = Some(Leg { purpose, commit, suites: vec![suite], seq: c.seq, key: String::new(), results_dir: String::new(), tail: String::new(), window: None });
            self.go(c, "leg")?;
            self.go(c, "prepare failed")?;
            return self.leg_done(c, now_ms, &[], None, "the round VM gave no verdict earlier in this pass");
        }
        self.go(c, "leg")?;
        let cmds = self.leg(c, purpose, commit, vec![suite], window);
        Ok((c.clone(), cmds))
    }

    fn judged(&self, c: &mut Carry, now_ms: u64, e: &Event, a: Attribution) -> Step {
        let suite = e.suite().unwrap_or_default().to_string();
        let mk = |v: &Verdict| Row { commit: c.tip.clone(), round: c.round.clone(), suite: suite.clone(), verdict: *v, secs: None, mode: "rerun".into() };
        let rows: Vec<Row> = a.verdicts.iter().map(mk).collect();
        match judge(&a.verdicts) {
            Judgement::Inconclusive => {
                self.go(c, "inconclusive")?;
                self.hold(c, now_ms, &suite)
            }
            Judgement::Flaky => {
                self.go(c, "flaky")?;
                c.work.extra.extend(rows);
                let ev = Event::Flip { suite, round: c.round.clone(), commit: c.tip.clone() };
                c.work.filing = bead_title(&ev).map(|t| (t, bead_body(&ev)));
                c.work.event = Some(ev);
                self.file_or_emit(c, now_ms)
            }
            Judgement::Reproducible => {
                self.go(c, "reproducible")?;
                c.work.extra.extend(rows);
                match e {
                    Event::NewRed { last_green_commit, .. } => {
                        let window = Some((last_green_commit.clone(), c.tip.clone()));
                        self.attribute_leg(c, now_ms, a, Purpose::Green, last_green_commit.clone(), window)
                    }
                    _ => self.plain(c, now_ms, e, &a, "Reproducible, and no green round is known to bisect from."),
                }
            }
        }
    }

    fn bisect_next(&self, c: &mut Carry, now_ms: u64, e: &Event, a: Attribution) -> Step {
        if a.lo < a.hi {
            let mid = (a.lo + a.hi) / 2;
            let commit = a.members[mid].commit.clone();
            c.bisect.insert(e.suite().unwrap_or_default().to_string(), (a.members[a.lo].commit.clone(), a.members[a.hi].commit.clone()));
            return self.attribute_leg(c, now_ms, a, Purpose::Bisect(mid), commit, None);
        }
        let m = a.members[a.lo].clone();
        let culprit = Culprit { suite: e.suite().unwrap_or_default().into(), round: m.round.clone().unwrap_or_else(|| c.round.clone()), member: Member { commit: m.commit, bead: m.bead }, first_fail: first_fail_line(&a.text) };
        c.work.filing = Some((culprit_title(&culprit), culprit_body(&culprit, e)));
        c.work.event = Some(e.clone());
        self.file_or_emit(c, now_ms)
    }

    fn plain(&self, c: &mut Carry, now_ms: u64, e: &Event, a: &Attribution, note: &str) -> Step {
        let fail = first_fail_line(&a.text);
        c.work.filing = bead_title(e).map(|t| (t, red_body(e, fail.as_deref(), note)));
        c.work.event = Some(e.clone());
        self.file_or_emit(c, now_ms)
    }

    fn hold(&self, c: &mut Carry, now_ms: u64, suite: &str) -> Step {
        c.held.push(suite.into());
        c.work.attribution = None;
        c.work.event = None;
        c.work.filing = None;
        c.work.cursor += 1;
        if c.phase != Phase::Attribute {
            self.go(c, "next")?;
        }
        self.advance(c, now_ms)
    }

    /// The event at the cursor, attributed: filed when it carries a filing under the bound,
    /// held when the bound is reached, emitted otherwise.
    fn file_or_emit(&self, c: &mut Carry, now_ms: u64) -> Step {
        if c.phase != Phase::Attribute {
            self.go(c, "next")?;
        }
        c.work.attribution = None;
        let e = c.work.event.clone().ok_or("no event to file")?;
        let suite = e.suite().unwrap_or_default().to_string();
        if let (Some((title, body)), Some(kind)) = (c.work.filing.take(), filing_kind(&e)) {
            if c.filed >= self.max_beads {
                return self.hold(c, now_ms, &suite);
            }
            self.go(c, "file")?;
            let req = FileReq { title, body, suite, kind, priority: self.priority };
            return Ok((c.clone(), vec![Self::effect(self.file_bead, Self::json(&req), FILE_DEADLINE_MS)]));
        }
        c.work.emitted.push(e);
        c.work.event = None;
        c.work.cursor += 1;
        self.advance(c, now_ms)
    }

    /// Walks the events from the cursor: a red starts its reruns, anything else is filed or
    /// emitted; past the last, the record and the summary.
    fn advance(&self, c: &mut Carry, now_ms: u64) -> Step {
        if c.phase != Phase::Attribute {
            self.go(c, "next")?;
        }
        while let Some(e) = c.work.events.get(c.work.cursor).cloned() {
            if matches!(e, Event::NewRed { .. } | Event::RedUnbounded { .. }) {
                return self.attribute_leg(c, now_ms, Attribution::default(), Purpose::Rerun, c.tip.clone(), None);
            }
            c.work.filing = bead_title(&e).map(|t| (t, bead_body(&e)));
            c.work.event = Some(e);
            let (next, cmds) = self.file_or_emit(c, now_ms)?;
            *c = next;
            if !cmds.is_empty() || c.phase != Phase::Attribute {
                return Ok((c.clone(), cmds));
            }
        }
        self.go(c, "done")?;
        let w = std::mem::take(&mut c.work);
        let rows: Vec<Row> = w.rows.iter().chain(&w.extra).filter(|r| !c.held.contains(&r.suite)).cloned().collect();
        let mut events = w.emitted;
        events.push(Event::Summary { mode: self.mode.into(), round: c.round.clone(), ran: w.ran, red: w.red, fault: w.fault, wall_secs: (now_ms / 1000).saturating_sub(w.start_s) });
        let rec = Recording { rows, lines: events.iter().map(Event::render).collect(), events };
        self.go(c, "done")?;
        c.pending.clear();
        Ok((c.clone(), vec![Self::effect(self.record, Self::json(&rec), RECORD_DEADLINE_MS)]))
    }
}

/// The states every sweep shares after its pass leg starts.
pub const SHARED_ROWS: &[(Phase, &str, Phase)] = &[
    (Phase::Running, "prepared", Phase::Running),
    (Phase::Running, "unit.ran", Phase::Collect),
    (Phase::Running, "prepare failed", Phase::Collect),
    (Phase::Running, "silent", Phase::Collect),
    (Phase::Collect, "no results declined", Phase::Idle),
    (Phase::Collect, "no results", Phase::Idle),
    (Phase::Collect, "results", Phase::Attribute),
    (Phase::Collect, "verdict", Phase::Attribute),
    (Phase::Attribute, "leg", Phase::Running),
    (Phase::Attribute, "flaky", Phase::File),
    (Phase::Attribute, "reproducible", Phase::Attribute),
    (Phase::Attribute, "inconclusive", Phase::File),
    (Phase::Attribute, "file", Phase::File),
    (Phase::Attribute, "done", Phase::Record),
    (Phase::File, "next", Phase::Attribute),
    (Phase::File, "filed", Phase::Attribute),
    (Phase::File, "held", Phase::Attribute),
    (Phase::File, "file", Phase::File),
    (Phase::Record, "done", Phase::Idle),
];

/// The handler's input from `ev`: an observe answer or failure, a prepare or filing outcome,
/// this task's `unit.ran`, or the silence watch.
pub fn input_of<S: serde::de::DeserializeOwned>(sweep: &Sweep, view: &Projection, ev: &runtime::event::Event) -> Option<Input<S>> {
    if ev.kind.tag == sweep.watch {
        return ev.kind.body.parse().ok().map(Input::Silence);
    }
    if ev.kind.tag == runtime::units::UNIT_RAN {
        let (_, ran) = pattern::unit_ran(ev, sweep.task)?;
        return Some(Input::Ran(ran, RanOutput::from_event_kind(&ev.kind).unwrap_or_default()));
    }
    let name = crate::watchers::answered_effect(view, ev)?;
    let ok = ev.kind.tag == EFFECT_SUCCEEDED;
    let failed = ev.kind.tag == EFFECT_FAILED || ev.kind.tag == EFFECT_TIMED_OUT;
    if !ok && !failed {
        return None;
    }
    let why = || if ev.kind.body.is_empty() { ev.kind.tag.to_string() } else { ev.kind.body.clone() };
    if name == sweep.observe {
        return Some(if ok { serde_json::from_str(&ev.kind.body).map(Input::Observed).unwrap_or_else(|e| Input::Unobserved(e.to_string())) } else { Input::Unobserved(why()) });
    }
    if name == sweep.prepare {
        return Some(Input::Prepared(if ok { Ok(serde_json::from_str(&ev.kind.body).unwrap_or_default()) } else { Err(why()) }));
    }
    if name == sweep.file_bead {
        return Some(Input::Filed(if ok { Ok(ev.kind.body.clone()) } else { Err(why()) }));
    }
    None
}

/// The carry's `Emit` when it changed, then the move's own commands.
pub fn commands(state: EventKindTag, before: &Carry, after: &Carry, cmds: Vec<Command>) -> Vec<Command> {
    let mut out = pattern::commands(state, before, after, vec![]);
    out.extend(cmds);
    out
}

// ───────────────────────────── runners ─────────────────────────────

/// `git -C <repo> <args>`: git stays a child, through the runtime's process port.
// batch-job: cert_full's detached checkout and worktree add write a whole tree, as long as its size
pub fn git(repo: &str, args: &[&str]) -> Result<String, String> {
    use runtime::ports::Proc;
    let spec = runtime::ports::ProcSpec { prog: "git".into(), args: ["-C", repo].iter().chain(args).map(|s| s.to_string()).collect(), stdin: vec![], deadline: std::time::Duration::from_secs(60) };
    let out = runtime::ports::SysProc.run(&spec)?;
    if out.code != Some(0) {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The harness checkout the sweeps certify: the home repo's root in the repo map.
pub fn repo(e: &crate::Effects) -> Result<String, String> {
    e.world.home_path().map(|p| p.to_string_lossy().into_owned())
}

/// The ref a pass certifies: `local/main` when the repo has it, else the repo map's land ref.
pub fn landing_ref(repo: &str) -> String {
    if git(repo, &["rev-parse", "--verify", "-q", "local/main^{commit}"]).is_ok() {
        return "local/main".into();
    }
    let home = spira_config::resolve::locate_home_for_process().unwrap_or_default();
    let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), &home);
    spira_config::repos::landref(&reg, repo).unwrap_or_else(|| "local/main".into())
}

/// `(tip, round, suites)` at the landing ref (`cert-sweep/src/main.rs:242-254`).
pub fn tip_of(repo: &str) -> Result<(String, String, Vec<String>), String> {
    let base = landing_ref(repo);
    let tip = git(repo, &["rev-parse", &base])?;
    let round = git(repo, &["for-each-ref", "--points-at", &tip, "--format=%(refname:short)", "refs/archive/rounds"])?.lines().next().and_then(|r| r.rsplit('/').next()).unwrap_or("?").to_string();
    let suites = git(repo, &["ls-tree", "--name-only", &tip, "spira/"])?.lines().filter_map(|p| p.strip_prefix("spira/")).filter(|n| n.starts_with("test-") && n.ends_with(".sh")).map(str::to_string).collect();
    Ok((tip, round, suites))
}

/// Every history row, as a snapshot carries it.
pub fn history(run: &str) -> Vec<Row> {
    cert_sweep::store::read_history(std::path::Path::new(run))
}

/// A leg's results as snapshot rows: the results dir's `.result` files, else the parsed testenv
/// stdout; `None` when there are none at all.
pub fn results(look: &Look) -> Option<Vec<HistoryRow>> {
    let mut got = look.results_dir.as_deref().map(|d| cert_sweep::store::results_in(std::path::Path::new(d))).unwrap_or_default();
    if got.is_empty() {
        got = look.stdout.as_deref().map(cert_sweep::parse_testenv_stdout).unwrap_or_default();
    }
    let rows: Vec<HistoryRow> = got.iter().map(|o| HistoryRow { round: look.round.clone(), commit: look.commit.clone(), tree: String::new(), suite: o.suite.clone(), verdict: o.verdict.as_str().into() }).collect();
    (!rows.is_empty()).then_some(rows)
}

/// The window's members, each with the first round that contains it, for the bisect.
pub fn members(repo: &str, window: &Option<(String, String)>) -> Vec<MemberAt> {
    let Some((green, tip)) = window else { return vec![] };
    let log = git(repo, &["log", "--reverse", "--format=%H%x09%s", &format!("{green}..{tip}")]).unwrap_or_default();
    cert_sweep::parse_members(&log)
        .into_iter()
        .map(|m| {
            let round = git(repo, &["for-each-ref", "--contains", &m.commit, "--sort=committerdate", "--format=%(refname)", "refs/archive/rounds"]).ok().and_then(|r| r.lines().next().and_then(|l| l.rsplit('/').next().map(str::to_string)));
            MemberAt { commit: m.commit, bead: m.bead, round }
        })
        .collect()
}

/// The filing: the open-duplicate check over `content list` and the lifecycle's own rows, then
/// `work - file` (which runs `bead.sh file` and creates the row). An unreadable open list is an
/// error, so the suite is held.
pub fn file_bead(e: &crate::Effects, spec: &EffectSpec) -> Result<String, String> {
    let req: FileReq = serde_json::from_str(&spec.input).map_err(|err| format!("{}: bad input: {err}", spec.name))?;
    let lc = spira_config::lc_call::LcClient::socket(&e.world.lc_socket);
    let listed = lc.answer(&["content", "list", "--all", "--limit", "0", "--brief", "--json"]).map_err(|err| format!("open beads unreadable: {err}"))?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&listed).map_err(|err| format!("content list: {err}"))?;
    let all = rows.iter().filter_map(|r| Some((r.get("id")?.as_str()?.to_string(), r.get("title")?.as_str()?.to_string()))).collect();
    let states = spira_config::lc_state::list_on(&lc).map_err(|err| format!("lifecycle state unreadable: {err}"))?;
    let open = cert_sweep::still_open(all, &spira_config::lc_state::index(states));
    if let Some(id) = cert_sweep::open_duplicate(&open, &req.suite, req.kind) {
        return Ok(format!("{DUPLICATE}{id}"));
    }
    let filing = super::watch_statute_audit::Filing { title: req.title, body: cert_sweep::filing_text(&req.body), persona: "builder".into(), repo: "spira".into() };
    super::watch_statute_audit::file_work(&e.world.lc_socket, &filing, Some(&req.priority.to_string()), spec.deadline_ms)
}

/// The record: under the history's named lock, the event rows in order, then the history rows.
pub fn record_rows(run: &str, spec: &EffectSpec) -> Result<String, String> {
    let rec: Recording = serde_json::from_str(&spec.input).map_err(|err| format!("{}: bad input: {err}", spec.name))?;
    let run = std::path::Path::new(run);
    let _lock = cert_sweep::store::lock_history(run)?;
    let iso = crate::watchers::iso8601(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()));
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "unknown".into());
    let (summary, events) = match rec.events.split_last() {
        Some((last, rest)) if matches!(last, Event::Summary { .. }) => (Some(last), rest),
        _ => (None, rec.events.as_slice()),
    };
    for ev in events {
        cert_sweep::store::append(run, cert_sweep::EVENT_FAMILY, &iso, &host, &cert_sweep::event_fields(ev))?;
    }
    for r in &rec.rows {
        cert_sweep::store::append(run, cert_sweep::FAMILY, &iso, &host, &r.fields())?;
    }
    if let Some(s) = summary {
        cert_sweep::store::append(run, cert_sweep::EVENT_FAMILY, &iso, &host, &cert_sweep::event_fields(s))?;
    }
    Ok(rec.lines.join("\n"))
}
