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

pub static TASKS: &[Task] = &[];

/// The family's rows, complete but not yet in `TASKS`.
pub static PENDING: &[Task] = &[
    Task {
        name: hel_sweep::NAME,
        boot: HelSweep::boot,
        cadence_ms: cadence_ms::<HelSweep>,
        effects: hel_sweep::EFFECTS,
        runners: &[
            (hel_sweep::OBSERVE, not_ported::<HelSweep>),
            (hel_sweep::PR_TIP, not_ported::<HelSweep>),
            (hel_sweep::SEND, not_ported::<HelSweep>),
            (hel_sweep::CONTENT_ON_BASE, not_ported::<HelSweep>),
            (hel_sweep::CLOSE_ON_LAND, not_ported::<HelSweep>),
            (hel_sweep::ARCHIVE, not_ported::<HelSweep>),
            (hel_sweep::FREE_WORKTREE, not_ported::<HelSweep>),
            (hel_sweep::ORPHAN_WORKTREE, not_ported::<HelSweep>),
            (hel_sweep::PRUNE, not_ported::<HelSweep>),
        ],
        keys: &[hel_sweep::KEYS],
        build: build::<HelSweep>,
    },
    Task {
        name: hel_reap_terminal::NAME,
        boot: HelReapTerminal::boot,
        cadence_ms: cadence_ms::<HelReapTerminal>,
        effects: hel_reap_terminal::EFFECTS,
        runners: &[(hel_reap_terminal::OBSERVE, not_ported::<HelReapTerminal>), (hel_reap_terminal::DESTROY_WORKTREE, not_ported::<HelReapTerminal>), (hel_reap_terminal::REMOVE_SCRATCH, not_ported::<HelReapTerminal>), (hel_reap_terminal::PRUNE, not_ported::<HelReapTerminal>)],
        keys: &[hel_reap_terminal::KEYS],
        build: build::<HelReapTerminal>,
    },
    Task {
        name: hel_reap_stale::NAME,
        boot: HelReapStale::boot,
        cadence_ms: cadence_ms::<HelReapStale>,
        effects: hel_reap_stale::EFFECTS,
        runners: &[(hel_reap_stale::OBSERVE, not_ported::<HelReapStale>), (hel_reap_stale::DESTROY_WORKTREE, not_ported::<HelReapStale>), (hel_reap_stale::PRUNE, not_ported::<HelReapStale>)],
        keys: &[hel_reap_stale::KEYS],
        build: build::<HelReapStale>,
    },
    Task {
        name: sweep_reclaim::NAME,
        boot: SweepReclaim::boot,
        cadence_ms: cadence_ms::<SweepReclaim>,
        effects: sweep_reclaim::EFFECTS,
        runners: &[(sweep_reclaim::OBSERVE, not_ported::<SweepReclaim>), (sweep_reclaim::REMOVE_TARGET, not_ported::<SweepReclaim>), (sweep_reclaim::PODMAN_PRUNE, not_ported::<SweepReclaim>), (sweep_reclaim::TRIM_CACHE, not_ported::<SweepReclaim>)],
        keys: &[sweep_reclaim::KEYS],
        build: build::<SweepReclaim>,
    },
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
    Task { name: sweep_tsd::NAME, boot: SweepTsd::boot, cadence_ms: cadence_ms::<SweepTsd>, effects: sweep_tsd::EFFECTS, runners: &[(sweep_tsd::OBSERVE, not_ported::<SweepTsd>), (sweep_tsd::PRUNE, not_ported::<SweepTsd>)], keys: &[sweep_tsd::KEYS], build: build::<SweepTsd> },
    Task { name: roundvm_drain::NAME, boot: RoundvmDrain::boot, cadence_ms: cadence_ms::<RoundvmDrain>, effects: roundvm_drain::EFFECTS, runners: &[(roundvm_drain::OBSERVE, not_ported::<RoundvmDrain>), (roundvm_drain::RELEASE, not_ported::<RoundvmDrain>)], keys: &[roundvm_drain::KEYS], build: build::<RoundvmDrain> },
    Task {
        name: roundvm_ami_prune::NAME,
        boot: RoundvmAmiPrune::boot,
        cadence_ms: cadence_ms::<RoundvmAmiPrune>,
        effects: roundvm_ami_prune::EFFECTS,
        runners: &[(roundvm_ami_prune::OBSERVE, not_ported::<RoundvmAmiPrune>), (roundvm_ami_prune::DEREGISTER, not_ported::<RoundvmAmiPrune>), (roundvm_ami_prune::DELETE_SNAPSHOT, not_ported::<RoundvmAmiPrune>)],
        keys: &[roundvm_ami_prune::KEYS],
        build: build::<RoundvmAmiPrune>,
    },
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
