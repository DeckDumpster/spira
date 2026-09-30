//! The applications ledger: one JSON object per line, timestamp first, append-only, sorted
//! by construction. `applied` writes it; `log` reads it back filtered.

use serde_json::{json, Value};

pub struct Record {
    pub ts: String,
    pub epoch: u64,
    pub sop: String,
    /// Exactly one of `bead`/`pass` is set — never both, never neither.
    pub bead: Option<String>,
    pub pass: Option<String>,
    pub check: String,
    pub held: String,
    pub actor: String,
    pub shelf: String,
    pub note: String,
    pub why: String,
}

impl Record {
    /// The ledger line, `why` truncated to `why_cap` *words joined by single spaces* —
    /// matching the bash's `" ".join(why.split())[:cap]`, a character cap over the
    /// whitespace-normalised text, not a word cap.
    pub fn to_line(&self, why_cap: usize) -> String {
        let why_norm: String = self.why.split_whitespace().collect::<Vec<_>>().join(" ");
        let why_trunc: String = why_norm.chars().take(why_cap).collect();
        let mut obj = json!({
            "ts": self.ts,
            "epoch": self.epoch,
            "sop": self.sop,
        });
        let map = obj.as_object_mut().unwrap();
        if let Some(b) = &self.bead {
            map.insert("bead".to_string(), json!(b));
        } else {
            map.insert("pass".to_string(), json!(self.pass.clone().unwrap_or_default()));
        }
        map.insert("check".to_string(), json!(self.check));
        map.insert("held".to_string(), json!(self.held));
        map.insert("actor".to_string(), json!(self.actor));
        map.insert("shelf".to_string(), json!(self.shelf));
        map.insert("note".to_string(), json!(self.note));
        map.insert("why".to_string(), json!(why_trunc));
        serde_json::to_string(&obj).unwrap_or_default()
    }
}

pub struct LogFilter {
    pub bead: Option<String>,
    pub pass: Option<String>,
    pub sop: Option<String>,
    pub check: Option<String>,
    pub since: Option<u64>,
}

/// `log`'s three-valued read over ledger text (already loaded — the caller decides whether
/// the file exists at all, which is the `Unreadable` case `log_lines` cannot see from text
/// alone).
pub enum LogOutcome {
    /// At least one matching line, in file order.
    Found(Vec<String>),
    /// The ledger parsed; nothing matched.
    Empty,
    /// Every line failed to parse as JSON — corrupt, not empty. Carries the line count.
    Corrupt(usize),
}

pub fn log_lines(text: &str, f: &LogFilter) -> LogOutcome {
    let mut seen = 0usize;
    let mut bad = 0usize;
    let mut shown = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        seen += 1;
        let Ok(r) = serde_json::from_str::<Value>(line) else {
            bad += 1;
            continue;
        };
        if let Some(b) = &f.bead {
            if r.get("bead").and_then(Value::as_str) != Some(b.as_str()) {
                continue;
            }
        }
        if let Some(p) = &f.pass {
            if r.get("pass").and_then(Value::as_str) != Some(p.as_str()) {
                continue;
            }
        }
        if let Some(s) = &f.sop {
            if r.get("sop").and_then(Value::as_str) != Some(s.as_str()) {
                continue;
            }
        }
        if let Some(c) = &f.check {
            if r.get("check").and_then(Value::as_str) != Some(c.as_str()) {
                continue;
            }
        }
        if let Some(since) = f.since {
            match r.get("epoch").and_then(Value::as_u64) {
                Some(e) if e >= since => {}
                _ => continue,
            }
        }
        shown.push(line.to_string());
    }
    if seen > 0 && bad == seen {
        return LogOutcome::Corrupt(seen);
    }
    if shown.is_empty() {
        LogOutcome::Empty
    } else {
        LogOutcome::Found(shown)
    }
}

