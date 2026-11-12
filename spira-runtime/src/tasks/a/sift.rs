//! # sift — contract
//!
//! The pre-round screen, moving from `round-controller sifts` into the scheduler. Pattern P1, family
//! A of [[scheduler-consolidation-2026-11-03]]. Paths are relative to `spira-releases/current`
//! (0ef4f0f93); `rs` = `round-controller/src/sifts.rs`, `sl` = `sift/src/lib.rs`, `sg` =
//! `sift/src/git.rs`.
//!
//! ## Tick
//!
//! - Interval 60 s (`spira-sift-prod.timer`: `OnBootSec=150s`, `OnUnitActiveSec=60s`; the service
//!   carries `CPUQuota=50%`, `TimeoutStartSec=900`).
//! - Also woken by a bead entering `SUBMITTED` or `CERTIFIED`, or a tip moving (store `watch`
//!   events via S7), so a submission is screened without waiting a minute (proposed).
//! - Single-flight per repository. The binary has no lock of its own; overlap is prevented only by
//!   systemd's oneshot (unverified that two passes were never concurrent).
//! - Scope: every registry repo whose land is `queue` or `queue.local` (`rs:64-68`). Code and
//!   catalogue differ: `queue.forge` repos are **not** sifted by this unit.
//!
//! ## Observe
//!
//! Two steps, because the screen judges against a refreshed base:
//!
//! 1. `sift.fetch_base` (git, 60 s): `git fetch -q <remote>` for a non-local repo (`rs:557-574`).
//!    A failed fetch refuses the repo's pass: "an unrefreshed base is the wrong base".
//! 2. `sift.observe` (probe, git class, 120 s) per repo, returning:
//!
//! | Field | Today | From (after the move) |
//! |---|---|---|
//! | `rows` — SUBMITTED + CERTIFIED with `tip`, `holds`, `ejected_red_tip`, `sifted_tip` | `spira-lc list --state SUBMITTED,CERTIFIED` fork (`rs:252-258`) | LcClient `list --state SUBMITTED,CERTIFIED` |
//! | `outside` rows of stacked-on beads not in the pool | `spira-lc list --ids` (`rs:260-266`) | LcClient `list --ids` |
//! | `stack[id]`, `prereq_state[id]` | `spira-lc show <id>` per member and per prerequisite (`rs:394-438`) | LcClient `show` (batched) |
//! | `items[id]` — description + notes (target verdict) | `spira-lc plan items --full --ids` (`rs:481-513`) | LcClient `plan items --full` |
//! | `open_round` members `id:tip` | `queue batch show --repo` fork (`rs:517-553`) | the S7b round/batch projection |
//! | `branch_tips` | `git for-each-ref refs/heads/spira/*` (`rs:321-343`) | git (edge, unchanged) |
//! | per candidate: `base`, `merge` (clean/conflict files), `patch_id`, `stacked_on`, `identity_diff` | `git rev-parse`, `merge-tree --write-tree`, `commit-tree`, `diff`, `patch-id --stable`, `log` (`sg:38-97`) | git (edge), computed in the probe so `decide` does no IO |
//! | `identity_pages` | brain wiki pages under `SPIRA_WIKI/SPIRA_IDENTITY_PROJECTS` (`sg:95`, `sift/src/identity.rs`) | file read in the probe (unchanged) |
//!
//! `git merge-tree --write-tree` and `commit-tree` write loose objects (`sg:42-55`): harmless, but the
//! observe probe is not strictly read-only. The per-(id, tip, base) verdict cache in
//! `run/sift/verdicts/` (`sift/src/store.rs:18`) keeps the git work to new tips; it moves into the
//! probe as a memory cache.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (carry', acts)` is `sift::screen` (`sl:133-262`) run over a snapshot-backed
//! `Probe` and a recording `Acts`. It is already pure behind its three traits; the port is two
//! adapters, not a rewrite.
//!
//! - **Pool** (`rs:292-305`, `353-390`): unheld pool rows at their live branch tip, with a content
//!   row, not ejected at the tip, not stacked (transitively) on a barred bead (`rs:462-478`).
//! - **Verdict per candidate**: duplicate patch-id → supersede (keeper prefers an open-round member,
//!   `sl:158-170`); conflict, a stacked-on bead in REWORK, a target gap, an identity gap → findings
//!   (`sl:85-100`, `203-217`); none → pass. An identity check that could not run holds the bead
//!   out (`sl:211-215`).
//! - **Carried state**: per bead the `Sifted` machine (today `run/sift/events/<id>.json` and
//!   `send-backs/<id>`, `sift/src/store.rs:18-28`), states `screened`, `sent_back(n)`, `capped`,
//!   `mending` (`sift/src/machine.rs:18-31`); cap `SEND_BACK_CAP = 3` (`sl:16`). A conflict is never
//!   counted (`sl:229-230`, `249`).
//! - **Config pinned** (S10): `SPIRA_RUN`, `SPIRA_LC_TIMEOUT`, `SPIRA_WIKI`, `SPIRA_IDENTITY_PROJECTS`,
//!   `SPIRA_CONCIERGE_INBOX` (`rs:33-49`); the repo registry (`rs:56`).
//! - **Fixtures**: `sift/src/tests.rs` (29 tests), `sift/src/identity.rs` (6), `rs:677-738` (4).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sift.fetch_base` | git | 60 s | `<repo>:<tick at>` | a `queue.local` repo (no remote, `rs:558`) |
//! | `sift.note` | store | 10 s | `<id>:<tip>:note` | nothing new; today `bash bead.sh amend --note` (`rs:664-671`), after the move LcClient `content` amend. Precedes `sift.send_back`; if it fails the bead is held, not sent back (`sl:242-246`) |
//! | `sift.send_back` | store | 10 s | `<id>:<tip>:GateRed` | a bead no longer SUBMITTED/CERTIFIED or whose tip moved (`rs:626-639`); a stale `--version` (store CAS) |
//! | `sift.record_pass` | store | 10 s | `<id>:<tip>:Sifted` | skipped when `sifted_tip == tip` (`rs:189-193`); a bead out of the pool states records none (`rs:643-652`); a moved tip (store) |
//! | `sift.supersede` | store | 10 s | `<id>:superseded-by:<keeper>` | the store's close refusals; today `spira-lc close --superseded-by` via `spira-store/src/lifecycle_row.rs:293-318` |
//! | `sift.record` | — (carry) | — | `<id>:<event>:<tip>` | a move the `Sifted` machine refuses (`sift/src/machine.rs:89-100`) — an `Emit`, not an effect |
//! | `inbox.append` | host | 10 s | line text | — the `SIFT superseded / sent / REPEAT` lines, today appended straight to the inbox file (`rs:195-212`) |
//!
//! Store writes go through LcClient `event bead <id> --expect <state> --version <v> --actor sift`
//! (`rs:603-622`), which is the refusal that matters: a moved bead fails the CAS.
//!
//! ## Silence
//!
//! An idle tick (empty pool for every repo, `rs:138-140`) emits the probe and nothing else; the
//! fetch is skipped when the pool is empty (move the fetch after the pool read, as `rs:141` already
//! orders it). A pool whose verdicts are all cached passes already recorded emits nothing.
//!
//! ## Replaces
//!
//! `spira-sift-prod.{service,timer}`; children per pass: `timeout spira-lc` list/show/plan/event/close
//! (`rs:238-244`, ~2 + 2 per member), `timeout queue batch show` (`rs:517-523`), `bash bead.sh amend`
//! (`rs:666`), git `for-each-ref`/`fetch`/`rev-parse`/`merge-tree`/`commit-tree`/`diff`/`patch-id`/`log`.
//! The catalogue omits `bead.sh` and the direct inbox write. Retires `run/sift/{events,send-backs}`
//! into carried state.
//!
//! ## The port
//!
//! `decide` is `sift::screen` over three adapters on the snapshot: a `Probe` answering from the
//! probe's git facts, a `Store` on the carried `Sifted` machines (the per-tip verdict cache is
//! gone: the probe computes each candidate's facts, and decide judges them every tick), and an
//! `Acts` recording the writes. The pool is `round_controller::sifts::select_pool`, the same
//! selection the binary's pass makes. The probe is `round_controller::sifts::observe_repo` per
//! `queue`/`queue.local` repository: the store over `LcClient`, the open round from the
//! projection, git for the branch tips, the base fetch and each candidate. A send-back waits in
//! the carry until its evidence note has landed, so the note precedes the GateRed and a note that
//! fails holds the bead instead.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. `Observe` mode (S4) journals the four store writes as
//! `effect.shadowed`; the screen still computes, so parity can be read off the shadowed specs.
//!

