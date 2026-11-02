//! Typed, stamped outcomes. A line opens with a UTC stamp, a kind and a cause from closed sets,
//! and no line says an act happened unless the verb's call returned `Ok`
//! (law-record-the-act-only-on-the-act): an [`Act`] can only be made from one.

use std::sync::Mutex;

use state_store::Rev;

use crate::orphans::{Orphan, Redrive};
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
    /// An orphan's runner was launched under a new lease.
    Redriven,
    RedriveFailed,
    /// An orphan whose last launch failed, inside its back-off.
    RedriveWaiting,
    /// An orphan whose old runner is not proven dead: held.
    PriorRunnerAlive,
    /// An orphan proven dead with no runner slot free to re-drive it on.
    RedriveNoSlot,
    RedriveExhausted,
    StoreRefused,
    /// A step withheld its act for a typed cause, which [`Cause::Refusal`] keys.
    Declined,
    /// The landing ref already held the round's head, so it was recorded LANDED from git.
    FastForward,
    /// A GREEN round the Lander did not land, kept with its typed blocker.
    Held,
    /// The repo's section was written.
    Published,
    /// An intent on the repo's section was acknowledged, with its result as the cause.
    Acked,
}

impl Kind {
    /// Every kind's word, each once.
    pub fn words() -> Vec<&'static str> {
        let acts = Verb::ALL.iter().map(|v| Kind::Did(Act(*v)));
        [
            Kind::Visited,
            Kind::Refused(Verb::Open),
            Kind::Redriven,
            Kind::RedriveFailed,
            Kind::RedriveWaiting,
            Kind::PriorRunnerAlive,
            Kind::RedriveNoSlot,
            Kind::RedriveExhausted,
            Kind::StoreRefused,
            Kind::Declined,
            Kind::FastForward,
            Kind::Held,
            Kind::Published,
            Kind::Acked,
        ]
        .into_iter()
        .chain(acts)
        .map(|k| k.word())
        .collect()
    }

    pub fn word(&self) -> &'static str {
        match self {
            Kind::Visited => "visited",
            Kind::Redriven => "redriven",
            Kind::RedriveFailed => "redrive-failed",
            Kind::RedriveWaiting => "redrive-waiting",
            Kind::PriorRunnerAlive => "prior-runner-alive",
            Kind::RedriveNoSlot => "redrive-no-slot",
            Kind::RedriveExhausted => "redrive-exhausted",
            Kind::StoreRefused => "store-refused",
            Kind::Did(a) => match a.0 {
                Verb::Open => "opened",
                Verb::Status => "read-status",
                Verb::Certify => "certify-started",
                Verb::Eject => "ejected",
                Verb::Land => "landed",
                Verb::Abandon => "abandoned",
                Verb::PassStart => "pass-started",
                Verb::Supersede => "superseded",
            },
            Kind::Refused(_) => "refused",
            Kind::Declined => "declined",
            Kind::FastForward => "fast-forward",
            Kind::Held => "held",
            Kind::Published => "published",
            Kind::Acked => "acked",
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
    /// An orphan whose lease `expired` lapsed; `text` is what its re-drive found.
    Orphan {
        expired: Rev,
        text: String,
    },
    Store {
        text: String,
    },
    /// A step's own refusal: its word, and the key that names its cause, so a repeat of the
    /// same refusal is the same key.
    Refusal {
        word: &'static str,
        key: String,
    },
    /// A landing: how long the round waited GREEN for it.
    Dwell {
        ms: u64,
    },
    /// A published section's levels the rounds dashboard plots.
    Section {
        green_unlanded_minutes: u64,
        repeat_identity_passes_24h: u64,
    },
    /// An intent's ack: `applied`, `already-applied` or `refused`, and the intent's key.
    Ack {
        result: &'static str,
        key: String,
    },
    /// spira-lc refused a write: the key and field it names, and the rule it broke.
    Admission {
        key: String,
        rule: String,
    },
}

