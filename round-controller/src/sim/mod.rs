//! The scenario harness: a scenario file seeds the store, scripts the queue verbs and the round
//! VM, runs `pass` on a virtual clock and asserts counts, under every profile in
//! `scenarios/profiles.toml`.

pub mod scenario;
pub mod verbs;
pub mod vm;

pub use scenario::{harness_seed, parse, profiles, run, run_all, Profile, Report, Scenario};
pub use verbs::{Call, FakeVerbs};
pub use vm::{FakeVm, VmRun};
