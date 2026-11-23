//! The world's work-plane halt as a watcher sees it: a declared state, so a condition of "nothing
//! is cutting or landing" is held by it rather than paged.

use serde::{Deserialize, Serialize};

use super::iso8601;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Halt {
    pub since: u64,
    pub why: String,
}

impl Halt {
    pub fn held_text(&self, now: u64) -> String {
        format!("held by world halt since {} ({}s), why: {}", iso8601(self.since), now.saturating_sub(self.since), self.why)
    }
}
