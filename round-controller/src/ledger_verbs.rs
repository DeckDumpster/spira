//! `ledger record` and `ledger refuses`: the ledger's own answer for an identity named on the
//! command line, printed as `key=value` lines. They exist so a world can drive the ledger
//! through the real store door; nothing in a pass calls them.

use crate::ledger::{self, FaultPolicy, LedgerRefusal};
use crate::ports::{Member, Ports};
use crate::resources::{Attempt, Backoff, Cause, Class, InitiatedBy, PassIdentity};

const IDENTITY_FLAGS: [&str; 6] = [
    "--repo",
    "--members",
    "--base",
    "--image-digest",
    "--harness-version",
    "--selection",
];

#[derive(Debug)]
pub struct Record {
    pub identity: PassIdentity,
    pub batch: String,
    pub class: Class,
    pub evidence: String,
}

fn flags(args: &[&str], allowed: &[&str]) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if !allowed.contains(flag) {
            return Err(format!("unknown argument {flag:?}"));
        }
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if out.iter().any(|(f, _)| f == flag) {
            return Err(format!("{flag} given twice"));
        }
        out.push((flag.to_string(), value.to_string()));
    }
    Ok(out)
}

fn take<'a>(got: &'a [(String, String)], flag: &str) -> Result<&'a str, String> {
    got.iter()
        .find(|(f, _)| f == flag)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("{flag} is required"))
}

