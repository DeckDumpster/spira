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

    const BASE: &str = "description = \"t\"\nrepos = [\"a\"]\n[run]\nepoch = 1800000000\npasses = 2\nevery_secs = 30\n[ledger]\nfault_backoff_secs = 10\nfault_retries = 1\n[[identity]]\nname = \"x\"\nrepo = \"a\"\nmembers = [\"sp-x:1\"]\n";

    fn step(pass: u32, body: &str) -> String {
        format!("[[ledger_step]]\npass = {pass}\nidentity = \"x\"\n{body}\n")
    }

    fn go(extra: &str) -> Result<crate::sim::Report, String> {
        let sc = parse(&format!("{BASE}{extra}"))?;
        run(&sc, &profiles().unwrap()[1], 1)
    }

    #[test]
    fn every_act_answers_in_its_own_words() {
        let steps = [
            step(1, "act = \"record\"\nclass = \"green\"\nwant = \"attempt-1\""),
            step(1, "act = \"record\"\nclass = \"flaky\"\nwant = \"attempt-2\""),
            step(1, "act = \"refuses\"\nwant = \"admitted\""),
            step(1, "act = \"prove\"\nmembers = [\"sp-x:1\"]\nwant = \"error NotRed { hash: \\\"H\\\" }\""),
            step(1, "act = \"lane-fault\"\nclass = \"fault\"\ncause = \"install\"\nwant = \"new\""),
            step(2, "act = \"lane-fault\"\nclass = \"fault\"\ncause = \"base\"\nwant = \"standing\""),
            step(2, "act = \"lane-fault\"\nclass = \"fault\"\ncause = \"vm\"\nwant = \"error NotALaneCause { cause: Vm }\""),
            step(2, "act = \"refuses\"\nwant = \"lane-fault\""),
            step(2, "act = \"clear-lane-fault\"\nwant = \"ok\""),
            step(2, "act = \"record\"\nclass = \"fault\"\ncause = \"over-cap\"\nwant = \"attempt-3\""),
            step(2, "act = \"refuses\"\nwant = \"backing-off\""),
            step(2, "act = \"record\"\nclass = \"fault\"\ncause = \"fences-cap\"\nbatch = \"b2\"\nwant = \"attempt-4\""),
            step(2, "act = \"record\"\nclass = \"fault\"\ncause = \"image\"\nwant = \"attempt-5\""),
        ]
        .concat();
        let hash = parse(BASE).unwrap().identities["x"].hash();
        go(&steps.replace("\\\"H\\\"", &format!("\\\"{hash}\\\""))).unwrap();
    }

    #[test]
    fn an_answer_other_than_the_one_wanted_is_named_with_its_step_and_pass() {
        let e = go(&step(
            2,
            "act = \"record\"\nclass = \"red\"\nstep = \"s\"\nwant = \"attempt-2\"",
        ))
        .unwrap_err();
        assert_eq!(
            e,
            "ledger step 1 (Record x) unmet under reference-minimum after pass 2: wanted attempt-2, got attempt-1"
        );
    }

    #[test]
    fn a_store_the_ledger_cannot_read_is_an_error_answer() {
        let key = crate::ledger::summary_key(&parse(BASE).unwrap().identities["x"]);
        let seed = format!("[[store]]\nkey = \"{key}\"\ndata = \"garbled\"\n");
        let steps = [
            step(1, "act = \"record\"\nclass = \"green\"\nwant = \"x\""),
            step(1, "act = \"refuses\"\nwant = \"x\""),
        ]
        .concat();
        let e = go(&format!("{seed}{steps}")).unwrap_err();
        assert!(e.contains("wanted x, got error Undecodable"), "{e}");
        assert_eq!(e.lines().count(), 2, "{e}");
    }

    #[test]
    fn the_loader_refuses_a_step_that_cannot_run_naming_it() {
        let cases = [
            (step(1, "act = \"refuses\"\nwant = \"admitted\"\nwhen = 1"), "when"),
            (step(1, "act = \"explode\"\nwant = \"x\""), "explode"),
            (step(1, "act = \"record\"\nclass = \"red\"\nwant = \"x\""), "do not fit the act"),
            (step(1, "act = \"refuses\"\nclass = \"green\"\nwant = \"x\""), "do not fit the act"),
            (step(1, "act = \"prove\"\nmembers = [\"sp-x\"]\nwant = \"x\""), "member \"sp-x\" is not id:tip"),
            (
                "[[ledger_step]]\npass = 1\nidentity = \"nobody\"\nact = \"refuses\"\nwant = \"x\"\n".into(),
                "no [[identity]] named nobody",
            ),
            (
                "[[identity]]\nname = \"x\"\nrepo = \"a\"\nmembers = [\"sp-y:1\"]\n".into(),
                "identity x is named twice",
            ),
            (
                "[[identity]]\nname = \"e\"\nrepo = \"a\"\nmembers = []\n".into(),
                "identity e: NoMembers",
            ),
            (
                "[[identity]]\nname = \"e\"\nrepo = \"a\"\nmembers = [\":1\"]\n".into(),
                "member \":1\" is not id:tip",
            ),
            (
                "[[expect]]\nname = \"n\"\ncount = \"attempts:nobody\"\nequals = 0\n".into(),
                "expectation n counts",
            ),
            (
                "[[expect]]\nname = \"n\"\ncount = \"keys:\"\nequals = 0\n".into(),
                "expectation n counts",
            ),
        ];
        for (extra, want) in cases {
            let e = parse(&format!("{BASE}{extra}")).unwrap_err();
            assert!(e.contains(want), "{want}: {e}");
        }
        let no_config = BASE.replace("[ledger]\nfault_backoff_secs = 10\nfault_retries = 1\n", "");
        let e = parse(&format!(
            "{no_config}{}",
            step(1, "act = \"record\"\nclass = \"green\"\nwant = \"x\"")
        ))
        .unwrap_err();
        assert!(e.contains("a record step needs [ledger]"), "{e}");
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
