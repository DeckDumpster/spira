//! `mend-watch [--seen FILE]`: one alarm line per breach of the mender bounds. A MENDING row
//! past its deadline should never exist (the sweep returns it), and a bead mended more than
//! `MAX_MENDS_PER_DAY` times in a day is a loop. Silence on stdout means no breach; with
//! `--seen`, a breach already announced is not repeated until it clears and returns.

use std::collections::BTreeSet;

use crate::db::now_epoch;
use crate::{flag, CANNOT_TELL};

pub const MAX_MENDS_PER_DAY: i64 = 2;
const DAY_S: i64 = 86_400;

#[derive(Debug, PartialEq, Eq)]
pub struct Breach {
    pub key: String,
    pub line: String,
}

pub fn overdue_breach(batch: &str, pass: &str, suite: &str, deadline: i64, now: i64) -> Breach {
    let key = format!("overdue:{batch}#{pass}#{suite}");
    let line = format!("MEND-WATCH OVERDUE {batch}#{pass}#{suite} is MENDING {}s past its deadline; the sweep did not return it", now - deadline);
    Breach { key, line }
}

pub fn mends_breach(bead: &str, n: i64) -> Option<Breach> {
    (n > MAX_MENDS_PER_DAY).then(|| Breach {
        key: format!("mends:{bead}"),
        line: format!("MEND-WATCH MENDS {bead} was mended {n} times in 24h (limit {MAX_MENDS_PER_DAY})"),
    })
}

/// The breaches not yet in `seen`, in order.
pub fn fresh<'a>(breaches: &'a [Breach], seen: &BTreeSet<String>) -> Vec<&'a Breach> {
    breaches.iter().filter(|b| !seen.contains(&b.key)).collect()
}

fn text(row: &serde_json::Value, col: &str) -> String {
    match row.get(col) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => String::new(),
    }
}

const READ_ATTEMPTS: u32 = 4;
const READ_PAUSE: std::time::Duration = std::time::Duration::from_secs(3);

/// A store read that timed out or lost its reply is retried: the store restarting under the
/// poll is not a reason to exit "cannot tell" for a probe that runs every two minutes.
pub fn with_retries<T, E>(attempts: u32, pause: std::time::Duration, mut read: impl FnMut() -> Result<T, E>) -> Result<T, E> {
    let mut last = read();
    for _ in 1..attempts {
        if last.is_ok() {
            break;
        }
        std::thread::sleep(pause);
        last = read();
    }
    last
}

/// The rows still MENDING past their deadline, soonest first, with no ask holding them.
fn overdue_rows<B: state_store::Backend>(store: &crate::control::store::Store<B>, now: i64) -> Result<Vec<serde_json::Value>, state_store::Error> {
    let mut rows: Vec<_> = store
        .live_mendings()?
        .into_iter()
        .filter(|m| m.row.state == lifecycle::mending::MendingState::Mending && m.row.awaiting_ask.is_none())
        .filter_map(|m| m.row.deadline.filter(|d| *d <= now).map(|d| (d, m)))
        .collect();
    rows.sort_by_key(|(d, _)| *d);
    Ok(rows.into_iter().map(|(d, m)| serde_json::json!({ "batch_id": m.row.batch_id, "pass": m.row.pass.to_string(), "suite": m.row.suite, "deadline": d.to_string() })).collect())
}

/// Per bead, the mends recorded in the last day.
fn mend_counts<B: state_store::Backend>(store: &crate::control::store::Store<B>, now: i64) -> Result<Vec<serde_json::Value>, state_store::Error> {
    let mut counts: std::collections::BTreeMap<String, u64> = Default::default();
    for m in store.mends_since(now - DAY_S)? {
        *counts.entry(m.bead_id).or_default() += 1;
    }
    Ok(counts.into_iter().map(|(bead, n)| serde_json::json!({ "bead": bead, "n": n.to_string() })).collect())
}

/// Overdue rows (deadline at or before `now`) then beads over the daily cap, as breaches.
pub(crate) fn collect(overdue: &[serde_json::Value], mends: &[serde_json::Value], now: i64) -> Vec<Breach> {
    let mut breaches: Vec<Breach> = overdue
        .iter()
        .filter(|r| text(r, "deadline").parse().map_or(true, |d: i64| d <= now))
        .map(|r| overdue_breach(&text(r, "batch_id"), &text(r, "pass"), &text(r, "suite"), text(r, "deadline").parse().unwrap_or(now), now))
        .collect();
    breaches.extend(mends.iter().filter_map(|r| {
        let bead = text(r, "bead");
        (!bead.is_empty() && bead != "null").then(|| mends_breach(&bead, text(r, "n").parse().unwrap_or(0))).flatten()
    }));
    breaches
}

