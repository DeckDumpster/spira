//! The goodput view: the pane's top line and its section rows, and loom's page, from one
//! function over the snapshot keys the collector's goodput probe writes.
//!
//! An absent key renders `?`, never 0 (law-absence-needs-a-positive-control).

use super::colors::*;
use super::model::Snapshot;

/// Loom's page width: its lines wrap here, where a phone shows them without scrolling.
pub const PHONE_COLS: i64 = 40;

pub struct View {
    pub stale: bool,
    pub top: Vec<String>,
    pub rows: Vec<String>,
}

fn or_q(v: Option<&str>) -> &str {
    v.filter(|s| !s.is_empty()).unwrap_or("?")
}

/// Cuts `text` at spaces into lines of at most `width` characters; a continuation is indented.
fn wrap(text: &str, width: i64, indent: &str) -> Vec<String> {
    let width = width.max(8) as usize;
    let mut lines: Vec<String> = Vec::new();
    let mut line = indent.to_string();
    for word in text.split(' ').filter(|w| !w.is_empty()) {
        let word: String = word.chars().take(width.saturating_sub(indent.len() + 2)).collect();
        let fresh = line.trim().is_empty();
        if !fresh && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::replace(&mut line, format!("{indent}  ")));
        } else if !fresh {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.trim().is_empty() {
        lines.push(line);
    }
    lines
}

fn top_text(snap: &Snapshot, stale: bool) -> Vec<String> {
    let age = snap.get("SP_GP_PASS_AGE_S").filter(|s| !s.is_empty());
    let pass = match (stale, age) {
        (true, Some(a)) => format!("stale pass, {a}s old"),
        (_, Some(a)) => format!("pass {a}s"),
        (_, None) => "pass ?".to_string(),
    };
    let goodput = format!("GOODPUT {}/day of {}", or_q(snap.get("SP_GP_RATE")), or_q(snap.get("SP_GP_TARGET")));
    let binding = match snap.get("SP_GP_BINDING") {
        Some("-") => "no gap open".to_string(),
        Some(b) if !b.is_empty() => {
            let mut s = format!("{b}: {}", or_q(snap.get("SP_GP_DISTANCE")));
            if let Some(step) = snap.get("SP_GP_STEP").filter(|s| !s.is_empty()) {
                let ack = snap.get("SP_GP_STEP_ACK").filter(|s| !s.is_empty()).map_or("unacked".to_string(), |a| format!("acked {a}"));
                s += &format!("; step `{step}` {ack}, horizon {}", or_q(snap.get("SP_GP_STEP_HORIZON")));
            }
            s
        }
        _ => "binding ?".to_string(),
    };
    let head = if stale { format!("{pass} \u{b7} {goodput}") } else { format!("{goodput} \u{b7} {pass}") };
    vec![head, binding]
}

fn row_text(snap: &Snapshot, i: usize) -> String {
    let k = |f: &str| snap.get(&format!("SP_GP_SEC{i}_{f}")).filter(|s| !s.is_empty());
    let age = k("AGE").map_or("age ?".to_string(), |a| format!("age {a}s"));
    format!("{} distance {} {age} {}", or_q(k("REF")), or_q(k("DIST")), or_q(k("FRESH")))
}

/// The plain lines, shared by the pane (which styles them) and loom (which prints them).
pub fn view(snap: &Snapshot, cols: i64, deadman_secs: i64) -> View {
    let stale = snap.get("SP_GP_PASS_AGE_S").and_then(|a| a.parse::<i64>().ok()).is_some_and(|a| a > deadman_secs);
    let top = top_text(snap, stale).iter().flat_map(|l| wrap(l, cols, "")).collect();
    let mut rows = vec!["SECTIONS".to_string()];
    match snap.get("SP_GP_SEC_N").and_then(|n| n.parse::<usize>().ok()) {
        Some(n) => (0..n).for_each(|i| rows.extend(wrap(&row_text(snap, i), cols, "  "))),
        None => rows[0].push_str(" ?"),
    }
    View { stale, top, rows }
}

pub fn top_lines(snap: &Snapshot, cols: i64, deadman_secs: i64) -> Vec<String> {
    let v = view(snap, cols, deadman_secs);
    let style = if v.stale { DIM } else { B };
    v.top.into_iter().map(|l| format!("{style}{l}{RST}")).collect()
}

