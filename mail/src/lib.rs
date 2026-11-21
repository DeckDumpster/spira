//! `mail` — Maildir mailboxes for operator/concierge messages. Contract: DESIGN.md.
//! `main.rs` is the thin CLI shell; everything testable lives in these modules.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod bead;
#[cfg(test)]
mod channel_tests;
pub mod cmds;
pub mod content;
pub mod env;
pub mod kinds;
pub mod lc;
pub mod lint;
pub mod maildir;
pub mod message;
pub mod reconcile;
pub mod repeat;
pub mod rfc822;
#[cfg(test)]
mod rune_briefs_tests;
pub mod sendmail;
pub mod tidy;
#[cfg(test)]
mod verdict_tests;
