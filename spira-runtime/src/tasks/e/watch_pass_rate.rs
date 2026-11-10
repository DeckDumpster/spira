//! # watch.pass_rate — contract
//!
//! watchd row `pass-rate` (`spira/watchers:161`), target `@SPIRA_HOME@/pass-rate.sh watch --interval 900`; pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! Catalogue differs: the script does **not** write the Concierge inbox. It mails the crossing to the `concierge` mailbox (`spira/pass-rate.sh:182-191`, `mail send concierge --kind event`) and prints the line to stdout. Its children are `lib.sh`, `mail`, `awk`, `date`. The Act below keeps a mail effect for parity and offers `inbox.append` as the convergent alternative — a decision, flagged.
//!
//! ## Tick
//!
//! - Cadence 900 s (`spira/watchers:161`; loop `:234-251`). A gate-timing ledger append (`spira/gate-timing.sh`) is the natural extra wake (unverified: no such event). Single-flight.
//!
//! ## Observe
//!
//! `watch.pass_rate.observe` (host class) returns:
//!
//! ```text
//! Snapshot { readable: bool, passes: u64, landed: u64, pct: u64, state: Ok | Over }
//! ```
//!
//! - Source: the batch timing ledger `SPIRA_BATCH_LEDGER` (default `$SPIRA_RUN/batch-timing.tsv`, `spira/conf.d.index:353`), written by `gate-timing.sh` — the component that records each gate pass. Rows with column 2 ≥ now − `SPIRA_PASS_RATE_WINDOW`; a pass is a row, a landing is a row whose column 5 is `green` (`pass-rate.sh:174-179`).
//! - `pct = passes*100/landed`; passes with zero landings → 999999 (over), never a divide-by-zero read as fine (`:177`). Missing ledger → `0 0 0 ok` (`:172`). An unreadable window (date failure) → `readable=false` (`:195`).
//!
//! ## Decide
//!
//! - Carry `watch.pass_rate.state` = `Ok | Over` — today `run/watchd/pass-rate.state` (`:167`, written every tick `:203-204`).
//! - `readable=false` → "BLIND: the pass-rate watcher cannot read the ledger window" **every tick** (`:195`, no latch — kept for parity, flagged as noisy).
//! - `Over` and prev ≠ `Over` → "OVER: <p> gate pass(es) for <g> landing(s) in <W>s (limit <M>%) — rounds are not landing on their first green pass" (`:199`).
//! - `Ok` and prev = `Over` → "CLEAR: <p> gate pass(es) for <g> landing(s), back under <M>%" (`:201`).
//! - Emit `state` only when it changes (today the file is rewritten every tick).
//! - Fixtures: none in the release (unverified: no `test-pass-rate*.sh`); `pass-rate.sh check --now EPOCH` (`:207-219`) is the existing fixture seam. New: empty ledger; passes with no landing; under; crossing; standing over; clear.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.pass_rate.observe` | host | 10 s | (probe) | — |
//! | `watch.pass_rate.mail` | store (mail writes through the bead store) | 10 s | `pass_rate/<state event id>` | an empty subject |
//! | `inbox.append` (shared, alternative) | host | 10 s | same | — |
//!
//! Mail today (`pass-rate.sh:184-190`): mailbox `concierge`, From `Pass Rate <pass-rate@spira>`, Subject `pass-rate: <line>`, kind `event`, body `## Event\n<line>`. The runtime's existing `concierge_note` (`spira-runtime/src/lib.rs:78`) fixes kind `note` and its own From, so it cannot carry this unchanged; `watch.pass_rate.mail` is its own runner on `mail::cmds::send`.
//!
//! If the inbox route is chosen instead, the line is the stdout text with the house prefix: `<YYYY-MM-DDTHH:MM:SSZ> [watch:pass-rate] <line>` (the line as printed at `:183`).
//!
//! Today the stdout copy lands in `run/watchd/pass-rate.log` (`:183`). Readers of that log: none by name in the release or `run/concierge-notes`; watchd's generic walkers only; no cursor exists. Dropped.
//!
//! Health (`pass-rate.sh health`, `:221-232`) → S13 `scheduler status`.
//!
//! ## Silence
//!
//! Readable, state unchanged → probe only, no `Emit`, no mail.
//!
//! ## Replaces
//!
//! - `spira-watch-pass-rate-prod.service`; row `spira/watchers:161`.
//! - Children: `bash lib.sh` (`:165`), `awk` (`:174`), `date` (`:173`), `mail send` (`:184`).
//! - Files retired: `run/watchd/pass-rate.{state,health,log,restarts}`.
//! - Config pinned: `SPIRA_BATCH_LEDGER`, `SPIRA_PASS_RATE_WINDOW`, `SPIRA_PASS_RATE_MAX_PCT` (`spira/conf.d/SPIRA_PASS_RATE_*`).
//!
//! ## Reverse
//!
//! Release rollback restores unit and row; `Observe` mode shadows the mail.

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use crate::tasks::d::pattern;
use crate::tasks::e::alert::Alert;
use crate::watchers::iso8601;

