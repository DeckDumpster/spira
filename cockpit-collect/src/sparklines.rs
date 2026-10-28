//! The BEADS section's opened/closed/landed counts and sparklines, from bead JSON and
//! landing.log alone.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::timeutil::{parse_iso, parse_strict};

const BUCKETS: usize = 8;
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn spark(b: &[i64]) -> String {
    let (lo, hi) = (*b.iter().min().unwrap(), *b.iter().max().unwrap());
    if hi == lo {
        return std::iter::repeat_n(BLOCKS[if hi == 0 { 0 } else { 3 }], b.len()).collect();
    }
    b.iter().map(|v| BLOCKS[(((v - lo) as f64 / (hi - lo) as f64 * 7.999) as usize).min(7)]).collect()
}

fn bucket(ts: f64, cut: f64) -> usize {
    (((ts - cut) / (86400.0 / BUCKETS as f64)) as usize).min(BUCKETS - 1)
}

fn field<'a>(row: &'a Value, k: &str) -> Option<&'a str> {
    row.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn when(row: &Value, k: &str) -> Option<f64> {
    field(row, k).and_then(parse_iso)
}

fn closed_when(row: &Value) -> Option<f64> {
    when(row, "closed_at")
}

/// `None` when stdin is not JSON: nothing is emitted, as before.
pub fn render(stdin: &str, landing_log: &str, now: i64) -> Option<String> {
    let d: Value = serde_json::from_str(stdin).ok()?;
    let rows: Vec<&Value> = match &d {
        Value::Array(a) => a.iter().collect(),
        v => vec![v],
    };
    let cut = now as f64;
    let cut = cut - 86400.0;
    let is_closed = |r: &&Value| field(r, "closed_at").is_some();
    let closed: Vec<&&Value> = rows.iter().filter(|r| is_closed(r) && closed_when(r).unwrap_or(cut) >= cut).collect();
    let opened = rows.iter().filter(|r| when(r, "created_at").unwrap_or(cut - 86400.0) >= cut).count();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &closed {
        *kinds.entry(field(r, "issue_type").unwrap_or("task")).or_default() += 1;
    }
    let mut kinds: Vec<_> = kinds.into_iter().collect();
    kinds.sort_by_key(|e| std::cmp::Reverse(e.1));
    let kinds_s = kinds.iter().take(4).map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ");

    let mut out = format!("SP_CLOSED_24H={}\nSP_OPENED_24H={opened}\n", closed.len());
    out += &format!("SP_CLOSED_KINDS={}\n", if kinds_s.is_empty() { "-" } else { &kinds_s });

    let mut ob = [0i64; BUCKETS];
    let mut cb = [0i64; BUCKETS];
    for r in &rows {
        if let Some(t) = when(r, "created_at").filter(|t| *t >= cut) {
            ob[bucket(t, cut)] += 1;
        }
        if is_closed(r) {
            if let Some(t) = closed_when(r).filter(|t| *t >= cut) {
                cb[bucket(t, cut)] += 1;
            }
        }
    }
    out += &format!("SP_BEADS_SPARK_OPENED={}\nSP_BEADS_SPARK_CLOSED={}\n", spark(&ob), spark(&cb));

    let landed = (!landing_log.is_empty() && std::path::Path::new(landing_log).exists())
        .then(|| std::fs::read(landing_log).ok())
        .flatten()
        .map(|b| {
            let text = String::from_utf8_lossy(&b).into_owned();
            let mut lb = [0i64; BUCKETS];
            let mut n = 0;
            for line in text.lines().filter(|l| l.contains("spira: landed")) {
                if let Some(t) = line.get(..20).and_then(parse_strict).map(|t| t as f64).filter(|t| *t >= cut) {
                    n += 1;
                    lb[bucket(t, cut)] += 1;
                }
            }
            (lb, n)
        });
    match landed {
        Some((lb, n)) => out += &format!("SP_BEADS_SPARK_LANDED={}\nSP_BEADS_LANDED_24H={n}\n", spark(&lb)),
        None => out += "SP_BEADS_SPARK_LANDED=?\nSP_BEADS_LANDED_24H=?\n",
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spark_flat_rules() {
        assert_eq!(spark(&[0; 8]), "▁▁▁▁▁▁▁▁");
        assert_eq!(spark(&[2; 8]), "▄▄▄▄▄▄▄▄");
        assert_eq!(spark(&[0, 0, 2, 0, 0, 1, 0, 0]).chars().nth(2), Some('█'));
    }

    #[test]
    fn counts_and_missing_log() {
        let now = 1788998400 + 86400;
        let at = |off: i64| {
            let t = now - 86400 + off;
            let s = t.rem_euclid(86400);
            format!("2026-09-{:02}T{:02}:{:02}:{:02}Z", 10 + t.div_euclid(86400) - 1788998400 / 86400, s / 3600, s % 3600 / 60, s % 60)
        };
        let json = format!(
            r#"[{{"issue_type":"bug","created_at":"{}","closed_at":"{}"}},{{"status":"open","created_at":"{}"}}]"#,
            at(3600),
            at(14400),
            at(57000)
        );
        let out = render(&json, "/nonexistent", now).unwrap();
        assert!(out.contains("SP_CLOSED_24H=1\nSP_OPENED_24H=2\nSP_CLOSED_KINDS=bug 1\n"), "{out}");
        assert!(out.ends_with("SP_BEADS_SPARK_LANDED=?\nSP_BEADS_LANDED_24H=?\n"));
        assert!(render("not json", "", now).is_none());
    }
}
