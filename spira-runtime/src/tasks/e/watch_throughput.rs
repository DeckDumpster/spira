//! # watch.throughput — the landing stall
//!
//! Every minute, per registered repo: no landing for `SPIRA_THROUGHPUT_STALL_SECS` while
//! beads sit SUBMITTED or IN_DELIVERY there is a stall. A stall files one express P0 incident
//! (hourly landings, the repo's rounds, the last landing) for the ops lane to root-cause and
//! repair, and appends one inbox line once the filing succeeded. The episode is cleared by the
//! next landing; a stall older than the 24 h landing window is one episode. A low trailing-day rate alone triggers nothing, and nothing here halts the world.
//!
//! A second condition, per repo, pages (and stops nothing) when `SPIRA_THROUGHPUT_ABANDON_COUNT`
//! rounds were abandoned inside `SPIRA_THROUGHPUT_ABANDON_WINDOW` seconds with no landing.
//!
//! Two more per-repo conditions page and stop nothing. A repo whose rounds pass CI far more often
//! than they land (landed under `SPIRA_THROUGHPUT_RATIO_MIN_PCT` per cent of passing runs, once
//! there are `SPIRA_THROUGHPUT_RATIO_MIN_RUNS` of them) is paged, and so is a round that passed and
//! has not landed within `SPIRA_THROUGHPUT_PASSED_GRACE` seconds. Round record files naming a round
//! the store has ended, or never held, are removed each tick (`CLEAR`); a file naming a round the
//! store holds open, or cannot answer for, is refused clearing and paged by name once.
//!
//! A landing is a commit naming a bead on a repo's landing ref (resolved as `spira_landref` does,
//! never the lane name): the ref is the authority on what landed. A count that cannot be read
//! pages BLIND once and files nothing.

use runtime::event::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use spira_config::process::cfg_parse;

use crate::tasks::d::pattern;
use crate::watchers::halt::Halt;
use crate::watchers::{carried, iso8601, just_activated, INBOX_APPEND};

pub const NAME: &str = "watch.throughput";
pub const STREAM: &str = "watch.throughput";
pub const TICK: EventKindTag = "watch.throughput.tick";
pub const STATE: EventKindTag = "watch.throughput.state";
pub const OBSERVE: &str = "watch.throughput.observe";
pub const FILE: &str = "watch.throughput.file";
pub const CLEAR: &str = "watch.throughput.clear";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE, CLEAR, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_THROUGHPUT_STALL_SECS", "SPIRA_THROUGHPUT_ABANDON_COUNT", "SPIRA_THROUGHPUT_ABANDON_WINDOW", "SPIRA_THROUGHPUT_RATIO_MIN_PCT", "SPIRA_THROUGHPUT_RATIO_MIN_RUNS", "SPIRA_THROUGHPUT_PASSED_GRACE", "SPIRA_ID_PREFIX"];
pub const CADENCE_MS: u64 = 60_000;
const DEADLINE_MS: u64 = 60_000;
const FILE_DEADLINE_MS: u64 = 120_000;
const TAG: &str = "throughput";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub stall_s: u64,
    pub abandon_count: u64,
    pub abandon_window_s: u64,
    pub ratio_min_pct: u64,
    pub ratio_min_runs: u64,
    pub passed_grace_s: u64,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { stall_s: cfg_parse("SPIRA_THROUGHPUT_STALL_SECS")?, abandon_count: cfg_parse("SPIRA_THROUGHPUT_ABANDON_COUNT")?, abandon_window_s: cfg_parse("SPIRA_THROUGHPUT_ABANDON_WINDOW")?, ratio_min_pct: cfg_parse("SPIRA_THROUGHPUT_RATIO_MIN_PCT")?, ratio_min_runs: cfg_parse("SPIRA_THROUGHPUT_RATIO_MIN_RUNS")?, passed_grace_s: cfg_parse("SPIRA_THROUGHPUT_PASSED_GRACE")? })
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

