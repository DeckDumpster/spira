//! The goodput view: the pane's top line and its section rows, and loom's page, from one
//! function over the snapshot keys the collector's goodput probe writes.
//!
//! An absent key renders `?`, never 0 (law-absence-needs-a-positive-control).

use super::colors::*;
use super::model::Snapshot;

pub struct View {
    pub stale: bool,
    pub top: Vec<String>,
    pub rows: Vec<String>,
}

pub fn view(_snap: &Snapshot, _cols: i64, _deadman_secs: i64) -> View {
    View { stale: false, top: Vec::new(), rows: Vec::new() }
}

pub fn top_lines(_snap: &Snapshot, _cols: i64, _deadman_secs: i64) -> Vec<String> {
    let _ = (DIM, RST);
    Vec::new()
}

pub fn sections_section(_snap: &Snapshot, _cols: i64, _deadman_secs: i64) -> Vec<String> {
    Vec::new()
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
        let top = strip(&top_lines(&snap("14"), 120, DEADMAN).join("\n"));
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
        assert!(top.contains("GOODPUT ?") && top.contains("pass ?"), "{top}");
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
}
