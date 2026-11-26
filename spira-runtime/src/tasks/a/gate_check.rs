//! `gate.check` — pattern P1, every 2 minutes: resolves `gh:run` gates whose CI escalated. It makes
//! no `gh` call; CI runs are not read, ingested or filed from here.

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "gate.check";
pub const STREAM: &str = "task.gate.check";
pub const TICK: EventKindTag = "gate.check.tick";
pub const STATE: EventKindTag = "gate.check.state";
pub const OBSERVE: &str = "gate.check.observe";
pub const RESOLVE: &str = "gate.check.resolve";
pub const CI_FAILED: &str = "gate.check.ci_failed";

pub const CADENCE_MS: u64 = 120_000;
/// The contract gives the probe no deadline of its own; its slowest read is the 5 s LcClient `list`.
pub const OBSERVE_DEADLINE_MS: u64 = 30_000;

pub const ACTS: &[(&str, u64)] = &[(RESOLVE, 10_000), (CI_FAILED, 5_000)];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, RESOLVE, CI_FAILED];
pub const KEYS: &[&str] = &[];
pub const UNREGISTERED: &[&str] = &[];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// `bd gate check --type=gh:run`, stdout and stderr merged.
    pub gate_check_out: String,
    /// The bead each ESCALATE line's gate blocks (`bd show <gate>`), `None` for none.
    pub gate_blocked: BTreeMap<String, Option<String>>,
}

/// Carried between ticks; the `ingested` rows are kept as written before CI ingest was removed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub ingested: BTreeSet<String>,
}

/// `ci.failed`'s input: the event `spira_event ci.failed <blocked> <title> <escalate line>`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiFailed {
    pub blocked: String,
    pub gate: String,
    pub title: String,
    pub detail: String,
}

/// The walk of `gate-check/src/main.rs:63-190` over the snapshot, with the pure core of
/// `gate_check::engine`.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    use gate_check::engine;
    let mut acts = vec![];
    // Each ESCALATE line's gate, when it blocks a bead.
    for esc in engine::escalate_lines(&snap.gate_check_out) {
        // The id as `gate_id_in` reads it, else a gate the snapshot knows named as a word in it.
        let named = || snap.gate_blocked.keys().find(|g| esc.split(|c: char| c.is_whitespace() || c == ':').any(|w| w == g.as_str())).cloned();
        let Some(gate) = engine::gate_id_in(esc).filter(|g| snap.gate_blocked.contains_key(g)).or_else(named) else { continue };
        let Some(Some(blocked)) = snap.gate_blocked.get(&gate) else { continue };
        acts.push(Act::new(RESOLVE, gate.clone()).with_input(gate.clone()));
        let event = CiFailed { blocked: blocked.clone(), gate: gate.clone(), title: format!("CI red — {blocked} returned to queue"), detail: esc.to_string() };
        acts.push(Act::new(CI_FAILED, format!("ci.failed/{blocked}/{gate}")).with_input(serde_json::to_string(&event).unwrap_or_default()));
    }
    (carry.clone(), acts)
}

// ---------------------------------------------------------------------------------------------
// Runners. The store is reached over `LcClient` (its content door for gates, beads and notes);
// GitHub through `gh`, the one allowed child, on the `forge` class; the writes the store has no
// verb for yet run as transient units through the Units port, never as this process's children.

/// The store's 5 s door (S7a).
pub fn store(e: &crate::Effects) -> spira_store::lc_call::LcClient {
    spira_store::lc_call::LcClient::socket(&e.world.lc_socket).within(std::time::Duration::from_secs(5))
}

/// One `git -C <repo>` call through the process port: stdout when it exited 0.
pub fn git(repo: &std::path::Path, args: &[&str], deadline_secs: u64) -> Option<String> {
    git_out(repo, args, deadline_secs).ok().filter(|(code, _, _)| *code == Some(0)).map(|(_, out, _)| out)
}

