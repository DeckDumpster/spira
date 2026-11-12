//! Family C, sweeps, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`.
//!
//! As in family B (`tasks::b`), each module under `c/` is a red-first stub on `b::kit`, and its
//! row waits in [`PENDING`] until its keys and mode are registered; promoting a row is moving it
//! into [`TASKS`].

use super::Task;
use crate::tasks::b::kit::{build, cadence_ms};

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
        plane: runtime::Plane::Work,
        signal: &[],
        boot: HelSweep::boot,
        cadence_ms: cadence_ms::<HelSweep>,
        effects: hel_sweep::EFFECTS,
        runners: &[
            (hel_sweep::OBSERVE, |e, _, s| hel_sweep::observe(&sweep_world(e)?, &crate::tasks::b::lc(e), &hel_sweep::Pinned::read(&pinned)?, &s.input)),
            (hel_sweep::PR_TIP, |e, _, s| hel_sweep::pr_tip(&sweep_world(e)?, &s.input)),
            (hel_sweep::SEND, |e, _, s| hel_sweep::send(&sweep_world(e)?, &crate::tasks::b::lc(e), e.units.as_deref(), &hel_sweep::Pinned::read(&pinned)?, &s.input)),
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
        plane: runtime::Plane::Work,
        signal: &[],
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
        plane: runtime::Plane::Work,
        signal: &[],
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
        plane: runtime::Plane::Work,
        signal: &[],
        boot: RoundvmAmiPrune::boot,
        cadence_ms: cadence_ms::<RoundvmAmiPrune>,
        effects: roundvm_ami_prune::EFFECTS,
        runners: &[(roundvm_ami_prune::OBSERVE, |e, _, _| roundvm_ami_prune::observe(&e.world.run_dir)), (roundvm_ami_prune::DEREGISTER, |_, _, s| roundvm_ami_prune::deregister(&s.input)), (roundvm_ami_prune::DELETE_SNAPSHOT, |_, _, s| roundvm_ami_prune::delete_snapshots(&s.input))],
        keys: &[roundvm_ami_prune::KEYS],
        build: build::<RoundvmAmiPrune>,
    },
    Task { name: roundvm_drain::NAME, plane: runtime::Plane::Work, signal: &[], boot: RoundvmDrain::boot, cadence_ms: cadence_ms::<RoundvmDrain>, effects: roundvm_drain::EFFECTS, runners: &[(roundvm_drain::OBSERVE, |e, _, _| roundvm_drain::observe(&e.world.run_dir, &pinned("SPIRA_ROUND_VM_SPILL")?, &pinned("SPIRA_ROUND_VM_WARM_IDLE_SECS")?)), (roundvm_drain::RELEASE, |_, _, s| roundvm_drain::release(&s.input))], keys: &[roundvm_drain::KEYS], build: build::<RoundvmDrain> },
    Task {
        name: sweep_reclaim::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
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
    Task { name: sweep_tsd::NAME, plane: runtime::Plane::Work, signal: &[], boot: SweepTsd::boot, cadence_ms: cadence_ms::<SweepTsd>, effects: sweep_tsd::EFFECTS, runners: &[(sweep_tsd::OBSERVE, |e, _, _| sweep_tsd::observe(&e.world.run_dir, &pinned(sweep_tsd::KEEP_DAYS_KEY)?, &pinned(sweep_tsd::BY_FAMILY_KEY)?)), (sweep_tsd::PRUNE, |e, _, s| sweep_tsd::prune(&e.world.run_dir, &s.input))], keys: &[sweep_tsd::KEYS], build: build::<SweepTsd> },
    Task { name: mail_tidy::NAME, plane: runtime::Plane::Work, signal: &[], boot: MailTidy::boot, cadence_ms: cadence_ms::<MailTidy>, effects: mail_tidy::EFFECTS, runners: &[(mail_tidy::OBSERVE, |e, _, s| serde_json::to_string(&mail_tidy::observe(&crate::tasks::b::lc(e), &s.input, mail_tidy::wall_secs())).map_err(|e| e.to_string())), (mail_tidy::ARCHIVE, |_, _, s| mail_tidy::archive(&s.input))], keys: &[mail_tidy::KEYS], build: build::<MailTidy> },
    Task {
        name: asks_moot::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: AsksMoot::boot,
        cadence_ms: cadence_ms::<AsksMoot>,
        effects: asks_moot::EFFECTS,
        runners: &[(asks_moot::OBSERVE, |e, _, _| asks_moot::observe(&crate::tasks::b::lc(e))), (asks_moot::CHECK, |_, _, s| asks_moot::check(&runtime::ports::SysProc, &s.input)), (asks_moot::RESOLVE, |e, _, s| asks_moot::resolve(&crate::tasks::b::lc(e), &s.input)), (asks_moot::WITHDRAW, |e, _, s| asks_moot::withdraw(&crate::tasks::b::lc(e), &s.input))],
        keys: &[asks_moot::KEYS],
        build: build::<AsksMoot>,
    },
    Task {
        name: sweep_archive::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: SweepArchive::boot,
        cadence_ms: cadence_ms::<SweepArchive>,
        effects: sweep_archive::EFFECTS,
        runners: &[
            (sweep_archive::OBSERVE, |_, _, _| sweep_archive::observe(&archive_paths()?)),
            (sweep_archive::STORE, |_, _, s| sweep_archive::store(&archive_paths()?, &runtime::ports::SysProc, &s.input)),
            (sweep_archive::INDEX, |_, _, _| sweep_archive::archive::index(&archive_paths()?)),
        ],
        keys: &[sweep_archive::KEYS],
        build: build::<SweepArchive>,
    },
    Task {
        name: sweep_nix_gc::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: SweepNixGc::boot,
        cadence_ms: cadence_ms::<SweepNixGc>,
        effects: sweep_nix_gc::EFFECTS,
        runners: &[
            (sweep_nix_gc::OBSERVE, |_, _, _| serde_json::to_string(&sweep_nix_gc::observe(&spira_config::process::cfg("SPIRA_NIX_ENV_LINK")?, &spira_config::process::cfg("SPIRA_NIX_PROFILE_BIN")?)).map_err(|e| e.to_string())),
            (sweep_nix_gc::COLLECT, |e, _, s| sweep_nix_gc::collect(e.units.as_deref(), &spira_config::process::cfg("SPIRA_NIX_ENV_LINK")?, &spira_config::process::cfg("SPIRA_NIX_PROFILE_BIN")?, &s.input)),
        ],
        keys: &[sweep_nix_gc::KEYS],
        build: build::<SweepNixGc>,
    },
    Task {
        name: sweep_export_beads::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: SweepExportBeads::boot,
        cadence_ms: cadence_ms::<SweepExportBeads>,
        effects: sweep_export_beads::EFFECTS,
        runners: &[
            (sweep_export_beads::OBSERVE, |e, _, _| sweep_export_beads::observe(&crate::tasks::b::lc(e).within(std::time::Duration::from_secs(300)), &spira_config::process::cfg("SPIRA_WIKI")?)),
            (sweep_export_beads::WRITE, |e, _, s| sweep_export_beads::write(&crate::tasks::b::lc(e), &spira_config::process::cfg("SPIRA_WIKI")?, &s.input, crate::tasks::c::mail_tidy::wall_secs())),
            (sweep_export_beads::COMMIT, |_, _, s| sweep_export_beads::commit(&runtime::ports::SysProc, &spira_config::process::cfg("SPIRA_WIKI")?, &spira_config::process::cfg("SPIRA_EXPORT_GIT_RETRY_WAIT")?, &s.input, crate::tasks::c::mail_tidy::wall_secs())),
            (sweep_export_beads::PUBLISH, |_, _, s| sweep_export_beads::publish(&runtime::ports::SysProc, &spira_config::process::cfg("SPIRA_WIKI")?, &s.input)),
            (sweep_export_beads::TOUCH, |_, _, _| sweep_export_beads::touch(&spira_config::process::cfg("SPIRA_WIKI")?)),
        ],
        keys: &[sweep_export_beads::KEYS],
        build: build::<SweepExportBeads>,
    },
    // RETIRE-CANDIDATE
    Task {
        name: sweep_escape_census::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: SweepEscapeCensus::boot,
        cadence_ms: cadence_ms::<SweepEscapeCensus>,
        effects: sweep_escape_census::EFFECTS,
        runners: &[(sweep_escape_census::OBSERVE, |e, _, _| sweep_escape_census::observe(&e.world.run_dir, e.world.metrics_endpoint.as_deref(), crate::tasks::c::mail_tidy::wall_secs())), (sweep_escape_census::REPORT, |e, _, s| sweep_escape_census::report(&e.world.run_dir, &s.input))],
        keys: &[sweep_escape_census::KEYS],
        build: build::<SweepEscapeCensus>,
    },
];

/// `sweep.archive`'s paths from the pinned config; the home directory names the default
/// transcript directory the sweep does not refuse on.
fn archive_paths() -> Result<sweep_archive::archive::Paths, String> {
    sweep_archive::paths(&spira_config::process::cfg, &std::env::home_dir().unwrap_or_default())
}

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

/// The family's rows not yet ported: none; every C row is in `TASKS`.
pub static PENDING: &[Task] = &[
];
