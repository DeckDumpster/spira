//! release — the release producer. Contract: DESIGN.md.
//!
//! Builds, verifies, activates, rolls back and prunes `spira-releases/<sha>/`: the one thing
//! the running system executes.

pub mod activate;
pub mod build;
pub mod config;
pub mod fsutil;
pub mod git;
pub mod manifest;
pub mod prune;
pub mod systemctl;
pub mod units;
pub mod verify;
pub mod workspace;

/// The directories every launcher's PATH ends with (DESIGN.md "Name clashes"). A release
/// executable with the same name as a command in one of these would shadow it.
pub const SYSTEM_DIRS: &[&str] = &["/usr/local/bin", "/usr/bin", "/bin"];

/// A full commit sha: 40 lowercase hex digits. Release directories are named by exactly this.
pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests;
