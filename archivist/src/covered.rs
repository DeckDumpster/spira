//! Is a candidate ask already held — by an open ask in the store, or by the live session's
//! own transcript? Pure over its inputs, so the matcher is table-tested.

use std::collections::BTreeSet;

use serde_json::Value;

const SIMILAR: f64 = 0.6;
const SAME_BEAD_SIMILAR: f64 = 0.3;
pub const ANSWERED_WINDOW_SECS: i64 = 3600;

const STOP: &[&str] = &["the", "a", "an", "to", "of", "and", "or", "is", "it", "in", "on", "for", "we", "you", "do", "be", "this", "that", "with", "should", "can", "approve", "please"];

fn words(s: &str) -> BTreeSet<String> {
    s.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|w| w.len() > 1 && !STOP.contains(w))
        .map(str::to_string)
        .collect()
}

fn bead_ids(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| w.len() >= 6 && w.starts_with("sp-") && w[3..].chars().all(|c| c.is_ascii_alphanumeric()))
        .map(str::to_string)
        .collect()
}

fn similarity(a: &str, b: &str) -> f64 {
    let (wa, wb) = (words(a), words(b));
    let inter = wa.intersection(&wb).count() as f64;
    let smaller = wa.len().min(wb.len()) as f64;
    if smaller == 0.0 {
        return 0.0;
    }
    inter / smaller
}

/// One open ask: the id, and everything it says (subject and body).
pub struct OpenAsk {
    pub id: String,
    pub subject: String,
    pub text: String,
}

/// Rows of `work list --label <ask-label> --json`; rows with no id are dropped.
pub fn open_asks(json: &str) -> Vec<OpenAsk> {
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(json) else { return vec![] };
    let s = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    rows.iter()
        .filter_map(|r| {
            let id = s(r, "id");
            if id.is_empty() {
                return None;
            }
            let subject = s(r, "title");
            let text = format!("{subject} {}", s(r, "description"));
            Some(OpenAsk { id, subject, text })
        })
        .collect()
}

/// The id of the open ask that already holds `question`: it cites that ask's id, or says the
/// same thing, or says a similar thing about a bead in `beads`.
pub fn held_by_open_ask(question: &str, beads: &[String], asks: &[OpenAsk]) -> Option<String> {
    let cited = bead_ids(question);
    asks.iter()
        .find(|a| {
            let sim = similarity(question, &a.subject).max(similarity(question, &a.text));
            let shares_bead = beads.iter().any(|b| bead_ids(&a.text).contains(b));
            cited.contains(&a.id) || sim >= SIMILAR || (shares_bead && sim >= SAME_BEAD_SIMILAR)
        })
        .map(|a| a.id.clone())
}

fn subject_of(command: &str) -> Option<String> {
    let at = command.find("--subject")? + "--subject".len();
    let rest = command[at..].trim_start().trim_start_matches('=');
    let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let body = &rest[1..];
    Some(body[..body.find(q)?].to_string())
}

fn epoch_of(ts: &str) -> Option<i64> {
    let (d, t) = ts.get(..19)?.split_once('T')?;
    let n = |s: &str| s.parse::<i64>().ok();
    let mut dp = d.split('-');
    let (y, m, day) = (n(dp.next()?)?, n(dp.next()?)?, n(dp.next()?)?);
    let mut tp = t.split(':');
    let secs = n(tp.next()?)? * 3600 + n(tp.next()?)? * 60 + n(tp.next()?)?;
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + secs)
}

fn result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

/// Why the session's own transcript already covers `question`, if it does: it filed a
/// question whose subject matches and got an sp- id back, or put it to the operator in chat
/// and was answered within [`ANSWERED_WINDOW_SECS`] of `now`.
pub fn covered_by_transcript(question: &str, transcript: &str, now: i64) -> Option<String> {
    let rows: Vec<Value> = transcript.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mut filed: Vec<(String, String)> = Vec::new();
    let mut returned: Vec<(String, String)> = Vec::new();
    let mut asked_at: Vec<Option<i64>> = Vec::new();
    let mut answers: Vec<i64> = Vec::new();
    for row in &rows {
        let ts = row.get("timestamp").and_then(Value::as_str).and_then(epoch_of);
        let Some(content) = row.get("message").and_then(|m| m.get("content")) else { continue };
        let blocks: Vec<Value> = match content {
            Value::Array(a) => a.clone(),
            Value::String(s) => vec![serde_json::json!({"type": "text", "text": s})],
            _ => continue,
        };
        let is_user = row.get("type").and_then(Value::as_str) == Some("user");
        for b in &blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let cmd = b.get("input").and_then(|i| i.get("command")).and_then(Value::as_str).unwrap_or("");
                    if cmd.contains("--kind question") {
                        if let (Some(id), Some(subject)) = (b.get("id").and_then(Value::as_str), subject_of(cmd)) {
                            filed.push((id.to_string(), subject));
                        }
                    }
                }
                Some("tool_result") => {
                    let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("").to_string();
                    returned.push((id, result_text(b)));
                }
                Some("text") if is_user => {
                    if let Some(t) = ts {
                        answers.push(t);
                    }
                }
                Some("text") => {
                    let text = b.get("text").and_then(Value::as_str).unwrap_or("");
                    if similarity(question, text) >= SIMILAR && text.contains('?') {
                        asked_at.push(ts);
                    }
                }
                _ => {}
            }
        }
    }
    for (use_id, subject) in &filed {
        if similarity(question, subject) < SIMILAR {
            continue;
        }
        let got = returned.iter().find(|(id, _)| id == use_id).and_then(|(_, t)| bead_ids(t).into_iter().next());
        if let Some(sp) = got {
            return Some(format!("the session filed it as {sp}"));
        }
    }
    asked_at
        .iter()
        .flatten()
        .find(|asked| answers.iter().any(|a| a > *asked && now - a <= ANSWERED_WINDOW_SECS))
        .map(|_| "the session asked it in chat and was answered within the hour".to_string())
}

