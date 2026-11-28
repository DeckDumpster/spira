//! The red-streak rule: the newest N gates of one repo all failed branch-red, on N different
//! beads, at one unit and one line. That is a red the branches share, not N branches each wrong.

use serde_json::Value;

use crate::classify::Firing;

pub const KEY_PREFIX: &str = "gate-red-streak/";
pub const NOVERDICT_PREFIX: &str = "gate-noverdict-streak/";

#[derive(Debug, PartialEq, Eq)]
pub struct Streak {
    pub beads: Vec<String>,
    pub unit: String,
    pub line: String,
}

fn field<'a>(row: &'a Value, k: &str) -> &'a str {
    row.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Whether `key` is one the gate rows decide, so unread rows say nothing about it.
pub fn is_streak(key: &str) -> bool {
    key.starts_with(KEY_PREFIX) || key.starts_with(NOVERDICT_PREFIX)
}

/// `rows` newest first. A streak is `n` consecutive rows, since the repo's last PASS, that are
/// all branch-red FAILs on `n` different beads at one unit and one line: a NO_VERDICT or a
/// different red arriving after it does not end it, only a PASS does.
pub fn find(rows: &[Value], n: usize) -> Option<Streak> {
    let since_pass = rows.iter().position(|r| field(r, "status") == "PASS").unwrap_or(rows.len());
    rows[..since_pass].windows(n.max(1)).filter(|_| n > 0).find_map(|w| window_streak(w, n))
}

fn window_streak(window: &[Value], n: usize) -> Option<Streak> {
    if !window.iter().all(|r| field(r, "status") == "FAIL" && field(r, "reason") == "branch-red") {
        return None;
    }
    let (unit, line) = (field(&window[0], "red_unit"), field(&window[0], "red_line"));
    if !window.iter().all(|r| field(r, "red_unit") == unit && field(r, "red_line") == line) {
        return None;
    }
    let mut beads: Vec<String> = window.iter().map(|r| field(r, "bead").to_string()).collect();
    beads.sort();
    beads.dedup();
    if beads.len() != n || beads.iter().any(String::is_empty) {
        return None;
    }
    Some(Streak { beads, unit: unit.to_string(), line: line.to_string() })
}

