//! The open asks, as `spira-lc list-asks` prints them: the one record every count of them reads.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct OpenAsk {
    pub ask_id: String,
    pub opened_at: i64,
    pub title: String,
    pub class: String,
    pub work_bead: String,
}

/// A non-array, or a row without an `ask_id`, is an error: an unreadable list is not an empty one.
pub fn parse(out: &str) -> Result<Vec<OpenAsk>, String> {
    let v: Value = serde_json::from_str(out).map_err(|e| format!("list-asks: {e}"))?;
    let rows = v.as_array().ok_or("list-asks: not an array")?;
    rows.iter()
        .map(|r| {
            let text = |k: &str| r[k].as_str().unwrap_or_default().to_string();
            let ask_id = text("ask_id");
            if ask_id.is_empty() {
                return Err(format!("list-asks: row without ask_id: {r}"));
            }
            Ok(OpenAsk { ask_id, opened_at: r["opened_at"].as_i64().unwrap_or(0), title: text("title"), class: text("class"), work_bead: text("work_bead") })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lc_call::fake;

    #[test_id(tx9u5up)]
    #[test]
    fn the_shared_fixture_parses_to_its_three_asks() {
        let ids: Vec<_> = parse(&fake::open_asks_json()).unwrap().into_iter().map(|a| a.ask_id).collect();
        assert_eq!(ids, fake::OPEN_ASK_IDS);
    }

    #[test_id(tb07udw)]
    #[test]
    fn a_non_array_or_a_row_without_an_ask_id_is_an_error_and_an_empty_array_is_none() {
        assert!(parse("{}").is_err());
        assert!(parse("").is_err());
        assert!(parse(r#"[{"title":"x"}]"#).is_err());
        assert_eq!(parse("[]").unwrap(), vec![]);
    }
}