/// Whether a claude client process holds `sid` right now: a `<pid>.json` in the client's
/// session registry names it and that pid is running. Transcript age says nothing about it.
pub fn session_is_live(registry: &std::path::Path, proc_root: &std::path::Path, sid: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(registry) else { return false };
    rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).any(|e| {
        let Ok(text) = std::fs::read_to_string(e.path()) else { return false };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return false };
        v.get("sessionId").and_then(Value::as_str) == Some(sid) && v.get("pid").and_then(Value::as_u64).is_some_and(|p| proc_root.join(p.to_string()).exists())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(id: &str, title: &str, desc: &str) -> OpenAsk {
        OpenAsk { id: id.into(), subject: title.into(), text: format!("{title} {desc}") }
    }

    #[test]
    fn an_open_ask_with_the_same_question_holds_it() {
        let asks = [ask("sp-e1k1gt", "Approve kit-catalog v2 rollout", "")];
        assert_eq!(held_by_open_ask("kit-catalog v2 approval needed?", &[], &asks).as_deref(), Some("sp-e1k1gt"));
    }

    #[test]
    fn an_unrelated_question_is_not_held() {
        let asks = [ask("sp-e1k1gt", "Approve kit-catalog v2 rollout", "")];
        assert_eq!(held_by_open_ask("Which timezone should the digest mail use", &[], &asks), None);
    }

    #[test]
    fn citing_an_open_ask_id_is_held() {
        let asks = [ask("sp-cbz7we", "Two MFA codes", "")];
        assert!(held_by_open_ask("Follow-up to sp-cbz7we about retention", &[], &asks).is_some());
    }

    #[test]
    fn a_shared_bead_lowers_the_bar_but_does_not_remove_it() {
        let asks = [ask("sp-ask111", "Gate budget for sp-bead99", "raise the gate budget for sp-bead99")];
        let beads = ["sp-bead99".to_string()];
        assert!(held_by_open_ask("gate budget too small?", &beads, &asks).is_some());
        assert!(held_by_open_ask("rename the dashboard", &beads, &asks).is_none());
    }

    #[test]
    fn open_asks_parse_and_drop_rows_without_an_id() {
        let a = open_asks(r#"[{"id":"sp-a","title":"t","description":"d"},{"title":"x"}]"#);
        assert_eq!(a.len(), 1);
        assert!(open_asks("not json").is_empty());
    }

    fn filed_row(subject: &str, result: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-10-30T10:00:00Z","message":{{"content":[{{"type":"tool_use","id":"tu1","name":"Bash","input":{{"command":"mail send operator --subject \"{subject}\" --kind question"}}}}]}}}}
{{"type":"user","timestamp":"2026-10-30T10:00:01Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"tu1","content":"{result}"}}]}}}}"#
        )
    }

    #[test]
    fn a_question_the_session_filed_and_got_an_id_for_is_covered() {
        let t = filed_row("kit-catalog v2 approval", "filed sp-e1k1gt");
        assert!(covered_by_transcript("approve kit-catalog v2?", &t, 0).unwrap().contains("sp-e1k1gt"));
    }

    #[test]
    fn a_filing_that_returned_no_id_is_not_covered() {
        let t = filed_row("kit-catalog v2 approval", "refused: no default");
        assert!(covered_by_transcript("approve kit-catalog v2?", &t, 0).is_none());
    }

    #[test]
    fn a_question_asked_in_chat_is_covered_only_once_answered_within_the_hour() {
        let asked = r#"{"type":"assistant","timestamp":"2026-10-30T10:00:00Z","message":{"content":[{"type":"text","text":"Should the digest mail use UTC timezone?"}]}}"#;
        let answer = r#"{"type":"user","timestamp":"2026-10-30T10:05:00Z","message":{"content":"yes UTC"}}"#;
        let at = epoch_of("2026-10-30T10:30:00Z").unwrap();
        assert!(covered_by_transcript("digest mail timezone UTC?", &format!("{asked}\n{answer}"), at).is_some());
        assert!(covered_by_transcript("digest mail timezone UTC?", asked, at).is_none());
        let later = epoch_of("2026-10-30T12:00:00Z").unwrap();
        assert!(covered_by_transcript("digest mail timezone UTC?", &format!("{asked}\n{answer}"), later).is_none());
    }

    #[test]
    fn epoch_of_matches_a_known_instant() {
        assert_eq!(epoch_of("1970-01-02T00:00:01Z"), Some(86_401));
        assert_eq!(epoch_of("2000-03-01T00:00:00Z"), Some(951_868_800));
    }

    #[test]
    fn liveness_comes_from_a_running_pid_in_the_registry_not_file_age() {
        let root = testkit::TempDir::new("archivist-registry");
        let reg = root.join("sessions");
        let procs = root.join("proc");
        std::fs::create_dir_all(&reg).unwrap();
        std::fs::create_dir_all(procs.join("4242")).unwrap();
        std::fs::write(reg.join("4242.json"), r#"{"pid":4242,"sessionId":"live-sid"}"#).unwrap();
        std::fs::write(reg.join("999.json"), r#"{"pid":999,"sessionId":"dead-sid"}"#).unwrap();
        assert!(session_is_live(&reg, &procs, "live-sid"));
        assert!(!session_is_live(&reg, &procs, "dead-sid"), "pid not running");
        assert!(!session_is_live(&reg, &procs, "absent"));
    }
}
