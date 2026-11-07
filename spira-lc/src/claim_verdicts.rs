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
pub(crate) struct Payload {
    pub(crate) computed_at: String,
    pub(crate) entries: Vec<Entry>,
}

#[derive(Deserialize)]
pub(crate) struct Entry {
    pub(crate) id: String,
    pub(crate) verdict: String,
    #[serde(default)]
    pub(crate) blocker: Option<String>,
    #[serde(default)]
    pub(crate) runes: Vec<String>,
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

/// The payload as either store takes it: parsed, and every entry well formed, or the refusal.
pub(crate) fn checked(body: &str) -> Result<Payload, String> {
    let payload: Payload = serde_json::from_str(body).map_err(|e| format!("claim-verdicts: {e}"))?;
    script(&payload).map_err(|e| format!("claim-verdicts: {e}"))?;
    Ok(payload)
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

    fn one(computed_at: &str, entry: &str) -> Payload {
        payload(&format!(r#"{{"computed_at":"{computed_at}","entries":[{entry}]}}"#))
    }

    #[test]
    fn a_bead_the_pass_does_not_name_is_not_touched() {
        let (sql, ids) = script(&one("2026-10-30T22:20:00Z", r#"{"id":"sp-a","verdict":"claimable"}"#)).unwrap();
        assert_eq!(ids, ["sp-a"]);
        assert_eq!(sql.matches("UPDATE").count(), 1);
        assert!(sql.contains("sp-a") && !sql.contains("sp-b"), "{sql}");
        assert_eq!(script(&one("2026-10-30T22:20:00Z", "")).unwrap(), (String::new(), vec![]), "a pass naming nobody writes nothing");
    }

    #[test]
    fn a_pass_writes_the_verdict_the_blocker_the_runes_and_the_stamp() {
        let (sql, _) = script(&one("2026-10-30T22:20:00Z", r#"{"id":"sp-lc-ph","verdict":"blocked","blocker":"sp-lc-blocker"}"#)).unwrap();
        assert_eq!(
            sql,
            "UPDATE bead SET claim_verdict = 'blocked', claim_blocker = 'sp-lc-blocker', claim_runes = NULL, claim_at = '2026-10-30T22:20:00Z' WHERE bead_id = 'sp-lc-ph';\n"
        );
    }

    #[test]
    fn the_next_pass_replaces_the_verdict_whole_nulls_included() {
        let (sql, _) = script(&one("2026-10-30T22:21:00Z", r#"{"id":"sp-lc-ph","verdict":"claimable","runes":["builder","ops"]}"#)).unwrap();
        assert_eq!(
            sql,
            "UPDATE bead SET claim_verdict = 'claimable', claim_blocker = NULL, claim_runes = 'builder,ops', claim_at = '2026-10-30T22:21:00Z' WHERE bead_id = 'sp-lc-ph';\n"
        );
    }

    #[test]
    fn a_bad_stamp_is_refused_naming_it() {
        let err = script(&one("yesterday", r#"{"id":"sp-a","verdict":"claimable"}"#)).err().unwrap();
        assert!(err.contains("yesterday"), "{err}");
        assert!(checked(r#"{"computed_at":"2026-10-30 22:20:00Z","entries":[]}"#).is_err());
    }

    #[test]
    fn the_script_never_touches_the_version() {
        let (sql, _) = script(&one("2026-10-30T22:20:00Z", r#"{"id":"sp-a","verdict":"claimable","blocker":"sp-b","runes":["x"]}"#)).unwrap();
        for col in ["version", "updated_at", "state"] {
            assert!(!sql.contains(col), "{col} in {sql}");
        }
    }

    #[test]
    fn one_malformed_entry_refuses_the_whole_pass_and_writes_nothing() {
        let body = r#"{"computed_at":"2026-10-30T22:20:00Z","entries":[{"id":"sp-a","verdict":"claimable"},{"id":"sp-b","verdict":"NOT A WORD"}]}"#;
        assert!(checked(body).is_err());
    }
}
