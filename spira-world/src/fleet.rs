//! A faithful, native port of lib.sh's `dwarves_live_total` (DESIGN.md) — NOT a
//! reimplementation of `world.sh`'s `live_dwarves`, which is a different count for a
//! different purpose (per-pid identity, to slay one) and stays in `proc.rs`. This one
//! exists because `dwarves.sh status` is a DISPLAY of the exact number the call gate
//! itself binds on — `lib.sh`'s own comment: "duplicating that logic here is how the two
//! would drift" — so it has to be the same query, not a proc scan that happens to agree
//! most of the time. Ported rather than called through a seam because it is one small,
//! self-contained branch (a systemctl count) with no
//! dependency on a `.rune` file's own bash evaluation.

/// The production branch: how many `spira-dwarf-*` units systemd knows about right now.
/// `unit_count` is injected so a test supplies a fixed list instead of a real systemd.
pub fn live_total_systemd(unit_count: impl Fn(&str) -> usize) -> u32 {
    unit_count("spira-dwarf-*") as u32
}

/// The live dwarves split by what governs them: lane dwarves count against the fleet
/// ceiling, directed ones (`dwarf direct`) run outside every cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roster {
    pub lane: u32,
    pub directed: u32,
}

impl Roster {
    pub fn of(units: &[String]) -> Roster {
        let directed = units.iter().filter(|u| runtime::units::is_directed(u)).count() as u32;
        Roster { lane: units.len() as u32 - directed, directed }
    }

    pub fn total(&self) -> u32 {
        self.lane + self.directed
    }

    /// The "live now" lines, and the spill line when lane dwarves exceed the ceiling.
    /// Only lane dwarves are measured against the ceiling.
    pub fn live_lines(&self, ceiling: Option<u32>, spill_enabled: bool) -> Vec<String> {
        let mut out = vec![format!("  live now          {}   ({} lane + {} directed)", self.total(), self.lane, self.directed)];
        if let (Some(c), true) = (ceiling, spill_enabled) {
            if self.lane > c {
                out.push(format!("  spill             lane {} / ceiling {c} — {} lane over, draining", self.lane, self.lane - c));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_total_systemd_counts_whatever_the_probe_returns() {
        assert_eq!(live_total_systemd(|pat| if pat == "spira-dwarf-*" { 3 } else { 0 }), 3);
    }

    fn units(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_directed_dwarf_is_named_directed_and_outside_the_over_cap_arithmetic() {
        let r = Roster::of(&units(&[
            "spira-dwarf-builder-1.service",
            "spira-dwarf-builder-2.service",
            "spira-dwarf-builder-3.service",
            "spira-dwarf-builder-4.service",
            "spira-dwarf-builder-directed-1794009721.service",
        ]));
        assert_eq!(r, Roster { lane: 4, directed: 1 });
        let lines = r.live_lines(Some(4), true);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("5") && lines[0].contains("4 lane + 1 directed"), "{lines:?}");
        assert!(r.live_lines(Some(3), true)[1].contains("1 lane over"));
    }
}
