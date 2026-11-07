//! The event-continuity scan kept incrementally: the serving process remembers each key's last
//! applied `to_state` and the first break it saw, and reads only the events past the last
//! `seq` it has seen. The sweeper does the work, so a request never pays for the whole log.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::db::Conn;

/// A full rescan, off the request path, in case a lower `seq` ever committed after a higher.
const RESCAN: Duration = Duration::from_secs(86400);

pub type Break = (String, String, String, Value);

/// `(machine, key, seq)` -> the reason it was acknowledged.
pub type Acked = HashMap<(String, String, String), String>;

#[derive(Default)]
pub struct Tracker {
    upto: i64,
    built: Option<Instant>,
    last: HashMap<(String, String), String>,
    first: HashMap<(String, String), (i64, Value)>,
}

fn field(r: &Value, c: &str) -> String {
    r.get(c).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

impl Tracker {
    pub(crate) fn absorb(&mut self, rows: &[Value]) {
        for r in rows {
            let (machine, key, seq) = (field(r, "machine"), field(r, "lc_key"), field(r, "seq").parse::<i64>().unwrap_or(0));
            let (from, to) = (field(r, "from_state"), field(r, "to_state"));
            let k = (machine.clone(), key.clone());
            if let Some(prev) = self.last.get(&k).filter(|p| **p != from) {
                self.first.entry(k.clone()).or_insert_with(|| {
                    (seq, serde_json::json!({
                        "machine": machine, "key": key, "seq": seq.to_string(),
                        "event": field(r, "event"), "from_state": from, "previous_to_state": prev,
                    }))
                });
            }
            self.last.insert(k, to);
            self.upto = self.upto.max(seq);
        }
    }

    pub fn breaks(&self) -> Vec<Break> {
        let mut out: Vec<(i64, Break)> = self.first.iter().map(|((m, k), (seq, j))| (*seq, (m.clone(), k.clone(), seq.to_string(), j.clone()))).collect();
        out.sort_by_key(|(seq, _)| *seq);
        out.into_iter().map(|(_, b)| b).collect()
    }
}

/// The acknowledgement file's rows (`seq`, machine, key, reason; tab-separated); the first
/// malformed line is the error.
pub fn parse_acked(text: &str) -> Result<Acked, String> {
    let mut acked = Acked::new();
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = l.splitn(4, '\t').collect();
        if f.len() != 4 {
            return Err(l.to_string());
        }
        acked.insert((f[1].to_string(), f[2].to_string(), f[0].to_string()), f[3].to_string());
    }
    Ok(acked)
}

/// The rows `--acknowledge` appends: every unacknowledged break at or below `through`.
pub fn ack_rows(breaks: &[Break], acked: &Acked, reason: Option<String>, through: Option<String>) -> Result<String, String> {
    let (Some(reason), Some(through)) = (reason, through.and_then(|t| t.parse::<i64>().ok())) else {
        return Err("refused: --acknowledge needs --reason R and --through SEQ".to_string());
    };
    if reason.trim().is_empty() || reason.contains(['\t', '\n']) {
        return Err("refused: --reason must be one non-empty line".to_string());
    }
    let mut rows = String::new();
    for (m, k, seq, _) in breaks {
        if seq.parse::<i64>().unwrap_or(i64::MAX) <= through && !acked.contains_key(&(m.clone(), k.clone(), seq.clone())) {
            rows.push_str(&format!("{seq}\t{m}\t{k}\t{reason}\n"));
        }
    }
    Ok(rows)
}

/// Exit 3 when any break is not acknowledged at its own `seq`; each break is reported either
/// way, with its reason when acknowledged.
pub fn judge(breaks: &[Break], acked: &Acked) -> (i32, Value) {
    let mut fresh = false;
    let mut report = Vec::new();
    for (m, k, seq, j) in breaks {
        let mut j = j.clone();
        match acked.get(&(m.clone(), k.clone(), seq.clone())) {
            Some(reason) => j["acknowledged"] = Value::String(reason.clone()),
            None => fresh = true,
        }
        report.push(j);
    }
    (if fresh { crate::cutover::REFUSED } else { 0 }, Value::Array(report))
}

/// One JSON line per break.
pub fn render(report: &Value) -> String {
    report.as_array().map(|a| a.iter().map(|j| j.to_string()).collect::<Vec<_>>().join("\n")).unwrap_or_default()
}