pub fn sections_section(snap: &Snapshot, cols: i64, deadman_secs: i64) -> Vec<String> {
    view(snap, cols, deadman_secs)
        .rows
        .into_iter()
        .map(|l| match l.contains("stale") {
            true => format!("{WARN}{l}{RST}"),
            false => format!("{DIM}{l}{RST}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEADMAN: i64 = 300;

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                for n in it.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn snap(age: &str) -> Snapshot {
        Snapshot::parse(&format!(
            "SP_GP_RATE='3.2'\nSP_GP_TARGET='5'\nSP_GP_PASS_AT='2026-11-12T14:05:00Z'\nSP_GP_PASS_AGE_S='{age}'\n\
             SP_GP_BINDING='round-controller/pokebot'\nSP_GP_DISTANCE='1 repeat identity open'\n\
             SP_GP_STEP='hold-member b-17@3f2a'\nSP_GP_STEP_ACK='14:02'\nSP_GP_STEP_HORIZON='10m'\n\
             SP_GP_SEC_N='3'\n\
             SP_GP_SEC0_REF='round-controller/pokebot'\nSP_GP_SEC0_DIST='1'\nSP_GP_SEC0_AGE='12'\nSP_GP_SEC0_FRESH='fresh'\n\
             SP_GP_SEC1_REF='lifecycle/all'\nSP_GP_SEC1_DIST='0'\nSP_GP_SEC1_AGE='40'\nSP_GP_SEC1_FRESH='fresh'\n\
             SP_GP_SEC2_REF='alarms/all'\nSP_GP_SEC2_FRESH='stale'\n"
        ))
    }

    #[test]
    fn the_top_line_names_goodput_the_binding_condition_and_the_step_in_flight() {
        let top = strip(&top_lines(&snap("14"), 200, DEADMAN).join("\n"));
        assert!(top.contains("3.2/day of 5"), "{top}");
        assert!(top.contains("pass 14s"), "{top}");
        assert!(
            top.contains("round-controller/pokebot: 1 repeat identity open; step `hold-member b-17@3f2a` acked 14:02, horizon 10m"),
            "{top}"
        );
    }

    #[test]
    fn each_section_has_one_row_with_its_distance_age_and_freshness() {
        let rows = sections_section(&snap("14"), 120, DEADMAN).iter().map(|r| strip(r)).collect::<Vec<_>>();
        for (r, want) in [("round-controller/pokebot", "12s"), ("lifecycle/all", "40s")] {
            assert_eq!(rows.iter().filter(|l| l.contains(r)).count(), 1, "{rows:?}");
            assert!(rows.iter().any(|l| l.contains(r) && l.contains(want)), "{rows:?}");
        }
        let stale = rows.iter().find(|l| l.contains("alarms/all")).expect("a row for alarms/all");
        assert!(stale.contains("stale") && stale.contains("age ?") && stale.contains("distance ?"), "{stale}");
    }

    #[test]
    fn a_pass_older_than_the_dead_man_threshold_renders_dim_with_its_age() {
        let fresh = top_lines(&snap("300"), 120, DEADMAN).join("\n");
        assert!(!fresh.contains(DIM) && !strip(&fresh).contains("stale pass"), "{fresh}");
        let old = top_lines(&snap("901"), 120, DEADMAN);
        assert!(old.iter().all(|l| l.starts_with(DIM)), "{old:?}");
        assert!(strip(&old.join("\n")).contains("stale pass, 901s old"), "{old:?}");
        assert!(view(&snap("901"), 120, DEADMAN).stale && !view(&snap("300"), 120, DEADMAN).stale);
    }

    #[test]
    fn with_the_pass_record_absent_everything_reads_a_question_mark_and_nothing_reads_zero() {
        let none = Snapshot::parse("SP_GP_SEC_N='0'\n");
        let top = strip(&top_lines(&none, 120, DEADMAN).join("\n"));
        assert!(top.contains("GOODPUT ?/day of ?") && top.contains("pass ?") && top.contains("binding ?"), "{top}");
        assert!(!top.contains('0') && !top.contains("stale"), "{top}");
        let nothing = Snapshot::parse("");
        let rows = sections_section(&nothing, 120, DEADMAN).iter().map(|r| strip(r)).collect::<Vec<_>>().join("\n");
        assert!(rows.contains('?') && !rows.contains('0'), "{rows}");
    }

    #[test]
    fn no_line_is_wider_than_the_page() {
        for cols in [30, 40, 60] {
            let v = view(&snap("901"), cols, DEADMAN);
            let all: Vec<_> = v.top.iter().chain(&v.rows).collect();
            assert!(all.len() > 4, "{all:?}");
            for l in all {
                assert!(l.chars().count() as i64 <= cols, "{cols}: {l:?}");
            }
        }
        assert!(view(&snap("14"), 40, DEADMAN).top.len() > 1, "a long top line wraps instead of being cut");
    }

    #[test]
    fn a_step_not_yet_acked_says_so_and_a_pass_with_no_open_gap_names_none() {
        let unacked = Snapshot::parse("SP_GP_PASS_AGE_S='1'\nSP_GP_BINDING='a/b'\nSP_GP_DISTANCE='2 x'\nSP_GP_STEP='hold t'\nSP_GP_STEP_HORIZON='5m'\n");
        let top = strip(&top_lines(&unacked, 200, DEADMAN).join("\n"));
        assert!(top.contains("a/b: 2 x; step `hold t` unacked, horizon 5m"), "{top}");
        let idle = strip(&top_lines(&Snapshot::parse("SP_GP_PASS_AGE_S='1'\nSP_GP_BINDING='-'\n"), 200, DEADMAN).join("\n"));
        assert!(idle.contains("no gap open") && !idle.contains("binding ?"), "{idle}");
        let unread = strip(&top_lines(&Snapshot::parse("SP_GP_PASS_AGE_S='1'\nSP_GP_BINDING='a/b'\n"), 200, DEADMAN).join("\n"));
        assert!(unread.contains("a/b: ?") && !unread.contains("step"), "{unread}");
    }

    #[test]
    fn a_word_longer_than_the_page_is_cut_rather_than_overflowing() {
        let long = format!("SP_GP_PASS_AGE_S='1'\nSP_GP_BINDING='{}/x'\n", "k".repeat(80));
        for l in view(&Snapshot::parse(&long), 30, DEADMAN).top {
            assert!(l.chars().count() <= 30, "{l:?}");
        }
    }
}