pub fn run(args: &[String]) -> (i32, String) {
    let store = match crate::control::served::store() {
        Ok(s) => s,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let now = now_epoch();
    let overdue = with_retries(READ_ATTEMPTS, READ_PAUSE, || overdue_rows(&store, now));
    let mends = with_retries(READ_ATTEMPTS, READ_PAUSE, || mend_counts(&store, now));
    let (overdue, mends) = match (overdue, mends) {
        (Ok(o), Ok(m)) => (o, m),
        (Err(e), _) | (_, Err(e)) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let breaches = collect(&overdue, &mends, now);
    let Some(path) = flag(args, "--seen") else {
        return (0, breaches.iter().map(|b| b.line.as_str()).collect::<Vec<_>>().join("\n"));
    };
    let fs = crate::disk::fs();
    let seen: BTreeSet<String> = crate::disk::read_to_string(&path).unwrap_or_default().lines().map(String::from).collect();
    let out = fresh(&breaches, &seen).iter().map(|b| b.line.as_str()).collect::<Vec<_>>().join("\n");
    let now_seen: Vec<&str> = breaches.iter().map(|b| b.key.as_str()).collect();
    if let Err(e) = fs.write(std::path::Path::new(&path), now_seen.iter().map(|k| format!("{k}\n")).collect::<String>().as_bytes()) {
        return (CANNOT_TELL, format!("cannot record {path}: {e}"));
    }
    (0, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::keys;
    use crate::control::records::encode;
    use crate::control::store::{MendRecord, MendingRecord, Store};
    use state_store::{Config, Contract, MemBackend, Object, Ports, Request, Txn};

    fn store() -> Store<MemBackend> {
        let clock = runtime::ports::VirtualClock::new(0);
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        Store::new(c, 1_000)
    }

    fn put(s: &Store<MemBackend>, key: &str, v: serde_json::Value) {
        s.contract().txn(&Txn::new().put(key, Object::new(v)), &Request::new(&format!("put-{key}"), "t", "t")).unwrap();
    }

    #[test_id(ttnzvja)]
    #[test]
    fn a_mending_row_past_its_deadline_with_no_ask_is_overdue_and_a_waiting_or_asked_one_is_not() {
        let s = store();
        let row = |suite: &str, state: lifecycle::mending::MendingState, deadline: i64, ask: Option<&str>| {
            let mut m = MendingRecord::failed("batch-1", 1, suite, 10);
            m.row.state = state;
            m.row.deadline = Some(deadline);
            m.row.awaiting_ask = ask.map(String::from);
            put(&s, &keys::mending(&m.row.key()), encode(&m).unwrap());
        };
        use lifecycle::mending::MendingState::{Mending, Waiting};
        row("late.sh", Mending, 900, None);
        row("early.sh", Mending, 1_100, None);
        row("asked.sh", Mending, 900, Some("ask-1"));
        row("waiting.sh", Waiting, 900, None);
        let rows = overdue_rows(&s, 1_000).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((rows[0]["suite"].as_str(), rows[0]["deadline"].as_str()), (Some("late.sh"), Some("900")));
    }

    #[test_id(tlnq23q)]
    #[test]
    fn mends_are_counted_per_bead_over_the_last_day_only() {
        let s = store();
        let mend = |bead: &str, n: usize, at: i64| {
            let m = MendRecord { batch_id: "b".into(), pass: 1, suite: "s".into(), bead_id: bead.into(), from_tip: "f".into(), tip: "t".into(), ask_id: None, checked: vec![], diagnosis: None, at, opened_at: at };
            put(&s, &keys::mend(bead, at as u64, n), encode(&m).unwrap());
        };
        let now = 200_000;
        for n in 0..3 {
            mend("sp-loop", n, now - 100 + n as i64);
        }
        mend("sp-once", 0, now - 100);
        mend("sp-loop", 9, now - DAY_S - 1);
        let counts = mend_counts(&s, now).unwrap();
        let n = |bead: &str| counts.iter().find(|r| r["bead"] == bead).map(|r| r["n"].as_str().unwrap().to_string());
        assert_eq!((n("sp-loop"), n("sp-once")), (Some("3".to_string()), Some("1".to_string())));
        assert!(collect(&[], &counts, now).iter().any(|b| b.key == "mends:sp-loop"));
    }

    #[test_id(tnp3hpw)]
    #[test]
    fn mend_counts_read_the_opened_at_index_not_every_mend_ever() {
        use crate::control::store::counting::Counting;
        let clock = runtime::ports::VirtualClock::new(0);
        let c = Contract::open(Counting::default(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let s = Store::new(c, 1_000);
        let now = 400_000;
        let record = |bead: &str, n: usize, at: i64| {
            let mut row = MendingRecord::failed("b", n as u32, "s", at);
            row.row.state = lifecycle::mending::MendingState::Mending;
            let key = keys::mending(&row.row.key());
            s.contract().txn(&Txn::new().put(&key, Object::new(encode(&row).unwrap())), &Request::new(&format!("row-{bead}-{n}"), "t", "t")).unwrap();
            s.record_mend(&row.row, &[(bead.into(), "f".into(), "t".into())], None, &[], None, "a", at, &Request::new(&format!("mend-{bead}-{n}"), "t", "t")).unwrap();
        };
        for n in 0..200 {
            record("sp-old", n, now - DAY_S - 1_000 - n as i64);
        }
        for n in 0..3 {
            record("sp-new", 1_000 + n, now - 100 - n as i64);
        }
        s.contract().backend().take();
        let counts = mend_counts(&s, now).unwrap();
        let n = |bead: &str| counts.iter().find(|r| r["bead"] == bead).map(|r| r["n"].as_str().unwrap().to_string());
        assert_eq!((n("sp-new"), n("sp-old")), (Some("3".to_string()), None));
        let reads = s.contract().backend().take();
        assert!(reads.iter().all(|(k, p, _)| !(*k == "range" && p.starts_with(keys::MENDS))), "{reads:?}");
        assert!(reads.iter().any(|(k, p, _)| *k == "range_newest_opened" && p.starts_with(keys::MENDS)), "{reads:?}");
    }

    #[test_id(tjccnx6)]
    #[test]
    fn more_recent_mends_than_the_first_page_are_all_counted() {
        let clock = runtime::ports::VirtualClock::new(0);
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let s = Store::new(c, 1_000);
        let now = 400_000;
        for n in 0..300usize {
            let mut row = MendingRecord::failed("b", n as u32, "s", now - 100 - n as i64);
            row.row.state = lifecycle::mending::MendingState::Mending;
            let key = keys::mending(&row.row.key());
            s.contract().txn(&Txn::new().put(&key, Object::new(encode(&row).unwrap())), &Request::new(&format!("row-{n}"), "t", "t")).unwrap();
            s.record_mend(&row.row, &[("sp-many".into(), "f".into(), "t".into())], None, &[], None, "a", now - 100 - n as i64, &Request::new(&format!("mend-{n}"), "t", "t")).unwrap();
        }
        let counts = mend_counts(&s, now).unwrap();
        assert_eq!(counts.iter().find(|r| r["bead"] == "sp-many").map(|r| r["n"].as_str().unwrap().to_string()), Some("300".to_string()));
    }

    #[test_id(t49peov)]
    #[test]
    fn a_read_that_fails_then_succeeds_is_retried_and_one_that_never_does_is_not_hidden() {
        let mut n = 0;
        let r = with_retries(4, std::time::Duration::ZERO, || { n += 1; if n < 3 { Err("deadline") } else { Ok(n) } });
        assert_eq!(r, Ok(3));
        let mut m = 0;
        let r: Result<(), &str> = with_retries(4, std::time::Duration::ZERO, || { m += 1; Err("deadline") });
        assert_eq!((r, m), (Err("deadline"), 4));
    }

    #[test_id(t212mol)]
    #[test]
    fn more_than_two_mends_in_a_day_breaches_and_two_does_not() {
        assert!(mends_breach("sp-x", 2).is_none());
        assert_eq!(mends_breach("sp-x", 3).unwrap().key, "mends:sp-x");
    }

    #[test_id(tykrraf)]
    #[test]
    fn an_announced_breach_is_not_repeated_until_it_clears() {
        let bs = vec![overdue_breach("b", "1", "s", 10, 20), mends_breach("sp-x", 3).unwrap()];
        let seen: BTreeSet<String> = ["overdue:b#1#s".to_string()].into();
        assert_eq!(fresh(&bs, &seen).iter().map(|b| b.key.as_str()).collect::<Vec<_>>(), vec!["mends:sp-x"]);
        assert_eq!(fresh(&bs, &BTreeSet::new()).len(), 2);
    }

    #[test_id(t6boc8r)]
    #[test]
    fn the_overdue_line_says_how_late() {
        assert!(overdue_breach("b", "1", "s", 10, 70).line.contains("60s past its deadline"));
    }

    fn overdue_row(deadline: i64) -> serde_json::Value {
        serde_json::json!({"batch_id": "batch-mend", "pass": "1", "suite": "test-f.sh", "deadline": deadline.to_string()})
    }

    #[test_id(tvk3ij5)]
    #[test]
    fn a_row_inside_its_deadline_is_no_breach() {
        assert!(collect(&[overdue_row(1_100)], &[], 1_000).is_empty());
    }

    #[test_id(tvwuwjy)]
    #[test]
    fn an_overdue_row_fires_the_alarm_naming_its_key() {
        let bs = collect(&[overdue_row(970)], &[], 1_000);
        assert_eq!(bs.len(), 1);
        assert!(bs[0].line.starts_with("MEND-WATCH OVERDUE batch-mend#1#test-f.sh "), "{}", bs[0].line);
    }

    #[test_id(tacd8tu)]
    #[test]
    fn without_a_seen_set_every_breach_is_reported() {
        let bs = collect(&[overdue_row(970)], &[], 1_000);
        assert_eq!(fresh(&bs, &BTreeSet::new()).len(), 1);
    }

    #[test_id(torbq5h)]
    #[test]
    fn a_third_mend_in_a_day_fires_the_alarm_and_two_do_not() {
        let mends = [serde_json::json!({"bead": "sp-twice", "n": "2"}), serde_json::json!({"bead": "sp-loop", "n": "3"})];
        let bs = collect(&[], &mends, 1_000);
        assert_eq!(bs.len(), 1);
        assert!(bs[0].line.starts_with("MEND-WATCH MENDS sp-loop was mended 3 times"), "{}", bs[0].line);
    }
}