use std::cell::RefCell;
use std::collections::BTreeMap;

use round_controller::sifts;
use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_store::lc_call::LcClient;

use super::kit::{self, Act, Cadence, Shape};
use crate::Effects;

pub const NAME: &str = "sift";
pub const STREAM: &str = "task.sift";
pub const TICK: EventKindTag = "sift.tick";
pub const STATE: EventKindTag = "sift.state";
/// A move of one bead's `Sifted` machine: an Emit, not an effect.
pub const RECORD: EventKindTag = "sift.record";
pub const OBSERVE: &str = "sift.observe";
pub const FETCH_BASE: &str = "sift.fetch_base";
pub const NOTE: &str = "sift.note";
pub const SEND_BACK: &str = "sift.send_back";
pub const RECORD_PASS: &str = "sift.record_pass";
pub const SUPERSEDE: &str = "sift.supersede";
pub const INBOX_APPEND: &str = crate::watchers::INBOX_APPEND;

pub const CADENCE_MS: u64 = 60_000;
pub const OBSERVE_DEADLINE_MS: u64 = 120_000;
/// `SEND_BACK_CAP` (`sift/src/lib.rs:16`).
pub const SEND_BACK_CAP: u8 = sift::SEND_BACK_CAP as u8;

pub const ACTS: &[(&str, u64)] = &[(FETCH_BASE, 60_000), (NOTE, 10_000), (SEND_BACK, 10_000), (RECORD_PASS, 10_000), (SUPERSEDE, 10_000), (INBOX_APPEND, 10_000)];
/// `work.submitted` goes to both batchers when a pass is recorded: the bead is cuttable now.
pub const EMITS: &[EventKindTag] = &[RECORD, kit::WORK_SUBMITTED];
pub const EFFECTS: &[&str] = &[OBSERVE, FETCH_BASE, NOTE, SEND_BACK, RECORD_PASS, SUPERSEDE, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_LC_TIMEOUT", "SPIRA_WIKI", "SPIRA_IDENTITY_PROJECTS", "SPIRA_CONCIERGE_INBOX", "SPIRA_REPO_MAP"];
pub const UNREGISTERED: &[&str] = &[];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// One SUBMITTED or CERTIFIED row.
pub use round_controller::sifts::PoolRow;
/// The git facts of one candidate, computed in the probe so decide does no IO.
pub use round_controller::sifts::CandidateFacts as Candidate;
/// One repository's snapshot.
pub use round_controller::sifts::RepoFacts as Repo;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub repos: BTreeMap<String, Repo>,
}

