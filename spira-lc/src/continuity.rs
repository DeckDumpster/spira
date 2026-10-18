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
    fn absorb(&mut self, rows: &[Value]) {
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
}
