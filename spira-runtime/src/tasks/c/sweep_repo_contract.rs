//! # sweep.repo_contract — contract
//!
//! A repo that breaks its landing contract is not dispatched to until it meets it again. Its
//! Acceptance is the test file `tests/it/task_c_sweep_repo_contract.rs`.
//!
//! ## Tick
//!
//! - **Calendar:** `OnCalendar=daily` by default, jittered by `Ctx::rand_u64`. The pass is stamped
//!   when it starts. A boot observes only when a slot after the stamp has passed with no pass
//!   started; a pass lost to a restart waits for the next slot, and a task that never fired
//!   arms its slot and runs nothing at boot.
//! - **Single-flight:** yes.
//!
//! ## Observe
//!
//! `sweep.repo_contract.observe` runs `repo-contract check --all` (which names every registered
//! repo but the home repo), which reads files and refs only and runs no suite, and reads every scoped stop on `<run>`:
//!
//! ```text
//! Snapshot { repos: [RepoState{ repo, verdict: Met|NotMet|CannotTell, lines }], stops: [StopView{ repo, owner }] }
//! ```
//!
//! A repo is `NotMet` when any of its lines is `not met`, else `CannotTell` when any line is
//! neither met nor not met, or it printed none, else `Met`. The home repo is dropped if it ever appears.
//!
//! ## Decide
//!
//! Pure, per repo: `Stop(lines)` when `NotMet` and the repo has no stop at all, or its own stop names other lines; `Lift` when `Met`
//! and the repo's stop is this task's; `Keep` otherwise. A repo holds one stop record, so a stop
//! owned by anyone else is left alone and nothing is written over it. `CannotTell` decides nothing
//! for the repo and writes one inbox line until the repo is told again.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.repo_contract.stop` | host | 10 s | `repo-contract/stop/<repo>/<secs>` | a repo whose stop is anyone else's |
//! | `sweep.repo_contract.lift` | host | 10 s | `repo-contract/lift/<repo>/<secs>` | a stop that is not this task's |
//!
//! A stop is on the `claims` lane, owner `repo-contract`, reason the not-met lines, until
//! `repo-contract check <repo> met`. Each stop and lift writes one `inbox.append` line.
//!
//! ## Silence
//!
//! A tick that changes nothing writes nothing. The stop is deliberate state, never a fault.
//!
//! ## Replaces
//!
//! Nothing; a broken contract was found by the first builder to be blocked on it.
//!
//! ## Reverse
//!
//! Removing the task leaves its stops in force; lift them with `world.sh unpause`.
//!

use runtime::event::EventKindTag;
use runtime::ports::{Proc, ProcSpec};
use serde::{Deserialize, Serialize};
use spira_config::scoped_stop;

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};
pub use crate::watchers::INBOX_APPEND;

pub const NAME: &str = "sweep.repo_contract";
pub const STREAM: &str = "task.sweep.repo_contract";
pub const TICK: EventKindTag = "sweep.repo_contract.tick";
pub const STATE: EventKindTag = "sweep.repo_contract.state";

pub const OBSERVE: &str = "sweep.repo_contract.observe";
pub const STOP: &str = "sweep.repo_contract.stop";
pub const LIFT: &str = "sweep.repo_contract.lift";
pub const EFFECTS: &[&str] = &[OBSERVE, STOP, LIFT, INBOX_APPEND];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_REPO_CONTRACT_CALENDAR";
pub const JITTER_KEY: &str = "SPIRA_RUNTIME_SWEEP_REPO_CONTRACT_JITTER_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, JITTER_KEY, "SPIRA_HOME_REPO"];

/// The owner of every stop this task writes, and the only owner whose stop it lifts.
pub const OWNER: &str = spira_config::scoped_stop::CONTRACT_OWNER;
const LANE: &str = "claims";
const CHECK_DEADLINE_MS: u64 = 120_000;

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: CHECK_DEADLINE_MS, cadence_key: CADENCE_KEY, calendar: true, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Met,
    NotMet,
    #[default]
    CannotTell,
}

