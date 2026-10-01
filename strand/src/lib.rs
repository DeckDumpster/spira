//! strand as a library (wave 4.23, sp-0ffox): the liveness/fleet-counting family
//! (lib.sh family E — `aeon_alive`, `aeon_count`, `aeons_live_total`, `aeons_live_lanes`)
//! lives in [`probe`] so other crates can call it in-process instead of each keeping its
//! own copy (`bead`, `cockpit-collect`) or bridging back into lib.sh.
//!
//! `probe::holder_alive` is itself a thin wrapper onto `sending::reap::holder_alive` — the
//! destruction chokepoint's own canonical implementation (sp-9envm) — so there is exactly
//! ONE holder-liveness predicate in the whole tree, not a third copy here.
//!
//! [`detectors`] (wave 4.29, sp-8ofmt) is the strand-side half of lib.sh family T, the
//! stranded-work detectors: `detect_livelocked` and its STATE-scan siblings, plus
//! `all_partition_members`. `groomer`, `maechen-trigger` and `cockpit-collect` call it
//! in-process instead of shelling back into lib.sh.

pub mod check;
pub mod classify;
pub mod config;
pub mod detectors;
pub mod model;
pub mod probe;
pub mod state;
pub mod timefmt;