/// One `git -C <repo>` call: (exit code, stdout, stderr), or why it could not run.
pub fn git_out(repo: &std::path::Path, args: &[&str], deadline_secs: u64) -> Result<(Option<i32>, String, String), String> {
    use runtime::ports::Proc;
    let mut all = vec!["-C".to_string(), repo.display().to_string()];
    all.extend(args.iter().map(|a| a.to_string()));
    let out = runtime::ports::SysProc.run(&runtime::ports::ProcSpec { prog: "git".into(), args: all, stdin: vec![], deadline: std::time::Duration::from_secs(deadline_secs) })?;
    Ok((out.code, String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// A leg the store has no in-process verb for yet: the tool the old unit ran, started as a
/// transient unit named for `(task, key)` so a re-issued effect names the same unit.
pub fn unit_leg(e: &crate::Effects, task: &str, key: &str, argv: Vec<String>, cwd: Option<std::path::PathBuf>) -> Result<String, String> {
    let units = e.units.as_deref().ok_or("no unit manager configured")?;
    let spec = runtime::units::UnitSpec { task: task.into(), key: key.into(), argv, env: vec![], cwd };
    let h = units.start(&spec).map_err(|e| e.0)?;
    serde_json::to_string(&h).map_err(|e| e.to_string())
}

/// `bd gate check` has no store verb, so no ESCALATE line is observed until one exists.
pub fn observe() -> Result<String, String> {
    serde_json::to_string(&Snapshot::default()).map_err(|e| e.to_string())
}

/// `ci.failed`: `spira_event`, now `bead::event::emit` in this process, at bead's own default
/// cooldown of an hour per (kind, target).
pub fn ci_failed(e: &crate::Effects, input: &str) -> Result<String, String> {
    let ev: CiFailed = serde_json::from_str(input).map_err(|e| format!("{CI_FAILED}: bad input: {e}"))?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    bead::event::emit(&e.world.run_dir, 3_600, now, "ci.failed", &ev.blocked, &ev.title, &ev.detail).map_err(|_| format!("{CI_FAILED}: the event for {} could not be written", ev.blocked))?;
    Ok(format!("ci.failed {}", ev.blocked))
}

fn resolve_argv(gate: &str) -> Vec<String> {
    ["spira-lc", "content", "gate", "resolve", gate].map(String::from).to_vec()
}

/// `resolve`: `spira-lc content gate resolve <gate>`, as a unit leg.
pub fn resolve(e: &crate::Effects, key: &str, input: &str) -> Result<String, String> {
    unit_leg(e, NAME, key, resolve_argv(input), None)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateCheck {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl GateCheck {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        GateCheck { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("gate.check/boot", STREAM, EventKind::new(TICK, ""))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("ingested")];

impl Handler for GateCheck {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return kit::on_tick(&SHAPE, serde_json::to_string(&carry).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}

#[cfg(test)]
mod no_gh_tests {
    use super::*;

    #[test_id(tg0nd2q)]
    #[test]
    fn a_tick_over_a_snapshot_full_of_ci_facts_emits_nothing_that_runs_gh() {
        let snap: Snapshot = serde_json::from_value(serde_json::json!({
            "gate_list": [], "gate_check_out": "", "gate_blocked": {}, "beads_by_title": {}, "pr_repos": [],
            "lc_open": [],
            "flaky_gh_repo": "org/repo",
            "ci": {
                "recent_runs": [{"id": "7", "repo": "org/repo", "branch": "main", "conclusion": "failure", "head_sha": "abc"}],
                "failed_main_runs": [{"id": "7", "repo": "org/repo", "branch": "main", "conclusion": "failure", "head_sha": "abc"}],
                "last_green_sha": null, "fail_lines": {},
                "annotations_by_job": {"7/1": ["[{\"message\":\"suite red: lint.sh\"}]"]}
            }
        }))
        .unwrap();
        let (carry, acts) = decide(&Carry { ingested: ["1".to_string()].into() }, &snap);
        assert!(acts.is_empty(), "{acts:?}");
        assert_eq!(carry.ingested.len(), 1);
        for gone in ["gate.check.file_bead", "gate.check.raise_priority", "gate.check.note", "gate.check.tsd_ingest"] {
            assert!(!EFFECTS.contains(&gone), "{gone} survives");
        }
    }
}

#[cfg(test)]
mod leg_argv_tests {
    #[test_id(tlf5wrf)]
    #[test]
    fn the_gate_legs_go_to_the_content_door_not_bdq() {
        assert_eq!(super::super::resolve_argv("sp-g").join(" "), "spira-lc content gate resolve sp-g");
    }
}
