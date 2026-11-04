//! Family A, loop core, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`.
//!
//! Each task's contract (`wiki/projects/spira/designs/tasks/<task>.md`) is its module under
//! `a/`: the handler, its snapshot and carry, its act table and, for a P2 machine, its
//! `state × event → state` table as data. `decide` and the runners are not ported yet, so the
//! rows wait in `STAGED`: a row joins `TASKS` (and so `tasks::all()`, the boot list, the
//! settings fixture and `SPIRA_TASK_MODES`) in the commit that ports it, with its effects named
//! in `sim::doubles::SERVICE_EFFECTS` and its keys in the settings fixture. Until then its
//! `tests/task_a_<task>.rs` T1 is red on exactly that.

use super::Task;

pub mod kit;

pub mod batcher_forge;
pub mod batcher_local;
pub mod czar;
pub mod sentinel_call;
pub mod sentinel_pass;
pub mod sift;

/// Every effect of module `$m` listed, each with a runner that refuses until it is ported.
macro_rules! stubs {
    ($m:ident: $($e:ident),* $(,)?) => {
        &[$(($m::$e, |_, _, _| unimplemented!("{}: runner for {} not ported yet", $m::NAME, $m::$e))),*]
    };
}

pub static TASKS: &[Task] = &[];

/// Family A's rows, built from their contracts and waiting for their ports.
pub static STAGED: &[Task] = &[
    Task {
        name: sentinel_call::NAME,
        boot: sentinel_call::SentinelCall::boot,
        cadence_ms: |_| 60_000,
        effects: sentinel_call::EFFECTS,
        runners: stubs!(sentinel_call: OBSERVE, UNIT_START, PREEMPT, PREEMPT_LOG, METRIC),
        keys: &[sentinel_call::KEYS],
        build: |_| sentinel_call::SentinelCall::from_cfg().registration(),
    },
    Task {
        name: sentinel_pass::NAME,
        boot: sentinel_pass::SentinelPass::boot,
        cadence_ms: |_| sentinel_pass::CADENCE_MS,
        effects: sentinel_pass::EFFECTS,
        runners: stubs!(sentinel_pass: OBSERVE, LC_EVENT, LC_HOLD, LC_UNHOLD, LC_FACT, LC_CLOSE, CONTENT_LABEL, CONTENT_NOTE, CONTENT_CLOSE, RECOMPUTE_BLOCKED, STOP_HOLDER, LANDING_START, WATCHTOWER, STRAND, PILGRIMAGE_CLOSE, ASK, INCIDENT_EVENT, REFLECT, PHASE_METRIC),
        keys: &[sentinel_pass::KEYS],
        build: |_| sentinel_pass::SentinelPass::from_cfg().registration(),
    },
    Task {
        name: czar::NAME,
        boot: czar::Czar::boot,
        cadence_ms: |_| czar::CADENCE_MS,
        effects: czar::EFFECTS,
        runners: stubs!(czar: OBSERVE, FILE_INCIDENT, FILE_BASE_RED, RERUN_WORKFLOW, RESTART_LANDING, STATUS),
        keys: &[czar::KEYS],
        build: |_| czar::Czar::from_cfg().registration(),
    },
    Task {
        name: sift::NAME,
        boot: sift::Sift::boot,
        cadence_ms: |_| sift::CADENCE_MS,
        effects: sift::EFFECTS,
        runners: stubs!(sift: OBSERVE, FETCH_BASE, NOTE, SEND_BACK, RECORD_PASS, SUPERSEDE),
        keys: &[sift::KEYS],
        build: |_| sift::Sift::from_cfg().registration(),
    },
    Task {
        name: batcher_local::NAME,
        boot: batcher_local::BatcherLocal::boot,
        cadence_ms: |_| batcher_local::CADENCE_MS,
        effects: batcher_local::EFFECTS,
        runners: stubs!(batcher_local: OBSERVE, FETCH, ASSEMBLE, ROUND_OPEN, CORPUS, STAGE, STAGE_TEST, EJECT, WITHDRAW, REBASE_STALE, CERTIFY, LC_CERTIFY, LAND, ABANDON, PROMOTE, REDRIVE, FILE, PAUSE),
        keys: &[batcher_local::KEYS],
        build: |_| batcher_local::BatcherLocal::from_cfg().registration(),
    },
    Task {
        name: batcher_forge::NAME,
        boot: batcher_forge::BatcherForge::boot,
        cadence_ms: |_| batcher_forge::CADENCE_MS,
        effects: batcher_forge::EFFECTS,
        runners: stubs!(batcher_forge: OBSERVE, ARM, FETCH, ASSEMBLE, CORPUS, WITHDRAW, SHOTS, BATCH_OPEN, FILE, VERDICT_WRITE),
        keys: &[batcher_forge::KEYS],
        build: |_| batcher_forge::BatcherForge::from_cfg().registration(),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_staged_row_declares_each_effect_once_and_has_a_runner_for_each_unshared_one() {
        // `inbox.append` is in `SHARED`; the Units port's `unit.*` effects are every unit-starting
        // task's, so their runner joins `SHARED` with the port, not a row.
        let shared: Vec<&str> = crate::tasks::SHARED.iter().map(|(n, _)| *n).chain(runtime::units::EFFECTS).collect();
        for t in STAGED {
            let mut names = t.effects.to_vec();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), t.effects.len(), "{}: an effect is declared twice", t.name);
            for e in t.effects.iter().filter(|e| !shared.contains(e)) {
                assert!(t.runners.iter().any(|(n, _)| n == e), "{}: {e} has no runner", t.name);
            }
            assert!(t.runners.iter().all(|(n, _)| t.effects.contains(n)), "{}: a runner for an undeclared effect", t.name);
        }
    }

    #[test]
    fn no_staged_row_shares_a_name_or_a_stream_with_the_service() {
        let mut families: Vec<&[Task]> = crate::tasks::FAMILIES.to_vec();
        families.push(STAGED);
        assert_eq!(crate::tasks::clashes(&families), Vec::<String>::new());
    }
}
