//! # watch.throughput — the landing floor
//!
//! Every 15 minutes: count the beads landed over the trailing 24 h across every registered repo.
//! Below `SPIRA_THROUGHPUT_FLOOR_PER_DAY` the scheduler is halted, the `world.halted` stamp is
//! written and one P0 line goes to the Concierge inbox (law-throughput-floor). A second condition,
//! per repo, pages (and stops nothing) when `SPIRA_THROUGHPUT_ABANDON_COUNT` rounds were abandoned
//! inside `SPIRA_THROUGHPUT_ABANDON_WINDOW` seconds with no landing.
//!
//! Two more per-repo conditions page and stop nothing. A repo whose rounds pass CI far more often
//! than they land (landed under `SPIRA_THROUGHPUT_RATIO_MIN_PCT` per cent of passing runs, once
//! there are `SPIRA_THROUGHPUT_RATIO_MIN_RUNS` of them) is paged, and so is a round that passed and
//! has not landed within `SPIRA_THROUGHPUT_PASSED_GRACE` seconds. Round record files naming a round
//! the store has ended, or never held, are removed each tick (`CLEAR`); a file naming a round the
//! store holds open, or cannot answer for, is refused clearing and paged by name once.
//!
//! The count is the distinct beads named by commits on each repo's landing ref (resolved as
//! `spira_landref` does, never the lane name) inside the day: the ref is the authority on what
//! landed, and one `git log` per repo reads it. The metrics series it once read (`bead-stage`) has
//! been blind since the control-store cutover (sp-uf50sm). A count that cannot be read pages BLIND
//! once and stops nothing. After a stop the task waits for the world to be resumed and
//! `SPIRA_THROUGHPUT_RESTOP_AFTER` seconds more before it will stop it again.
//!
//! The halt is the scheduler's own (`scheduler.halted` on the halt stream) plus the stamp; live
//! dwarves are not slain from here, since no task launches a process.

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::cfg_parse;

use crate::tasks::d::pattern;
use crate::watchers::{carried, INBOX_APPEND};

pub const NAME: &str = "watch.throughput";
pub const STREAM: &str = "watch.throughput";
pub const TICK: EventKindTag = "watch.throughput.tick";
pub const STATE: EventKindTag = "watch.throughput.state";
pub const OBSERVE: &str = "watch.throughput.observe";
pub const STAMP: &str = "watch.throughput.stamp";
pub const CLEAR: &str = "watch.throughput.clear";
pub const EFFECTS: &[&str] = &[OBSERVE, STAMP, CLEAR, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_THROUGHPUT_FLOOR_PER_DAY", "SPIRA_THROUGHPUT_ABANDON_COUNT", "SPIRA_THROUGHPUT_ABANDON_WINDOW", "SPIRA_THROUGHPUT_RESTOP_AFTER", "SPIRA_THROUGHPUT_RATIO_MIN_PCT", "SPIRA_THROUGHPUT_RATIO_MIN_RUNS", "SPIRA_THROUGHPUT_PASSED_GRACE", "SPIRA_ID_PREFIX"];
pub const CADENCE_MS: u64 = 900_000;
const DEADLINE_MS: u64 = 60_000;
const TAG: &str = "throughput";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub floor: u64,
    pub abandon_count: u64,
    pub abandon_window_s: u64,
    pub restop_after_s: u64,
    pub ratio_min_pct: u64,
    pub ratio_min_runs: u64,
    pub passed_grace_s: u64,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { floor: cfg_parse("SPIRA_THROUGHPUT_FLOOR_PER_DAY")?, abandon_count: cfg_parse("SPIRA_THROUGHPUT_ABANDON_COUNT")?, abandon_window_s: cfg_parse("SPIRA_THROUGHPUT_ABANDON_WINDOW")?, restop_after_s: cfg_parse("SPIRA_THROUGHPUT_RESTOP_AFTER")?, ratio_min_pct: cfg_parse("SPIRA_THROUGHPUT_RATIO_MIN_PCT")?, ratio_min_runs: cfg_parse("SPIRA_THROUGHPUT_RATIO_MIN_RUNS")?, passed_grace_s: cfg_parse("SPIRA_THROUGHPUT_PASSED_GRACE")? })
    }
}

/// One repo's rounds: over the day, and over the abandon window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRounds {
    pub repo: String,
    pub cut: u64,
    pub abandoned: u64,
    pub landed: u64,
    pub abandoned_recent: u64,
    pub landed_recent: u64,
    /// Test passes that ended green over the day, whether or not their round landed.
    pub green_runs: u64,
}

/// A round that passed CI and has not reached a landed state, and how long it has sat there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unlanded {
    pub repo: String,
    pub batch: String,
    pub state: String,
    pub age_s: u64,
}

/// What the queue's round record files stand at: the paths nothing live names, and the files
/// refused clearing, each with its reason.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Records {
    pub clear: Vec<String>,
    pub held: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub readable: bool,
    pub halted: bool,
    /// Beads landed on the landing refs per hour, the oldest hour first.
    pub hourly: Vec<u64>,
    /// Distinct beads named by commits on the landing refs over the same day.
    pub landed_refs: u64,
    pub repos: Vec<RepoRounds>,
    pub unlanded: Vec<Unlanded>,
    pub records: Records,
}

