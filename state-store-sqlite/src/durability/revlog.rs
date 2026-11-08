//! One revision-log run: every key every txn in `from..=to` wrote, with full values, as one
//! object. The first line is the header, and carries the body's SHA-256, so a run that changed
//! after it was written is refused, not replayed. A run may also carry the revisions the store
//! recorded as never used (a jump on open, sp-r3yb7x): those are the only mains it may lack.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use state_store::{LogEntry, Rev};

pub const REVLOG: &str = "revlog";
pub const SNAPSHOT: &str = "snapshot";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub from: u64,
    pub to: u64,
    pub entries: Vec<LogEntry>,
    /// Recorded jumps `(first, last)` inside `from..=to`, in order: unused revisions, not lost ones.
    pub jumps: Vec<(u64, u64)>,
}

/// Zero-padded, so the names sort as the revisions do.
pub fn run_name(from: u64, to: u64) -> String {
    format!("{from:020}-{to:020}")
}

pub fn parse_run_name(name: &str) -> Option<(u64, u64)> {
    let (a, b) = name.split_once('-')?;
    let digits = |s: &str| s.len() == 20 && s.bytes().all(|c| c.is_ascii_digit());
    if !digits(a) || !digits(b) {
        return None;
    }
    let (from, to) = (a.parse().ok()?, b.parse().ok()?);
    (from <= to).then_some((from, to))
}

pub fn snapshot_name(rev: u64) -> String {
    format!("{rev:020}")
}

pub fn parse_snapshot_name(name: &str) -> Option<u64> {
    (name.len() == 20 && name.bytes().all(|c| c.is_ascii_digit()))
        .then(|| name.parse().ok())
        .flatten()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn rev_json(r: Option<Rev>) -> Value {
    r.map_or(Value::Null, |r| json!([r.main, r.sub]))
}

fn rev_of(v: &Value) -> Result<Option<Rev>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Array(a) if a.len() == 2 => {
            let main = a[0].as_u64().ok_or("a revision's main is not a number")?;
            let sub = a[1].as_u64().ok_or("a revision's sub is not a number")?;
            let sub = u32::try_from(sub).map_err(|_| "a revision's sub exceeds u32")?;
            Ok(Some(Rev::new(main, sub)))
        }
        _ => Err("a revision is not [main, sub]".into()),
    }
}

fn entry_line(e: &LogEntry) -> String {
    let mut o = json!({
        "k": e.key, "m": e.rev.main, "s": e.rev.sub,
        "c": rev_json(e.create_rev), "p": rev_json(e.prev),
    });
    match e.value.as_deref().map(std::str::from_utf8) {
        None => {}
        Some(Ok(text)) => o["v"] = json!(text),
        Some(Err(_)) => o["vx"] = json!(hex(e.value.as_deref().unwrap_or_default())),
    }
    o.to_string()
}

pub fn encode(run: &Run) -> Vec<u8> {
    let mut body = String::new();
    for e in &run.entries {
        body.push_str(&entry_line(e));
        body.push('\n');
    }
    let sum = hex(&Sha256::digest(body.as_bytes()));
    let mut header =
        json!({ "from": run.from, "to": run.to, "entries": run.entries.len(), "sha256": sum });
    if !run.jumps.is_empty() {
        header["jumps"] = json!(run.jumps.iter().map(|(a, b)| [*a, *b]).collect::<Vec<_>>());
    }
    format!("{header}\n{body}").into_bytes()
}

fn entry_of(line: &str) -> Result<LogEntry, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("an entry is not JSON: {e}"))?;
    let key = v["k"].as_str().ok_or("an entry has no key")?.to_string();
    let main = v["m"].as_u64().ok_or("an entry has no main")?;
    let sub = v["s"]
        .as_u64()
        .and_then(|s| u32::try_from(s).ok())
        .ok_or("an entry has no sub")?;
    let value = match (v.get("v"), v.get("vx")) {
        (Some(t), None) => Some(
            t.as_str()
                .ok_or("a value is not a string")?
                .as_bytes()
                .to_vec(),
        ),
        (None, Some(x)) => Some(unhex(x.as_str().unwrap_or("-")).ok_or("a value is not hex")?),
        (None, None) => None,
        (Some(_), Some(_)) => return Err("an entry carries two values".into()),
    };
    Ok(LogEntry {
        key,
        rev: Rev::new(main, sub),
        value,
        create_rev: rev_of(&v["c"])?,
        prev: rev_of(&v["p"])?,
    })
}

