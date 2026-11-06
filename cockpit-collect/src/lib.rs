//! cockpit-collect as a library: the probe library, the supervisor's fragment lifecycle and
//! merge, and [`tiers`], the in-process face the scheduler's three cockpit tier tasks
//! (`cockpit-tier-5s`, `-60s`, `-600s`) call instead of re-executing this binary per probe.
//! `main.rs` is the CLI over the same modules.

pub mod io;
pub mod metrics;
pub mod probes;
pub mod quoting;
pub mod sparklines;
pub mod store;
pub mod supervisor;
pub mod tiers;
pub mod timeutil;
