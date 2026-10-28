//! The end of a red pass: wait for every mender, bounded by the last deadline, then hand the
//! settled outcomes to the pure decision. The machine owns each mender's deadline; this wait
//! only reads the rows, asks the machine to sweep what is overdue, and never outlives the
//! last deadline whatever the rows say.

use batcher::mender::{Failure, Outcome};
use serde_json::Value;

pub const POLL_SECS: i64 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub suite: String,
    pub state: String,
    pub deadline: Option<i64>,
    pub diagnosis: String,
    /// The mend waits on an open ask; no deadline binds the wait while this holds.
    pub ask_open: bool,
}

pub trait Menders {
    fn now(&self) -> i64;
    fn sleep(&mut self, secs: i64);
    fn rows(&mut self) -> Result<Vec<Row>, String>;
    /// Applies the machine's expiry to every overdue MENDING row, and gives a row whose awaited
    /// ask has closed a fresh `deadline_s` to record its mend.
    fn sweep(&mut self, deadline_s: i64) -> Result<(), String>;
}

pub fn parse_rows(json: &str, batch_pass: u32) -> Result<Vec<Row>, String> {
    let v: Vec<Value> = serde_json::from_str(json).map_err(|e| format!("mending list: unparsed reply: {e}"))?;
    Ok(v.iter()
        .filter(|r| r.get("pass").and_then(Value::as_u64) == Some(batch_pass as u64))
        .filter_map(|r| {
            Some(Row {
                suite: r.get("suite")?.as_str()?.to_string(),
                state: r.get("state")?.as_str()?.to_string(),
                deadline: r.get("deadline").and_then(Value::as_i64),
                diagnosis: r.get("diagnosis").and_then(Value::as_str).unwrap_or("").to_string(),
                ask_open: r.get("ask_open").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect())
}

fn live(r: &Row) -> bool {
    matches!(r.state.as_str(), "WAITING" | "MENDING")
}

fn outcome(state: &str) -> Option<Outcome> {
    Some(match state {
        "MENDED" => Outcome::Mended,
        "ENVIRONMENT" => Outcome::Environment,
        "FLAKY" => Outcome::Flaky,
        "RETURNED" => Outcome::Returned,
        "UNSURE" => Outcome::Unsure,
        _ => return None,
    })
}

/// The latest moment the wait may still be going: the last MENDING deadline, and for a row not
/// yet picked up a full `deadline_s` from now, which is the latest its own deadline can fall.
fn cap(rows: &[Row], now: i64, deadline_s: i64) -> i64 {
    rows.iter().filter(|r| live(r)).map(|r| r.deadline.unwrap_or(now + deadline_s)).max().unwrap_or(now)
}

/// Blocks until every row of the pass is settled. A row still live once the cap has passed and
/// the machine has swept is reported as unsure, so that a guess never ejects anyone.
pub fn wait<M: Menders>(m: &mut M, deadline_s: i64) -> Result<Vec<Failure>, String> {
    let mut rows = m.rows()?;
    let mut ceiling = cap(&rows, m.now(), deadline_s);
    while rows.iter().any(live) {
        let now = m.now();
        if rows.iter().any(|r| live(r) && r.ask_open) {
            m.sleep(POLL_SECS);
            m.sweep(deadline_s)?;
            rows = m.rows()?;
            ceiling = ceiling.max(m.now() + deadline_s);
            continue;
        }
        if now >= ceiling {
            m.sweep(deadline_s)?;
            rows = m.rows()?;
            break;
        }
        m.sleep(POLL_SECS.min(ceiling - now));
        rows = m.rows()?;
        ceiling = ceiling.max(rows.iter().filter(|r| r.state == "MENDING").filter_map(|r| r.deadline).max().unwrap_or(ceiling));
    }
    Ok(rows
        .into_iter()
        .map(|r| match outcome(&r.state) {
            Some(o) => Failure { suite: r.suite, outcome: o, diagnosis: r.diagnosis, members: vec![] },
            None => Failure { suite: r.suite, outcome: Outcome::Unsure, diagnosis: format!("no mender outcome by the last deadline ({})", r.state), members: vec![] },
        })
        .collect())
}

pub struct LcMenders<'a> {
    pub env: &'a crate::io::Env,
    pub batch: &'a str,
    pub pass: u32,
}

impl Menders for LcMenders<'_> {
    fn now(&self) -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
    }
    fn sleep(&mut self, secs: i64) {
        std::thread::sleep(std::time::Duration::from_secs(secs.max(0) as u64));
    }
    fn rows(&mut self) -> Result<Vec<Row>, String> {
        parse_rows(&crate::io::lcq(self.env, &["mending", "list", "--batch", self.batch])?, self.pass)
    }
    fn sweep(&mut self, deadline_s: i64) -> Result<(), String> {
        crate::io::lcq(self.env, &["mending", "sweep", "--actor", "batcher", "--deadline-secs", &deadline_s.to_string()]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        t: i64,
        slept: i64,
        script: Vec<(i64, Vec<Row>)>,
        swept: u32,
        stuck: bool,
    }
    fn row(suite: &str, state: &str, deadline: Option<i64>) -> Row {
        Row { suite: suite.into(), state: state.into(), deadline, diagnosis: "d".into(), ask_open: false }
    }
    impl Menders for Fake {
        fn now(&self) -> i64 {
            self.t
        }
        fn sleep(&mut self, secs: i64) {
            self.t += secs;
            self.slept += secs;
        }
        fn rows(&mut self) -> Result<Vec<Row>, String> {
            let t = self.t;
            Ok(self.script.iter().rev().find(|(at, _)| *at <= t).map(|(_, r)| r.clone()).unwrap_or_default())
        }
        fn sweep(&mut self, _deadline_s: i64) -> Result<(), String> {
            self.swept += 1;
            if !self.stuck {
                for (_, rows) in self.script.iter_mut() {
                    for r in rows.iter_mut().filter(|r| r.state == "MENDING") {
                        r.state = "RETURNED".into();
                    }
                }
            }
            Ok(())
        }
    }
    fn fake(script: Vec<(i64, Vec<Row>)>) -> Fake {
        Fake { t: 1000, slept: 0, script, swept: 0, stuck: false }
    }

    #[test]
    fn settled_rows_return_without_waiting() {
        let mut f = fake(vec![(0, vec![row("a", "MENDED", Some(1300)), row("b", "FLAKY", None)])]);
        let out = wait(&mut f, 300).unwrap();
        assert_eq!((f.slept, f.swept), (0, 0));
        assert_eq!(out.iter().map(|x| x.outcome).collect::<Vec<_>>(), [Outcome::Mended, Outcome::Flaky]);
    }

    #[test]
    fn the_wait_ends_when_the_last_mender_settles_not_at_its_deadline() {
        let mut f = fake(vec![(0, vec![row("a", "MENDING", Some(1300))]), (1040, vec![row("a", "ENVIRONMENT", Some(1300))])]);
        let out = wait(&mut f, 300).unwrap();
        assert!((40..=42).contains(&f.slept), "slept {}", f.slept);
        assert_eq!(out[0].outcome, Outcome::Environment);
    }

    #[test]
    fn an_overdue_row_is_swept_at_the_deadline_and_the_wait_never_passes_it() {
        let mut f = fake(vec![(0, vec![row("a", "MENDING", Some(1300)), row("b", "MENDING", Some(1200))])]);
        let out = wait(&mut f, 300).unwrap();
        assert!(f.t <= 1300, "waited until {}", f.t);
        assert_eq!(f.swept, 1);
        assert!(out.iter().all(|x| x.outcome == Outcome::Returned));
    }

    #[test]
    fn a_row_the_machine_does_not_settle_is_unsure_and_the_wait_still_ends() {
        let mut f = fake(vec![(0, vec![row("a", "MENDING", Some(1100))])]);
        f.stuck = true;
        let out = wait(&mut f, 300).unwrap();
        assert!(f.t <= 1100);
        assert_eq!(out[0].outcome, Outcome::Unsure);
        assert!(out[0].diagnosis.contains("MENDING"));
    }

    #[test]
    fn a_waiting_row_is_bounded_by_a_full_deadline_from_now() {
        let mut f = fake(vec![(0, vec![row("a", "WAITING", None)])]);
        f.stuck = true;
        let out = wait(&mut f, 300).unwrap();
        assert!(f.t <= 1300, "waited until {}", f.t);
        assert_eq!(out[0].outcome, Outcome::Unsure);
    }

    #[test]
    fn a_waiting_row_picked_up_late_never_extends_the_wait_past_a_deadline_from_when_the_wait_began() {
        let mut f = fake(vec![
            (0, vec![row("a", "WAITING", None)]),
            (1100, vec![row("a", "MENDING", Some(1400))]),
        ]);
        f.stuck = true;
        wait(&mut f, 300).unwrap();
        assert!(f.t <= 1400);
    }

    #[test]
    fn a_mend_waiting_on_an_open_ask_is_not_returned_at_its_deadline() {
        let waiting = Row { ask_open: true, ..row("a", "MENDING", Some(1300)) };
        let mut f = fake(vec![(0, vec![waiting]), (2000, vec![row("a", "MENDED", Some(2300))])]);
        f.stuck = true;
        let out = wait(&mut f, 300).unwrap();
        assert!(f.t >= 2000, "returned at {}", f.t);
        assert_eq!(out[0].outcome, Outcome::Mended);
    }

    #[test]
    fn the_listing_is_filtered_to_the_pass() {
        let json = r#"[{"pass":1,"suite":"a","state":"MENDED","deadline":5,"diagnosis":null},{"pass":2,"suite":"b","state":"MENDING","deadline":9,"diagnosis":"x"}]"#;
        assert_eq!(parse_rows(json, 2).unwrap(), [Row { suite: "b".into(), state: "MENDING".into(), deadline: Some(9), diagnosis: "x".into(), ask_open: false }]);
        assert!(parse_rows("nope", 1).is_err());
    }
}
