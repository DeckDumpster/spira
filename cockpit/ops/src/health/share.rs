//! `share` — the round-robin row allocator: given a pane height, how many rows does each
//! elastic section get?
//!
//! THREE TIERS, because two round-robins cannot say all three things a section needs to say:
//! a BASE it needs, a MAX it could use, and whether it is served FIRST. Tier 0 fills every
//! `first`-tagged section to its max before anything else grows past its own first row —
//! this is the guarantee that NOW (work in flight) fills before the list sections compete for
//! space. Tier 1 then fills every remaining section to its base. Tier 2 is the slack, where
//! only a section with room above its base bids.
//!
//! EVERY SECTION KEEPS ITS FIRST ROW even with zero budget: `give[i]` starts at 1 whenever
//! `max[i] > 0`, before the budget is computed from what is left. The bottom of a short pane
//! then loses content from the BOTTOM of the frame (`render`'s job), never by a section being
//! silently allocated zero rows it never gets a chance to show at all.

#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub base: i64,
    pub max: i64,
    pub first: bool,
}

impl Spec {
    pub fn new(base: i64, max: i64, first: bool) -> Spec {
        Spec { base, max, first }
    }
}

/// `rows <= 0` means no limit (what `once` uses): every section gets everything it rendered.
pub fn share(rows: i64, fixed: i64, specs: &[Spec]) -> Vec<i64> {
    let n = specs.len();
    // A max below its base is the base — the two are computed independently at the call
    // site and a section with three rows of data must not be asked for five.
    let max: Vec<i64> = specs.iter().map(|s| s.max.max(s.base)).collect();
    let base: Vec<i64> = specs.iter().map(|s| s.base).collect();
    let first: Vec<bool> = specs.iter().map(|s| s.first).collect();
    let mut give: Vec<i64> = max.iter().map(|&m| if m > 0 { 1 } else { 0 }).collect();

    if rows <= 0 {
        return max;
    }

    let mut budget = rows - fixed - give.iter().sum::<i64>();
    for tier in 0..3 {
        loop {
            if budget <= 0 {
                break;
            }
            let mut moved = false;
            for i in 0..n {
                if budget <= 0 {
                    break;
                }
                let lim = match tier {
                    0 => {
                        if !first[i] {
                            continue;
                        }
                        max[i]
                    }
                    1 => base[i],
                    _ => max[i],
                };
                if give[i] < lim {
                    give[i] += 1;
                    budget -= 1;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
    }
    give
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_rows_returns_max_directly() {
        let specs = [Spec::new(5, 12, true), Spec::new(6, 6, false)];
        assert_eq!(share(0, 0, &specs), vec![12, 6]);
    }

    #[test]
    fn zero_or_negative_budget_gives_every_nonempty_section_its_first_row_only() {
        let specs = [Spec::new(5, 12, true), Spec::new(0, 0, false), Spec::new(5, 5, false)];
        // rows so small that fixed alone consumes the budget.
        assert_eq!(share(3, 10, &specs), vec![1, 0, 1]);
    }

    #[test]
    fn first_served_sections_fill_to_max_before_others_reach_base() {
        // NOW (first, max 12) vs NEXT/RECENT (base 6 each, not first). Budget after fixed and
        // the three initial "first row" grants: rows=20, fixed=0 -> budget = 20 - 3 = 17.
        // Tier 0 gives NOW up to 11 more (to reach 12), consuming 11 of the 17. Tier 1 then
        // gives NEXT/RECENT up to their base (5 more each) but only 6 remain, so the round
        // robin splits it 3/3.
        let specs = [Spec::new(6, 12, true), Spec::new(6, 6, false), Spec::new(6, 6, false)];
        let give = share(20, 0, &specs);
        assert_eq!(give[0], 12); // NOW reached its max under tier 0.
        assert_eq!(give[1] + give[2], 6 + 2); // whatever tier-1 budget remained, split evenly.
        assert_eq!(give[1], give[2]);
    }

    #[test]
    fn tier_two_only_grows_sections_with_room_above_base() {
        // INFLOW is the only bidder in tier two: base 6, max 41. Everything else's base ==
        // max, so once tier one satisfies every base, only INFLOW can absorb more.
        let specs = [Spec::new(12, 12, true), Spec::new(6, 6, false), Spec::new(6, 41, false)];
        let give = share(30, 0, &specs);
        assert_eq!(give[0], 12);
        assert_eq!(give[1], 6);
        assert_eq!(give[2], 12); // 30 - 12 - 6 = 12 left over, all absorbed by INFLOW.
    }

    #[test]
    fn a_section_with_nothing_rendered_gets_nothing() {
        let specs = [Spec::new(0, 0, true), Spec::new(5, 5, false)];
        let give = share(20, 0, &specs);
        assert_eq!(give[0], 0);
    }

    #[test]
    fn max_below_base_is_raised_to_base() {
        let specs = [Spec::new(5, 2, false)];
        assert_eq!(share(0, 0, &specs), vec![5]);
    }
}
