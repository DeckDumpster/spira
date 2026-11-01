//! Typed, stamped outcomes. A line opens with a UTC stamp, a kind and a cause from closed sets,
//! and no line says an act happened unless the verb's call returned `Ok`
//! (law-record-the-act-only-on-the-act): an [`Act`] can only be made from one.

use std::sync::Mutex;

use crate::ports::{Verb, VerbRefusal};

/// Proof that a verb's call returned `Ok`; its field is private so nothing else can make one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Act(Verb);

impl Act {
    pub fn verb(self) -> Verb {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A pass visited the repo and ran its steps.
    Visited,
    Did(Act),
    Refused(Verb),
}

impl Kind {
    /// Every kind's word, each once.
    pub fn words() -> Vec<&'static str> {
        let acts = Verb::ALL.iter().map(|v| Kind::Did(Act(*v)));
        [Kind::Visited, Kind::Refused(Verb::Open)]
            .into_iter()
            .chain(acts)
            .map(|k| k.word())
            .collect()
    }

    pub fn word(&self) -> &'static str {
        match self {
            Kind::Visited => "visited",
            Kind::Did(a) => match a.0 {
                Verb::Open => "opened",
                Verb::Status => "read-status",
                Verb::Certify => "certify-started",
                Verb::Eject => "ejected",
                Verb::Land => "landed",
                Verb::Abandon => "abandoned",
                Verb::PassStart => "pass-started",
            },
            Kind::Refused(_) => "refused",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The routine pass: every step ran.
    Steps,
    Ok,
    /// The verb ran and exited with this code.
    Exit {
        code: i32,
        text: String,
    },
    /// The verb never reached an exit: not spawned, or past its deadline.
    NotRun {
        text: String,
    },
}

impl Cause {
    pub fn word(&self) -> &'static str {
        match self {
            Cause::Steps => "steps",
            Cause::Ok => "ok",
            Cause::Exit { .. } => "exit",
            Cause::NotRun { .. } => "not-run",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Epoch milliseconds on the controller's clock.
    pub at: u64,
    pub repo: String,
    pub batch: Option<String>,
    pub kind: Kind,
    pub cause: Cause,
}

impl Outcome {
    pub fn visited(at: u64, repo: &str) -> Outcome {
        Outcome {
            at,
            repo: repo.into(),
            batch: None,
            kind: Kind::Visited,
            cause: Cause::Steps,
        }
    }

    /// The outcome of one verb call: an act only on `Ok`, a typed refusal otherwise.
    pub fn of<T>(
        at: u64,
        repo: &str,
        batch: Option<&str>,
        verb: Verb,
        r: &Result<T, VerbRefusal>,
    ) -> Outcome {
        let (kind, cause) = match r {
            Ok(_) => (Kind::Did(Act(verb)), Cause::Ok),
            Err(e) => {
                let text = e.text.clone();
                (
                    Kind::Refused(verb),
                    e.code
                        .map_or(Cause::NotRun { text: text.clone() }, |code| Cause::Exit {
                            code,
                            text,
                        }),
                )
            }
        };
        Outcome {
            at,
            repo: repo.into(),
            batch: batch.map(str::to_string),
            kind,
            cause,
        }
    }

    pub fn line(&self) -> String {
        let mut s = format!(
            "{} {} {} repo={} batch={}",
            utc_stamp(self.at),
            self.kind.word(),
            self.cause.word(),
            self.repo,
            self.batch.as_deref().unwrap_or("-")
        );
        if let Kind::Refused(v) = &self.kind {
            s += &format!(" verb={}", v.word());
        }
        match &self.cause {
            Cause::Exit { code, text } => s += &format!(" code={code} text={text:?}"),
            Cause::NotRun { text } => s += &format!(" text={text:?}"),
            Cause::Steps | Cause::Ok => {}
        }
        s
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` of epoch milliseconds.
pub fn utc_stamp(ms: u64) -> String {
    let t = ms / 1000;
    let (days, rem) = ((t / 86400) as i64, t % 86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub trait Log {
    fn write(&self, o: &Outcome);
}

/// Each outcome's line on stdout, which the unit appends to its log.
pub struct StdoutLog;

impl Log for StdoutLog {
    fn write(&self, o: &Outcome) {
        println!("{}", o.line());
    }
}

/// Outcomes kept in order, for a scenario to count.
#[derive(Default)]
pub struct MemLog(Mutex<Vec<Outcome>>);

impl MemLog {
    pub fn outcomes(&self) -> Vec<Outcome> {
        crate::lock(&self.0).clone()
    }
}

impl Log for MemLog {
    fn write(&self, o: &Outcome) {
        crate::lock(&self.0).push(o.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(code: Option<i32>) -> Result<(), VerbRefusal> {
        Err(VerbRefusal {
            verb: Verb::Eject,
            code,
            text: "no such suite \"ci\"\nsecond line".into(),
        })
    }

    #[test]
    fn a_line_opens_with_the_utc_stamp_then_the_kind_and_cause_words() {
        let o = Outcome::visited(1_793_486_548_000, "spira");
        assert_eq!(
            o.line(),
            "2026-10-31T22:42:28Z visited steps repo=spira batch=-"
        );
    }

    #[test]
    fn an_ok_call_is_the_act_and_a_refusal_never_says_it_happened() {
        let ok = Outcome::of(0, "r", Some("b1"), Verb::Eject, &Ok::<(), VerbRefusal>(()));
        assert_eq!(ok.line(), "1970-01-01T00:00:00Z ejected ok repo=r batch=b1");
        assert_eq!(ok.kind, Kind::Did(Act(Verb::Eject)));
        let no = Outcome::of(0, "r", Some("b1"), Verb::Eject, &refusal(Some(1)));
        assert_eq!(no.line(), "1970-01-01T00:00:00Z refused exit repo=r batch=b1 verb=eject code=1 text=\"no such suite \\\"ci\\\"\\nsecond line\"");
        assert!(!no.line().contains("ejected"));
        let unrun = Outcome::of(0, "r", None, Verb::Eject, &refusal(None));
        assert!(
            unrun.line().starts_with(
                "1970-01-01T00:00:00Z refused not-run repo=r batch=- verb=eject text="
            ),
            "{}",
            unrun.line()
        );
    }

    #[test]
    fn every_kind_has_its_own_word() {
        let words: std::collections::BTreeSet<&str> = Kind::words().into_iter().collect();
        assert_eq!(words.len(), Verb::ALL.len() + 2);
        assert!(
            words.contains("visited")
                && words.contains("refused")
                && words.contains("pass-started")
        );
        assert_eq!(Act(Verb::Land).verb(), Verb::Land);
    }

    #[test]
    fn the_stamp_crosses_a_leap_day_and_a_century() {
        assert_eq!(utc_stamp(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(utc_stamp(4_107_542_399_000), "2100-02-28T23:59:59Z");
    }

    #[test]
    fn the_memory_log_keeps_outcomes_in_order() {
        let log = MemLog::default();
        log.write(&Outcome::visited(1, "a"));
        log.write(&Outcome::visited(2, "b"));
        assert_eq!(
            log.outcomes()
                .iter()
                .map(|o| o.repo.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        StdoutLog.write(&Outcome::visited(0, "a"));
    }
}
