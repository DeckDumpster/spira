//! Longest-processing-time-first suite order for a full-corpus round: with every slot busy,
//! the longest suite must start first or it becomes the tail the round waits on.

use std::collections::HashMap;

/// `all` ordered longest recorded median first, ties by name. A suite with no recorded time goes
/// before every timed one (an unknown is the costliest late start) and is returned second so the
/// caller can name it.
pub fn order(all: &[String], medians: &HashMap<String, f64>) -> (Vec<String>, Vec<String>) {
    let mut unknown: Vec<String> = all.iter().filter(|s| !medians.contains_key(*s)).cloned().collect();
    let mut known: Vec<&String> = all.iter().filter(|s| medians.contains_key(*s)).collect();
    unknown.sort();
    known.sort_by(|a, b| medians[*b].partial_cmp(&medians[*a]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(b)));
    let list = unknown.iter().cloned().chain(known.into_iter().cloned()).collect();
    (list, unknown)
}

/// Parses `testenv --report` output (`[{"suite","median","n"}]`).
pub fn parse_medians(json: &str) -> Result<HashMap<String, f64>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("testenv --report: not JSON: {e}"))?;
    let rows = v.as_array().ok_or("testenv --report: not a list")?;
    Ok(rows
        .iter()
        .filter_map(|r| Some((r.get("suite")?.as_str()?.to_string(), r.get("median")?.as_f64()?)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_longest_suite_is_first() {
        let m = parse_medians(r#"[{"suite":"test-a.sh","median":3.0,"n":4},{"suite":"test-b.sh","median":300.0,"n":4},{"suite":"test-c.sh","median":3.0,"n":4}]"#).unwrap();
        let (list, unknown) = order(&names(&["test-a.sh", "test-b.sh", "test-c.sh"]), &m);
        assert_eq!(list, names(&["test-b.sh", "test-a.sh", "test-c.sh"]));
        assert!(unknown.is_empty());
    }

    #[test]
    fn unrecorded_suites_go_first_and_are_named() {
        let m = parse_medians(r#"[{"suite":"test-b.sh","median":300.0,"n":4}]"#).unwrap();
        let (list, unknown) = order(&names(&["test-b.sh", "test-z.sh", "test-n.sh"]), &m);
        assert_eq!(list, names(&["test-n.sh", "test-z.sh", "test-b.sh"]));
        assert_eq!(unknown, names(&["test-n.sh", "test-z.sh"]));
    }

    #[test]
    fn the_list_covers_exactly_the_corpus() {
        let m = parse_medians(r#"[{"suite":"gone.sh","median":999.0,"n":1}]"#).unwrap();
        let all = names(&["test-a.sh", "test-b.sh"]);
        let (mut list, _) = order(&all, &m);
        list.sort();
        assert_eq!(list, all);
    }

    #[test]
    fn a_non_report_is_refused() {
        assert!(parse_medians("garbage").is_err());
        assert!(parse_medians("{}").is_err());
    }
}
