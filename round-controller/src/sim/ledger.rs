//! Scripted ledger acts. Until the Judge and the Planner call the ledger from `pass`, a scenario
//! drives it directly: each `[[ledger_step]]` runs after its pass, on that pass's clock, and
//! says what it wants back; a different answer fails the scenario, naming the step.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::ledger::{
    clear_lane_fault, mark_proven, record_attempt, record_lane_fault, refuses, FaultPolicy,
    LaneFault, LaneFaultRecorded,
};
use crate::ports::{Member, Ports};
use crate::resources::{Attempt, Cause, Class, InitiatedBy, PassIdentity};

/// `[ledger]`: the fault policy the scenario's steps record under.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerConfig {
    pub fault_backoff_secs: u64,
    pub fault_retries: u32,
}

/// `[[identity]]`: a pass identity the steps name.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentitySpec {
    pub name: String,
    pub repo: String,
    /// `id:tip`, in any order.
    pub members: Vec<String>,
    #[serde(default = "base")]
    pub base: String,
    #[serde(default = "image")]
    pub image_digest: String,
    #[serde(default = "harness")]
    pub harness_version: String,
    #[serde(default = "selection")]
    pub selection: String,
}

fn base() -> String {
    "base-0".into()
}
fn image() -> String {
    "sha256:image-0".into()
}
fn harness() -> String {
    "harness-0".into()
}
fn selection() -> String {
    "all".into()
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Act {
    Record,
    Prove,
    LaneFault,
    ClearLaneFault,
    Refuses,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClassWord {
    Green,
    Red,
    Fault,
    Flaky,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CauseWord {
    Image,
    Base,
    Install,
    Vm,
    OverCap,
    FencesCap,
}

/// `[[ledger_step]]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerStep {
    /// The 1-based pass it runs after.
    pub pass: u32,
    pub act: Act,
    pub identity: String,
    #[serde(default)]
    pub class: Option<ClassWord>,
    #[serde(default)]
    pub cause: Option<CauseWord>,
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default = "batch")]
    pub batch: String,
    /// `prove`: the proven members, `id:tip`.
    #[serde(default)]
    pub members: Vec<String>,
    /// `record`: `attempt-<n>`; `refuses`: `admitted` or the refusal's word; `prove` and
    /// `clear-lane-fault`: `ok`; `lane-fault`: `new` or `standing`.
    pub want: String,
}

fn batch() -> String {
    "b1".into()
}

fn member(s: &str) -> Result<Member, String> {
    let (id, tip) = s
        .split_once(':')
        .filter(|(i, t)| !i.is_empty() && !t.is_empty())
        .ok_or_else(|| format!("member {s:?} is not id:tip"))?;
    Ok(Member {
        id: id.into(),
        tip: tip.into(),
    })
}

pub fn identities(specs: &[IdentitySpec]) -> Result<BTreeMap<String, PassIdentity>, String> {
    let mut out = BTreeMap::new();
    for s in specs {
        let members = s
            .members
            .iter()
            .map(|m| member(m))
            .collect::<Result<_, _>>()?;
        let id = PassIdentity::new(
            &s.repo,
            members,
            &s.base,
            &s.image_digest,
            &s.harness_version,
            &s.selection,
        )
        .map_err(|e| format!("identity {}: {e}", s.name))?;
        if out.insert(s.name.clone(), id).is_some() {
            return Err(format!("identity {} is named twice", s.name));
        }
    }
    Ok(out)
}

fn cause(w: CauseWord, id: &PassIdentity) -> Cause {
    match w {
        CauseWord::Image => Cause::Image {
            digest: id.image_digest.clone(),
        },
        CauseWord::Base => Cause::Base,
        CauseWord::Install => Cause::Install,
        CauseWord::Vm => Cause::Vm,
        CauseWord::OverCap => Cause::OverCap,
        CauseWord::FencesCap => Cause::FencesCap,
    }
}

/// Every step's fields checked before the first pass, so a mistyped step fails the load.
pub fn check(
    steps: &[LedgerStep],
    ids: &BTreeMap<String, PassIdentity>,
    config: Option<&LedgerConfig>,
) -> Result<(), String> {
    for (i, s) in steps.iter().enumerate() {
        let at = format!("ledger step {} ({:?} {})", i + 1, s.act, s.identity);
        let id = ids
            .get(&s.identity)
            .ok_or_else(|| format!("{at}: no [[identity]] named {}", s.identity))?;
        if s.act == Act::Record && config.is_none() {
            return Err(format!("{at}: a record step needs [ledger]"));
        }
        class(s, id).map_err(|e| format!("{at}: {e}"))?;
        for m in &s.members {
            member(m).map_err(|e| format!("{at}: {e}"))?;
        }
    }
    Ok(())
}

