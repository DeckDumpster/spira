//! `tsd_keys` — the ops pane's flow headlines, read from `tsd-query.sh where` and `rework`.
//! A query that fails, prints nothing parseable or names no known fields is `?`, never `0`.

use super::{push, Kv};
use crate::io;
use serde_json::Value;

pub fn tsd_keys() -> Kv {
    tsd_keys_with(|q| io::run_tool("tsd-query.sh", &[q], None))
}

fn tsd_keys_with(run: impl Fn(&str) -> Option<String>) -> Kv {
    let mut out = Kv::new();
    push(&mut out, "SP_TSD_WHERE", rows(run("where")).and_then(|r| where_line(&r)).unwrap_or_else(unread));
    push(&mut out, "SP_TSD_REWORK", rows(run("rework")).and_then(|r| rework_line(&r)).unwrap_or_else(unread));
    out
}

fn unread() -> String {
    "?".to_string()
}

fn rows(raw: Option<String>) -> Option<Vec<Value>> {
    match serde_json::from_str::<Value>(raw?.trim()).ok()? {
        Value::Array(a) => Some(a),
        _ => None,
    }
}

fn mins(secs: i64) -> String {
    if secs >= 3600 { format!("{}h", secs / 3600) } else { format!("{}m", secs / 60) }
}

fn where_line(rows: &[Value]) -> Option<String> {
    let mut parts = Vec::new();
    for r in rows {
        let state = r.get("state")?.as_str()?;
        let wip = r.get("wip")?.as_i64()?;
        let dwell = r.get("dwell_p50_s")?.as_i64()?;
        parts.push(format!("{state} {wip} ({})", mins(dwell)));
    }
    Some(if parts.is_empty() { "nothing in flight".to_string() } else { parts.join(" \u{b7} ") })
}

fn rework_line(rows: &[Value]) -> Option<String> {
    let total = rows.iter().find(|r| r.get("reason").and_then(Value::as_str) == Some("*"))?;
    let reopens = total.get("reopens")?.as_i64()?;
    let landed = total.get("landed")?.as_i64()?;
    let mut line = format!("{reopens} reopened / {landed} landed");
    let top: Vec<String> = rows
        .iter()
        .filter(|r| r.get("reason").and_then(Value::as_str) != Some("*"))
        .take(3)
        .filter_map(|r| Some(format!("{} {}", r.get("reason")?.as_str()?, r.get("reopens")?.as_i64()?)))
        .collect();
    if !top.is_empty() {
        line.push_str(&format!("  {}", top.join(", ")));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(kv: &'a Kv, k: &str) -> &'a str {
        &kv.iter().find(|(key, _)| key == k).unwrap().1
    }

    #[test]
    fn known_rows_render_the_headlines() {
        let kv = tsd_keys_with(|q| {
            Some(match q {
                "where" => r#"[{"state":"WORKING","wip":3,"dwell_p50_s":720,"dwell_max_s":900},{"state":"REWORK","wip":1,"dwell_p50_s":7300,"dwell_max_s":7300}]"#,
                _ => r#"[{"reason":"*","reopens":3,"landed":9},{"reason":"gate-red","reopens":2,"landed":9},{"reason":"ejected","reopens":1,"landed":9}]"#,
            }
            .to_string())
        });
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "WORKING 3 (12m) \u{b7} REWORK 1 (2h)");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "3 reopened / 9 landed  gate-red 2, ejected 1");
    }

    #[test]
    fn a_failed_query_is_a_question_mark_never_zero() {
        let kv = tsd_keys_with(|_| None);
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "?");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "?");
    }

    #[test]
    fn unparseable_or_foreign_output_is_a_question_mark() {
        let kv = tsd_keys_with(|q| Some(if q == "where" { "Conversion Error: nope".into() } else { r#"[{"x":1}]"#.into() }));
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "?");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "?");
    }

    #[test]
    fn an_empty_pipeline_says_so() {
        let kv = tsd_keys_with(|_| Some("[]".to_string()));
        assert_eq!(get(&kv, "SP_TSD_WHERE"), "nothing in flight");
        assert_eq!(get(&kv, "SP_TSD_REWORK"), "?");
    }
}
