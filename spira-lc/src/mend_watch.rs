//! `mend-watch [--seen FILE]`: one alarm line per breach of the mender bounds. A MENDING row
//! past its deadline should never exist (the sweep returns it), and a bead mended more than
//! `MAX_MENDS_PER_DAY` times in a day is a loop. Silence on stdout means no breach; with
//! `--seen`, a breach already announced is not repeated until it clears and returns.

use std::collections::BTreeSet;

use crate::db::{now_epoch, Conn};
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

pub fn run(args: &[String], conn: &Conn) -> (i32, String) {
    let now = now_epoch();
    let overdue_sql = format!("SELECT batch_id, pass, suite, deadline FROM mending WHERE state = 'MENDING' AND awaiting_ask IS NULL AND deadline <= {now} ORDER BY deadline");
    let mends_sql = format!("SELECT bead_id AS bead, COUNT(*) AS n FROM mend WHERE at >= {} GROUP BY bead_id", now - DAY_S);
    let overdue = with_retries(READ_ATTEMPTS, READ_PAUSE, || conn.query(&overdue_sql));
    let mends = with_retries(READ_ATTEMPTS, READ_PAUSE, || conn.query(&mends_sql));
    let (overdue, mends) = match (overdue, mends) {
        (Ok(o), Ok(m)) => (o, m),
        (Err(e), _) | (_, Err(e)) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    let mut breaches: Vec<Breach> = overdue
        .iter()
        .map(|r| overdue_breach(&text(r, "batch_id"), &text(r, "pass"), &text(r, "suite"), text(r, "deadline").parse().unwrap_or(now), now))
        .collect();
    breaches.extend(mends.iter().filter_map(|r| {
        let bead = text(r, "bead");
        (!bead.is_empty() && bead != "null").then(|| mends_breach(&bead, text(r, "n").parse().unwrap_or(0))).flatten()
    }));
    let Some(path) = flag(args, "--seen") else {
        return (0, breaches.iter().map(|b| b.line.as_str()).collect::<Vec<_>>().join("\n"));
    };
    let seen: BTreeSet<String> = std::fs::read_to_string(&path).unwrap_or_default().lines().map(String::from).collect();
    let out = fresh(&breaches, &seen).iter().map(|b| b.line.as_str()).collect::<Vec<_>>().join("\n");
    let now_seen: Vec<&str> = breaches.iter().map(|b| b.key.as_str()).collect();
    if let Err(e) = std::fs::write(&path, now_seen.iter().map(|k| format!("{k}\n")).collect::<String>()) {
        return (CANNOT_TELL, format!("cannot record {path}: {e}"));
    }
    (0, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_that_fails_then_succeeds_is_retried_and_one_that_never_does_is_not_hidden() {
        let mut n = 0;
        let r = with_retries(4, std::time::Duration::ZERO, || { n += 1; if n < 3 { Err("deadline") } else { Ok(n) } });
        assert_eq!(r, Ok(3));
        let mut m = 0;
        let r: Result<(), &str> = with_retries(4, std::time::Duration::ZERO, || { m += 1; Err("deadline") });
        assert_eq!((r, m), (Err("deadline"), 4));
    }

    #[test]
    fn more_than_two_mends_in_a_day_breaches_and_two_does_not() {
        assert!(mends_breach("sp-x", 2).is_none());
        assert_eq!(mends_breach("sp-x", 3).unwrap().key, "mends:sp-x");
    }

    #[test]
    fn an_announced_breach_is_not_repeated_until_it_clears() {
        let bs = vec![overdue_breach("b", "1", "s", 10, 20), mends_breach("sp-x", 3).unwrap()];
        let seen: BTreeSet<String> = ["overdue:b#1#s".to_string()].into();
        assert_eq!(fresh(&bs, &seen).iter().map(|b| b.key.as_str()).collect::<Vec<_>>(), vec!["mends:sp-x"]);
        assert_eq!(fresh(&bs, &BTreeSet::new()).len(), 2);
    }

    #[test]
    fn the_overdue_line_says_how_late() {
        assert!(overdue_breach("b", "1", "s", 10, 70).line.contains("60s past its deadline"));
    }
}