/// One bead's `Sifted` machine (`sift/src/machine.rs:18-31`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sifted {
    #[default]
    Unrecorded,
    Screened,
    SentBack(u8),
    Capped,
    Mending,
}

/// The legal moves: screened; sent back one at a time up to the cap; capped at the cap; any
/// state to mending.
pub static MOVES: &[(Sifted, Sifted)] = &[
    (Sifted::Unrecorded, Sifted::Screened),
    (Sifted::Screened, Sifted::SentBack(1)),
    (Sifted::SentBack(1), Sifted::SentBack(2)),
    (Sifted::SentBack(2), Sifted::SentBack(3)),
    (Sifted::SentBack(3), Sifted::Capped),
    (Sifted::Unrecorded, Sifted::Mending),
    (Sifted::Screened, Sifted::Mending),
    (Sifted::SentBack(1), Sifted::Mending),
    (Sifted::SentBack(2), Sifted::Mending),
    (Sifted::SentBack(3), Sifted::Mending),
    (Sifted::Capped, Sifted::Mending),
];

/// Carried between ticks: each bead's `Sifted` state and the tip it was judged at, and each
/// send-back waiting on its evidence note (by the note's key).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub beads: BTreeMap<String, (Sifted, String)>,
    pub pending: BTreeMap<String, Act>,
}