fn read_after(conn: &Conn, after: i64) -> Result<Vec<Value>, String> {
    conn.query(&format!("SELECT seq, machine, lc_key, event, from_state, to_state FROM event WHERE applied = 1 AND seq > {after} ORDER BY seq"))
        .map_err(|e| format!("{e:?}"))
}

/// Everything, from scratch: what a one-shot CLI call (no serving process) does.
pub fn scan(conn: &Conn) -> Result<Tracker, String> {
    let mut t = Tracker::default();
    t.absorb(&read_after(conn, 0)?);
    t.built = Some(Instant::now());
    Ok(t)
}

/// The serving process's answer: the cached tracker brought up to date by the new events only.
pub fn current(conn: &Conn, cell: &Mutex<Tracker>) -> Result<Vec<Break>, String> {
    let mut t = cell.lock().unwrap_or_else(|e| e.into_inner());
    if t.built.is_none() {
        *t = scan(conn)?;
    } else {
        let rows = read_after(conn, t.upto)?;
        t.absorb(&rows);
    }
    Ok(t.breaks())
}

/// One pass of the sweeper: build the tracker, catch it up, or rebuild it once a day. The
/// rebuild is read outside the lock, so a request meanwhile is answered from the old one.
pub fn tick(conn: &Conn) -> Result<(), String> {
    let Some(live) = conn.live.as_ref() else { return Ok(()) };
    let cell = live.continuity();
    let age = cell.lock().unwrap_or_else(|e| e.into_inner()).built.map(|b| b.elapsed());
    match age {
        Some(a) if a >= RESCAN => {
            let fresh = scan(conn)?;
            *cell.lock().unwrap_or_else(|e| e.into_inner()) = fresh;
            Ok(())
        }
        _ => current(conn, cell).map(|_| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i64, key: &str, from: &str, to: &str) -> Value {
        serde_json::json!({"seq": seq.to_string(), "machine": "bead", "lc_key": key, "event": "E", "from_state": from, "to_state": to})
    }

    #[test]
    fn only_a_from_state_that_disagrees_with_the_previous_to_state_is_a_break() {
        let mut t = Tracker::default();
        t.absorb(&[ev(1, "a", "OPEN", "READY"), ev(2, "a", "READY", "WORKING"), ev(3, "b", "OPEN", "READY")]);
        assert!(t.breaks().is_empty());
        t.absorb(&[ev(4, "a", "READY", "WORKING"), ev(5, "b", "READY", "WORKING"), ev(6, "a", "OPEN", "OPEN")]);
        let got: Vec<(String, String)> = t.breaks().into_iter().map(|(_, k, seq, _)| (k, seq)).collect();
        assert_eq!(got, [("a".to_string(), "4".to_string())], "the first break per key, and the clean key stays clean");
    }

    #[test]
    fn batches_absorbed_in_pieces_find_what_one_pass_finds() {
        let all = [ev(1, "a", "X", "Y"), ev(2, "a", "Y", "Z"), ev(3, "a", "Q", "R"), ev(4, "b", "X", "Y"), ev(5, "b", "Z", "Y")];
        let (mut whole, mut parts) = (Tracker::default(), Tracker::default());
        whole.absorb(&all);
        for piece in all.chunks(2) {
            parts.absorb(piece);
        }
        assert_eq!(whole.breaks().len(), 2);
        assert_eq!(whole.breaks(), parts.breaks());
        assert_eq!(parts.upto, 5);
    }

    fn bev(seq: i64, key: &str, event: &str, from: &str, to: &str) -> Value {
        serde_json::json!({"seq": seq.to_string(), "machine": "bead", "lc_key": key, "event": event, "from_state": from, "to_state": to})
    }

    fn breaks_of(rows: &[Value]) -> Vec<Break> {
        let mut t = Tracker::default();
        t.absorb(rows);
        t.breaks()
    }

    fn planted(key: &str, at: i64) -> Vec<Value> {
        vec![bev(at, key, "A", "X", "Y"), bev(at + 1, key, "B", "Q", "R")]
    }

    #[test]
    fn a_log_whose_events_chain_is_continuous() {
        let b = breaks_of(&[bev(1, "a", "Submit", "WORKING", "SUBMITTED"), bev(2, "a", "Deliver", "SUBMITTED", "IN_DELIVERY")]);
        assert_eq!(judge(&b, &Acked::new()), (0, serde_json::json!([])));
    }

    #[test]
    fn a_planted_break_is_reported_once_naming_the_key_and_exits_3() {
        let rows = [bev(1, "sp-lc-brk", "A", "X", "Y"), bev(2, "sp-lc-brk", "B", "Q", "R"), bev(3, "sp-lc-brk", "C", "Q", "S")];
        let (code, report) = judge(&breaks_of(&rows), &Acked::new());
        let out = render(&report);
        assert_eq!(code, 3);
        assert!(out.contains(r#""key":"sp-lc-brk""#), "{out}");
        assert_eq!(out.lines().count(), 1, "the first break only: {out}");
        assert!(out.contains(r#""seq":"2""#), "{out}");
    }

    #[test]
    fn an_unacknowledged_old_break_is_red() {
        assert_eq!(judge(&breaks_of(&planted("sp-lc-old", 1)), &Acked::new()).0, 3);
    }

    #[test]
    fn acknowledging_with_a_reason_and_a_watermark_passes_and_still_reports_the_reason() {
        let b = breaks_of(&planted("sp-lc-old", 1));
        let rows = ack_rows(&b, &Acked::new(), Some("historical, unrepairable".into()), Some("999999999".into())).unwrap();
        let (code, report) = judge(&b, &parse_acked(&rows).unwrap());
        let out = render(&report);
        assert_eq!(code, 0);
        assert!(out.contains(r#""key":"sp-lc-old""#), "{out}");
        assert!(out.contains(r#""acknowledged":"historical, unrepairable""#), "{out}");
    }

    #[test]
    fn acknowledging_without_a_reason_or_a_watermark_is_refused() {
        let b = breaks_of(&planted("sp-lc-old", 1));
        assert!(ack_rows(&b, &Acked::new(), None, Some("1".into())).unwrap_err().starts_with("refused:"));
        assert!(ack_rows(&b, &Acked::new(), Some("why".into()), None).is_err());
        assert!(ack_rows(&b, &Acked::new(), Some("  ".into()), Some("1".into())).is_err());
        assert!(ack_rows(&b, &Acked::new(), Some("a\tb".into()), Some("1".into())).is_err());
    }

    #[test]
    fn the_watermark_acknowledges_only_breaks_at_or_below_it() {
        let mut rows = planted("low", 1);
        rows.extend(planted("high", 10));
        let b = breaks_of(&rows);
        let acked = parse_acked(&ack_rows(&b, &Acked::new(), Some("r".into()), Some("5".into())).unwrap()).unwrap();
        assert_eq!(acked.len(), 1);
        assert_eq!(judge(&b, &acked).0, 3, "the break above the watermark stays red");
    }

    #[test]
    fn a_new_break_is_red_while_the_old_one_stays_acknowledged() {
        let old = breaks_of(&planted("sp-lc-old", 1));
        let acked = parse_acked(&ack_rows(&old, &Acked::new(), Some("historical".into()), Some("999".into())).unwrap()).unwrap();
        let mut rows = planted("sp-lc-old", 1);
        rows.extend(planted("sp-lc-new", 5));
        let (code, report) = judge(&breaks_of(&rows), &acked);
        let out = render(&report);
        assert_eq!(code, 3);
        assert_eq!(out.lines().count(), 2, "{out}");
        assert!(out.lines().next().unwrap().contains(r#""acknowledged":"historical""#));
        assert!(out.lines().nth(1).unwrap().contains(r#""key":"sp-lc-new""#) && !out.lines().nth(1).unwrap().contains("acknowledged"));
        let rebroken = breaks_of(&planted("sp-lc-old", 50));
        assert_eq!(judge(&rebroken, &acked).0, 3, "the acknowledged key breaking again at a later seq is a new break");
    }

    #[test]
    fn a_red_judgement_carries_the_refused_exit_code() {
        assert_eq!(judge(&breaks_of(&planted("k", 1)), &Acked::new()).0, crate::cutover::REFUSED);
    }

    #[test]
    fn a_malformed_acknowledgement_row_is_named() {
        assert_eq!(parse_acked("1\tbead\tk\twhy\n\nnot a row\n").unwrap_err(), "not a row");
    }
}
