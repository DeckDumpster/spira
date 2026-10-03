//! What is stuck, and for how long (UC-26, sp-fhzib). Reads `gate.log`'s `waited=Ns` rows
//! inside a trailing window and reports the longest. The window is a DURATION, never a row
//! count, and no row inside it renders `?`, not `0` — "no gate has waited recently" and
//! "no gate has RUN recently" are opposite facts (law-absence-needs-a-positive-control).
//!
//! The cutoff is a STRING comparison: `land_mark` writes `date -u +%Y-%m-%dT%H:%M:%SZ`, and
//! fixed-width ISO-8601 UTC timestamps sort lexicographically in chronological order, so no
//! date parsing is needed here either.

use crate::log::fmt_iso;

pub struct GateWait {
    pub oldest_wait: Option<i64>,
    pub oldest_branch: String,
}

fn is_iso_timestamp(s: &str) -> bool {
    // `^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:]+Z$`
    let b = s.as_bytes();
    if b.len() < 11 {
        return false;
    }
    let digit = |i: usize| b.get(i).map(|c| c.is_ascii_digit()).unwrap_or(false);
    (0..4).all(digit) && b[4] == b'-' && (5..7).all(digit) && b[7] == b'-' && (8..10).all(digit) && b[10] == b'T'
        && s.ends_with('Z')
        && s[11..s.len() - 1].bytes().all(|c| c.is_ascii_digit() || c == b':')
}

fn waited_seconds(line: &str) -> Option<i64> {
    let idx = line.find("waited=")?;
    let rest = &line[idx + "waited=".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

pub fn compute(gate_log_text: &str, now: i64, window_s: i64) -> GateWait {
    let since = fmt_iso(now - window_s);
    let mut best: Option<(i64, String)> = None;
    for line in gate_log_text.lines() {
        let mut fields = line.split_whitespace();
        let Some(ts) = fields.next() else { continue };
        if ts < since.as_str() || !is_iso_timestamp(ts) {
            continue;
        }
        let Some(w) = waited_seconds(line) else {
            continue;
        };
        let branch = line.split_whitespace().nth(2).unwrap_or("").to_string();
        match &best {
            Some((bw, _)) if *bw >= w => {}
            _ => best = Some((w, branch)),
        }
    }
    match best {
        Some((w, b)) => GateWait {
            oldest_wait: Some(w),
            oldest_branch: b,
        },
        None => GateWait {
            oldest_wait: None,
            oldest_branch: String::new(),
        },
    }
}

pub struct Silence {
    pub submitted: Vec<String>,
    pub last_gate: Option<String>,
}

/// A bead reached SUBMITTED inside the window while `gate.log` gained no line in it. The
/// last gate timestamp is taken over the whole log, not the window, so the alarm can name it.
pub fn silence(gate_log_text: &str, submitted_beads: &[crate::lc::BeadRow], now: i64, window_s: i64) -> Option<Silence> {
    let since = fmt_iso(now - window_s);
    let mut last_gate: Option<&str> = None;
    for line in gate_log_text.lines() {
        let Some(ts) = line.split_whitespace().next() else { continue };
        if is_iso_timestamp(ts) && last_gate.map(|l| ts > l).unwrap_or(true) {
            last_gate = Some(ts);
        }
    }
    if last_gate.map(|l| l >= since.as_str()).unwrap_or(false) {
        return None;
    }
    let mut submitted: Vec<String> = submitted_beads
        .iter()
        .filter(|r| r.since.map(|a| a >= now - window_s).unwrap_or(false))
        .map(|r| r.id.clone())
        .collect();
    if submitted.is_empty() {
        return None;
    }
    submitted.sort();
    Some(Silence { submitted, last_gate: last_gate.map(str::to_string) })
}

pub fn disp(oldest_wait: Option<i64>) -> String {
    match oldest_wait {
        Some(w) => format!("{w}s"),
        None => "?".to_string(),
    }
}

pub fn window_label(window_s: i64) -> String {
    if window_s >= 3600 {
        format!("last {}h", window_s / 3600)
    } else {
        format!("last {}m", window_s / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_100_000;

    #[test]
    fn picks_the_longest_wait_inside_the_window() {
        let t1 = fmt_iso(NOW - 100);
        let t2 = fmt_iso(NOW - 50);
        let log = format!(
            "{t1} gate sp-a waited=30s ok\n{t2} gate sp-b waited=90s ok\n"
        );
        let gw = compute(&log, NOW, 21600);
        assert_eq!(gw.oldest_wait, Some(90));
        assert_eq!(gw.oldest_branch, "sp-b");
    }

    #[test]
    fn rows_outside_the_window_are_excluded() {
        let old = fmt_iso(NOW - 100_000);
        let log = format!("{old} gate sp-old waited=999s ok\n");
        let gw = compute(&log, NOW, 21600);
        assert_eq!(gw.oldest_wait, None);
    }

    #[test]
    fn no_rows_in_window_renders_unknown_not_zero() {
        let gw = compute("", NOW, 21600);
        assert_eq!(gw.oldest_wait, None);
        assert_eq!(disp(gw.oldest_wait), "?");
    }

    #[test]
    fn window_label_switches_from_minutes_to_hours() {
        assert_eq!(window_label(1800), "last 30m");
        assert_eq!(window_label(21600), "last 6h");
    }

    #[test]
    fn a_line_with_no_waited_field_is_ignored() {
        let t1 = fmt_iso(NOW - 10);
        let log = format!("{t1} gate sp-a no-wait-field-here\n");
        let gw = compute(&log, NOW, 21600);
        assert_eq!(gw.oldest_wait, None);
    }

    fn rec(id: &str, at: i64) -> crate::lc::BeadRow {
        crate::lc::BeadRow { id: id.into(), since: Some(at), ..Default::default() }
    }

    #[test]
    fn submission_with_a_stale_gate_log_is_silence_and_names_the_last_gate() {
        let old = fmt_iso(NOW - 50_000);
        let log = format!("{old} gate sp-a waited=1s ok\n");
        let s = silence(&log, &[rec("sp-b", NOW - 600)], NOW, 3600).expect("silence");
        assert_eq!(s.submitted, vec!["sp-b"]);
        assert_eq!(s.last_gate.as_deref(), Some(old.as_str()));
    }

    #[test]
    fn a_fresh_gate_line_is_not_silence() {
        let log = format!("{} gate sp-a waited=1s ok\n", fmt_iso(NOW - 60));
        assert!(silence(&log, &[rec("sp-b", NOW - 600)], NOW, 3600).is_none());
    }

    #[test]
    fn no_submission_in_the_window_is_not_silence() {
        let log = format!("{} gate sp-a waited=1s ok\n", fmt_iso(NOW - 50_000));
        let recs = [rec("sp-b", NOW - 7200), rec("sp-c", NOW - 8000)];
        assert!(silence(&log, &recs, NOW, 3600).is_none());
    }

    #[test]
    fn an_absent_gate_log_with_a_submission_is_silence_with_no_last_gate() {
        let s = silence("", &[rec("sp-b", NOW - 60)], NOW, 3600).expect("silence");
        assert_eq!(s.last_gate, None);
    }
}