/// `sift::machine::Sifted` standing at `s`: the events that lead there.
fn machine_at(id: &str, s: Sifted) -> sift::machine::Sifted {
    use sift::machine::SiftEvent;
    let mut m = sift::machine::Sifted::new(id);
    let tip = String::new();
    let mut push = |e: SiftEvent| m.events.push(sift::machine::Recorded { event: e, at: 0 });
    match s {
        Sifted::Unrecorded => {}
        Sifted::Screened => push(SiftEvent::Screened { tip }),
        Sifted::SentBack(n) => (1..=u32::from(n)).for_each(|n| push(SiftEvent::SentBack { n, tip: tip.clone(), reason: String::new() })),
        Sifted::Capped => {
            (1..=sift::SEND_BACK_CAP).for_each(|n| push(SiftEvent::SentBack { n, tip: tip.clone(), reason: String::new() }));
            push(SiftEvent::Capped { tip });
        }
        Sifted::Mending => push(SiftEvent::Mending { tip, bead: String::new() }),
    }
    m
}

/// Move bead `id` from `from` to `to`, or refuse naming the state it is in: the `sift` crate's
/// own machine (`sift::machine::Sifted::refusal`) judges the move.
pub fn record(id: &str, from: Sifted, to: Sifted) -> Result<Sifted, String> {
    use sift::machine::SiftEvent;
    let tip = String::new();
    let event = match to {
        Sifted::Unrecorded => return Err(format!("{id} is {from:?}; nothing moves a bead back to unrecorded")),
        Sifted::Screened => SiftEvent::Screened { tip },
        Sifted::SentBack(n) => SiftEvent::SentBack { n: u32::from(n), tip, reason: String::new() },
        Sifted::Capped => SiftEvent::Capped { tip },
        Sifted::Mending => SiftEvent::Mending { tip, bead: String::new() },
    };
    match machine_at(id, from).refusal(&event) {
        Some(why) => Err(why),
        None => Ok(to),
    }
}

/// The screen's view of one repository's facts.
struct SnapProbe<'a> {
    repo: &'a Repo,
    /// tip → bead id, over the pool and the open round.
    by_tip: BTreeMap<String, String>,
}

impl SnapProbe<'_> {
    fn facts(&self, tip: &str) -> Result<&Candidate, String> {
        let id = self.by_tip.get(tip).ok_or_else(|| format!("no facts for tip {tip}"))?;
        let f = self.repo.candidates.get(id).ok_or_else(|| format!("no facts for {id}"))?;
        match &f.error {
            Some(e) => Err(e.clone()),
            None => Ok(f),
        }
    }
}

impl sift::Probe for SnapProbe<'_> {
    fn base(&self) -> Result<String, String> {
        self.repo.candidates.values().map(|c| c.base.clone()).find(|b| !b.is_empty()).ok_or_else(|| "no base was read".to_string())
    }

    fn merge(&self, tip: &str) -> Result<sift::Merge, String> {
        let f = self.facts(tip)?;
        Ok(if f.conflicts.is_empty() { sift::Merge::Clean(f.base.clone()) } else { sift::Merge::Conflict(f.conflicts.clone()) })
    }

    fn patch_id(&self, tip: &str) -> Result<String, String> {
        self.facts(tip)?.patch_id.clone().ok_or_else(|| format!("no patch-id for tip {tip}"))
    }

    fn stacked_on(&self, _id: &str, tip: &str) -> Result<Vec<String>, String> {
        Ok(self.facts(tip)?.stacked_on.clone())
    }

    fn state(&self, id: &str) -> Result<String, String> {
        let row = || self.repo.rows.iter().chain(&self.repo.outside).find(|r| r.id == id).map(|r| r.state.clone());
        self.repo.prereq_state.get(id).cloned().or_else(row).ok_or_else(|| format!("no state read for {id}"))
    }

    fn target_gap(&self, id: &str) -> Result<Option<String>, String> {
        self.repo.items.get(id).map(|text| spira_config::target::gap(text)).ok_or_else(|| format!("no item row for {id}"))
    }

    fn identity_gap(&self, tip: &str) -> Result<Option<String>, String> {
        if self.repo.identity_pages.is_none() {
            return Err(format!("{}the identity pages could not be read", sift::INFRA_FAULT));
        }
        let f = self.facts(tip)?;
        match &f.identity_error {
            Some(e) => Err(e.clone()),
            None => Ok(f.identity_gap.clone()),
        }
    }
}