fn identity(got: &[(String, String)]) -> Result<PassIdentity, String> {
    let members = take(got, "--members")?
        .split(',')
        .map(|m| match m.split_once('@') {
            Some((id, tip)) if !id.is_empty() && !tip.is_empty() => Ok(Member {
                id: id.into(),
                tip: tip.into(),
            }),
            _ => Err(format!("member {m:?} is not <id>@<tip>")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    PassIdentity::new(
        take(got, "--repo")?,
        members,
        take(got, "--base")?,
        take(got, "--image-digest")?,
        take(got, "--harness-version")?,
        take(got, "--selection")?,
    )
    .map_err(|e| e.to_string())
}

pub fn parse_class(word: &str) -> Result<Class, String> {
    let (head, rest) = match word.split_once(':') {
        Some((h, r)) => (h, Some(r)),
        None => (word, None),
    };
    match (head, rest) {
        ("green", None) => Ok(Class::Green),
        ("flaky", None) => Ok(Class::Flaky),
        ("red", Some(step)) if !step.is_empty() => Ok(Class::Red { step: step.into() }),
        ("fault", Some(cause)) => Ok(Class::Fault {
            cause: parse_cause(cause)?,
        }),
        _ => Err(format!(
            "class {word:?} is not green, flaky, red:<step> or fault:<cause>"
        )),
    }
}

fn parse_cause(word: &str) -> Result<Cause, String> {
    match word {
        "base" => Ok(Cause::Base),
        "install" => Ok(Cause::Install),
        "vm" => Ok(Cause::Vm),
        "over-cap" => Ok(Cause::OverCap),
        "fences-cap" => Ok(Cause::FencesCap),
        _ => match word.strip_prefix("image:") {
            Some(digest) if !digest.is_empty() => Ok(Cause::Image {
                digest: digest.into(),
            }),
            _ => Err(format!(
                "cause {word:?} is not base, install, vm, over-cap, fences-cap or image:<digest>"
            )),
        },
    }
}

/// `--batch <b> --class <c> [--evidence <text>]` beside the identity's six flags.
pub fn parse_record(args: &[&str]) -> Result<Record, String> {
    let mut allowed = IDENTITY_FLAGS.to_vec();
    allowed.extend(["--batch", "--class", "--evidence"]);
    let got = flags(args, &allowed)?;
    Ok(Record {
        identity: identity(&got)?,
        batch: take(&got, "--batch")?.into(),
        class: parse_class(take(&got, "--class")?)?,
        evidence: take(&got, "--evidence").unwrap_or_default().into(),
    })
}

pub fn parse_refuses(args: &[&str]) -> Result<PassIdentity, String> {
    identity(&flags(args, &IDENTITY_FLAGS)?)
}

fn backoff_word(b: Option<Backoff>) -> String {
    match b {
        None => "none".into(),
        Some(Backoff::Standing) => "standing".into(),
        Some(Backoff::Until(t)) => format!("until={t}"),
    }
}

pub fn render_refusal(r: &Option<LedgerRefusal>) -> String {
    match r {
        None => "refuses=none".into(),
        Some(r) => {
            let detail = match r {
                LedgerRefusal::RedIdentity { step, .. } => format!(" step={step}"),
                LedgerRefusal::RedMember { id, tip, .. } => format!(" member={id}@{tip}"),
                LedgerRefusal::LaneFault { key, cause } => {
                    format!(" key={key} cause={}", cause.word())
                }
                LedgerRefusal::BackingOff { until, .. } => match until {
                    Some(t) => format!(" until={t}"),
                    None => " until=standing".into(),
                },
            };
            format!("refuses={}{detail}", r.word())
        }
    }
}

pub fn record(p: &Ports, policy: &FaultPolicy, r: &Record) -> Result<String, String> {
    let attempt = Attempt {
        class: r.class.clone(),
        initiated_by: InitiatedBy::Judge,
        sub_passes: vec![],
        evidence: r.evidence.clone(),
        at: p.clock.now_ms(),
    };
    let rec = ledger::record_attempt(p, &r.identity, &r.batch, &attempt, policy)
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "hash={}\nattempt={}\nclass={}\nbackoff={}",
        r.identity.hash(),
        rec.n,
        rec.summary.class.word(),
        backoff_word(rec.summary.backoff_until)
    ))
}

pub fn refuses(p: &Ports, id: &PassIdentity) -> Result<String, String> {
    let answer = ledger::refuses(p, id).map_err(|e| e.to_string())?;
    Ok(format!("hash={}\n{}", id.hash(), render_refusal(&answer)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: [&str; 12] = [
        "--repo",
        "r",
        "--members",
        "sp-b@t2,sp-a@t1",
        "--base",
        "b0",
        "--image-digest",
        "d1",
        "--harness-version",
        "h1",
        "--selection",
        "all",
    ];

    fn with(extra: &[&'static str]) -> Vec<&'static str> {
        let mut v = ID.to_vec();
        v.extend_from_slice(extra);
        v
    }

    #[test]
    fn a_record_names_its_identity_batch_and_class() {
        let r = parse_record(&with(&[
            "--batch",
            "b1",
            "--class",
            "red:build",
            "--evidence",
            "e",
        ]))
        .unwrap();
        assert_eq!(r.batch, "b1");
        assert_eq!(
            r.class,
            Class::Red {
                step: "build".into()
            }
        );
        assert_eq!(r.evidence, "e");
        let ids: Vec<&str> = r.identity.members.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["sp-a", "sp-b"]);
    }

    #[test]
    fn every_class_and_cause_spelling_parses_and_an_unknown_one_is_refused() {
        for (w, c) in [
            ("green", Class::Green),
            ("flaky", Class::Flaky),
            ("fault:vm", Class::Fault { cause: Cause::Vm }),
            (
                "fault:over-cap",
                Class::Fault {
                    cause: Cause::OverCap,
                },
            ),
            (
                "fault:fences-cap",
                Class::Fault {
                    cause: Cause::FencesCap,
                },
            ),
            ("fault:base", Class::Fault { cause: Cause::Base }),
            (
                "fault:install",
                Class::Fault {
                    cause: Cause::Install,
                },
            ),
            (
                "fault:image:sha256:x",
                Class::Fault {
                    cause: Cause::Image {
                        digest: "sha256:x".into(),
                    },
                },
            ),
        ] {
            assert_eq!(parse_class(w), Ok(c), "{w}");
        }
        for w in [
            "",
            "red",
            "red:",
            "green:x",
            "fault:",
            "fault:image:",
            "fault:disk",
            "blue",
        ] {
            assert!(parse_class(w).is_err(), "{w}");
        }
    }

    #[test]
    fn a_missing_repeated_unknown_or_malformed_argument_is_named() {
        let err = |a: Vec<&str>| parse_refuses(&a).unwrap_err();
        assert!(err(ID[2..].to_vec()).contains("--repo is required"));
        assert!(err(with(&["--base", "b2"])).contains("--base given twice"));
        assert!(err(with(&["--batch", "b1"])).contains("unknown argument"));
        assert!(err(with(&["--base"])).contains("--base needs a value"));
        let mut bad = ID.to_vec();
        bad[3] = "sp-a";
        assert!(err(bad).contains("not <id>@<tip>"));
        assert!(parse_record(&with(&["--class", "green"]))
            .unwrap_err()
            .contains("--batch"));
    }

    #[test]
    fn every_refusal_renders_its_word_and_what_it_stands_on() {
        let hash = || "h".to_string();
        assert_eq!(render_refusal(&None), "refuses=none");
        for (r, want) in [
            (
                LedgerRefusal::RedIdentity {
                    hash: hash(),
                    step: "build".into(),
                },
                "refuses=red-identity step=build",
            ),
            (
                LedgerRefusal::RedMember {
                    id: "sp-a".into(),
                    tip: "t1".into(),
                    hash: hash(),
                },
                "refuses=red-member member=sp-a@t1",
            ),
            (
                LedgerRefusal::LaneFault {
                    key: "faults/r/b0/d1".into(),
                    cause: Cause::Base,
                },
                "refuses=lane-fault key=faults/r/b0/d1 cause=base",
            ),
            (
                LedgerRefusal::BackingOff {
                    hash: hash(),
                    until: Some(9),
                },
                "refuses=backing-off until=9",
            ),
            (
                LedgerRefusal::BackingOff {
                    hash: hash(),
                    until: None,
                },
                "refuses=backing-off until=standing",
            ),
        ] {
            assert_eq!(render_refusal(&Some(r)), want);
        }
    }
}
