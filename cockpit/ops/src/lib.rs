//! cockpit-ops — the ops dashboard pane, the tmux layout it lives in, and the bead-answer
//! plumbing beside it.
//!
//! Replaces `cockpit/health.sh`, `cockpit/layout.sh`, `cockpit/rebuild.sh`,
//! `cockpit/resolve.sh` and `cockpit/reply.sh` (sp-llbmi). See `DESIGN.md`.
//!
//! One crate, five binaries (`health`, `layout`, `rebuild`, `resolve`, `reply`), because the
//! five scripts already shared one concern — the cockpit that is on screen right now — and a
//! crate per script would recreate the five-copies-of-one-idea problem `cockpit/db.sh`'s own
//! header names as the reason it is one function and not five.

pub mod lcview;
pub mod lctui;
pub mod conf;
pub mod db;
pub mod health;
pub mod layout;
pub mod procfs;
pub mod rebuild;
pub mod reply;
pub mod resolve;
pub mod tmux;
