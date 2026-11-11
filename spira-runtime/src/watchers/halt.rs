//! The world's work-plane halt as a watcher sees it: a declared state, so a condition of "nothing
//! is cutting or landing" is held by it rather than paged.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::iso8601;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Halt {
    pub since: u64,
    pub why: String,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Halt {
    /// The work plane's stamp under `run_dir`, if the plane is halted. The stamp's age is its
    /// mtime; the present stands in when the filesystem will not say.
    pub fn read(run_dir: &Path) -> Option<Halt> {
        let stamp = run_dir.join("world.halted");
        let meta = std::fs::metadata(&stamp).ok()?;
        let since = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or_else(now, |d| d.as_secs());
        let text = std::fs::read_to_string(&stamp).unwrap_or_default();
        let why = text.lines().find_map(|l| l.strip_prefix("why: ")).unwrap_or("unstated").to_string();
        Some(Halt { since, why })
    }

    pub fn held_text(&self, now: u64) -> String {
        format!("held by world halt since {} ({}s), why: {}", iso8601(self.since), now.saturating_sub(self.since), self.why)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stamp_gives_the_halts_reason_and_a_missing_stamp_is_no_halt() {
        let tmp = testkit::TempDir::new("halt-read");
        let dir = tmp.path().to_path_buf();
        assert_eq!(Halt::read(&dir), None);
        std::fs::write(dir.join("world.halted"), "2026-11-11T05:10:52Z\nwhy: operator order: freeze\n").unwrap();
        let h = Halt::read(&dir).expect("halted");
        assert_eq!(h.why, "operator order: freeze");
        assert!(h.since > 1_000_000_000, "{h:?}");
    }
}