/// One repo's landings over the day: beads per hour (oldest first), the time of the last, and
/// the beads waiting to land.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoLanding {
    pub repo: String,
    pub hourly: Vec<u64>,
    /// Epoch seconds of the last landing inside the day; 0 when the day holds none.
    pub last_landing: u64,
    /// Beads SUBMITTED or IN_DELIVERY for this repo.
    pub waiting: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub readable: bool,
    pub landings: Vec<RepoLanding>,
    pub repos: Vec<RepoRounds>,
    pub unlanded: Vec<Unlanded>,
    pub records: Records,
    #[serde(default)]
    pub halt: Option<Halt>,
    /// The newest release activation, epoch seconds.
    #[serde(default)]
    pub activated_at: Option<u64>,
}

/// What `watch.throughput.file` is handed: one stall episode and the incident that names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub repo: String,
    pub last_landing: u64,
    pub title: String,
    pub body: String,
    pub line: String,
}

impl Filing {
    /// The incident's dedup reference: one per repo and stall episode, an episode being the
    /// stretch since one landing.
    pub fn reference(&self) -> String {
        format!("incident:throughput-stall:{}:{}", self.repo, self.last_landing)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub blind: bool,
    /// Repo → the last landing of the stall episode already filed.
    pub stalled: BTreeMap<String, u64>,
    pub starved: Vec<String>,
    pub diluted: Vec<String>,
    pub unlanded: Vec<String>,
    pub held: Vec<String>,
    /// What holds the stall conditions while the work plane is halted; empty when it is not.
    pub halted: String,
    /// The last time a halt was seen: no wait counts from before it.
    pub resumed_at: u64,
    /// The read just after an activation that was let go unreported; the next unreadable one pages.
    pub retried: bool,
}

pub struct Verdict {
    pub carry: Carry,
    pub acts: Vec<EffectSpec>,
}

fn say(now_ms: u64, line: &str) -> EffectSpec {
    EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:{now_ms}:{:016x}", fnv1a(line)), pattern::inbox_line(now_ms, TAG, line), DEADLINE_MS)
}

fn stall_filing(l: &RepoLanding, rounds: Option<&RepoRounds>, now_ms: u64, cfg: &Config) -> Filing {
    let hourly: Vec<String> = l.hourly.iter().map(u64::to_string).collect();
    let since = if l.last_landing == 0 { "no landing in the last 24h".to_string() } else { format!("last landing {} ({}s ago)", crate::watchers::iso8601(l.last_landing), (now_ms / 1000).saturating_sub(l.last_landing)) };
    let rounds = rounds.map_or("none".to_string(), |r| format!("cut {} abandoned {} landed {}", r.cut, r.abandoned, r.landed));
    let title = format!("landing stalled on {}: {} beads waiting, no landing for {}s", l.repo, l.waiting, cfg.stall_s);
    let body = format!("## Problem\nNo bead has landed on {} for at least {}s while {} sit SUBMITTED or IN_DELIVERY there.\n\n## Evidence\n- {since}\n- landings per hour over 24h, oldest first: {}\n- rounds over 24h: {rounds}\n\n## Ask\nRoot-cause why landing stopped and repair it. Nothing was halted.\n", l.repo, cfg.stall_s, l.waiting, hourly.join(" "));
    let line = format!("P0 {}: landing stalled — {} beads waiting, {since}; incident filed for the ops lane", l.repo, l.waiting);
    Filing { repo: l.repo.clone(), last_landing: l.last_landing, title, body, line }
}

fn is_stalled(l: &RepoLanding, now_ms: u64, cfg: &Config) -> bool {
    l.waiting > 0 && (now_ms / 1000).saturating_sub(l.last_landing) >= cfg.stall_s
}

fn past_halt(carry: &Carry, now_ms: u64, bound_s: u64) -> bool {
    (now_ms / 1000).saturating_sub(carry.resumed_at) >= bound_s
}

pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> Verdict {
    let mut next = carry.clone();
    let mut acts = vec![];
    if let Some(h) = &snap.halt {
        next.halted = h.held_text(now_ms / 1000);
        next.resumed_at = now_ms / 1000;
        return Verdict { carry: next, acts };
    }
    next.halted.clear();
    if !snap.readable {
        if !carry.blind && !carry.retried && just_activated(snap.activated_at, now_ms / 1000) {
            next.retried = true;
            return Verdict { carry: next, acts };
        }
        if !carry.blind {
            let after = if carry.retried { snap.activated_at.map_or(String::new(), |at| format!(" (still unreadable on the retry after the activation at {})", iso8601(at))) } else { String::new() };
            acts.push(say(now_ms, &format!("P0 BLIND: the landings cannot be read{after}, so landing stalls are not being checked")));
        }
        next.blind = true;
        next.retried = false;
        return Verdict { carry: next, acts };
    }
    next.blind = false;
    next.retried = false;

    next.stalled.retain(|repo, _| snap.landings.iter().any(|l| l.repo == *repo && is_stalled(l, now_ms, cfg)));
    for l in snap.landings.iter().filter(|l| is_stalled(l, now_ms, cfg)) {
        if next.stalled.get(&l.repo) == Some(&l.last_landing) || !past_halt(carry, now_ms, cfg.stall_s) {
            continue;
        }
        let f = stall_filing(l, snap.repos.iter().find(|r| r.repo == l.repo), now_ms, cfg);
        acts.push(EffectSpec::keyed(FILE, format!("{NAME}:file:{}:{}", f.repo, f.last_landing), serde_json::to_string(&f).unwrap_or_default(), FILE_DEADLINE_MS));
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

    let stuck: Vec<&Unlanded> = snap.unlanded.iter().filter(|u| u.age_s >= cfg.passed_grace_s && past_halt(carry, now_ms, cfg.passed_grace_s)).collect();
    for u in &stuck {
        if !carry.unlanded.contains(&u.batch) {
            acts.push(say(now_ms, &format!("P0 {}: round {} passed {}s ago and has not landed (still {})", u.repo, u.batch, u.age_s, u.state)));
        }
    }
    next.unlanded = stuck.iter().map(|u| u.batch.clone()).collect();

    if !snap.records.clear.is_empty() {
        acts.push(EffectSpec::keyed(CLEAR, format!("{NAME}:clear:{now_ms}"), serde_json::to_string(&snap.records.clear).unwrap_or_default(), DEADLINE_MS));
    }
    for h in &snap.records.held {
        if !carry.held.contains(h) {
            acts.push(say(now_ms, &format!("P0 round record refused clearing: {h}")));
        }
    }
    next.held = snap.records.held.clone();
    Verdict { carry: next, acts }
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
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("blind"), runtime::rows::Part::Map { name: "stalled", drop: &[] }, runtime::rows::Part::Field("starved"), runtime::rows::Part::Field("diluted"), runtime::rows::Part::Field("unlanded"), runtime::rows::Part::Field("held"), runtime::rows::Part::Field("halted"), runtime::rows::Part::Field("resumed_at"), runtime::rows::Part::Field("retried")];

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
        if let Some(f) = crate::watchers::answered_spec(view, ev).filter(|s| s.name == FILE).and_then(|s| serde_json::from_str::<Filing>(&s.input).ok()) {
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            let mut next = carry.clone();
            next.stalled.insert(f.repo.clone(), f.last_landing);
            return pattern::commands(STATE, &carry, &next, vec![say(ev.at, &f.line)]);
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
        pattern::commands(STATE, &carry, &v.carry, v.acts)
    }
}

/// The incident for one stall episode, filed express at P0 against the home repo, keyed so a
/// refiling of the same episode bumps the existing bead rather than opening another.
pub fn file(input: &str) -> Result<String, String> {
    let f: Filing = serde_json::from_str(input).map_err(|e| format!("{FILE}: {e}"))?;
    let reference = f.reference();
    incident::intake::file_payload_with(&f.title, f.body.as_bytes(), |env| {
        env.priority = "0".into();
        env.express = true;
        env.fenrir_exempt = true;
        env.reference = Some(reference);
    })
    .map(|()| format!("filed {}", f.title))
}