/// A repo's verdict and the clause lines that are not met (or cannot be told).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoState {
    pub repo: String,
    pub verdict: Verdict,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StopView {
    pub repo: String,
    pub owner: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub repos: Vec<RepoState>,
    pub stops: Vec<StopView>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub last_fired: Option<u64>,
    /// Repos the last pass could not tell, so a second pass that cannot tell says nothing more.
    pub unsure: Vec<String>,
}

pub struct SweepRepoContract {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// What [`STOP`] carries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    pub repo: String,
    pub reason: String,
    pub until: String,
    pub now_secs: u64,
}

/// What [`LIFT`] carries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lift {
    pub repo: String,
}

pub fn until(repo: &str) -> String {
    format!("repo-contract check {repo} met")
}

fn act(name: &str, key: String, input: String) -> Act {
    Act { name: name.into(), key, input, deadline_ms: 10_000, probe: false }
}

fn inbox(now_ms: u64, key: String, what: String) -> Act {
    act(INBOX_APPEND, key, format!("{} [task:{NAME}] {what}", crate::watchers::iso8601(now_ms / 1000)))
}

pub fn decide(_cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((Carry { last_fired: Some(now_ms), ..carry.clone() }, vec![])),
        Input::Observed(snap) => {
            let secs = now_ms / 1000;
            let mut acts = vec![];
            let mut unsure = vec![];
            for r in &snap.repos {
                let own = snap.stops.iter().find(|s| s.repo == r.repo && s.owner == OWNER);
                let other = snap.stops.iter().any(|s| s.repo == r.repo && s.owner != OWNER);
                let lines = r.lines.join("; ");
                match r.verdict {
                    Verdict::NotMet if (own.is_none() && !other) || own.is_some_and(|s| s.reason != lines) => {
                        let body = Stop { repo: r.repo.clone(), reason: lines.clone(), until: until(&r.repo), now_secs: secs };
                        acts.push(act(STOP, format!("repo-contract/stop/{}/{secs}", r.repo), serde_json::to_string(&body).map_err(|e| e.to_string())?));
                        if own.is_none() {
                            acts.push(inbox(now_ms, format!("repo-contract/stopped/{}/{secs}", r.repo), format!("{} does not meet the landing contract; claims stopped: {lines}", r.repo)));
                        }
                    }
                    Verdict::Met if own.is_some() => {
                        acts.push(act(LIFT, format!("repo-contract/lift/{}/{secs}", r.repo), serde_json::to_string(&Lift { repo: r.repo.clone() }).map_err(|e| e.to_string())?));
                        acts.push(inbox(now_ms, format!("repo-contract/lifted/{}/{secs}", r.repo), format!("{} meets the landing contract again; claims stop lifted", r.repo)));
                    }
                    Verdict::CannotTell => {
                        unsure.push(r.repo.clone());
                        if !carry.unsure.contains(&r.repo) {
                            acts.push(inbox(now_ms, format!("repo-contract/unsure/{}/{secs}", r.repo), format!("{} cannot be judged against the landing contract, left as it is: {lines}", r.repo)));
                        }
                    }
                    _ => {}
                }
            }
            Ok((Carry { last_fired: carry.last_fired, unsure }, acts))
        }
        Input::Outcome { effect, ok: false, body } if effect == STOP || effect == LIFT => {
            Ok((carry.clone(), vec![inbox(now_ms, format!("repo-contract/failed/{effect}/{}", now_ms / 1000), format!("{effect} did not apply, the next tick tries again: {body}"))]))
        }
        Input::Outcome { effect, .. } if effect == STOP || effect == LIFT || effect == INBOX_APPEND => Ok((carry.clone(), vec![])),
        Input::Outcome { effect, .. } => Err(format!("{NAME}: no outcome of {effect} is expected")),
        Input::Ran(r) => Err(format!("{NAME}: no unit is expected, got {:?}", r.handle)),
    }
}

/// A boot whose next slot after the last pass started has gone by observes at once; a task that never fired only arms its slot.
pub fn catch_up(carry: &Carry, now_ms: u64, cadence: &Cadence) -> Option<String> {
    carry.last_fired.filter(|last| cadence.next(*last) <= now_ms).map(|_| String::new())
}

