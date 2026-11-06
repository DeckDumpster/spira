//! # watch.refusals — contract
//!
//! Replaces `spira-refusal-watch-prod` (`refusal-watch pass --state run/refusal-watch.state --window-secs 86400 --baseline 5 --repeat-secs 86400 --max-files 10`), pattern P1.
//!
//! ## Tick
//!
//! Hourly (`OnUnitActiveSec=1h`, `OnBootSec=15min`); single-flight. Nothing else wakes it.
//!
//! ## Observe
//!
//! `watch.refusals.observe` returns `{ now, classes: [{actor, event, from_state, refusal, n, first_at, last_at, examples[(bead, at)]}] }`.
//!
//! - The source is the store verb `ops-refusals <window>`, today forked as `spira-lc ops-refusals 86400` (`refusal-watch/src/main.rs:42-56`) and parsed by `parse` (`refusal-watch/src/lib.rs:189-215`).
//! - In the task it is the same method over the in-process `LcClient` (S7a). The lifecycle store decides what a refusal is; the task never re-derives it from events.
//! - An unreadable answer is "could not check" (today exit 3, `main.rs:96-102`). The task journals `watch.refusals.blind`, never a green pass.
//!
//! ## Decide
//!
//! - `decide(carry, snap, cfg) -> (Carry, Vec<Filing>)` is today's `due` (`lib.rs:232-249`) plus the selection half of `pass` (`lib.rs:291-317`): classes over baseline, not filed inside `repeat_secs`, worst first, at most `max_files`.
//! - Carry is today's state file, `class key → filed_at` (`parse_state` / `render_state`, `lib.rs:217-229`). Keys older than 7 × repeat are pruned (`lib.rs:310`). It is emitted as `watch.refusals.state`.
//! - A class is recorded only when its filing **succeeded** (`lib.rs:302-307`). The carry update therefore happens on the `effect.succeeded` of each `watch.refusals.file`, not at decide time.
//! - Config: `window_secs` 86400, `baseline` 5, `repeat_secs` 86400, `max_files` 10 (unit argv; defaults at `main.rs:13-18`).
//! - Fixtures: `refusal-watch/src/tests.rs`:
//!   - planted class over baseline filed (`:43`)
//!   - at/under baseline not filed (`:63`)
//!   - once per window (`:73`)
//!   - failed filing retried (`:92`)
//!   - batch bounded worst-first (`:101`)
//!   - malformed answer is an error (`:117`)
//!   - state round-trips (`:125`)
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.refusals.observe` (probe) | store | 5 s (S7 socket deadline) | — | — |
//! | `watch.refusals.file` | store | 120 s | `watch.refusals:<class key>:<now / repeat_secs>` | a class whose key is in carry inside the repeat window |
//!
//! - **`watch.refusals.file`** files an incident with title `refused <event> from <from_state> (<refusal>) by <actor>` (`lib.rs:158-163`). The body is `body()` (`lib.rs:270-280`), ending "…`spira-lc ops-bead <bead>` shows the whole timeline."
//! - Today that is `incident.sh file <title> -` with the body on stdin (`main.rs:58-81`), overridable by `REFUSAL_WATCH_FILER`. It becomes a library call into `incident` (plan §2).
//!
//! No inbox line. The pass summary `refusal-watch: <n> classes, <f> filed, <x> failed, <d> deferred` and `refusal-watch: filed <title>` (`main.rs:110-122`) go to `run/refusal-watch.log` via the unit's stdout, which no watchd row reads. The task drops them; the filings and the journal carry the facts.
//!
//! ## Silence
//!
//! An idle tick (no class due) emits the probe only. No `watch.refusals.state` Emit is made unless pruning changed the carry.
//!
//! Today every pass prints a summary line. That is a behaviour change, and intended (S11).
//!
//! ## Replaces
//!
//! - `spira-refusal-watch-prod.service` and `spira-refusal-watch-prod.timer` (`CPUQuota=25%`, `TimeoutStartSec=300`).
//! - Children: `spira-lc ops-refusals` (`main.rs:43`) and `incident.sh file` (`main.rs:64`).
//! - Keys pinned: the four argv numbers become settings keys `SPIRA_REFUSAL_WINDOW_SECS`, `_BASELINE`, `_REPEAT_SECS`, `_MAX_FILES` (names proposed; unverified that any such key exists today, since the values live only in the unit argv). `REFUSAL_WATCH_FILER` is a test seam and is not carried.
//!
//! The catalogue row matches the code.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Observe mode shadows `watch.refusals.file`; the carry still advances only on success, so a shadowed filing is retried after the flip.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::pattern;
use crate::watchers::{answered_spec, carried};

pub const NAME: &str = "watch.refusals";
pub const STREAM: &str = "watch.refusals";
pub const TICK: EventKindTag = "watch.refusals.tick";
pub const STATE: EventKindTag = "watch.refusals.state";
/// An unreadable answer: "could not check", never a green pass.
pub const BLIND: EventKindTag = "watch.refusals.blind";
pub const OBSERVE: &str = "watch.refusals.observe";
pub const FILE: &str = "watch.refusals.file";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE];
/// The four numbers live only in the unit argv today; the contract's proposed keys
/// (`SPIRA_REFUSAL_WINDOW_SECS`, `_BASELINE`, `_REPEAT_SECS`, `_MAX_FILES`) are not registered,
/// so the row reads none and carries the argv as constants until they are.
pub const KEYS: &[&str] = &[];
pub const WINDOW_SECS: u64 = 86_400;
pub const BASELINE: u64 = 5;
pub const REPEAT_SECS: u64 = 86_400;
pub const MAX_FILES: usize = 10;
/// `OnUnitActiveSec=1h`.
pub const CADENCE_MS: u64 = 3_600_000;
const OBSERVE_DEADLINE_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub window_secs: u64,
    pub baseline: u64,
    pub repeat_secs: u64,
    pub max_files: usize,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { window_secs: WINDOW_SECS, baseline: BASELINE, repeat_secs: REPEAT_SECS, max_files: MAX_FILES })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Class {
    pub actor: String,
    pub event: String,
    pub from_state: String,
    pub refusal: String,
    pub n: u64,
    pub first_at: u64,
    pub last_at: u64,
    pub examples: Vec<(String, u64)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub now: u64,
    pub classes: Vec<Class>,
}

/// Today's state file: class key → when its filing succeeded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub filed: BTreeMap<String, u64>,
}

const FILE_DEADLINE_MS: u64 = 120_000;

/// What `watch.refusals.file` is handed: the class key the carry records on success, and the
/// incident's title and body as `refusal_watch::{title, body}` wrote them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub key: String,
    pub title: String,
    pub body: String,
}

fn lib_config(cfg: &Config) -> refusal_watch::Config {
    refusal_watch::Config { window_secs: cfg.window_secs, baseline: cfg.baseline, repeat_secs: cfg.repeat_secs, max_files: cfg.max_files }
}

fn signed(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl Class {
    fn lib(&self) -> refusal_watch::Class {
        refusal_watch::Class { actor: self.actor.clone(), event: self.event.clone(), from_state: self.from_state.clone(), refusal: self.refusal.clone(), n: self.n, first_at: signed(self.first_at), last_at: signed(self.last_at), examples: self.examples.iter().map(|(b, at)| (b.clone(), signed(*at))).collect() }
    }

    fn of(c: refusal_watch::Class) -> Class {
        let at = |t: i64| u64::try_from(t).unwrap_or(0);
        Class { actor: c.actor, event: c.event, from_state: c.from_state, refusal: c.refusal, n: c.n, first_at: at(c.first_at), last_at: at(c.last_at), examples: c.examples.into_iter().map(|(b, t)| (b, at(t))).collect() }
    }
}

/// The carry without keys older than 7 × repeat (`refusal-watch/src/lib.rs:310`).
fn pruned(carry: &Carry, now: u64, cfg: &Config) -> Carry {
    let keep = cfg.repeat_secs.saturating_mul(7);
    Carry { filed: carry.filed.iter().filter(|(_, t)| now.saturating_sub(**t) < keep).map(|(k, t)| (k.clone(), *t)).collect() }
}

/// `due` plus the selection half of `pass`: the filings to request, worst first, at most
/// `max_files`; the carry changes here only by pruning.
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let now = now_ms / 1000;
    let lcfg = lib_config(cfg);
    let classes: Vec<refusal_watch::Class> = snap.classes.iter().map(Class::lib).collect();
    let state = carry.filed.iter().map(|(k, t)| (k.clone(), signed(*t))).collect();
    let acts = refusal_watch::due(&classes, &lcfg, &state, signed(now))
        .into_iter()
        .take(cfg.max_files)
        .map(|c| {
            let f = Filing { key: c.key(), title: c.title(), body: refusal_watch::body(c, &lcfg) };
            EffectSpec { name: FILE.into(), input: serde_json::to_string(&f).unwrap_or_default(), deadline_ms: FILE_DEADLINE_MS }
        })
        .collect();
    (pruned(carry, now, cfg), acts)
}

/// The carry once the filing `spec` succeeded: a class is recorded only on success.
pub fn filed(carry: &Carry, spec: &EffectSpec, now_ms: u64) -> Carry {
    let mut next = carry.clone();
    if let Ok(f) = serde_json::from_str::<Filing>(&spec.input) {
        next.filed.insert(f.key, now_ms / 1000);
    }
    next
}

/// The probe: the store's `ops-refusals <window>` over `LcClient`, parsed by `refusal_watch::parse`.
pub fn observe(cfg: &Config) -> Result<String, String> {
    let lc = spira_config::lc_call::LcClient::configured()?;
    let (now, classes) = refusal_watch::read_refusals(&lc, cfg.window_secs).and_then(|j| refusal_watch::parse(&j))?;
    let snap = Snapshot { now: u64::try_from(now).unwrap_or(0), classes: classes.into_iter().map(Class::of).collect() };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The filing: an incident, in-process, exactly as `incident file <title> -` files it.
pub fn file(input: &str) -> Result<String, String> {
    let f: Filing = serde_json::from_str(input).map_err(|e| format!("{FILE}: {e}"))?;
    refusal_watch::file(&f.title, &f.body).map(|_| format!("filed {}", f.title))
}

pub struct WatchRefusals {
    pub config: Result<Config, String>,
}

impl WatchRefusals {
    pub fn new(config: Config) -> Self {
        WatchRefusals { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchRefusals { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.refusals/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchRefusals {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == EFFECT_FAILED {
            // An unreadable answer is "could not check", journaled, never a green pass.
            return if crate::watchers::answers_observe(view, ev, OBSERVE) { vec![Command::Emit(EventKind::new(BLIND, &ev.kind.body))] } else { vec![] };
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        if let Some(spec) = answered_spec(view, ev).filter(|s| s.name == FILE && ev.kind.tag == EFFECT_SUCCEEDED) {
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            return pattern::commands(STATE, &carry, &filed(&carry, &spec, ev.at), vec![]);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