/// The screen's store: the carried machines. The verdict cache answers nothing: every verdict
/// is judged afresh from the probe's facts.
struct CarryStore {
    carry: RefCell<Carry>,
}

impl CarryStore {
    fn state(&self, id: &str) -> Sifted {
        self.carry.borrow().beads.get(id).map_or(Sifted::Unrecorded, |b| b.0)
    }

    fn set(&self, id: &str, to: Sifted, tip: &str) -> Result<(), String> {
        let from = self.state(id);
        if self.carry.borrow().beads.get(id) == Some(&(to, tip.to_string())) {
            return Ok(());
        }
        record(id, from, to)?;
        self.carry.borrow_mut().beads.insert(id.to_string(), (to, tip.to_string()));
        Ok(())
    }
}

impl sift::Store for CarryStore {
    fn get(&self, _id: &str, _tip: &str) -> Option<sift::Verdict> {
        None
    }

    fn put(&self, _v: &sift::Verdict) -> Result<(), String> {
        Ok(())
    }

    fn send_backs(&self, id: &str) -> u32 {
        match self.state(id) {
            Sifted::SentBack(n) => u32::from(n),
            Sifted::Capped => sift::SEND_BACK_CAP,
            _ => 0,
        }
    }

    fn record_screened(&self, id: &str, tip: &str) -> Result<(), String> {
        self.set(id, Sifted::Screened, tip)
    }

    fn record_send_back(&self, id: &str, tip: &str, _reason: &str) -> Result<u32, String> {
        let n = self.send_backs(id) + 1;
        self.set(id, Sifted::SentBack(n.min(u32::from(u8::MAX)) as u8), tip)?;
        Ok(n)
    }

    fn first_cap_report(&self, id: &str, tip: &str) -> bool {
        self.carry.borrow().beads.get(id) != Some(&(Sifted::Capped, tip.to_string())) && self.set(id, Sifted::Capped, tip).is_ok()
    }
}

/// The screen's writes, recorded as acts in the order it made them.
struct Recorded<'a> {
    acts: Vec<Act>,
    tips: BTreeMap<String, String>,
    sifted: BTreeMap<String, String>,
    stamp: &'a str,
}

/// What a store write's runner needs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Write {
    pub id: String,
    pub tip: String,
    pub text: String,
    pub keeper: String,
}

impl Recorded<'_> {
    fn push(&mut self, effect: &str, key: String, w: Write) {
        self.acts.push(Act::new(effect, key).with_input(serde_json::to_string(&w).unwrap_or_default()));
    }
}

impl sift::Acts for Recorded<'_> {
    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        let tip = self.tips.get(id).cloned().unwrap_or_default();
        self.push(NOTE, format!("{id}:{tip}:note"), Write { id: id.into(), tip, text: text.into(), ..Write::default() });
        Ok(())
    }

    fn gate_red(&mut self, id: &str, tip: &str, reason: &str) -> Result<(), String> {
        self.push(SEND_BACK, format!("{id}:{tip}:GateRed"), Write { id: id.into(), tip: tip.into(), text: reason.into(), ..Write::default() });
        Ok(())
    }

    fn supersede(&mut self, id: &str, keeper: &str) -> Result<(), String> {
        self.push(SUPERSEDE, format!("{id}:superseded-by:{keeper}"), Write { id: id.into(), keeper: keeper.into(), ..Write::default() });
        Ok(())
    }

    fn pass(&mut self, id: &str, tip: &str) -> Result<(), String> {
        if self.sifted.get(id).map(String::as_str) != Some(tip) {
            self.push(RECORD_PASS, format!("{id}:{tip}:Sifted"), Write { id: id.into(), tip: tip.into(), ..Write::default() });
        }
        Ok(())
    }

    fn tell(&mut self, msg: &str) {
        let line = format!("{} [watch:sift] {msg}", self.stamp);
        self.acts.push(Act::new(INBOX_APPEND, &line).with_input(line));
    }
}

