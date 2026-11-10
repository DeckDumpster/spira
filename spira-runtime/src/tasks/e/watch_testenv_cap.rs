//! # watch.testenv_cap — contract
//!
//! watchd row `testenv-cap` (`spira/watchers:175`), target `testenv-cap watch --interval 15` (crate `testenv`, bin `testenv-cap` = `testenv/src/cap_main.rs`, `testenv/Cargo.toml:23`); pattern P1 (§3 of [[scheduler-consolidation-2026-11-03]]).
//!
//! Catalogue differs: children "—" is right for forks, but the unit is not a passive watcher — it **SIGKILLs** over-cap `testenv` runs and their descendants (`testenv/src/cap.rs:339-347`) and pushes a metric (`cap_main.rs:52`). The kill is a host effect with its own refusal.
//!
//! ## Tick
//!
//! - Cadence 15 s (`spira/watchers:175`; loop `cap_main.rs:87-99`). Extra wake worth adding: a `unit.ran`/start of a testenv run is not needed — the cap is age-based. Single-flight; at 15 s the observe must stay cheap (one `/proc` walk).
//!
//! ## Observe
//!
//! `watch.testenv_cap.observe` (host class) returns:
//!
//! ```text
//! Snapshot { procs: Vec<Proc { pid, start_ticks, argv, age_secs }> }   // testenv runs only
//! ```
//!
//! - From procfs: `cap::live()` (`testenv/src/cap.rs:320-337`) reads `/proc/<pid>/cmdline`, `/proc/<pid>/stat` field 22 and `/proc/uptime`; filtered to runs by `cap::is_run` (`:267-273`, basename `testenv`, not a subcommand). The kernel is the authority on which runs exist and how old they are; testenv's in-run cap (`cap::arm`, `:192`) is the primary and this task is its backstop (`cap_main.rs:1-4`).
//!
//! ## Decide
//!
//! - `cap::breaches(procs, suite_cap, gate_cap, load_wait, slot_wait, GRACE, None)` (`cap.rs:289-308`) — already pure. Gate tier when the argv carries `--deadline` (`:275-277`); suite tier gets `load_wait` on top; every run gets `slot_wait`; `GRACE = 60` (`:18`).
//! - No carried state: a killed pid is gone, so the next tick cannot re-find it. No `state` event at all.
//! - Fixtures to port: the `cap.rs` unit tests (`:356-447`): deadline run takes the gate cap; explicit deadline governs; only past cap+grace; suite load-wait allowance; slot-wait allowance; hold stops the clock; `--` branch named like a subcommand is still a run; a pid whose cmdline changed is not killed.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.testenv_cap.observe` | host | 5 s | (probe) | — |
//! | `watch.testenv_cap.kill` | host | 5 s | `testenv_cap/kill/<pid>/<start_ticks>` | a pid whose argv or start ticks no longer match what was judged (`cap::kill_verified`, `cap.rs:339-342`) — returns `refused: identity changed`, kills nothing |
//! | `inbox.append` (shared) | host | 10 s | `testenv_cap/<pid>/<start_ticks>` | — |
//! | `watch.testenv_cap.metric` | host (metrics push) | 5 s | same | — (`testenv::telemetry::push_over_cap`, `telemetry.rs:54`) |
//!
//! `inbox.append` and `metric` are emitted on the **outcome** of `kill` (`effect.succeeded` with `killed`), never alongside it, matching today's order (`cap_main.rs:50-53`): no line for a refused kill.
//!
//! Inbox line, exactly as today (`testenv/src/cap_main.rs:23-37`):
//!
//! ```text
//! <YYYY-MM-DDTHH:MM:SSZ> testenv over-cap killed: pid <pid> tier=<suite|gate> wall=<age>s cap=<cap>s cmd=<argv joined by space>
//! ```
//!
//! Note: unlike every script row, this line carries **no** `[watch:testenv-cap]` tag. Keep it byte-identical for parity, or add the tag and update `inbox-rules`/drop rules that may match the bare text (decision; unverified whether any rule does).
//!
//! Today the line goes to stdout → `run/watchd/testenv-cap.log` (`cap_main.rs:31`) and straight into `SPIRA_CONCIERGE_INBOX` (`:33-37`). `run/watchd/testenv-cap.log` is 0 bytes (last touched 2026-10-16): no kill since. Readers: none by name in the release or `run/concierge-notes`; watchd's generic walkers only; no cursor exists. Dropped.
//!
//! Health (`testenv-cap health`, `cap_main.rs:62-68`: the four cap keys parse) → S13 `scheduler status`, plus S10 refusing a bad key at load, which makes the parse check redundant.
//!
//! ## Silence
//!
//! No breach → probe only (not journaled), no `Emit`. At 5,760 ticks/day this is the task where T2 matters most.
//!
//! ## Replaces
//!
//! - `spira-watch-testenv-cap-prod.service`; row `spira/watchers:175`.
//! - Children: none forked; the in-process `kill(2)` calls move from the bin into the `kill` runner.
//! - Files retired: `run/watchd/testenv-cap.{log,restarts}`.
//! - Config pinned: `SPIRA_TESTENV_SUITE_CAP`, `SPIRA_TESTENV_GATE_CAP`, `SPIRA_TESTENV_LOAD_WAIT`, `SPIRA_TESTENV_SLOT_WAIT` (`cap_main.rs:15-18`), `SPIRA_CONCIERGE_INBOX` (`:45`).
//!
//! ## Reverse
//!
//! Release rollback restores unit and row. `Observe` mode shadows `kill` (journaled as `effect.shadowed` with pid, tier, wall) — the safe first activation for a task that kills.

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use crate::tasks::d::pattern;
use crate::watchers::{answered_spec, iso8601, INBOX_APPEND};

