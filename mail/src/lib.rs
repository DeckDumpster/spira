//! `mail` — Maildir mailboxes for operator/concierge messages. Contract: DESIGN.md.
//! `main.rs` is the thin CLI shell; everything testable lives in these modules.

pub mod bead;
pub mod cmds;
pub mod env;
pub mod kinds;
pub mod lint;
pub mod maildir;
pub mod message;
pub mod repeat;
pub mod rfc822;
pub mod sendmail;
pub mod tidy;
