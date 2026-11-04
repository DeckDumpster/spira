//! Family A, loop core, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`.
//!
//! Each task's contract (`wiki/projects/spira/designs/tasks/<task>.md`) is its module under
//! `a/`: the handler, its snapshot and carry, its act table and, for a P2 machine, its
//! `state × event → state` table as data. `decide` and the runners are not ported yet, so the
//! rows wait in `PENDING`: a row joins `TASKS` (and so `tasks::all()`, the boot list, the
//! settings fixture and `SPIRA_TASK_MODES`) in the commit that ports it, with its effects named
//! in `sim::doubles::TASK_EFFECTS` and its keys in the settings fixture. Until then its
//! `tests/task_a_<task>.rs` T1 is red on exactly that.

use super::Task;

pub mod kit;

pub mod batcher_forge;
pub mod batcher_local;
pub mod czar;
pub mod gate_check;
pub mod gate_worker;
pub mod landing;
pub mod publish;
pub mod queue_step;
pub mod sentinel_call;
pub mod sentinel_pass;
pub mod sift;
pub mod submission_provision;
pub mod sweep_target;

/// Every effect of module `$m` listed, each with a runner that refuses until it is ported.
macro_rules! stubs {
    ($m:ident: $($e:ident),* $(,)?) => {
        &[$(($m::$e, |_, _, _| unimplemented!("{}: runner for {} not ported yet", $m::NAME, $m::$e))),*]
    };
}

pub static TASKS: &[Task] = &[
    Task {
        name: sweep_target::NAME,
        boot: sweep_target::SweepTarget::boot,
        cadence_ms: |_| sweep_target::CADENCE_MS,
        effects: sweep_target::EFFECTS,
        runners: &[(sweep_target::OBSERVE, |e, _, _| sweep_target::observe(&e.world.run_dir)), (sweep_target::REMOVE_TARGET, |_, _, s| sweep_target::remove_target(&s.input)), (sweep_target::REMOVE_FOREIGN_RUN, |_, _, s| sweep_target::remove_foreign_run(&s.input))],
        keys: &[sweep_target::KEYS],
        build: |_| sweep_target::SweepTarget::from_cfg().registration(),
    },
    Task {
        name: gate_worker::NAME,
        boot: gate_worker::GateWorker::boot,
        cadence_ms: |_| gate_worker::CADENCE_MS,
        effects: gate_worker::EFFECTS,
        runners: {
            use gate_worker::*;
            &[(OBSERVE, |e, _, s| observe(e, &s.input)), (TIP, |_, _, s| tip(&s.input)), (CLAIM, |e, _, s| claim(e, &s.input)), (DISCARD, |e, _, s| discard(e, &s.input)), (RETAIN_OUTPUT, |e, _, s| retain_output(e, &s.input)), (COMPLETE, |e, _, s| complete(e, &s.input)), (RECOVER, |e, _, s| recover(e, &s.input))]
        },
        keys: &[gate_worker::KEYS],
        build: |_| gate_worker::GateWorker::from_cfg().registration(),
    },
    Task {
        name: gate_check::NAME,
        boot: gate_check::GateCheck::boot,
        cadence_ms: |_| gate_check::CADENCE_MS,
        effects: gate_check::EFFECTS,
        runners: {
            use gate_check::*;
            &[(OBSERVE, |e, _, _| observe(e)), (DISCOVER, |e, k, s| discover(e, k, &s.input)), (RESOLVE, |e, k, s| resolve(e, k, &s.input)), (CI_FAILED, |e, _, s| ci_failed(e, &s.input)), (FILE_BEAD, |e, k, s| file_bead(e, k, &s.input)), (RAISE_PRIORITY, |e, k, s| raise_priority(e, k, &s.input)), (NOTE, |e, _, s| note(e, &s.input)), (TSD_INGEST, |e, k, s| tsd_ingest(e, k, &s.input))]
        },
        keys: &[gate_check::KEYS],
        build: |_| gate_check::GateCheck::from_cfg().registration(),
    },
];

/// Family A's rows, built from their contracts and waiting for their ports.
pub static PENDING: &[Task] = &[
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
    Task {
        name: queue_step::NAME,
        boot: queue_step::QueueStep::boot,
        cadence_ms: |_| queue_step::CADENCE_MS,
        effects: queue_step::EFFECTS,
        runners: stubs!(queue_step: OBSERVE, PUSH, LAND, CLOSE_BEAD, GH_CLOSEOUT, PR_CLOSE, CANCEL_RUNS, RERUN, JUDGEMENT, OBSERVE_FLAKE, EJECT, RECONCILE, SWEEP, FILE_BUG, NOTIFY, RECORD),
        keys: &[queue_step::KEYS],
        build: |_| queue_step::QueueStep::from_cfg().registration(),
    },
    Task {
        name: publish::NAME,
        boot: publish::Publish::boot,
        cadence_ms: |_| publish::CADENCE_MS,
        effects: publish::EFFECTS,
        runners: stubs!(publish: OBSERVE, PUSH_BRANCH, PUSH_FORGE, PR_CREATE, PR_CLOSE, CANCEL_RUNS, RERUN, RECORD, NOTIFY),
        keys: &[publish::KEYS],
        build: |_| publish::Publish::from_cfg().registration(),
    },
    Task {
        name: landing::NAME,
        boot: landing::Landing::boot,
        cadence_ms: |_| landing::CADENCE_MS,
        effects: landing::EFFECTS,
        runners: stubs!(landing: OBSERVE, FETCH, DELIVER_PR_MERGED, DELIVER_PR_CLOSED, DELIVER_BY_CONTENT, REOPEN, EVENT, NOTE, BUMP_REQUEUE, ASK, REBASE, CONFINE, FORCE_PUSH, PR_CREATE, PR_AUTOMERGE),
        keys: &[landing::KEYS],
        build: |_| landing::Landing::from_cfg().registration(),
    },
    Task {
        name: submission_provision::NAME,
        boot: submission_provision::SubmissionProvision::boot,
        // The daily calendar; its first tick is the boot tick's, two minutes after start.
        cadence_ms: |_| runtime::taskrig::DAY_MS,
        effects: submission_provision::EFFECTS,
        runners: stubs!(submission_provision: OBSERVE, CERT_RENEW, CONFIG_SET, PASSWORD_WRITE, FIREWALL_OPEN, UNIT_RESTART),
        keys: &[submission_provision::KEYS],
        build: |_| submission_provision::SubmissionProvision::from_cfg().registration(),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pending_row_declares_each_effect_once_and_has_a_runner_for_each_unshared_one() {
        // `inbox.append` and the Units port's `unit.*` effects are in `SHARED`, not on a row.
        let shared: Vec<&str> = crate::tasks::SHARED.iter().map(|(n, _)| *n).collect();
        for t in PENDING {
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
    fn no_pending_row_shares_a_name_or_a_stream_with_the_service() {
        let mut families: Vec<&[Task]> = crate::tasks::FAMILIES.to_vec();
        families.push(PENDING);
        assert_eq!(crate::tasks::clashes(&families), Vec::<String>::new());
    }
}
