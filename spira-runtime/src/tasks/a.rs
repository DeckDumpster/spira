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

pub static TASKS: &[Task] = &[
    Task {
        name: sentinel_call::NAME,
        boot: sentinel_call::SentinelCall::boot,
        cadence_ms: |_| 60_000,
        effects: sentinel_call::EFFECTS,
        runners: &[(sentinel_call::OBSERVE, |e, _, s| sentinel_call::observe(e, &s.input)), (sentinel_call::UNIT_START, |e, _, s| sentinel_call::unit_start(e, s)), (sentinel_call::PREEMPT, |e, _, s| sentinel_call::preempt(e, &s.input)), (sentinel_call::PREEMPT_LOG, |e, _, s| sentinel_call::preempt_log(e, &s.input)), (sentinel_call::METRIC, |e, _, s| sentinel_call::metric(e, &s.input))],
        keys: &[sentinel_call::KEYS],
        build: |_| sentinel_call::SentinelCall::from_cfg().registration(),
    },
    Task {
        name: sentinel_pass::NAME,
        boot: sentinel_pass::SentinelPass::boot,
        cadence_ms: |_| sentinel_pass::CADENCE_MS,
        effects: sentinel_pass::EFFECTS,
        runners: &[
            (sentinel_pass::OBSERVE, |e, _, s| sentinel_pass::observe(e, &s.input)),
            (sentinel_pass::LC_EVENT, |e, _, s| sentinel_pass::lc_event(e, s)),
            (sentinel_pass::LC_HOLD, |e, _, s| sentinel_pass::lc_hold(e, s)),
            (sentinel_pass::LC_UNHOLD, |e, _, s| sentinel_pass::lc_unhold(e, s)),
            (sentinel_pass::LC_FACT, |e, _, s| sentinel_pass::lc_fact(e, s)),
            (sentinel_pass::LC_CLOSE, |e, _, s| sentinel_pass::lc_close(e, s)),
            (sentinel_pass::CONTENT_LABEL, |_, _, s| sentinel_pass::unserved(s)),
            (sentinel_pass::CONTENT_NOTE, |e, _, s| sentinel_pass::content_note(e, s)),
            (sentinel_pass::CONTENT_CLOSE, |_, _, s| sentinel_pass::unserved(s)),
            (sentinel_pass::RECOMPUTE_BLOCKED, |_, _, s| sentinel_pass::unserved(s)),
            (sentinel_pass::STOP_HOLDER, |e, _, s| sentinel_pass::stop_holder(e, s)),
            (sentinel_pass::LANDING_START, |e, _, s| sentinel_pass::landing_start(e, s)),
            (sentinel_pass::WATCHTOWER, |_, _, s| sentinel_pass::not_ported(s)),
            (sentinel_pass::STRAND, |_, _, s| sentinel_pass::not_ported(s)),
            (sentinel_pass::PILGRIMAGE_CLOSE, |_, _, s| sentinel_pass::not_ported(s)),
            (sentinel_pass::ASK, |_, _, s| sentinel_pass::not_ported(s)),
            (sentinel_pass::INCIDENT_EVENT, |_, _, s| sentinel_pass::not_ported(s)),
            (sentinel_pass::REFLECT, |_, _, s| sentinel_pass::not_ported(s)),
            (sentinel_pass::PHASE_METRIC, |e, _, s| sentinel_pass::phase_metric(e, &s.input)),
        ],
        keys: &[sentinel_pass::KEYS],
        build: |_| sentinel_pass::SentinelPass::from_cfg().registration(),
    },
    Task {
        name: czar::NAME,
        boot: czar::Czar::boot,
        cadence_ms: |_| czar::CADENCE_MS,
        effects: czar::EFFECTS,
        runners: &[(czar::OBSERVE, |e, _, s| czar::observe(e, &s.input)), (czar::FILE_INCIDENT, |_, _, s| czar::file(&s.input)), (czar::FILE_BASE_RED, |_, _, s| czar::file(&s.input)), (czar::RERUN_WORKFLOW, |_, _, s| czar::rerun_workflow(&s.input)), (czar::RESTART_LANDING, |e, _, _| czar::restart_landing(e)), (czar::STATUS, |e, _, s| czar::status(e, &s.input))],
        keys: &[czar::KEYS],
        build: |_| czar::Czar::from_cfg().registration(),
    },
    Task {
        name: sift::NAME,
        boot: sift::Sift::boot,
        cadence_ms: |_| sift::CADENCE_MS,
        effects: sift::EFFECTS,
        runners: &[(sift::OBSERVE, |e, _, s| sift::observe(e, &s.input)), (sift::FETCH_BASE, |_, _, s| sift::fetch_base(&s.input)), (sift::NOTE, |e, _, s| sift::note(e, s)), (sift::SEND_BACK, |e, _, s| sift::send_back(e, s)), (sift::RECORD_PASS, |e, _, s| sift::record_pass(e, s)), (sift::SUPERSEDE, |e, _, s| sift::supersede(e, s))],
        keys: &[sift::KEYS],
        build: |_| sift::Sift::from_cfg().registration(),
    },
    Task {
        name: batcher_local::NAME,
        boot: batcher_local::BatcherLocal::boot,
        cadence_ms: |_| batcher_local::CADENCE_MS,
        effects: batcher_local::EFFECTS,
        runners: {
            use batcher_local::{edge as x, *};
            &[(OBSERVE, x::observe), (FETCH, x::fetch), (ASSEMBLE, x::assemble), (ROUND_OPEN, x::round_open), (CORPUS, x::corpus), (STAGE, x::stage), (STAGE_TEST, x::stage_test), (EJECT, x::eject), (WITHDRAW, x::withdraw), (REBASE_STALE, x::rebase_stale), (CERTIFY, x::certify), (LC_CERTIFY, x::lc_certify), (LAND, x::land), (ABANDON, x::abandon), (PROMOTE, x::promote), (REDRIVE, x::redrive), (FILE, x::file), (PAUSE, x::pause)]
        },
        keys: &[batcher_local::KEYS],
        build: |_| batcher_local::BatcherLocal::from_cfg().registration(),
    },
    Task {
        name: batcher_forge::NAME,
        boot: batcher_forge::BatcherForge::boot,
        cadence_ms: |_| batcher_forge::CADENCE_MS,
        effects: batcher_forge::EFFECTS,
        runners: {
            use batcher_forge::{edge as x, *};
            &[(OBSERVE, x::observe), (ARM, x::arm), (FETCH, x::fetch), (ASSEMBLE, x::assemble), (CORPUS, x::corpus), (WITHDRAW, x::withdraw), (SHOTS, x::shots), (BATCH_OPEN, x::batch_open), (FILE, x::file), (VERDICT_WRITE, x::verdict_write)]
        },
        keys: &[batcher_forge::KEYS],
        build: |_| batcher_forge::BatcherForge::from_cfg().registration(),
    },
    Task {
        name: queue_step::NAME,
        boot: queue_step::QueueStep::boot,
        cadence_ms: |_| queue_step::CADENCE_MS,
        effects: queue_step::EFFECTS,
        runners: {
            use queue_step::{edge as x, *};
            &[(OBSERVE, x::observe), (PUSH, x::push), (LAND, x::land), (CLOSE_BEAD, x::close_bead), (GH_CLOSEOUT, x::gh_closeout), (PR_CLOSE, x::pr_close), (CANCEL_RUNS, x::cancel_runs), (RERUN, x::rerun), (JUDGEMENT, x::judgement), (OBSERVE_FLAKE, x::observe_flake), (EJECT, x::eject), (RECONCILE, x::reconcile), (SWEEP, x::sweep), (FILE_BUG, x::file_bug), (NOTIFY, x::notify), (RECORD, x::record)]
        },
        keys: &[queue_step::KEYS],
        build: |_| queue_step::QueueStep::from_cfg().registration(),
    },
    Task {
        name: publish::NAME,
        boot: publish::Publish::boot,
        cadence_ms: |_| publish::CADENCE_MS,
        effects: publish::EFFECTS,
        runners: {
            use publish::{edge as x, *};
            &[(OBSERVE, x::observe), (PUSH_BRANCH, x::push_branch), (PUSH_FORGE, x::push_forge), (PR_CREATE, x::pr_create), (PR_CLOSE, x::pr_close), (CANCEL_RUNS, x::cancel_runs), (RERUN, x::rerun), (RECORD, x::record), (NOTIFY, x::notify)]
        },
        keys: &[publish::KEYS],
        build: |_| publish::Publish::from_cfg().registration(),
    },
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
    Task {
        name: landing::NAME,
        boot: landing::Landing::boot,
        cadence_ms: |_| landing::CADENCE_MS,
        effects: landing::EFFECTS,
        runners: {
            use landing::*;
            &[
                (OBSERVE, |e, _, _| observe(e)),
                (FETCH, |_, _, s| fetch(&s.input)),
                (DELIVER_PR_MERGED, |_, _, s| deliver_pr_merged(&s.input)),
                (DELIVER_PR_CLOSED, |_, _, s| deliver_pr_closed(&s.input)),
                (DELIVER_BY_CONTENT, |_, _, s| deliver_by_content(&s.input)),
                (REOPEN, |_, _, s| reopen(&s.input)),
                (EVENT, |e, _, s| event(e, &s.input)),
                (NOTE, |_, _, s| note(&s.input)),
                (BUMP_REQUEUE, |_, _, s| bump_requeue(&s.input)),
                (ASK, |e, _, s| ask(e, &s.input)),
                (REBASE, |_, _, s| rebase_branch(&s.input)),
                (CONFINE, |_, _, s| confine(&s.input)),
                (FORCE_PUSH, |_, _, s| force_push(&s.input)),
                (PR_CREATE, |_, _, s| pr_create(&s.input)),
                (PR_AUTOMERGE, |_, _, s| pr_automerge(&s.input)),
            ]
        },
        keys: &[landing::KEYS],
        build: |_| landing::Landing::from_cfg().registration(),
    },
    Task {
        name: submission_provision::NAME,
        boot: submission_provision::SubmissionProvision::boot,
        // The daily calendar's first tick, as every calendar row gives it: the boot tick's, two
        // minutes after start (`OnBootSec=2min`); the calendar arms each day after that.
        cadence_ms: |_| submission_provision::BOOT_DELAY_MS,
        effects: submission_provision::EFFECTS,
        runners: {
            use submission_provision::*;
            &[(OBSERVE, |_, _, _| observe()), (CERT_RENEW, |_, _, s| cert_renew(&s.input)), (CONFIG_SET, |_, _, s| config_set(&s.input)), (PASSWORD_WRITE, |_, _, s| password_write(&s.input)), (FIREWALL_OPEN, |_, _, s| firewall_open(&s.input)), (UNIT_RESTART, |e, _, s| unit_restart(e, &s.input))]
        },
        keys: &[submission_provision::KEYS],
        build: |_| submission_provision::SubmissionProvision::from_cfg().registration(),
    },
];

/// Family A's rows, built from their contracts and waiting for their ports.
pub static PENDING: &[Task] = &[];

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
