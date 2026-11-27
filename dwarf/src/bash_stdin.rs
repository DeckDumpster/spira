//! A heredoc leaves the client's shell reading its socket, so a stray `cat` or prompt hangs.
//! Pure: the payload in, the rewrite out; `dwarf bash-stdin` does the I/O.

use serde_json::{json, Value};

/// The PreToolUse rewrite for a heredoc-bearing Bash call; `None` leaves the call alone.
/// Only those are rewritten, so a client that refuses rewrites refuses no other call.
pub fn hook_output(raw: &str) -> Option<String> {
    let mut payload: Value = serde_json::from_str(raw).ok()?;
    if payload["tool_name"] != "Bash" {
        return None;
    }
    let mut input = payload.get_mut("tool_input")?.take();
    let command = input["command"].as_str().filter(|c| c.contains("<<"))?;
    input["command"] = Value::String(format!("exec 0</dev/null\n{command}"));
    Some(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": input}}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEREDOC: &str = "cat; cat > out <<'EOF'\nx\nEOF";

    fn payload(tool: &str, command: &str) -> String {
        json!({"tool_name": tool, "tool_input": {"command": command, "timeout": 7000, "description": "write a file"}}).to_string()
    }

    #[test_id(t0v435r)]
    #[test]
    fn a_heredoc_command_gets_stdin_at_end_of_file_and_keeps_its_other_fields() {
        let out: Value = serde_json::from_str(&hook_output(&payload("Bash", HEREDOC)).unwrap()).unwrap();
        assert_eq!(
            out,
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": {
                "command": format!("exec 0</dev/null\n{HEREDOC}"), "timeout": 7000, "description": "write a file"}}}),
        );
    }

    #[test_id(ta8utjl)]
    #[test]
    fn a_command_without_a_heredoc_another_tool_and_a_bad_payload_are_left_alone() {
        assert_eq!(hook_output(&payload("Bash", "readlink /proc/self/fd/0 < in")), None);
        assert_eq!(hook_output(&payload("Edit", HEREDOC)), None);
        assert_eq!(hook_output("not json <<"), None);
        assert_eq!(hook_output(r#"{"tool_name":"Bash"}"#), None);
        assert_eq!(hook_output("[]"), None);
    }
}
