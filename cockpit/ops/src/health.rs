//! `health` — the cockpit's right-hand column: the Spira ops dashboard. Replaces
//! `cockpit/health.sh` (sp-llbmi). See `../DESIGN.md`.
//!
//! ```text
//!     health             repaint every 2s forever (pane mode)
//!     health once [rows [cols]]
//!     health render-many <dir> [rows [cols]]
//! ```
//!
//! Reads the snapshot `spira/cockpit.sh` writes under `$SPIRA_RUN`, never calls `bd` or
//! `git` itself, and never clears the screen (see `frame::paint`'s module doc for why).

pub mod colors;
pub mod fmt;
pub mod frame;
pub mod model;
pub mod sections;
pub mod share;