/// `repo-contract check --all`'s lines, judged per repo; the home repo is dropped.
pub fn judge(output: &str, home: &str) -> Vec<RepoState> {
    let mut repos: Vec<RepoState> = vec![];
    let mut current: Option<(String, usize, Vec<String>, Vec<String>)> = None;
    let finish = |c: Option<(String, usize, Vec<String>, Vec<String>)>, repos: &mut Vec<RepoState>| {
        if let Some((repo, met, not_met, cannot)) = c {
            let (verdict, lines) = if !not_met.is_empty() {
                (Verdict::NotMet, not_met)
            } else if !cannot.is_empty() {
                (Verdict::CannotTell, cannot)
            } else if met == 0 {
                (Verdict::CannotTell, vec!["repo-contract printed no clause line".to_string()])
            } else {
                (Verdict::Met, vec![])
            };
            if repo != home {
                repos.push(RepoState { repo, verdict, lines });
            }
        }
    };
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("== ") {
            finish(current.take(), &mut repos);
            current = Some((name.trim().to_string(), 0, vec![], vec![]));
        } else if let Some(c) = current.as_mut() {
            if line.starts_with("not met ") {
                c.2.push(line.to_string());
            } else if line.starts_with("met ") {
                c.1 += 1;
            } else if !line.trim().is_empty() {
                c.3.push(line.to_string());
            }
        }
    }
    finish(current.take(), &mut repos);
    repos
}

/// The runner of [`OBSERVE`].
pub fn observe(proc: &dyn Proc, run: &std::path::Path, home: &str) -> Result<String, String> {
    if run.as_os_str().is_empty() {
        return Err("SPIRA_RUN is empty — refusing".into());
    }
    let spec = ProcSpec { prog: "repo-contract".into(), args: vec!["check".into(), "--all".into()], stdin: vec![], deadline: std::time::Duration::from_millis(CHECK_DEADLINE_MS - 20_000) };
    let out = proc.run(&spec)?;
    if !matches!(out.code, Some(0..=2)) {
        return Err(format!("repo-contract check --all exited {:?}: {}", out.code, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let repos = judge(&String::from_utf8_lossy(&out.stdout), home);
    if repos.is_empty() {
        return Err(format!("repo-contract check --all named no repo ({:?}): {}", out.code, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let stops = scoped_stop::repo_stops_strict(run)?.into_iter().map(|s| StopView { repo: s.repo, owner: s.owner, reason: s.reason }).collect();
    serde_json::to_string(&Snapshot { repos, stops }).map_err(|e| e.to_string())
}

fn stop_of(run: &std::path::Path, repo: &str) -> Result<Option<scoped_stop::RepoStop>, String> {
    Ok(scoped_stop::repo_stops_strict(run)?.into_iter().find(|s| s.repo == repo))
}

/// The runner of [`STOP`]: re-checked here, because a stop another owner wrote since the probe is
/// not this task's to overwrite.
pub fn stop(run: &std::path::Path, input: &str) -> Result<String, String> {
    let s: Stop = serde_json::from_str(input).map_err(|e| e.to_string())?;
    if let Some(other) = stop_of(run, &s.repo)?.filter(|o| o.owner != OWNER) {
        return Err(format!("{} is stopped by {}; left alone", s.repo, other.owner));
    }
    scoped_stop::pause_repo_typed(run, &s.repo, &[LANE.to_string()], OWNER, &s.reason, &s.until, s.now_secs, tsd::stop::Reason::Landing, None)?;
    Ok(format!("{}: {LANE} stopped", s.repo))
}

/// The runner of [`LIFT`]: only a stop this task owns.
pub fn lift(run: &std::path::Path, input: &str) -> Result<String, String> {
    let l: Lift = serde_json::from_str(input).map_err(|e| e.to_string())?;
    match stop_of(run, &l.repo)? {
        Some(s) if s.owner == OWNER => {
            scoped_stop::unpause_repo(run, &l.repo, &[LANE.to_string()])?;
            Ok(format!("{}: {LANE} stop lifted", l.repo))
        }
        Some(s) => Err(format!("{} is stopped by {}; left alone", l.repo, s.owner)),
        None => Ok(format!("{}: no stop to lift", l.repo)),
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("last_fired"), runtime::rows::Part::Field("unsure")];

crate::tasks::b::hosted!(SweepRepoContract, catch_up = catch_up);
