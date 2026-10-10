//! The round's suite list, ordered by the caller: longest-first from the host's recorded
//! wall times. The VM runs the list exactly as given, so an order that is wrong here is wrong
//! everywhere, and a suite with no record never falls back to alphabetical.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const BATCH_ROW: &str = "__batch__";

#[derive(Debug, PartialEq)]
pub struct Ordered {
    pub list: Vec<String>,
    /// Suites with no recorded wall time, scheduled first.
    pub unmeasured: Vec<String>,
}

fn mean_wall(history: &str) -> BTreeMap<String, f64> {
    let mut acc: BTreeMap<String, (f64, u64)> = BTreeMap::new();
    for l in history.lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
        let (Some(suite), Some(wall)) = (v.get("suite").and_then(Value::as_str), v.get("wall_secs").and_then(Value::as_f64)) else { continue };
        if suite == BATCH_ROW || !wall.is_finite() {
            continue;
        }
        let e = acc.entry(suite.to_string()).or_insert((0.0, 0));
        e.0 += wall;
        e.1 += 1;
    }
    acc.into_iter().map(|(k, (s, n))| (k, s / n as f64)).collect()
}

/// Every suite in `tree`: unmeasured ones first (tree order), then longest mean wall first,
/// ties in tree order.
pub fn longest_first(tree: &[String], history: &str) -> Ordered {
    let mean = mean_wall(history);
    let (mut unmeasured, mut measured): (Vec<&String>, Vec<&String>) = tree.iter().partition(|s| !mean.contains_key(*s));
    measured.sort_by(|a, b| mean[*b].partial_cmp(&mean[*a]).unwrap_or(std::cmp::Ordering::Equal));
    let list = unmeasured.iter().chain(measured.iter()).map(|s| s.to_string()).collect();
    Ordered { list, unmeasured: unmeasured.drain(..).cloned().collect() }
}

/// A list that does not name every suite in the tree is refused, naming what is missing.
pub fn covers(tree: &[String], list: &[String]) -> Result<(), String> {
    let have: BTreeSet<&String> = list.iter().collect();
    let missing: Vec<&str> = tree.iter().filter(|s| !have.contains(s)).map(String::as_str).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("suite list omits {} of {} suites in the tree: {}", missing.len(), tree.len(), missing.join(",")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str, w: u64) -> String {
        format!("{{\"suite\":\"{s}\",\"wall_secs\":{w}}}\n")
    }
    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn longest_first_by_mean_history_with_batch_rows_ignored() {
        let h = [row("test-a.sh", 5), row("test-b.sh", 50), row("test-c.sh", 40), row("test-c.sh", 80), row(BATCH_ROW, 999)].concat();
        let o = longest_first(&names(&["test-a.sh", "test-b.sh", "test-c.sh"]), &h);
        assert_eq!(o.list, names(&["test-c.sh", "test-b.sh", "test-a.sh"]));
        assert!(o.unmeasured.is_empty());
    }

    #[test]
    fn an_unmeasured_suite_goes_first_and_is_named() {
        let h = [row("test-a.sh", 5), row("test-b.sh", 50)].concat();
        let o = longest_first(&names(&["test-a.sh", "test-b.sh", "test-new.sh"]), &h);
        assert_eq!(o.list, names(&["test-new.sh", "test-b.sh", "test-a.sh"]));
        assert_eq!(o.unmeasured, names(&["test-new.sh"]));
    }

    #[test]
    fn no_history_at_all_is_every_suite_unmeasured_not_alphabetical_silence() {
        let o = longest_first(&names(&["test-a.sh", "test-b.sh"]), "");
        assert_eq!(o.unmeasured.len(), 2);
    }

    #[test]
    fn an_incomplete_list_refuses_and_names_the_missing() {
        let tree = names(&["test-a.sh", "test-b.sh"]);
        let e = covers(&tree, &names(&["test-a.sh"])).unwrap_err();
        assert!(e.contains("test-b.sh"), "{e}");
        assert!(covers(&tree, &names(&["test-b.sh", "test-a.sh"])).is_ok());
    }
}
