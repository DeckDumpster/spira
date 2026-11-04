//! Family C, sweeps, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`.
//!
//! As in family B (`tasks::b`), each module under `c/` is a red-first stub on `b::kit`, and its
//! row waits in [`PENDING`] until its keys and mode are registered; promoting a row is moving it
//! into [`TASKS`].

use super::Task;
use crate::tasks::b::kit::{build, cadence_ms, not_ported};

pub mod asks_moot;
pub mod hel_reap_stale;
pub mod hel_reap_terminal;
pub mod hel_sweep;
pub mod mail_tidy;
pub mod roundvm_ami_prune;
pub mod roundvm_drain;
pub mod sweep_archive;
pub mod sweep_escape_census;
pub mod sweep_export_beads;
pub mod sweep_nix_gc;
pub mod sweep_reclaim;
pub mod sweep_tsd;

use asks_moot::AsksMoot;
use hel_reap_stale::HelReapStale;
use hel_reap_terminal::HelReapTerminal;
use hel_sweep::HelSweep;
use mail_tidy::MailTidy;
use roundvm_ami_prune::RoundvmAmiPrune;
use roundvm_drain::RoundvmDrain;
use sweep_archive::SweepArchive;
use sweep_escape_census::SweepEscapeCensus;
use sweep_export_beads::SweepExportBeads;
use sweep_nix_gc::SweepNixGc;
use sweep_reclaim::SweepReclaim;
use sweep_tsd::SweepTsd;