pub const NAME: &str = "watch.testenv_cap";
pub const STREAM: &str = "watch.testenv_cap";
pub const TICK: EventKindTag = "watch.testenv_cap.tick";
/// Never emitted: the task carries no state.
pub const STATE: EventKindTag = "watch.testenv_cap.state";
pub const OBSERVE: &str = "watch.testenv_cap.observe";
pub const KILL: &str = "watch.testenv_cap.kill";
pub const METRIC: &str = "watch.testenv_cap.metric";
pub const EFFECTS: &[&str] = &[OBSERVE, KILL, INBOX_APPEND, METRIC];
pub const KEYS: &[&str] = &["SPIRA_TESTENV_SUITE_CAP", "SPIRA_TESTENV_GATE_CAP", "SPIRA_TESTENV_LOAD_WAIT", "SPIRA_TESTENV_SLOT_WAIT", "SPIRA_CONCIERGE_INBOX"];
/// `--interval 15`.
pub const CADENCE_MS: u64 = 15_000;
const OBSERVE_DEADLINE_MS: u64 = 5_000;
/// `GRACE` (`testenv/src/cap.rs:18`).
pub const GRACE_S: u64 = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub suite_cap_s: u64,
    pub gate_cap_s: u64,
    pub load_wait_s: u64,
    pub slot_wait_s: u64,
    pub inbox: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config {
            suite_cap_s: cfg_parse("SPIRA_TESTENV_SUITE_CAP")?,
            gate_cap_s: cfg_parse("SPIRA_TESTENV_GATE_CAP")?,
            load_wait_s: cfg_parse("SPIRA_TESTENV_LOAD_WAIT")?,
            slot_wait_s: cfg_parse("SPIRA_TESTENV_SLOT_WAIT")?,
            inbox: cfg("SPIRA_CONCIERGE_INBOX")?,
        })
    }
}

/// One live `testenv` run, from procfs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proc {
    pub pid: u32,
    pub start_ticks: u64,
    pub argv: Vec<String>,
    pub age_secs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub procs: Vec<Proc>,
}

/// What `kill` is asked to verify and kill, and what the line and the metric say after it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kill {
    pub proc: Proc,
    pub tier: String,
    pub cap_s: u64,
}

/// The outcome `kill` answers when the pid was verified and signalled.
pub const KILLED: &str = "killed";
/// The outcome `kill` answers when the pid no longer is the run that was judged.
pub const REFUSED: &str = "refused: identity changed";
const ACT_DEADLINE_MS: u64 = 5_000;
const INBOX_DEADLINE_MS: u64 = 10_000;