impl Cause {
    pub fn word(&self) -> &'static str {
        match self {
            Cause::Steps => "steps",
            Cause::Ok => "ok",
            Cause::Exit { .. } => "exit",
            Cause::NotRun { .. } => "not-run",
            Cause::Orphan { .. } => "orphan",
            Cause::Store { .. } => "store",
            Cause::Refusal { word, .. } => word,
            Cause::Dwell { .. } => "dwell",
            Cause::Section { .. } => "section",
            Cause::Ack { result, .. } => result,
            Cause::Admission { .. } => "admission-refused",
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
    /// A step withheld its act: `word` is the refusal's, `key` names its cause.
    pub fn declined(at: u64, repo: &str, word: &'static str, key: &str) -> Outcome {
        Outcome {
            at,
            repo: repo.into(),
            batch: None,
            kind: Kind::Declined,
            cause: Cause::Refusal {
                word,
                key: key.into(),
            },
        }
    }

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

    /// A landing `land` returned `Ok` for, with how long the round waited GREEN.
    pub fn landing(at: u64, repo: &str, batch: &str, dwell_ms: u64) -> Outcome {
        Outcome {
            cause: Cause::Dwell { ms: dwell_ms },
            ..Outcome::of::<()>(at, repo, Some(batch), Verb::Land, &Ok(()))
        }
    }

    /// A round recorded LANDED because the landing ref already held its head.
    pub fn fast_forward(at: u64, repo: &str, batch: &str, dwell_ms: u64) -> Outcome {
        Outcome {
            at,
            repo: repo.into(),
            batch: Some(batch.into()),
            kind: Kind::FastForward,
            cause: Cause::Dwell { ms: dwell_ms },
        }
    }

    /// A GREEN round held with the blocker `word`, kept at `key`.
    pub fn held(at: u64, repo: &str, batch: &str, word: &'static str, key: &str) -> Outcome {
        Outcome {
            at,
            repo: repo.into(),
            batch: Some(batch.into()),
            kind: Kind::Held,
            cause: Cause::Refusal {
                word,
                key: key.into(),
            },
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
            Cause::NotRun { text } | Cause::Store { text } => s += &format!(" text={text:?}"),
            Cause::Orphan { expired, text } => {
                s += &format!(" expired={}-{} text={text:?}", expired.main, expired.sub)
            }
            Cause::Refusal { key, .. } | Cause::Ack { key, .. } => s += &format!(" key={key}"),
            Cause::Dwell { ms } => s += &format!(" dwell_ms={ms}"),
            Cause::Section {
                green_unlanded_minutes,
                repeat_identity_passes_24h,
            } => {
                s += &format!(" green_unlanded_minutes={green_unlanded_minutes} repeat_identity_passes_24h={repeat_identity_passes_24h}")
            }
            Cause::Admission { key, rule } => s += &format!(" key={key} rule={rule:?}"),
            Cause::Steps | Cause::Ok => {}
        }
        s
    }

    pub fn published(at: u64, repo: &str, green_unlanded_minutes: u64, repeat_identity_passes_24h: u64) -> Outcome {
        Outcome {
            at,
            repo: repo.into(),
            batch: None,
            kind: Kind::Published,
            cause: Cause::Section {
                green_unlanded_minutes,
                repeat_identity_passes_24h,
            },
        }
    }

    pub fn acked(at: u64, repo: &str, key: &str, result: &'static str) -> Outcome {
        Outcome {
            at,
            repo: repo.into(),
            batch: None,
            kind: Kind::Acked,
            cause: Cause::Ack {
                result,
                key: key.into(),
            },
        }
    }

    /// A write spira-lc's admission refused, with the rule it broke.
    pub fn refused_write(at: u64, repo: &str, key: &str, rule: &str) -> Outcome {
        Outcome {
            at,
            repo: repo.into(),
            batch: None,
            kind: Kind::Declined,
            cause: Cause::Admission {
                key: key.into(),
                rule: rule.into(),
            },
        }
    }

    pub fn store_refused(at: u64, repo: &str, text: &str) -> Outcome {
        Outcome {
            at,
            repo: repo.into(),
            batch: None,
            kind: Kind::StoreRefused,
            cause: Cause::Store { text: text.into() },
        }
    }

    /// The line of one orphan's re-drive; `None` when another holder took its lease or the round
    /// turned terminal meanwhile, since then it is no longer an orphan.
    pub fn redrive(at: u64, o: &Orphan, r: &Redrive) -> Option<Outcome> {
        let (kind, text) = match r {
            Redrive::Launched { unit } => (Kind::Redriven, format!("launched {unit}")),
            Redrive::Failed {
                why,
                next_attempt_at_ms,
            } => (
                Kind::RedriveFailed,
                format!("{why}; next attempt {}", utc_stamp(*next_attempt_at_ms)),
            ),
            Redrive::Waiting { until_ms } => (
                Kind::RedriveWaiting,
                format!("backing off until {}", utc_stamp(*until_ms)),
            ),
            Redrive::PriorRunnerAlive { why } => (Kind::PriorRunnerAlive, why.clone()),
            Redrive::NoSlot { why } => (Kind::RedriveNoSlot, why.clone()),
            Redrive::Exhausted { failures } => (
                Kind::RedriveExhausted,
                format!("{failures} failed launches"),
            ),
            Redrive::Store(e) => (Kind::StoreRefused, e.clone()),
            Redrive::Leased(_) | Redrive::Terminal => return None,
        };
        Some(Outcome {
            at,
            repo: o.repo.clone(),
            batch: Some(o.batch.clone()),
            kind,
            cause: Cause::Orphan {
                expired: o.expired,
                text,
            },
        })
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
    fn a_declined_step_names_its_refusal_and_the_key_of_its_cause() {
        let o = Outcome::declined(0, "pokebot", "lane-fault", "faults/pokebot/b0/sha256:d1");
        assert_eq!(
            o.line(),
            "1970-01-01T00:00:00Z declined lane-fault repo=pokebot batch=- key=faults/pokebot/b0/sha256:d1"
        );
    }

    #[test]
    fn every_kind_has_its_own_word() {
        let words: std::collections::BTreeSet<&str> = Kind::words().into_iter().collect();
        assert_eq!(words.len(), Verb::ALL.len() + 14);
        assert!(
            words.contains("visited")
                && words.contains("refused")
                && words.contains("pass-started")
        );
        assert_eq!(Act(Verb::Land).verb(), Verb::Land);
    }

    #[test]
    fn each_redrive_is_one_line_naming_the_expired_token_and_a_race_is_none() {
        let o = Orphan {
            repo: "r".into(),
            batch: "b1".into(),
            expired: Rev::new(7, 0),
        };
        let line = |r: Redrive| Outcome::redrive(0, &o, &r).map(|o| o.line());
        let at = "1970-01-01T00:00:00Z";
        assert_eq!(
            line(Redrive::Launched { unit: "u".into() }).unwrap(),
            format!("{at} redriven orphan repo=r batch=b1 expired=7-0 text=\"launched u\"")
        );
        assert_eq!(
            line(Redrive::Failed {
                why: "x".into(),
                next_attempt_at_ms: 60_000
            })
            .unwrap(),
            format!("{at} redrive-failed orphan repo=r batch=b1 expired=7-0 text=\"x; next attempt 1970-01-01T00:01:00Z\"")
        );
        assert!(line(Redrive::Waiting { until_ms: 0 })
            .unwrap()
            .contains("redrive-waiting orphan"));
        assert!(line(Redrive::PriorRunnerAlive { why: "w".into() })
            .unwrap()
            .contains("prior-runner-alive orphan"));
        assert!(line(Redrive::NoSlot { why: "n".into() })
            .unwrap()
            .contains("redrive-no-slot orphan"));
        assert!(line(Redrive::Exhausted { failures: 3 })
            .unwrap()
            .contains("text=\"3 failed launches\""));
        assert!(line(Redrive::Store("s".into()))
            .unwrap()
            .contains("store-refused orphan"));
        assert_eq!(
            line(Redrive::Leased(crate::lease::Held {
                holder: None,
                token: Rev::new(8, 0),
                ttl_left_ms: 0
            })),
            None
        );
        assert_eq!(
            Outcome::store_refused(0, "r", "down").line(),
            format!("{at} store-refused store repo=r batch=- text=\"down\"")
        );
    }

    #[test]
    fn a_landing_names_its_dwell_and_a_hold_its_blocker_and_status_key() {
        assert_eq!(
            Outcome::landing(0, "r", "k1", 90_000).line(),
            "1970-01-01T00:00:00Z landed dwell repo=r batch=k1 dwell_ms=90000"
        );
        assert_eq!(
            Outcome::fast_forward(0, "r", "k1", 0).line(),
            "1970-01-01T00:00:00Z fast-forward dwell repo=r batch=k1 dwell_ms=0"
        );
        assert_eq!(
            Outcome::held(0, "r", "k2", "stacked-on-unlanded", "batches/r/k2/status").line(),
            "1970-01-01T00:00:00Z held stacked-on-unlanded repo=r batch=k2 key=batches/r/k2/status"
        );
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
