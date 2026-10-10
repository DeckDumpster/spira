//! A faithful, native port of lib.sh's `aeons_live_total` (DESIGN.md) — NOT a
//! reimplementation of `world.sh`'s `live_aeons`, which is a different count for a
//! different purpose (per-pid identity, to slay one) and stays in `proc.rs`. This one
//! exists because `aeons.sh status` is a DISPLAY of the exact number the summon gate
//! itself binds on — `lib.sh`'s own comment: "duplicating that logic here is how the two
//! would drift" — so it has to be the same query, not a proc scan that happens to agree
//! most of the time. Ported rather than called through a seam because it is one small,
//! self-contained branch (a systemctl count) with no
//! dependency on a `.fayth` file's own bash evaluation.

/// The production branch: how many `spira-aeon-*` units systemd knows about right now.
/// `unit_count` is injected so a test supplies a fixed list instead of a real systemd.
pub fn live_total_systemd(unit_count: impl Fn(&str) -> usize) -> u32 {
    unit_count("spira-aeon-*") as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_total_systemd_counts_whatever_the_probe_returns() {
        assert_eq!(live_total_systemd(|pat| if pat == "spira-aeon-*" { 3 } else { 0 }), 3);
    }
}
