//! install — rewrite wave 6a (sp-31dm0). Contract: DESIGN.md.
//!
//! Three programs share this crate: `install` (the root installer — nine phases, preflight
//! through verify), `units-install` (the per-instance unit renderer/writer that enables,
//! restarts and prunes — `deploy.sh`'s re-render step and `install`'s own phase 4 call it),
//! and `unit-ensure` (the non-disruptive sibling `landing-pass` calls after every pass).
//! Replaces `install.sh`, `systemd/install.sh`, `systemd/unit-ensure.sh`, `systemd/units.sh`
//! and `systemd/render.py`.

pub mod bootstrap;
pub mod checks;
pub mod decide;
pub mod ensure;
pub mod guards;
pub mod install_units;
pub mod manifest;
pub mod orchestrate;
pub mod seed_instance;
pub mod systemctl;
pub mod values;
