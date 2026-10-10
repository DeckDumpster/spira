//! The per-session state file — a contract with two readers (`ctx-meter.sh`'s status
//! line and `cockpit/health.sh`'s dashboard), both rendering
//! `$SPIRA_RUN/archivist/<session>.state` as flat `key=value` lines:
//!
//!   state=sweeping|archiving|safe|failed|capacity|timeout
//!   at_turn=<the session's turn count when this state was computed>
//!   items_filed=<how many beads and notes were written>
//!
//! `at_turn` is load-bearing, not bookkeeping: "safe to clear" describes the session as
//! it was when the sweep read it, and both readers demote a stale verdict to "safe as of
//! N turns ago" on this number. It is written as the work happens, never only at the end.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Sweeping,
    Archiving,
    Safe,
    Failed,
    Capacity,
    Timeout,
}

impl State {
    pub fn parse(s: &str) -> Option<State> {
        Some(match s {
            "sweeping" => State::Sweeping,
            "archiving" => State::Archiving,
            "safe" => State::Safe,
            "failed" => State::Failed,
            "capacity" => State::Capacity,
            "timeout" => State::Timeout,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            State::Sweeping => "sweeping",
            State::Archiving => "archiving",
            State::Safe => "safe",
            State::Failed => "failed",
            State::Capacity => "capacity",
            State::Timeout => "timeout",
        }
    }
}

/// The exit code `archive()` uses to say "the account refused this, and will refuse the
/// next one too" — out of the way of a real failure's own code, named so the two call
/// sites (the sweep loop, and whatever reads its exit) cannot drift apart.
pub const ARC_RC_CAPACITY: i32 = 77;

/// `crossed(next-band-name)` — how many bands a session has already crossed, from the
/// threshold it has NOT yet reached. `SP_CTX_NEXT` is the next threshold above the
/// current context, so "next is high" means `warn` is already behind it. `-1`: the meter
/// could not read it (an empty or unrecognised band name).
pub fn crossed(next: &str) -> i32 {
    match next {
        "warn" => 0,
        "high" => 1,
        "limit" => 2,
        "over" => 3,
        _ => -1,
    }
}

/// Render the three-line state file.
pub fn render(state: State, at_turn: u64, items_filed: u64) -> String {
    format!("state={}\nat_turn={at_turn}\nitems_filed={items_filed}\n", state.as_str())
}

/// Read one `key=value` line's value out of a state file's already-read text.
pub fn key(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    text.lines().find_map(|l| l.strip_prefix(prefix.as_str())).map(str::to_string)
}

pub fn write_state(dir: &Path, session: &str, state: State, at_turn: u64, items_filed: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{session}.{}", std::process::id()));
    std::fs::write(&tmp, render(state, at_turn, items_filed))?;
    std::fs::rename(&tmp, dir.join(format!("{session}.state")))
}

pub fn read_state_key(dir: &Path, session: &str, key_name: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(format!("{session}.state"))).ok()?;
    key(&text, key_name)
}

/// `<sid>.live` exists for as long as a sweep of a still-written session runs; the broker
/// refuses the archivist a work bead while any exists.
pub struct LiveMarker(std::path::PathBuf);

impl LiveMarker {
    pub fn path(dir: &Path, session: &str) -> std::path::PathBuf {
        dir.join(format!("{session}.live"))
    }

    pub fn set(dir: &Path, session: &str) -> LiveMarker {
        let p = Self::path(dir, session);
        let _ = std::fs::write(&p, "");
        LiveMarker(p)
    }
}

impl Drop for LiveMarker {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Coerce a meter or cursor value to a non-negative integer. The established
/// "unreadable" sentinel in this codebase is `-` or `?`; a caller's `unwrap_or(0)` does
/// not catch either because both are non-empty strings that would otherwise reach
/// arithmetic and panic (bash: crash the whole sweep). `None` here is "treat as 0, and
/// the caller should log why" — mirrors `arc_numeric`.
pub fn numeric_or_zero(v: Option<&str>) -> (u64, bool) {
    match v {
        Some(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => (s.parse().unwrap_or(0), true),
        _ => (0, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossed_orders_the_bands() {
        assert_eq!(crossed("warn"), 0);
        assert_eq!(crossed("high"), 1);
        assert_eq!(crossed("limit"), 2);
        assert_eq!(crossed("over"), 3);
        assert_eq!(crossed(""), -1);
        assert_eq!(crossed("garbage"), -1);
    }

    #[test]
    fn render_and_read_back_roundtrip() {
        let text = render(State::Safe, 42, 3);
        assert_eq!(key(&text, "state"), Some("safe".to_string()));
        assert_eq!(key(&text, "at_turn"), Some("42".to_string()));
        assert_eq!(key(&text, "items_filed"), Some("3".to_string()));
    }

    #[test]
    fn live_marker_exists_only_while_held() {
        let dir = testkit::TempDir::new("archivist-state");
        let m = LiveMarker::set(&dir, "sess-1");
        assert!(LiveMarker::path(&dir, "sess-1").is_file());
        drop(m);
        assert!(!LiveMarker::path(&dir, "sess-1").exists());
    }

    #[test]
    fn numeric_or_zero_accepts_plain_digits_only() {
        assert_eq!(numeric_or_zero(Some("42")), (42, true));
        assert_eq!(numeric_or_zero(Some("-")), (0, false));
        assert_eq!(numeric_or_zero(Some("?")), (0, false));
        assert_eq!(numeric_or_zero(Some("")), (0, false));
        assert_eq!(numeric_or_zero(None), (0, false));
    }

    #[test]
    fn write_and_read_state_file_roundtrip() {
        let dir = testkit::TempDir::new("archivist-state");
        write_state(&dir, "sess-1", State::Archiving, 10, 2).unwrap();
        assert_eq!(read_state_key(&dir, "sess-1", "state"), Some("archiving".to_string()));
        assert_eq!(read_state_key(&dir, "sess-1", "at_turn"), Some("10".to_string()));
    }
}