pub static TASKS: &[Task] = &[
    Task {
        name: hel_sweep::NAME,
        boot: HelSweep::boot,
        cadence_ms: cadence_ms::<HelSweep>,
        effects: hel_sweep::EFFECTS,
        runners: &[
            (hel_sweep::OBSERVE, |e, _, s| hel_sweep::observe(&sweep_world(e)?, &crate::tasks::b::lc(e), &hel_sweep::Pinned::read(&pinned)?, &s.input)),
            (hel_sweep::PR_TIP, |e, _, s| hel_sweep::pr_tip(&sweep_world(e)?, &s.input)),
            (hel_sweep::SEND, |e, _, s| hel_sweep::send(&sweep_world(e)?, &crate::tasks::b::lc(e), e.units.as_deref(), &s.input)),
            (hel_sweep::CONTENT_ON_BASE, |e, _, s| hel_sweep::content_on_base(&sweep_world(e)?, &crate::tasks::b::lc(e), &s.input)),
            (hel_sweep::CLOSE_ON_LAND, |e, _, s| hel_sweep::close_on_land(&sweep_world(e)?, &crate::tasks::b::lc(e), &s.input)),
            (hel_sweep::ARCHIVE, |e, _, s| hel_sweep::archive(&sweep_world(e)?, &s.input)),
            (hel_sweep::FREE_WORKTREE, |e, _, s| hel_sweep::destroy_worktree(&sweep_world(e)?, hel_sweep::FREE_WORKTREE, &s.input)),
            (hel_sweep::ORPHAN_WORKTREE, |e, _, s| hel_sweep::destroy_worktree(&sweep_world(e)?, hel_sweep::ORPHAN_WORKTREE, &s.input)),
            (hel_sweep::PRUNE, |e, _, s| hel_sweep::prune(&sweep_world(e)?, &s.input)),
        ],
        keys: &[hel_sweep::KEYS],
        build: build::<HelSweep>,
    },
    Task {
        name: hel_reap_terminal::NAME,
        boot: HelReapTerminal::boot,
        cadence_ms: cadence_ms::<HelReapTerminal>,
        effects: hel_reap_terminal::EFFECTS,
        runners: &[
            (hel_reap_terminal::OBSERVE, |e, _, _| hel_reap_terminal::observe(&hel_world(e)?, &crate::tasks::b::lc(e))),
            (hel_reap_terminal::DESTROY_WORKTREE, |e, _, s| hel_reap_terminal::destroy(&hel_world(e)?, &s.input)),
            (hel_reap_terminal::REMOVE_SCRATCH, |e, _, s| hel_reap_terminal::remove_scratch(&hel_world(e)?, &s.input)),
            (hel_reap_terminal::PRUNE, |e, _, s| hel_reap_terminal::prune(&hel_world(e)?, &s.input)),
        ],
        keys: &[hel_reap_terminal::KEYS],
        build: build::<HelReapTerminal>,
    },
    Task {
        name: hel_reap_stale::NAME,
        boot: HelReapStale::boot,
        cadence_ms: cadence_ms::<HelReapStale>,
        effects: hel_reap_stale::EFFECTS,
        runners: &[
            (hel_reap_stale::OBSERVE, |e, _, _| hel_reap_stale::observe(&hel_world(e)?, &crate::tasks::b::lc(e), &e.world.proc_dir)),
            (hel_reap_stale::DESTROY_WORKTREE, |e, _, s| hel_reap_stale::destroy(&hel_world(e)?, &s.input)),
            (hel_reap_stale::PRUNE, |e, _, _| hel_reap_stale::prune(&hel_world(e)?)),
        ],
        keys: &[hel_reap_stale::KEYS],
        build: build::<HelReapStale>,
    },
    Task {
        name: roundvm_ami_prune::NAME,
        boot: RoundvmAmiPrune::boot,
        cadence_ms: cadence_ms::<RoundvmAmiPrune>,
        effects: roundvm_ami_prune::EFFECTS,
        runners: &[(roundvm_ami_prune::OBSERVE, |e, _, _| roundvm_ami_prune::observe(&e.world.run_dir)), (roundvm_ami_prune::DEREGISTER, |_, _, s| roundvm_ami_prune::deregister(&s.input)), (roundvm_ami_prune::DELETE_SNAPSHOT, |_, _, s| roundvm_ami_prune::delete_snapshots(&s.input))],
        keys: &[roundvm_ami_prune::KEYS],
        build: build::<RoundvmAmiPrune>,
    },
    Task { name: roundvm_drain::NAME, boot: RoundvmDrain::boot, cadence_ms: cadence_ms::<RoundvmDrain>, effects: roundvm_drain::EFFECTS, runners: &[(roundvm_drain::OBSERVE, |e, _, _| roundvm_drain::observe(&e.world.run_dir, &pinned("SPIRA_ROUND_VM_SPILL")?, &pinned("SPIRA_ROUND_VM_WARM_IDLE_SECS")?)), (roundvm_drain::RELEASE, |_, _, s| roundvm_drain::release(&s.input))], keys: &[roundvm_drain::KEYS], build: build::<RoundvmDrain> },
    Task {
        name: sweep_reclaim::NAME,
        boot: SweepReclaim::boot,
        cadence_ms: cadence_ms::<SweepReclaim>,
        effects: sweep_reclaim::EFFECTS,
        runners: &[
            (sweep_reclaim::OBSERVE, |e, _, _| sweep_reclaim::observe(&sweep_reclaim::Pinned::read(&pinned)?, &e.world.proc_dir)),
            (sweep_reclaim::REMOVE_TARGET, |e, _, s| sweep_reclaim::remove_target(&sweep_reclaim::Pinned::read(&pinned)?, &e.world.proc_dir, &s.input)),
            (sweep_reclaim::PODMAN_PRUNE, |_, _, s| sweep_reclaim::podman_prune(&sweep_reclaim::Pinned::read(&pinned)?, &runtime::ports::SysProc, &s.input)),
            (sweep_reclaim::TRIM_CACHE, |_, _, s| sweep_reclaim::trim_cache(&sweep_reclaim::Pinned::read(&pinned)?, &s.input)),
        ],
        keys: &[sweep_reclaim::KEYS],
        build: build::<SweepReclaim>,
    },
    Task { name: sweep_tsd::NAME, boot: SweepTsd::boot, cadence_ms: cadence_ms::<SweepTsd>, effects: sweep_tsd::EFFECTS, runners: &[(sweep_tsd::OBSERVE, |e, _, _| sweep_tsd::observe(&e.world.run_dir, &pinned(sweep_tsd::KEEP_DAYS_KEY)?, &pinned(sweep_tsd::BY_FAMILY_KEY)?)), (sweep_tsd::PRUNE, |e, _, s| sweep_tsd::prune(&e.world.run_dir, &s.input))], keys: &[sweep_tsd::KEYS], build: build::<SweepTsd> },
];