/// `%Y%m%dT%H%M%SZ` of `ms`, the stamp the inbox's other writers use.
fn stamp(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, m, d) = crate::watchers::civil((secs / 86_400) as i64);
    let t = secs % 86_400;
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", t / 3600, t % 3600 / 60, t % 60)
}

/// `sift::screen` over the snapshot: supersede, send back, record a pass, or hold out. A repo
/// whose pool is empty does nothing; one whose base was not fetched asks for the fetch and
/// screens nothing ("an unrefreshed base is the wrong base").
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let store = CarryStore { carry: RefCell::new(carry.clone()) };
    let mut acts = vec![];
    for (name, repo) in &snap.repos {
        let stamp = stamp(repo.read_at_ms);
        let pool = repo.pool();
        if pool.is_empty() {
            continue;
        }
        if !repo.fetched {
            acts.push(Act::new(FETCH_BASE, format!("{name}:{}", repo.read_at_ms)).with_input(name.clone()));
            continue;
        }
        let open = repo.open_members();
        let by_tip = pool.iter().chain(&open).map(|c| (c.tip.clone(), c.id.clone())).collect();
        let probe = SnapProbe { repo, by_tip };
        let sifted = repo.rows.iter().filter_map(|r| r.sifted_tip.clone().map(|t| (r.id.clone(), t))).collect();
        let mut rec = Recorded { acts: vec![], tips: pool.iter().map(|c| (c.id.clone(), c.tip.clone())).collect(), sifted, stamp: &stamp };
        sift::screen(&probe, &store, &mut rec, &pool, &open);
        acts.extend(rec.acts);
    }
    (store.carry.into_inner(), acts)
}

/// The observe probe's input: the carry (which T3 reads back) and the pinned config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ObserveIn {
    #[serde(flatten)]
    pub carry: Carry,
    #[serde(default)]
    pub pinned: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sift {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl Sift {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        Sift { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("sift/boot", STREAM, EventKind::new(TICK, ""))
    }
}

/// A send-back held until its note landed: the note's key is the send-back's, `:note` for `:GateRed`.
fn note_key(send_back_key: &str) -> String {
    format!("{}:note", send_back_key.strip_suffix(":GateRed").unwrap_or(send_back_key))
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "beads", drop: &[] }, runtime::rows::Part::Map { name: "pending", drop: &[] }];

impl Handler for Sift {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            let input = ObserveIn { carry: carry.clone(), pinned: self.pinned.clone() };
            return kit::on_tick(&SHAPE, serde_json::to_string(&input).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        // A recorded pass made a submission cuttable: the batchers are woken now, not at their tick.
        if let Some(spec) = crate::watchers::answered_spec(view, ev).filter(|s| s.name == RECORD_PASS && ev.kind.tag == EFFECT_SUCCEEDED) {
            let Ok(w) = serde_json::from_str::<Write>(&spec.input) else { return vec![] };
            let handoff = Act::new(kit::WORK_SUBMITTED, format!("{}:{}", w.id, w.tip)).with_input(serde_json::json!({ "id": w.id, "tip": w.tip }).to_string());
            return kit::commands(&SHAPE, ACTS, EMITS, &carry, &carry, vec![handoff]);
        }
        if let Some(spec) = crate::watchers::answered_spec(view, ev).filter(|s| s.name == NOTE) {
            let Ok(w) = serde_json::from_str::<Write>(&spec.input) else { return vec![] };
            let mut next = carry.clone();
            let held = next.pending.remove(&format!("{}:{}:note", w.id, w.tip));
            let acts = held.filter(|_| ev.kind.tag == EFFECT_SUCCEEDED).into_iter().collect();
            return kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts);
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (mut next, acts) = decide(&carry, &snap);
        let (sends, now): (Vec<Act>, Vec<Act>) = acts.into_iter().partition(|a| a.effect == SEND_BACK);
        for a in sends {
            next.pending.insert(note_key(&a.key), a);
        }
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, now)
    }
}

