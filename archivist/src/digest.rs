//! `archivist digest <transcript> [<from-turn>] [--full]` — the transcript rendered
//! small enough for an agent to read. Deterministic and pure (once the file is read), so
//! two runs over one transcript see the same thing and this is table-tested, which a
//! prompt asking a model to "summarise the transcript" is not.
//!
//! Tool RESULTS are dropped and tool CALLS are kept: results are the bulk of the bytes by
//! an order of magnitude and are already summarised in the prose beside them, whereas the
//! calls are the record of work in flight — which branches were touched, which beads were
//! claimed, what was half-done. `--full` keeps a truncated result too, for a session whose
//! findings really are in its output.
//!
//! Turns are counted the way the context meter counts them: one per distinct assistant
//! message id, so "turn 240" here and "240t" in the status line are the same turn. A
//! prompt is numbered for the turn it PRODUCES (the counter advances on the assistant's
//! reply), so a user message read literally would land one turn early — the one exception
//! is a user row carrying visible text of its own (not just tool results), which is
//! numbered `turn + 1` because it is the prompt that produced the *next* reply.

use serde_json::Value;

enum Part {
    Text(String),
    Tool(String),
    Result(String),
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The human-readable parts of a `message.content` field, which may be a bare string or
/// a content-block list.
fn parts_of(content: &Value, full: bool) -> Vec<Part> {
    if let Some(s) = content.as_str() {
        return vec![Part::Text(s.to_string())];
    }
    let mut out = Vec::new();
    let Some(blocks) = content.as_array() else { return out };
    for b in blocks {
        let Some(t) = b.get("type").and_then(|v| v.as_str()) else { continue };
        match t {
            "text" => {
                if let Some(txt) = b.get("text").and_then(|v| v.as_str()) {
                    if !txt.trim().is_empty() {
                        out.push(Part::Text(txt.to_string()));
                    }
                }
            }
            "tool_use" => {
                let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let empty = Value::Object(Default::default());
                let inp = b.get("input").unwrap_or(&empty);
                let mut arg = String::new();
                for k in ["command", "file_path", "pattern", "path", "url", "prompt", "description"] {
                    if let Some(v) = inp.get(k).and_then(|v| v.as_str()) {
                        if !v.trim().is_empty() {
                            arg = truncate_chars(&collapse_ws(v), 160);
                            break;
                        }
                    }
                }
                out.push(Part::Tool(format!("{name} {arg}")));
            }
            "tool_result" if full => {
                let c2 = b.get("content");
                let text = match c2 {
                    Some(Value::Array(a)) => a.iter().filter_map(|x| x.get("text").and_then(|v| v.as_str())).collect::<Vec<_>>().join(" "),
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                if !text.trim().is_empty() {
                    out.push(Part::Result(truncate_chars(&collapse_ws(&text), 300)));
                }
            }
            _ => {}
        }
    }
    out
}

/// Render one transcript from `from_turn` onward, `full` controlling whether tool results
/// are kept (truncated) alongside tool calls.
pub fn render(transcript: &str, from_turn: u64, full: bool) -> String {
    let mut out = String::new();
    let mut turn: u64 = 0;
    let mut last_id: Option<String> = None;

    for line in transcript.lines() {
        let Ok(o) = serde_json::from_str::<Value>(line) else { continue };
        if o.get("type").and_then(|v| v.as_str()) == Some("queue-operation") {
            if o.get("operation").and_then(|v| v.as_str()) == Some("remove") {
                if let Some(content) = o.get("content").and_then(|v| v.as_str()) {
                    if !content.trim().is_empty() {
                        let shown = turn + 1;
                        if shown >= from_turn {
                            out.push_str(&format!("--- turn {shown} [user]\n"));
                            out.push_str(content.trim());
                            out.push('\n');
                        }
                    }
                }
            }
            continue;
        }
        let Some(m) = o.get("message").filter(|v| v.is_object()) else { continue };
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or_else(|| o.get("type").and_then(|v| v.as_str()).unwrap_or("")).to_string();
        if role == "assistant" {
            if let Some(mid) = m.get("id").and_then(|v| v.as_str()) {
                if last_id.as_deref() != Some(mid) {
                    turn += 1;
                    last_id = Some(mid.to_string());
                }
            }
        }
        let empty = Value::Null;
        let parts = parts_of(m.get("content").unwrap_or(&empty), full);
        if parts.is_empty() {
            continue;
        }
        let has_text = parts.iter().any(|p| matches!(p, Part::Text(_)));
        let shown = if role == "user" && has_text { turn + 1 } else { turn };
        if shown < from_turn {
            continue;
        }
        out.push_str(&format!("--- turn {shown} [{role}]\n"));
        for p in &parts {
            match p {
                Part::Text(t) => {
                    out.push_str(t.trim());
                    out.push('\n');
                }
                Part::Tool(t) => {
                    out.push_str("    · ");
                    out.push_str(t);
                    out.push('\n');
                }
                Part::Result(t) => {
                    out.push_str("    > ");
                    out.push_str(t);
                    out.push('\n');
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: serde_json::Value) -> String {
        v.to_string()
    }

    #[test]
    fn an_assistant_text_message_starts_turn_one() {
        let t = line(serde_json::json!({"message": {"role": "assistant", "id": "m1", "content": [{"type": "text", "text": "hello"}]}}));
        let out = render(&t, 0, false);
        assert_eq!(out, "--- turn 1 [assistant]\nhello\n");
    }

    #[test]
    fn a_repeated_assistant_id_does_not_advance_the_turn_counter() {
        let a = line(serde_json::json!({"message": {"role": "assistant", "id": "m1", "content": [{"type": "text", "text": "a"}]}}));
        let b = line(serde_json::json!({"message": {"role": "assistant", "id": "m1", "content": [{"type": "text", "text": "b"}]}}));
        let out = render(&format!("{a}\n{b}"), 0, false);
        assert_eq!(out, "--- turn 1 [assistant]\na\n--- turn 1 [assistant]\nb\n");
    }

    #[test]
    fn a_user_message_with_text_is_numbered_for_the_turn_it_produces() {
        let a = line(serde_json::json!({"message": {"role": "assistant", "id": "m1", "content": [{"type": "text", "text": "a"}]}}));
        let u = line(serde_json::json!({"message": {"role": "user", "content": [{"type": "text", "text": "go do X"}]}}));
        let out = render(&format!("{a}\n{u}"), 0, false);
        assert!(out.contains("--- turn 2 [user]\ngo do X\n"), "got: {out}");
    }

    #[test]
    fn a_tool_use_block_is_rendered_with_its_named_argument() {
        let t = line(serde_json::json!({"message": {"role": "assistant", "id": "m1", "content": [{"type": "tool_use", "name": "Bash", "input": {"command": "ls   -la   /tmp"}}]}}));
        let out = render(&t, 0, false);
        assert_eq!(out, "--- turn 1 [assistant]\n    · Bash ls -la /tmp\n");
    }

    #[test]
    fn tool_results_are_dropped_by_default_and_kept_with_full() {
        let t = line(serde_json::json!({"message": {"role": "user", "content": [{"type": "tool_result", "content": "some output"}]}}));
        assert_eq!(render(&t, 0, false), "", "tool-only user row with no text produces no header at all");
        let out_full = render(&t, 0, true);
        assert!(out_full.contains("    > some output"), "got: {out_full}");
    }

    #[test]
    fn from_turn_filters_out_earlier_turns() {
        let a = line(serde_json::json!({"message": {"role": "assistant", "id": "m1", "content": [{"type": "text", "text": "a"}]}}));
        let b = line(serde_json::json!({"message": {"role": "assistant", "id": "m2", "content": [{"type": "text", "text": "b"}]}}));
        let out = render(&format!("{a}\n{b}"), 2, false);
        assert_eq!(out, "--- turn 2 [assistant]\nb\n");
    }

    #[test]
    fn a_queue_operation_remove_with_content_is_rendered_as_a_user_turn() {
        let q = line(serde_json::json!({"type": "queue-operation", "operation": "remove", "content": "typed mid-turn"}));
        let out = render(&q, 0, false);
        assert_eq!(out, "--- turn 1 [user]\ntyped mid-turn\n");
    }

    #[test]
    fn a_queue_operation_dequeue_is_silent() {
        let q = line(serde_json::json!({"type": "queue-operation", "operation": "dequeue", "content": "should not appear"}));
        assert_eq!(render(&q, 0, false), "");
    }

    #[test]
    fn a_malformed_line_is_skipped_without_aborting_the_rest() {
        let out = render("not json\n{\"message\": {\"role\": \"assistant\", \"id\": \"m1\", \"content\": [{\"type\": \"text\", \"text\": \"ok\"}]}}", 0, false);
        assert_eq!(out, "--- turn 1 [assistant]\nok\n");
    }

    #[test]
    fn long_tool_arguments_are_truncated_to_160_chars() {
        let long = "x".repeat(200);
        let t = line(serde_json::json!({"message": {"role": "assistant", "id": "m1", "content": [{"type": "tool_use", "name": "Read", "input": {"file_path": long}}]}}));
        let out = render(&t, 0, false);
        let arg_line = out.lines().nth(1).unwrap();
        assert_eq!(arg_line.len(), "    · Read ".len() + 160);
    }
}