/// The hel world the hel tasks act through: this run, the pinned release, the store's socket.
pub fn hel_world(e: &crate::Effects) -> Result<hel::real::Real, String> {
    hel_reap_stale::world(&e.world.run_dir, &pinned("SPIRA_RELEASE")?, &crate::tasks::b::lc(e))
}

/// The hel world `hel.sweep` acts through: [`hel_world`] with the pinned forge program.
pub fn sweep_world(e: &crate::Effects) -> Result<hel::real::Real, String> {
    hel_sweep::world(&e.world.run_dir, &pinned("SPIRA_RELEASE")?, &crate::tasks::b::lc(e), &hel_sweep::Pinned::read(&pinned)?)
}

/// A key's value from the process's pinned config, for a runner whose task reads it at the act.
pub fn pinned(key: &str) -> Result<String, String> {
    spira_config::process::cfg(key)
}


/// The family's rows, complete but not yet in `TASKS`.
pub static PENDING: &[Task] = &[
    Task {
        name: asks_moot::NAME,
        boot: AsksMoot::boot,
        cadence_ms: cadence_ms::<AsksMoot>,
        effects: asks_moot::EFFECTS,
        runners: &[(asks_moot::OBSERVE, not_ported::<AsksMoot>), (asks_moot::CHECK, not_ported::<AsksMoot>), (asks_moot::RESOLVE, not_ported::<AsksMoot>), (asks_moot::WITHDRAW, not_ported::<AsksMoot>)],
        keys: &[asks_moot::KEYS],
        build: build::<AsksMoot>,
    },
    Task { name: mail_tidy::NAME, boot: MailTidy::boot, cadence_ms: cadence_ms::<MailTidy>, effects: mail_tidy::EFFECTS, runners: &[(mail_tidy::OBSERVE, not_ported::<MailTidy>), (mail_tidy::ARCHIVE, not_ported::<MailTidy>)], keys: &[mail_tidy::KEYS], build: build::<MailTidy> },
    Task {
        name: sweep_archive::NAME,
        boot: SweepArchive::boot,
        cadence_ms: cadence_ms::<SweepArchive>,
        effects: sweep_archive::EFFECTS,
        runners: &[(sweep_archive::OBSERVE, not_ported::<SweepArchive>), (sweep_archive::STORE, not_ported::<SweepArchive>), (sweep_archive::INDEX, not_ported::<SweepArchive>)],
        keys: &[sweep_archive::KEYS],
        build: build::<SweepArchive>,
    },
    Task { name: sweep_nix_gc::NAME, boot: SweepNixGc::boot, cadence_ms: cadence_ms::<SweepNixGc>, effects: sweep_nix_gc::EFFECTS, runners: &[(sweep_nix_gc::OBSERVE, not_ported::<SweepNixGc>), (sweep_nix_gc::COLLECT, not_ported::<SweepNixGc>)], keys: &[sweep_nix_gc::KEYS], build: build::<SweepNixGc> },
    Task {
        name: sweep_export_beads::NAME,
        boot: SweepExportBeads::boot,
        cadence_ms: cadence_ms::<SweepExportBeads>,
        effects: sweep_export_beads::EFFECTS,
        runners: &[
            (sweep_export_beads::OBSERVE, not_ported::<SweepExportBeads>),
            (sweep_export_beads::WRITE, not_ported::<SweepExportBeads>),
            (sweep_export_beads::COMMIT, not_ported::<SweepExportBeads>),
            (sweep_export_beads::PUBLISH, not_ported::<SweepExportBeads>),
            (sweep_export_beads::TOUCH, not_ported::<SweepExportBeads>),
        ],
        keys: &[sweep_export_beads::KEYS],
        build: build::<SweepExportBeads>,
    },
    // RETIRE-CANDIDATE
    Task { name: sweep_escape_census::NAME, boot: SweepEscapeCensus::boot, cadence_ms: cadence_ms::<SweepEscapeCensus>, effects: sweep_escape_census::EFFECTS, runners: &[(sweep_escape_census::OBSERVE, not_ported::<SweepEscapeCensus>), (sweep_escape_census::REPORT, not_ported::<SweepEscapeCensus>)], keys: &[sweep_escape_census::KEYS], build: build::<SweepEscapeCensus> },
];
