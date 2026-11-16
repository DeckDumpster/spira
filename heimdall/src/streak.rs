//! The red-streak rule: the newest N gates of one repo all failed branch-red, on N different
//! beads, at one unit and one line. That is a red the branches share, not N branches each wrong.

use serde_json::Value;

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

/// `rows` newest first. A window shorter than `n`, a row that is not a branch-red FAIL (a
/// PASS, NO_VERDICT, BASE_FAIL), a repeated bead, or two different reds is no streak.
pub fn find(rows: &[Value], n: usize) -> Option<Streak> {
    if n == 0 || rows.len() < n {
        return None;
    }
    let window = &rows[..n];
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