// ---------------------------------------------------------------------------------------------
// The runners.

fn client(e: &Effects, deadline_ms: u64) -> LcClient {
    LcClient::socket(&e.world.lc_socket).within(std::time::Duration::from_millis(deadline_ms))
}

fn registry(pinned: &BTreeMap<String, String>) -> Result<spira_config::repos::Registry, String> {
    let home = spira_config::resolve::locate_home_for_process()?;
    let env: BTreeMap<String, String> = std::env::vars().chain(pinned.clone()).collect();
    Ok(spira_config::repos::Registry::from_env(env, &home))
}

/// `sift.observe`: every screened repository's facts. A repository that cannot be read is left
/// out (its pass waits); when none can be, the probe fails.
pub fn observe(e: &Effects, input: &str) -> Result<String, String> {
    let req: ObserveIn = serde_json::from_str(input).unwrap_or_default();
    let secs = req.pinned.get("SPIRA_LC_TIMEOUT").and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(5);
    let lc = client(e, secs * 1000);
    let views = store_view::Projection::hot(lc.clone())?;
    let reg = registry(&req.pinned)?;
    let run = req.pinned.get("SPIRA_RUN").map(std::path::PathBuf::from).unwrap_or_else(|| e.world.run_dir.clone());
    let pin = |k: &str| req.pinned.get(k).cloned().unwrap_or_default();
    let (wiki, projects) = (pin("SPIRA_WIKI"), pin("SPIRA_IDENTITY_PROJECTS"));
    let reads = sifts::Reads { lc: &lc, views: &views, reg: &reg, wiki: &wiki, identity_projects: &projects, work: run.join("sift") };
    let mut snap = Snapshot::default();
    let mut failed = vec![];
    let repos = sifts::screened_repos(&reg);
    for (name, local) in &repos {
        match sifts::observe_repo(&reads, name, *local) {
            Ok(mut f) => {
                f.read_at_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
                snap.repos.insert(name.clone(), f);
            }
            Err(err) => failed.push(format!("{name}: {err}")),
        }
    }
    if !repos.is_empty() && snap.repos.is_empty() {
        return Err(format!("no repository could be read: {}", failed.join("; ")));
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

fn write_of(input: &str, what: &str) -> Result<Write, String> {
    serde_json::from_str(input).map_err(|e| format!("{what}: {e}"))
}

/// `sift.fetch_base`: the repository's base remote, fetched so the next probe judges against it.
pub fn fetch_base(input: &str) -> Result<String, String> {
    let reg = registry(&BTreeMap::new())?;
    let local = sifts::screened_repos(&reg).into_iter().find(|(n, _)| n == input).map(|(_, l)| l).ok_or_else(|| format!("{FETCH_BASE}: {input} is not a screened repository"))?;
    if local {
        return Err(format!("{FETCH_BASE}: {input} lands on a local ref and has no remote"));
    }
    sifts::fetch_repo_base(&reg, input, local).map(|()| format!("fetched {input}"))
}

pub fn note(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, NOTE)?;
    sifts::lc_note(&client(e, spec.deadline_ms), &w.id, &w.text).map(|()| format!("noted {}", w.id))
}

pub fn send_back(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, SEND_BACK)?;
    sifts::lc_gate_red(&client(e, spec.deadline_ms), &w.id, &w.tip, &w.text).map(|()| format!("sent {} back", w.id))
}

pub fn record_pass(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, RECORD_PASS)?;
    let mut said = Vec::new();
    sifts::lc_sifted(&client(e, spec.deadline_ms), &w.id, &w.tip, &mut said)?;
    Ok(String::from_utf8_lossy(&said).trim().to_string()).map(|s| if s.is_empty() { format!("sifted {} at {}", w.id, w.tip) } else { s })
}

pub fn supersede(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, SUPERSEDE)?;
    sifts::lc_supersede(&client(e, spec.deadline_ms), &w.id, &w.keeper).map(|()| format!("{} superseded by {}", w.id, w.keeper))
}
