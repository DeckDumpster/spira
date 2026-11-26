//! release — the release producer. Contract: DESIGN.md.
//!
//! Builds, verifies, activates, rolls back and prunes `spira-releases/<sha>/`: the one thing
//! the running system executes.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod acceptance;
pub mod activate;
pub mod artifacts;
pub mod build;
pub mod compat;
pub mod config;
pub mod config_delta;
pub mod deps_check;
pub mod fsutil;
pub mod git;
pub mod install;
pub mod intake;
pub mod machine;
pub mod manifest;
pub mod prune;
pub mod repo;
pub mod session_hook;
pub mod spira_env;
pub mod stage;
pub mod symbols;
pub mod systemctl;
pub mod telemetry;
pub mod units;
pub mod verify;
pub mod round_recert;
pub mod vm_repro;
pub mod workspace;

/// The directories every launcher's PATH ends with (DESIGN.md "Name clashes"). A release
/// executable with the same name as a command in one of these would shadow it.
pub const SYSTEM_DIRS: &[&str] = &[system_paths::USR_LOCAL_BIN, system_paths::USR_BIN, system_paths::BIN];

/// A full commit sha: 40 lowercase hex digits. Release directories are named by exactly this.
pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests;