pub const NAME: &str = "watch.pass_rate";
pub const STREAM: &str = "watch.pass_rate";
pub const TICK: EventKindTag = "watch.pass_rate.tick";
pub const STATE: EventKindTag = "watch.pass_rate.state";
pub const OBSERVE: &str = "watch.pass_rate.observe";
pub const MAIL: &str = "watch.pass_rate.mail";
/// The mail route, kept for parity; the `inbox.append` alternative is Ryan's decision and is not
/// declared until it is taken.
pub const EFFECTS: &[&str] = &[OBSERVE, MAIL];
pub const KEYS: &[&str] = &["SPIRA_BATCH_LEDGER", "SPIRA_PASS_RATE_WINDOW", "SPIRA_PASS_RATE_MAX_PCT"];
/// `--interval 900`.
pub const CADENCE_MS: u64 = 900_000;
const OBSERVE_DEADLINE_MS: u64 = 10_000;
const MAIL_DEADLINE_MS: u64 = 10_000;
/// The mail as `pass-rate.sh:184-190` sent it.
pub const MAILBOX: &str = "concierge";
pub const FROM: &str = "Pass Rate <pass-rate@spira>";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub ledger: String,
    pub window_s: u64,
    pub max_pct: u64,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { ledger: cfg("SPIRA_BATCH_LEDGER")?, window_s: cfg_parse("SPIRA_PASS_RATE_WINDOW")?, max_pct: cfg_parse("SPIRA_PASS_RATE_MAX_PCT")? })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rate {
    #[default]
    Ok,
    Over,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub readable: bool,
    pub passes: u64,
    pub landed: u64,
    pub pct: u64,
    pub state: Rate,
}

/// Today's `run/watchd/pass-rate.state`.
pub type Carry = Rate;

/// OVER on the crossing, CLEAR on the way back, BLIND on every unreadable tick (no latch).
pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let say = |line: String| vec![Alert::of(MAILBOX, FROM, "pass-rate", "event", &line).effect(MAIL, format!("{NAME}:{now_ms}"), MAIL_DEADLINE_MS)];
    if !snap.readable {
        return (*carry, say("BLIND: the pass-rate watcher cannot read the ledger window".into()));
    }
    let (p, g, m) = (snap.passes, snap.landed, cfg.max_pct);
    let acts = match (snap.state, carry) {
        (Rate::Over, Rate::Ok) => say(format!("OVER: {p} gate pass(es) for {g} landing(s) in {}s (limit {m}%) — rounds are not landing on their first green pass", cfg.window_s)),
        (Rate::Ok, Rate::Over) => say(format!("CLEAR: {p} gate pass(es) for {g} landing(s), back under {m}%")),
        _ => vec![],
    };
    (snap.state, acts)
}

/// `_pr_measure`'s awk over the ledger's rows: a pass is a row stamped (column 2, ISO, compared
/// as text) at or after `since`, a landing a pass whose column 5 is `green`.
pub fn measure(ledger: &str, since: &str, max_pct: u64) -> Snapshot {
    let (mut passes, mut landed) = (0u64, 0u64);
    for row in ledger.lines() {
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.get(1).is_some_and(|t| *t >= since) {
            passes += 1;
            landed += u64::from(cols.get(4) == Some(&"green"));
        }
    }
    let pct = (passes * 100).checked_div(landed).unwrap_or(if passes == 0 { 0 } else { 999_999 });
    Snapshot { readable: true, passes, landed, pct, state: if passes > 0 && pct > max_pct { Rate::Over } else { Rate::Ok } }
}

/// The observe runner: the ledger's window ending now; a missing ledger reads as no passes.
pub fn observe() -> Result<String, String> {
    let cfg = Config::pinned()?;
    let now_s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let snap = match std::fs::read(&cfg.ledger) {
        Err(_) => Snapshot { readable: true, state: Rate::Ok, ..Snapshot::default() },
        Ok(bytes) => match now_s.checked_sub(cfg.window_s) {
            Some(since) => measure(&String::from_utf8_lossy(&bytes), &iso8601(since), cfg.max_pct),
            None => Snapshot::default(),
        },
    };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

pub struct WatchPassRate {
    pub config: Result<Config, String>,
}

impl WatchPassRate {
    pub fn new(config: Config) -> Self {
        WatchPassRate { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchPassRate { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.pass_rate/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchPassRate {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, decide)
    }
}
