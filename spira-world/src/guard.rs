//! The work plane's start guard. Every work-plane service runs `world guard` as its
//! `ExecStartPre=`, so a halted world stays halted however a unit is started: by its timer,
//! by `default.target` at boot, or by hand. Stopping the timers alone is not enough, because
//! an enabled timer comes back with `timers.target` on the next boot.

use std::path::Path;

/// The refusal a work unit prints while the halt stamp is present, or `None` when the world
/// is not halted. `stamp_text` is the stamp's contents (`<since>\nwhy: <why>\n`), `None`
/// when it does not exist.
pub fn halt_refusal(stamp: &Path, stamp_text: Option<&str>) -> Option<String> {
    let text = stamp_text?;
    let mut lines = text.lines();
    let since = lines.next().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("an unknown time");
    let why = text.lines().find_map(|l| l.strip_prefix("why:")).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("unstated");
    Some(format!(
        "spira: refusing to start: the world is halted (stamp {}, since {since}, why: {why}). The halt is world's: `world stop` set it and only `world start` lifts it.",
        stamp.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_stamp_no_refusal() {
        assert_eq!(halt_refusal(Path::new("/run/world.halted"), None), None);
    }

    #[test]
    fn the_refusal_names_the_stamp_the_reason_and_what_lifts_it() {
        let r = halt_refusal(Path::new("/run/world.halted"), Some("2026-10-31T19:44:36Z\nwhy: disk pressure\n")).unwrap();
        assert!(r.contains("/run/world.halted") && r.contains("2026-10-31T19:44:36Z") && r.contains("why: disk pressure") && r.contains("world start"), "{r}");
    }

    #[test]
    fn an_empty_stamp_still_refuses() {
        let r = halt_refusal(Path::new("/run/world.halted"), Some("")).unwrap();
        assert!(r.contains("why: unstated"), "{r}");
    }
}