impl Snapshot {
    pub fn landed(&self) -> u64 {
        self.hourly.iter().sum::<u64>().max(self.landed_refs)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stop {
    #[default]
    Idle,
    Stopped,
    Resumed {
        at_ms: u64,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub stop: Stop,
    pub blind: bool,
    pub starved: Vec<String>,
    pub diluted: Vec<String>,
    pub unlanded: Vec<String>,
    pub held: Vec<String>,
}

pub struct Verdict {
    pub carry: Carry,
    pub acts: Vec<EffectSpec>,
    pub halt: Option<String>,
}

fn say(now_ms: u64, line: &str) -> EffectSpec {
    EffectSpec { name: INBOX_APPEND.into(), input: pattern::inbox_line(now_ms, TAG, line), deadline_ms: DEADLINE_MS }
}

fn floor_page(snap: &Snapshot, cfg: &Config) -> String {
    let hourly: Vec<String> = snap.hourly.iter().map(u64::to_string).collect();
    let repos: Vec<String> = snap.repos.iter().map(|r| format!("{}: cut {} abandoned {} landed {}", r.repo, r.cut, r.abandoned, r.landed)).collect();
    format!("P0 THROUGHPUT FLOOR: {} landed/24h < {} — world stopped; root-cause and propose | hourly: {} | rounds per repo: {}", snap.landed(), cfg.floor, hourly.join(" "), if repos.is_empty() { "none".into() } else { repos.join("; ") })
}

pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> Verdict {
    let mut next = carry.clone();
    let mut acts = vec![];
    if !snap.readable {
        if !carry.blind {
            acts.push(say(now_ms, "P0 BLIND: the landing count cannot be read, so the throughput floor is not being checked"));
        }
        next.blind = true;
        return Verdict { carry: next, acts, halt: None };
    }
    next.blind = false;

    next.stop = match (carry.stop, snap.halted) {
        (_, true) => Stop::Stopped,
        (Stop::Stopped, false) => Stop::Resumed { at_ms: now_ms },
        (s, false) => s,
    };
    let settled = match next.stop {
        Stop::Idle => true,
        Stop::Stopped => false,
        Stop::Resumed { at_ms } => now_ms >= at_ms + cfg.restop_after_s * 1000,
    };
    let mut halt = None;
    if settled && !snap.halted && snap.landed() < cfg.floor {
        let page = floor_page(snap, cfg);
        halt = Some(page.clone());
        acts.push(EffectSpec { name: STAMP.into(), input: page.clone(), deadline_ms: DEADLINE_MS });
        acts.push(say(now_ms, &page));
        next.stop = Stop::Stopped;
    }

    let starving: Vec<&RepoRounds> = snap.repos.iter().filter(|r| r.abandoned_recent >= cfg.abandon_count && r.landed_recent == 0).collect();
    for r in &starving {
        if !carry.starved.contains(&r.repo) {
            acts.push(say(now_ms, &format!("P0 {}: {} abandoned rounds and 0 landings in {}s — rounds are being cut and thrown away", r.repo, r.abandoned_recent, cfg.abandon_window_s)));
        }
    }
    next.starved = starving.iter().map(|r| r.repo.clone()).collect();

    let diluted: Vec<&RepoRounds> = snap.repos.iter().filter(|r| r.green_runs >= cfg.ratio_min_runs && r.landed * 100 < r.green_runs * cfg.ratio_min_pct).collect();
    for r in &diluted {
        if !carry.diluted.contains(&r.repo) {
            acts.push(say(now_ms, &format!("P0 {}: {} landed against {} passing test runs in 24h, under {}% — rounds pass and do not land", r.repo, r.landed, r.green_runs, cfg.ratio_min_pct)));
        }
    }
    next.diluted = diluted.iter().map(|r| r.repo.clone()).collect();

    let stuck: Vec<&Unlanded> = snap.unlanded.iter().filter(|u| u.age_s >= cfg.passed_grace_s).collect();
    for u in &stuck {
        if !carry.unlanded.contains(&u.batch) {
            acts.push(say(now_ms, &format!("P0 {}: round {} passed {}s ago and has not landed (still {})", u.repo, u.batch, u.age_s, u.state)));
        }
    }
    next.unlanded = stuck.iter().map(|u| u.batch.clone()).collect();

    if !snap.records.clear.is_empty() {
        acts.push(EffectSpec { name: CLEAR.into(), input: serde_json::to_string(&snap.records.clear).unwrap_or_default(), deadline_ms: DEADLINE_MS });
    }
    for h in &snap.records.held {
        if !carry.held.contains(h) {
            acts.push(say(now_ms, &format!("P0 round record refused clearing: {h}")));
        }
    }
    next.held = snap.records.held.clone();
    Verdict { carry: next, acts, halt }
}

pub struct WatchThroughput {
    pub config: Result<Config, String>,
}

impl WatchThroughput {
    pub fn new(config: Config) -> Self {
        WatchThroughput { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchThroughput { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.throughput/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchThroughput {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let input = match &self.config {
                Ok(c) => serde_json::json!({ "now": ev.at / 1000, "abandon_window_s": c.abandon_window_s }).to_string(),
                Err(_) => String::new(),
            };
            return pattern::tick(ev, OBSERVE, input, DEADLINE_MS, ev.at + CADENCE_MS);
        }
        let Some(snap) = pattern::answer::<Snapshot>(view, ev, OBSERVE) else { return vec![] };
        let cfg = match &self.config {
            Ok(c) => c,
            Err(e) => {
                ctx.event().set("config.refused", e.as_str());
                return vec![];
            }
        };
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let v = decide(&carry, &snap, ev.at, cfg);
        let mut out = pattern::commands(STATE, &carry, &v.carry, v.acts);
        if let Some(why) = v.halt {
            out.push(Command::EmitTo { stream: StreamKey(HALT_STREAM.into()), kind: EventKind::new(SCHEDULER_HALTED, &why) });
        }
        out
    }
}