/// `digest`: one `<key> <sha256-of-stripped-text>` line per SOP, sorted by key.
pub fn digest_lines(shelf: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    use sha2::{Digest, Sha256};
    shelf
        .iter()
        .map(|(k, v)| {
            use std::fmt::Write as _;
            let mut h = Sha256::new();
            h.update(v.trim().as_bytes());
            let hex = h.finalize().iter().fold(String::with_capacity(64), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            });
            format!("{k} {hex}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(sop: &str, bead: Option<&str>, pass: Option<&str>, epoch: u64, check: &str, held: &str) -> Record {
        Record {
            ts: "2026-09-30T00:00:00Z".to_string(),
            epoch,
            sop: sop.to_string(),
            bead: bead.map(String::from),
            pass: pass.map(String::from),
            check: check.to_string(),
            held: held.to_string(),
            actor: "tester".to_string(),
            shelf: "ok".to_string(),
            note: "ok".to_string(),
            why: "  some   why   text  ".to_string(),
        }
    }

    #[test]
    fn a_bead_record_carries_bead_not_pass() {
        let line = rec("sop-x", Some("sp-1"), None, 1, "pass", "yes").to_line(400);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["bead"], "sp-1");
        assert!(v.get("pass").is_none());
    }

    #[test]
    fn a_pass_record_carries_pass_not_bead() {
        let line = rec("sop-x", None, Some("pass-1"), 1, "pass", "yes").to_line(400);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["pass"], "pass-1");
        assert!(v.get("bead").is_none());
    }

    #[test]
    fn why_is_whitespace_normalised_and_capped() {
        let mut r = rec("sop-x", Some("sp-1"), None, 1, "pass", "yes");
        r.why = "  a   b\nc  ".to_string();
        let line = r.to_line(400);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["why"], "a b c");

        r.why = "word ".repeat(200);
        let line = r.to_line(10);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["why"].as_str().unwrap().chars().count(), 10);
    }

    #[test]
    fn log_filters_by_bead_and_since() {
        let lines = [
            rec("sop-a", Some("sp-1"), None, 100, "pass", "yes").to_line(400),
            rec("sop-b", Some("sp-2"), None, 200, "pass", "unknown").to_line(400),
        ];
        let text = lines.join("\n");

        let f = LogFilter { bead: Some("sp-1".into()), pass: None, sop: None, check: None, since: None };
        match log_lines(&text, &f) {
            LogOutcome::Found(v) => assert_eq!(v.len(), 1),
            _ => panic!("expected Found"),
        }

        let f2 = LogFilter { bead: None, pass: None, sop: None, check: None, since: Some(150) };
        match log_lines(&text, &f2) {
            LogOutcome::Found(v) => assert_eq!(v.len(), 1),
            _ => panic!("expected Found"),
        }

        let f3 = LogFilter { bead: Some("sp-999".into()), pass: None, sop: None, check: None, since: None };
        assert!(matches!(log_lines(&text, &f3), LogOutcome::Empty));
    }

    #[test]
    fn a_ledger_of_unparseable_lines_is_corrupt_not_empty() {
        let text = "not json\nalso not json\n";
        let f = LogFilter { bead: None, pass: None, sop: None, check: None, since: None };
        assert!(matches!(log_lines(text, &f), LogOutcome::Corrupt(2)));
    }

    #[test]
    fn digest_is_stable_and_sorted() {
        let mut shelf = std::collections::BTreeMap::new();
        shelf.insert("sop-b".to_string(), "text b".to_string());
        shelf.insert("sop-a".to_string(), "text a  ".to_string());
        let lines = digest_lines(&shelf);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("sop-a "));
        assert!(lines[1].starts_with("sop-b "));
        // Trailing whitespace does not change the digest (matches the shelf's own strip).
        let mut shelf2 = std::collections::BTreeMap::new();
        shelf2.insert("sop-a".to_string(), "text a".to_string());
        assert_eq!(digest_lines(&shelf2)[0], lines[0]);
    }
}
