//! The red-streak rule: the newest N gates of one repo all failed branch-red, on N different
//! beads, at one unit and one line. That is a red the branches share, not N branches each wrong.

use serde_json::Value;

use crate::classify::Firing;

pub const KEY_PREFIX: &str = "gate-red-streak/";

#[derive(Debug, PartialEq, Eq)]
pub struct Streak {
    pub beads: Vec<String>,
    pub unit: String,
    pub line: String,
}

fn field<'a>(row: &'a Value, k: &str) -> &'a str {
    row.get(k).and_then(Value::as_str).unwrap_or("")
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

/// With the gate rows unreadable, a streak already raised is neither confirmed nor ended: keep
/// it firing rather than clear it on a read that failed.
pub fn hold(raised: &[String], firing: &mut Vec<Firing>) {
    for key in raised.iter().filter(|k| k.starts_with(KEY_PREFIX)) {
        if !firing.iter().any(|f| &f.key == key) {
            firing.push(Firing { key: key.clone(), title: format!("{key}: landing is stopped by a red shared across branches (the gate rows are unreadable; holding)"), evidence: "The Gate section could not be read; this holds until a PASS is seen.".into() });
        }
    }
}