/// The step's verdict class, or its lane cause for `lane-fault`; `None` for the other acts.
fn class(s: &LedgerStep, id: &PassIdentity) -> Result<Option<Class>, String> {
    let c = match (s.act, s.class, s.cause, &s.step) {
        (Act::Record, Some(ClassWord::Green), None, None) => Class::Green,
        (Act::Record, Some(ClassWord::Flaky), None, None) => Class::Flaky,
        (Act::Record, Some(ClassWord::Red), None, Some(step)) => Class::Red { step: step.clone() },
        (Act::Record | Act::LaneFault, Some(ClassWord::Fault), Some(w), None) => Class::Fault {
            cause: cause(w, id),
        },
        (Act::Prove | Act::ClearLaneFault | Act::Refuses, None, None, None) => return Ok(None),
        _ => {
            return Err(
                "class, cause and step do not fit the act: record takes a class (red with a \
                 step, fault with a cause), lane-fault takes class fault with a cause, and the \
                 rest take none"
                    .into(),
            )
        }
    };
    Ok(Some(c))
}

/// Runs the steps scripted after pass `pass`; each answer that is not the one wanted is named.
pub fn run(
    p: &Ports,
    pass: u32,
    steps: &[LedgerStep],
    ids: &BTreeMap<String, PassIdentity>,
    config: Option<&LedgerConfig>,
    profile: &str,
) -> Vec<String> {
    let policy = config.map(|c| FaultPolicy {
        backoff_secs: c.fault_backoff_secs,
        retries: c.fault_retries,
    });
    let mut broken = Vec::new();
    for (i, s) in steps.iter().enumerate().filter(|(_, s)| s.pass == pass) {
        let id = &ids[&s.identity];
        let got = answer(p, s, id, policy.as_ref());
        if got != s.want {
            broken.push(format!(
                "ledger step {} ({:?} {}) unmet under {profile} after pass {pass}: wanted {}, got {got}",
                i + 1,
                s.act,
                s.identity,
                s.want
            ));
        }
    }
    broken
}

fn answer(p: &Ports, s: &LedgerStep, id: &PassIdentity, policy: Option<&FaultPolicy>) -> String {
    let now = p.clock.now_ms();
    let class = class(s, id).ok().flatten();
    let done = |r: Result<(), crate::ledger::LedgerError>| match r {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("error {e}"),
    };
    match (s.act, class, policy) {
        (Act::Record, Some(class), Some(policy)) => {
            let a = Attempt {
                class,
                initiated_by: InitiatedBy::Planner,
                sub_passes: vec![],
                evidence: String::new(),
                at: now,
            };
            match record_attempt(p, id, &s.batch, &a, policy) {
                Ok(r) => format!("attempt-{}", r.n),
                Err(e) => format!("error {e}"),
            }
        }
        (Act::Prove, _, _) => {
            let members: Vec<Member> = s.members.iter().filter_map(|m| member(m).ok()).collect();
            done(mark_proven(p, id, &members, now))
        }
        (Act::LaneFault, Some(Class::Fault { cause }), _) => {
            let f = LaneFault {
                cause,
                evidence: String::new(),
                since: now,
            };
            match record_lane_fault(p, &id.repo, &id.base, &id.image_digest, &f) {
                Ok(LaneFaultRecorded::New) => "new".into(),
                Ok(LaneFaultRecorded::Standing) => "standing".into(),
                Err(e) => format!("error {e}"),
            }
        }
        (Act::ClearLaneFault, _, _) => {
            done(clear_lane_fault(p, &id.repo, &id.base, &id.image_digest))
        }
        (Act::Refuses, _, _) => match refuses(p, id) {
            Ok(None) => "admitted".into(),
            Ok(Some(r)) => r.word().into(),
            Err(e) => format!("error {e}"),
        },
        _ => unreachable!("check refused a step whose act, class and config do not fit"),
    }
}

#[cfg(test)]
mod tests {
    use crate::sim::scenario::{parse, profiles, run};

    const BASE: &str = "description = \"t\"\nrepos = [\"a\"]\n[run]\nepoch = 1800000000\npasses = 2\nevery_secs = 30\n[ledger]\nfault_backoff_secs = 10\nfault_retries = 1\n[[identity]]\nname = \"x\"\nrepo = \"a\"\nmembers = [\"sp-x:1\"]\n[[store]]\nkey = \"lifecycle/sp-x\"\ndata = { repo = \"a\" }\n[[store]]\nkey = \"batches/a/b1\"\ndata = { repo = \"a\" }\n[[store]]\nkey = \"batches/a/b2\"\ndata = { repo = \"a\" }\n";

    fn step(pass: u32, body: &str) -> String {
        format!("[[ledger_step]]\npass = {pass}\nidentity = \"x\"\n{body}\n")
    }

    fn go(extra: &str) -> Result<crate::sim::Report, String> {
        let sc = parse(&format!("{BASE}{extra}"))?;
        run(&sc, &profiles().unwrap()[1], 1)
    }

    #[test]
    fn the_summary_count_reads_its_attempts_and_is_zero_without_one() {
        let expect = "[[expect]]\nname = \"s\"\ncount = \"summary:x\"\nequals = 1\n[[expect]]\nname = \"k\"\ncount = \"attempts:x\"\nequals = 1\n";
        go(&format!(
            "{}{expect}",
            step(
                1,
                "act = \"record\"\nclass = \"green\"\nwant = \"attempt-1\""
            )
        ))
        .unwrap();
        let e = go(expect).unwrap_err();
        assert!(
            e.contains("expectation s unmet under reference-minimum: counted 0, expected 1"),
            "{e}"
        );
    }
}
