//! `queue stats` — caught/escaped/batch/cost totals from landing.log's QUEUE lines
//! (DESIGN.md §3.5). Pure: text in, report out.

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub caught: u64,
    pub escaped: u64,
    pub batches: u64,
    pub members: u64,
    pub cost: u64,
    pub local_batches: u64,
    pub local_red: u64,
}

/// The digits right after `key=` (queue.sh's `sed 's/.*key=\([0-9]*\).*/\1/'`, which takes
/// the LAST occurrence because `.*` is greedy). Non-numeric or absent → 0.
fn num_after(line: &str, key: &str) -> u64 {
    let Some(pos) = line.rfind(key) else { return 0 };
    let digits: String = line[pos + key.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

/// `verdict=<[a-z]*>`, present only on local-gate lines.
fn verdict(line: &str) -> Option<String> {
    let pos = line.rfind("verdict=")?;
    let v: String = line[pos + 8..].chars().take_while(|c| c.is_ascii_lowercase()).collect();
    // queue.sh tested `[ -n "$_v" ]`: an empty verdict= is a CI line.
    (!v.is_empty()).then_some(v)
}

/// `cost=<n>s` — only counted when the digits are followed by `s`.
fn cost(line: &str) -> u64 {
    let Some(pos) = line.rfind("cost=") else { return 0 };
    let rest = &line[pos + 5..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if rest[digits.len()..].starts_with('s') {
        digits.parse().unwrap_or(0)
    } else {
        0
    }
}

pub fn tally(log: &str) -> Stats {
    let mut s = Stats::default();
    for line in log.lines() {
        if line.starts_with("QUEUE CAUGHT ") {
            s.caught += 1;
        } else if line.starts_with("QUEUE ESCAPED ") {
            s.escaped += 1;
        } else if line.starts_with("QUEUE BATCH ") {
            s.batches += 1;
            s.members += num_after(line, "members=");
            match verdict(line) {
                // A local-gate line: a non-empty `verdict=`.
                Some(v) => {
                    s.local_batches += 1;
                    if v == "red" {
                        s.local_red += 1;
                    }
                    s.cost += num_after(line, "gate_seconds=");
                }
                None => s.cost += cost(line),
            }
        } else if line.starts_with("QUEUE GATE_COST ") {
            s.cost += num_after(line, "seconds=");
        }
    }
    s
}

pub fn render(s: &Stats) -> String {
    let avg = if s.members > 0 { s.cost / s.members } else { 0 };
    let pct = if s.local_batches > 0 { s.local_red * 100 / s.local_batches } else { 0 };
    format!(
        "caught:          {}\nescaped:         {}\nbatches:         {} ({} members)\nlocal_red_rate:  {}/{} ({}%)\ncost:            {}s avg per branch\n",
        s.caught, s.escaped, s.batches, s.members, s.local_red, s.local_batches, pct, avg
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_log_is_all_zero() {
        assert_eq!(
            render(&tally("")),
            "caught:          0\nescaped:         0\nbatches:         0 (0 members)\nlocal_red_rate:  0/0 (0%)\ncost:            0s avg per branch\n"
        );
    }

    #[test]
    fn local_and_ci_batch_lines_are_costed_differently() {
        let log = "\
QUEUE CAUGHT 1 branch=sp-a
QUEUE GATE_COST 2 branch=sp-b seconds=30
QUEUE BATCH 3 repo=spira members=2 gate_seconds=100 verdict=red
QUEUE BATCH 4 repo=spira members=2 gate_seconds=0 verdict=green source=open-batch
QUEUE BATCH 5 repo=spira members=1 cost=50s
QUEUE ESCAPED 6 x
QUEUE PUBLISH 7 repo=spira pr=3 members=2
other line
";
        let s = tally(log);
        assert_eq!(s, Stats { caught: 1, escaped: 1, batches: 3, members: 5, cost: 180, local_batches: 2, local_red: 1 });
        let r = render(&s);
        assert!(r.contains("batches:         3 (5 members)"));
        assert!(r.contains("local_red_rate:  1/2 (50%)"));
        assert!(r.contains("cost:            36s avg per branch"));
    }

    #[test]
    fn gate_cost_seconds_key_is_not_confused_with_gate_seconds() {
        // "seconds=" also appears inside "gate_seconds="; GATE_COST lines never carry both.
        assert_eq!(tally("QUEUE GATE_COST 2 branch=x seconds=7").cost, 7);
    }
}
