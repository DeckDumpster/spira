//! spira-claim's public surface for OTHER crates to link against in-process: the
//! `READY_ARGS_BASE` constant (wave 4.25, sp-obhv6, "READY_ARGS as one const"), the
//! `verdicts` the dispatcher hands the lifecycle store, so a reader asks what the dispatcher
//! decided instead of re-deriving it, and [`cli`] — every verb the binary answers, called
//! in-process ([`cli::dispatch_in`]) instead of forking `spira-claim`. The bead-attempt
//! accounting, CHECK 4 poison decision, epic-first claim selection and `unpoison` stay
//! private behind it; see DESIGN.md.

extern crate self as spira_claim;

mod audit;
pub mod cli;
mod counters;
mod deadlock;
mod decide;
mod escalation;
mod events;
mod rank;
mod readiness;
mod ready;
mod ready_cache;
mod reopen;
mod store;
mod unpoison;
pub mod verdicts;

/// `READY_ARGS`'s fixed prefix (lib.sh's own array literal) — "the one definition of a bead
/// a dwarf can take" (lib.sh:434). `cockpit-collect`'s queue probe used to keep its own copy
/// of exactly these five tokens (`probes/queue.rs`); importing this one is the fix.
pub const READY_ARGS_BASE: &[&str] = &["ready", "--limit", "0", "--exclude-type", "epic,event", "-u"];
