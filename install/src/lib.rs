//! install — rewrite wave 6a (sp-31dm0). Contract: DESIGN.md.
//!
//! Three programs share this crate: `install` (the root installer — nine phases, preflight
//! through verify), `units-install` (the per-instance unit renderer/writer that enables,
//! restarts and prunes — `deploy.sh`'s re-render step and `install`'s own phase 4 call it),
//! and `unit-ensure` (the non-disruptive sibling `landing-pass` calls after every pass).
//! Replaces `install.sh`, `systemd/install.sh`, `systemd/unit-ensure.sh`, `systemd/units.sh`
//! and `systemd/render.py`.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
#[cfg(test)]
mod cells;
pub use release::spira_env::NIX_GC_UNIT_TASK;

pub mod bootstrap;
pub mod checkout_drift;
pub mod checks;
pub mod decide;
pub mod ensure;
pub mod guards;
pub mod install_units;
pub mod kit_bump;
/// Install's lifecycle-store phase (sp-xfqnr) lives in `release` since sp-880u4, so
/// `release stage up` builds the stage's store with the same code install does.
pub mod manifest;
pub mod orchestrate;
pub mod round_records;
pub mod seed_instance;
pub mod systemctl;
pub mod telemetry;
pub mod unit_drift;
pub mod values;
