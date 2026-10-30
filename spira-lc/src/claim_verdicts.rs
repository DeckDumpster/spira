//! `claim-verdicts`, a JSON object on stdin: the dispatcher's verdict on each READY or REWORK
//! bead from one pass, written onto the bead rows so the ops views read them with the rows.
//! It is a reading of the pass, not a transition — no event, no version bump. The verdict
//! vocabulary belongs to the producer (`spira-claim`); here a kind need only be a plain word.

use serde::Deserialize;

use crate::cutover::{flag, is_row_key, q};
use crate::db::Conn;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

#[derive(Deserialize)]
struct Payload {
    computed_at: String,
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    verdict: String,
    #[serde(default)]
    blocker: Option<String>,
    #[serde(default)]
    runes: Vec<String>,
}

fn word(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
}

fn stamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20 && b[19] == b'Z' && b[10] == b'T' && b.iter().enumerate().all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || c.is_ascii_digit())
}

fn script(p: &Payload) -> Result<(String, Vec<String>), String> {
    if !stamp(&p.computed_at) {
        return Err(format!("computed_at {:?} is not an ISO-8601 UTC stamp", p.computed_at));
    }
    let mut sql = String::new();
    for e in &p.entries {
        let runes = e.runes.join(",");
        if !is_row_key(&e.id) || !word(&e.verdict) || e.blocker.as_deref().is_some_and(|b| !is_row_key(b)) || runes.len() > 255 || e.runes.iter().any(|r| !word(r)) {
            return Err(format!("entry for {:?} is malformed", e.id));
        }
        sql.push_str(&format!(
            "UPDATE bead SET claim_verdict = {}, claim_blocker = {}, claim_runes = {}, claim_at = {} WHERE bead_id = {};\n",
            q(&e.verdict),
            e.blocker.as_deref().map_or("NULL".to_string(), q),
            if runes.is_empty() { "NULL".to_string() } else { q(&runes) },
            q(&p.computed_at),
            q(&e.id),
        ));
    }
    Ok((sql, p.entries.iter().map(|e| e.id.clone()).collect()))
}

pub fn cmd_claim_verdicts(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(body) = flag(args, "--json") else {
        return (CANNOT_TELL, "claim-verdicts: the payload is missing".into());
    };
    let payload: Payload = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => return (REFUSED, format!("claim-verdicts: {e}")),
    };
    let (sql, ids) = match script(&payload) {
        Ok(x) => x,
        Err(e) => return (REFUSED, format!("claim-verdicts: {e}")),
    };
    if ids.is_empty() {
        return (0, String::new());
    }
    match conn.run_plain_touching(&sql, crate::live::Touch::Keys(ids)) {
        Ok(()) => (0, String::new()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(json: &str) -> Payload {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_pass_is_one_update_per_bead_stamped_with_the_producers_clock() {
        let p = payload(r#"{"computed_at":"2026-10-30T22:20:00Z","entries":[{"id":"sp-p.1","verdict":"blocked","blocker":"sp-p"},{"id":"sp-a","verdict":"claimable","runes":["builder","ops"]}]}"#);
        let (sql, ids) = script(&p).unwrap();
        assert_eq!(ids, ["sp-p.1", "sp-a"]);
        assert!(sql.contains("claim_verdict = 'blocked', claim_blocker = 'sp-p', claim_runes = NULL, claim_at = '2026-10-30T22:20:00Z' WHERE bead_id = 'sp-p.1'"), "{sql}");
        assert!(sql.contains("claim_verdict = 'claimable', claim_blocker = NULL, claim_runes = 'builder,ops'"), "{sql}");
    }

    #[test]
    fn a_malformed_payload_is_refused_before_any_write() {
        for bad in [
            r#"{"computed_at":"yesterday","entries":[]}"#,
            r#"{"computed_at":"2026-10-30T22:20:00Z","entries":[{"id":"not a key","verdict":"blocked"}]}"#,
            r#"{"computed_at":"2026-10-30T22:20:00Z","entries":[{"id":"sp-a","verdict":"x'; DROP"}]}"#,
            r#"{"computed_at":"2026-10-30T22:20:00Z","entries":[{"id":"sp-a","verdict":"blocked","blocker":"x y"}]}"#,
        ] {
            assert!(script(&payload(bad)).is_err(), "{bad}");
        }
        let conn = Conn::unreachable();
        assert_eq!(cmd_claim_verdicts(&["--json".into(), "[]".into()], &conn).0, REFUSED);
        assert_eq!(cmd_claim_verdicts(&[], &conn).0, CANNOT_TELL);
    }

    #[test]
    fn an_empty_pass_writes_nothing() {
        let conn = Conn::unreachable();
        let body = r#"{"computed_at":"2026-10-30T22:20:00Z","entries":[]}"#;
        assert_eq!(cmd_claim_verdicts(&["--json".into(), body.into()], &conn).0, 0);
    }
}