/// Every main of `from..=to` present, in order, each one's subs numbered from 0: a run written
/// by [`encode`] from the backend's own log, and nothing else.
pub fn decode(bytes: &[u8]) -> Result<Run, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the run is not UTF-8".to_string())?;
    let (head, body) = text.split_once('\n').ok_or("the run has no header")?;
    let h: Value =
        serde_json::from_str(head).map_err(|e| format!("the header is not JSON: {e}"))?;
    let (from, to) = (
        h["from"].as_u64().ok_or("the header has no from")?,
        h["to"].as_u64().ok_or("the header has no to")?,
    );
    let sum = hex(&Sha256::digest(body.as_bytes()));
    if h["sha256"].as_str() != Some(sum.as_str()) {
        return Err(format!("its body's sha256 is {sum}, not the header's"));
    }
    let entries: Vec<LogEntry> = body.lines().map(entry_of).collect::<Result<_, _>>()?;
    if h["entries"].as_u64() != Some(entries.len() as u64) {
        return Err("the header's entry count is not the body's".into());
    }
    let jumps: Vec<(u64, u64)> = match h.get("jumps") {
        None => Vec::new(),
        Some(j) => j
            .as_array()
            .ok_or("the header's jumps are not a list")?
            .iter()
            .map(|p| {
                match p.as_array().map(|a| {
                    (
                        a.first().and_then(Value::as_u64),
                        a.get(1).and_then(Value::as_u64),
                    )
                }) {
                    Some((Some(a), Some(b))) if a <= b => Ok((a, b)),
                    _ => Err("a jump is not [first, last]".to_string()),
                }
            })
            .collect::<Result<_, _>>()?,
    };
    if from > to {
        return Err(format!("its range {from}..={to} is empty"));
    }
    // The mains the run must hold: every one of from..=to outside a declared jump, in order.
    let mut expect = Vec::new();
    let mut m = from;
    let mut js = jumps.iter().peekable();
    let mut last_jump = from.saturating_sub(1);
    while m <= to {
        match js.peek() {
            Some(&&(a, b)) if a <= m => {
                if a != m || b < a || b > to || a <= last_jump {
                    return Err(format!(
                        "the jump {a}..={b} is not inside {from}..={to} in order"
                    ));
                }
                last_jump = b;
                m = b + 1;
                js.next();
            }
            _ => {
                expect.push(m);
                m += 1;
            }
        }
    }
    if js.next().is_some() {
        return Err(format!("a jump lies outside {from}..={to}"));
    }
    let mut want = expect.iter().copied();
    let mut cur: Option<(u64, u32)> = None;
    for e in &entries {
        let next = match cur {
            Some((main, sub)) if e.rev.main == main => (main, sub + 1),
            _ => (
                want.next().ok_or_else(|| {
                    format!(
                        "entry {} at {}.{} is past the run's revisions",
                        e.key, e.rev.main, e.rev.sub
                    )
                })?,
                0,
            ),
        };
        if (e.rev.main, e.rev.sub) != next {
            return Err(format!(
                "entry {} at {}.{} where {}.{} was next",
                e.key, e.rev.main, e.rev.sub, next.0, next.1
            ));
        }
        cur = Some(next);
    }
    if let Some(m) = want.next() {
        return Err(format!(
            "its entries do not cover revision {m} of {from}..={to}, which no jump declares"
        ));
    }
    Ok(Run {
        from,
        to,
        entries,
        jumps,
    })
}

/// The run's entries grouped by main, in order.
pub fn mains(run: &Run) -> Vec<(u64, &[LogEntry])> {
    let mut out: Vec<(u64, &[LogEntry])> = Vec::new();
    let mut start = 0;
    for i in 1..=run.entries.len() {
        if i == run.entries.len() || run.entries[i].rev.main != run.entries[start].rev.main {
            out.push((run.entries[start].rev.main, &run.entries[start..i]));
            start = i;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, main: u64, sub: u32, value: Option<&[u8]>) -> LogEntry {
        LogEntry {
            key: key.into(),
            rev: Rev::new(main, sub),
            value: value.map(<[u8]>::to_vec),
            create_rev: value.map(|_| Rev::new(main, sub)),
            prev: None,
        }
    }

    fn run() -> Run {
        Run {
            jumps: vec![],
            from: 4,
            to: 5,
            entries: vec![
                entry("a", 4, 0, Some(br#"{"data":1}"#)),
                entry("b", 4, 1, Some(&[0xff, 0x00])),
                entry("a", 5, 0, None),
            ],
        }
    }

    #[test]
    fn a_run_round_trips_with_text_binary_and_deleted_values() {
        let r = run();
        assert_eq!(decode(&encode(&r)).unwrap(), r);
        assert_eq!(
            mains(&r)
                .iter()
                .map(|(m, e)| (*m, e.len()))
                .collect::<Vec<_>>(),
            [(4, 2), (5, 1)]
        );
    }

    #[test]
    fn names_sort_as_revisions_and_parse_back() {
        assert!(run_name(9, 10) < run_name(10, 10));
        assert_eq!(parse_run_name(&run_name(9, 12)), Some((9, 12)));
        assert_eq!(parse_run_name("9-12"), None);
        assert_eq!(parse_run_name(&run_name(12, 9)), None);
        assert_eq!(parse_snapshot_name(&snapshot_name(7)), Some(7));
        assert_eq!(parse_snapshot_name("7"), None);
    }

    #[test]
    fn a_run_whose_body_changed_or_skips_a_revision_is_refused() {
        let mut bytes = encode(&run());
        let at = bytes.len() - 3;
        bytes[at] ^= 1;
        assert!(decode(&bytes).unwrap_err().contains("sha256"));

        let mut gap = run();
        gap.to = 6;
        gap.entries[2].rev.main = 6;
        assert!(decode(&encode(&gap))
            .unwrap_err()
            .contains("6.0 where 5.0 was next"));

        let mut short = run();
        short.to = 7;
        assert!(decode(&encode(&short))
            .unwrap_err()
            .contains("do not cover"));
    }

    #[test]
    fn a_declared_jump_is_the_only_gap_a_run_may_have() {
        let mut run = Run {
            from: 1,
            to: 1_003,
            entries: vec![
                entry("a", 1, 0, Some(b"1")),
                entry("b", 1_003, 0, Some(b"2")),
            ],
            jumps: vec![(2, 1_002)],
        };
        assert_eq!(decode(&encode(&run)).unwrap(), run);
        run.jumps.clear();
        let err = decode(&encode(&run)).unwrap_err();
        assert!(
            err.contains("revision 2") || err.contains("1003.0 where 2.0"),
            "{err}"
        );
    }

    #[test]
    fn a_run_that_is_only_a_jump_decodes() {
        let run = Run {
            from: 5,
            to: 1_004,
            entries: vec![],
            jumps: vec![(5, 1_004)],
        };
        assert_eq!(decode(&encode(&run)).unwrap(), run);
    }
}
