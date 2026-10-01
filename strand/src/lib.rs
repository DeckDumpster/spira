//! strand as a library (wave 4.23, sp-0ffox): the liveness/fleet-counting family
//! (lib.sh family E — `aeon_alive`, `aeon_count`, `aeons_live_total`, `aeons_live_lanes`)
//! lives in [`probe`] so other crates can call it in-process instead of each keeping its
//! own copy (`bead`, `cockpit-collect`) or bridging back into lib.sh.
//!
//! `probe::holder_alive` is itself a thin wrapper onto `sending::reap::holder_alive` — the
//! destruction chokepoint's own canonical implementation (sp-9envm) — so there is exactly
//! ONE holder-liveness predicate in the whole tree, not a third copy here.

pub mod check;
pub mod classify;
pub mod config;
pub mod model;
pub mod probe;
pub mod state;
pub mod timefmt;
