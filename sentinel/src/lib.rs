//! The sentinel's pure core as a library, for the scheduler's `sentinel-call` and
//! `sentinel-pass` tasks: what the binary decides, without the binary's IO, and the
//! in-process doors its reads go through.

pub mod checks;
pub mod model;
pub mod plan;

/// The lifecycle events the checks write.
pub mod lifecycle_events {
    pub const HOLDER_DEAD: &str = "\"HolderDead\"";
}

/// The account's capacity pause, read as the binary reads it (`dwarf` owns the probe).
pub use dwarf::capacity;
/// Whether a dwarf's pidfile names a live process.
pub use hel::reap::dwarf_alive;
/// hel's holder verbs: whether a bead's holder lives, and stopping it through the Units port.
pub use hel::verbs as hel_verbs;
/// spira-claim's verbs in-process (`dispatch_in`), the machine's own ready answers.
pub use spira_claim::cli as claim;