/// The firing the streak raises, keyed per repo.
pub fn firing(repo: &str, s: &Streak) -> Firing {
    Firing {
        key: format!("{KEY_PREFIX}{repo}"),
        title: format!("Landing in {repo} is stopped by a red shared across branches: {} gates in a row failed the same way", s.beads.len()),
        evidence: format!(
            "BEADS     {}\nRED UNIT  {}\nLINE      {}\n\nThe last {} gates of {repo} failed branch-red on different beads at one unit and one line.\nBranches do not fail identically by coincidence: the red is in the base or the harness.\nIt clears itself at {repo}'s next gate PASS.",
            s.beads.join(", "), s.unit, s.line, s.beads.len()
        ),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct NoVerdictStreak {
    pub beads: Vec<String>,
    pub reason: String,
}

/// `rows` newest first. Since the repo's last PASS, the NO_VERDICT rows newest first that share
/// the newest one's reason, until `n` distinct beads are seen. A FAIL proves the gate runs, not
/// that it passes, so it is skipped; a NO_VERDICT of another reason ends the walk.
pub fn find_noverdict(rows: &[Value], n: usize) -> Option<NoVerdictStreak> {
    if n == 0 {
        return None;
    }
    let since_pass = rows.iter().position(|r| field(r, "status") == "PASS").unwrap_or(rows.len());
    let mut verdictless = rows[..since_pass].iter().filter(|r| field(r, "status") == "NO_VERDICT").peekable();
    let reason = field(verdictless.peek()?, "reason").to_string();
    let mut beads: Vec<String> = Vec::new();
    for r in verdictless.take_while(|r| field(r, "reason") == reason) {
        let bead = field(r, "bead");
        if !bead.is_empty() && !beads.iter().any(|b| b == bead) {
            beads.push(bead.to_string());
        }
        if beads.len() == n {
            beads.sort();
            return Some(NoVerdictStreak { beads, reason });
        }
    }
    None
}

pub fn firing_noverdict(repo: &str, s: &NoVerdictStreak) -> Firing {
    let n = s.beads.len();
    Firing {
        key: format!("{NOVERDICT_PREFIX}{repo}"),
        title: format!("Gate in {repo} has passed nothing: {n} gates in a row on different beads ended NO_VERDICT reason={}", s.reason),
        evidence: format!(
            "BEADS     {}\nREASON    {}\n\nThe last {n} judged gates of {repo} ended NO_VERDICT for one reason on different beads, with no PASS since.\nThe cause is on the landing ref or in the gate, not in the branches: look first at the last change landed before the first of these gates.\nIt clears at {repo}'s next gate PASS.",
            s.beads.join(", "), s.reason
        ),
    }
}

/// With the gate rows unreadable, a streak already raised is neither confirmed nor ended: keep
/// it firing rather than clear it on a read that failed.
pub fn hold(raised: &[String], firing: &mut Vec<Firing>) {
    for key in raised.iter().filter(|k| is_streak(k)) {
        if !firing.iter().any(|f| &f.key == key) {
            let what = if key.starts_with(NOVERDICT_PREFIX) { "the gate has passed nothing" } else { "landing is stopped by a red shared across branches" };
            firing.push(Firing { key: key.clone(), title: format!("{key}: {what} (the gate rows are unreadable; holding)"), evidence: "The Gate section could not be read; this holds until a PASS is seen.".into() });
        }
    }
}

#[cfg(test)]
mod noverdict_tests {
    use super::*;
    use serde_json::json;

    fn nv(bead: &str, reason: &str) -> Value {
        json!({ "bead": bead, "status": "NO_VERDICT", "reason": reason })
    }
    fn st(bead: &str, status: &str) -> Value {
        json!({ "bead": bead, "status": status, "reason": "branch-red" })
    }

    #[test_id(tl7ft60)]
    #[test]
    fn four_beads_with_one_reason_are_a_streak() {
        let rows = [nv("d", "fence-silent"), nv("c", "fence-silent"), nv("b", "fence-silent"), nv("a", "fence-silent")];
        let s = find_noverdict(&rows, 4).unwrap();
        assert_eq!((s.beads.join(","), s.reason.as_str()), ("a,b,c,d".into(), "fence-silent"));
    }

    #[test_id(tzrj4qp)]
    #[test]
    fn a_fail_between_no_verdicts_does_not_break_it() {
        let rows = [nv("d", "x"), st("e", "FAIL"), nv("c", "x"), nv("b", "x"), st("f", "FAIL"), nv("a", "x")];
        assert!(find_noverdict(&rows, 4).is_some());
    }

    #[test_id(thfdovk)]
    #[test]
    fn a_pass_ends_it_and_older_rows_do_not_count() {
        let rows = [nv("d", "x"), nv("c", "x"), st("p", "PASS"), nv("b", "x"), nv("a", "x")];
        assert_eq!(find_noverdict(&rows, 4), None);
    }

    #[test_id(tpcao6n)]
    #[test]
    fn another_reason_ends_the_walk() {
        let rows = [nv("d", "x"), nv("c", "x"), nv("b", "y"), nv("a", "x")];
        assert_eq!(find_noverdict(&rows, 4), None);
    }

    #[test_id(teqvzh3)]
    #[test]
    fn three_beads_or_one_bead_four_times_is_not_a_streak() {
        assert_eq!(find_noverdict(&[nv("a", "x"), nv("b", "x"), nv("c", "x")], 4), None);
        assert_eq!(find_noverdict(&[nv("a", "x"), nv("a", "x"), nv("a", "x"), nv("a", "x")], 4), None);
        assert_eq!(find_noverdict(&[nv("", "x"), nv("", "x"), nv("", "x"), nv("", "x")], 4), None);
    }

    #[test_id(t0mqxow)]
    #[test]
    fn zero_never_fires() {
        assert_eq!(find_noverdict(&[nv("a", "x")], 0), None);
    }
}