fn to_cap(p: &Proc) -> testenv::cap::Proc {
    testenv::cap::Proc { pid: p.pid, start_ticks: p.start_ticks, argv: p.argv.clone(), age_secs: p.age_secs }
}

fn from_cap(p: &testenv::cap::Proc) -> Proc {
    Proc { pid: p.pid, start_ticks: p.start_ticks, argv: p.argv.clone(), age_secs: p.age_secs }
}

/// `cap::breaches` over the runs: one `kill` per breach, and nothing else.
pub fn decide(snap: &Snapshot, cfg: &Config) -> Vec<EffectSpec> {
    let procs: Vec<testenv::cap::Proc> = snap.procs.iter().map(to_cap).collect();
    testenv::cap::breaches(&procs, cfg.suite_cap_s, cfg.gate_cap_s, cfg.load_wait_s, cfg.slot_wait_s, GRACE_S, None)
        .into_iter()
        .map(|b| Kill { proc: from_cap(&b.proc), tier: b.tier.into(), cap_s: b.cap })
        .map(|k| EffectSpec::by_input(KILL, serde_json::to_string(&k).unwrap_or_default(), ACT_DEADLINE_MS))
        .collect()
}

/// `report`'s line (`testenv/src/cap_main.rs:23-37`): no `[watch:…]` tag, as today.
pub fn line(k: &Kill, now_ms: u64) -> String {
    format!("{} testenv over-cap killed: pid {} tier={} wall={}s cap={}s cmd={}", iso8601(now_ms / 1000), k.proc.pid, k.tier, k.proc.age_secs, k.cap_s, k.proc.argv.join(" "))
}

/// On a `kill` that answered `killed`: the inbox line and the metric; nothing for a refused kill.
pub fn killed(kill: &EffectSpec, outcome: &str, now_ms: u64, _cfg: &Config) -> Vec<EffectSpec> {
    if outcome != KILLED {
        return vec![];
    }
    let Ok(k) = serde_json::from_str::<Kill>(&kill.input) else { return vec![] };
    vec![EffectSpec::by_input(INBOX_APPEND, line(&k, now_ms), INBOX_DEADLINE_MS), EffectSpec::by_input(METRIC, k.tier, ACT_DEADLINE_MS)]
}

/// The runs procfs shows, filtered to `testenv` runs (`cap::live`, `cap::is_run`).
pub fn observe() -> Result<String, String> {
    let procs = testenv::cap::live().into_iter().filter(|p| testenv::cap::is_run(&p.argv)).map(|p| from_cap(&p)).collect();
    serde_json::to_string(&Snapshot { procs }).map_err(|e| e.to_string())
}

/// Kills the judged run only when its argv and start ticks still match (`cap::kill_verified`).
pub fn kill(input: &str) -> Result<String, String> {
    let k: Kill = serde_json::from_str(input).map_err(|e| format!("{KILL}: bad input: {e}"))?;
    Ok(if testenv::cap::kill_verified(&to_cap(&k.proc)) { KILLED } else { REFUSED }.into())
}

/// `spira_testenv_over_cap_kills{tier}` +1, as `cap_main.rs:52` pushed it.
pub fn metric(tier: &str) -> Result<String, String> {
    testenv::telemetry::push_over_cap(&testenv::telemetry::sink(), tier);
    Ok(format!("over-cap kill pushed for tier {tier}"))
}

pub struct WatchTestenvCap {
    pub config: Result<Config, String>,
}

impl WatchTestenvCap {
    pub fn new(config: Config) -> Self {
        WatchTestenvCap { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchTestenvCap { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.testenv_cap/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchTestenvCap {
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
        if let (Some(kill), Ok(config)) = (answered_spec(view, ev).filter(|s| s.name == KILL && ev.kind.tag == EFFECT_SUCCEEDED), &self.config) {
            return killed(&kill, &ev.kind.body, ev.at, config).into_iter().map(Command::Effect).collect();
        }
        pattern::on_answer(ctx, view, ev, OBSERVE, STATE, &self.config, |_: &(), snap: &Snapshot, _, config: &Config| ((), decide(snap, config)))
    }
}
