pub fn landed_enough(row: &serde_json::Value) -> bool {
    row.get("status").and_then(|s| s.as_str()) == Some("closed")
}
