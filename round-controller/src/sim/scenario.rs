//! Scenario files: the loader (every unknown key refused by name), the run on a virtual clock,
//! the invariants every scenario is held to, and its counted expectations.

use std::collections::BTreeMap;
use std::sync::Mutex;

use runtime::ports::{Proc, ProcOutput, ProcSpec, SeededRng, VirtualClock};
use serde::Deserialize;
use state_store::{Config, Contract, MemBackend, Object, Request};

use crate::outcome::{utc_stamp, Kind, MemLog};
use crate::pass::{pass, pass_with, Step};
use crate::ports::{Ports, Verb};
use crate::sim::verbs::{Call, FakeVerbs, VerbScript};
use crate::sim::vm::{FakeVm, VmScript};

pub const PROFILES: &str = include_str!("../../scenarios/profiles.toml");
/// The profiles every scenario runs under, in this order.
pub const PROFILE_NAMES: [&str; 2] = ["default", "reference-minimum"];
/// The seed a scenario runs under when `SPIRA_SEED` does not name one.
pub const DEFAULT_SEED: u64 = 1;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(skip)]
    pub name: String,
    pub runner_slots: usize,
    pub vm_vcpus: u32,
    pub vm_mem_mib: u32,
    /// Virtual pass durations are multiplied by this.
    pub duration_factor: f64,
    pub pass_cap_secs: u64,
    #[serde(default)]
    pub note: String,
}

pub fn parse_profiles(text: &str) -> Result<Vec<Profile>, String> {
    let mut all: BTreeMap<String, Profile> =
        toml::from_str(text).map_err(|e| format!("profiles: {e}"))?;
    PROFILE_NAMES
        .iter()
        .map(|n| {
            let mut p = all
                .remove(*n)
                .ok_or_else(|| format!("profiles: no profile {n}"))?;
            p.name = n.to_string();
            Ok(p)
        })
        .collect()
}

pub fn profiles() -> Result<Vec<Profile>, String> {
    parse_profiles(PROFILES)
}

pub fn harness_seed(raw: Option<&str>) -> u64 {
    crate::seed_from(raw).unwrap_or(DEFAULT_SEED)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    /// Virtual epoch seconds the first pass runs at.
    pub epoch: u64,
    pub passes: u32,
    pub every_secs: u64,
    /// Each later pass starts up to this much later, drawn from the seeded generator.
    #[serde(default)]
    pub jitter_secs: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreSeed {
    pub key: String,
    pub data: toml::Value,
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Count {
    Calls(Option<Verb>),
    Outcomes(Option<&'static str>),
    StoreWrites,
    VmRuns,
    /// The longest queue lock hold any verb reported.
    LockHoldMs,
}

impl Count {
    fn parse(s: &str) -> Option<Count> {
        let (what, of) = s.split_once(':').map_or((s, None), |(w, o)| (w, Some(o)));
        match (what, of) {
            ("calls", None) => Some(Count::Calls(None)),
            ("calls", Some(v)) => Verb::from_word(v).map(|v| Count::Calls(Some(v))),
            ("outcomes", None) => Some(Count::Outcomes(None)),
            ("outcomes", Some(k)) => Kind::words()
                .into_iter()
                .find(|w| *w == k)
                .map(|w| Count::Outcomes(Some(w))),
            ("store-writes", None) => Some(Count::StoreWrites),
            ("vm-runs", None) => Some(Count::VmRuns),
            ("lock-hold-ms", None) => Some(Count::LockHoldMs),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectDef {
    pub name: String,
    pub count: String,
    pub equals: u64,
    /// A positive control: this expectation must fail, naming itself.
    #[serde(default)]
    pub inverted: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    description: String,
    repos: Vec<String>,
    run: RunSpec,
    #[serde(default)]
    store: Vec<StoreSeed>,
    #[serde(default)]
    verb: Vec<VerbScript>,
    #[serde(default)]
    vm: Vec<VmScript>,
    #[serde(default)]
    expect: Vec<ExpectDef>,
}

#[derive(Clone, Debug)]
pub struct Expect {
    pub name: String,
    pub count: Count,
    pub equals: u64,
    pub inverted: bool,
}

#[derive(Clone, Debug)]
pub struct Scenario {
    pub description: String,
    pub repos: Vec<String>,
    pub run: RunSpec,
    pub store: Vec<StoreSeed>,
    pub verbs: Vec<VerbScript>,
    pub vm: Vec<VmScript>,
    pub expects: Vec<Expect>,
}

pub fn parse(text: &str) -> Result<Scenario, String> {
    let f: File = toml::from_str(text).map_err(|e| format!("scenario: {e}"))?;
    let expects = f
        .expect
        .into_iter()
        .map(|e| {
            let count = Count::parse(&e.count).ok_or_else(|| {
                format!(
                    "scenario: expectation {} counts {:?}, which is nothing the harness counts",
                    e.name, e.count
                )
            })?;
            Ok(Expect {
                name: e.name,
                count,
                equals: e.equals,
                inverted: e.inverted,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Scenario {
        description: f.description,
        repos: f.repos,
        run: f.run,
        store: f.store,
        verbs: f.verb,
        vm: f.vm,
        expects,
    })
}

/// A scenario spawns no process: every effect has a port of its own.
#[derive(Default)]
struct NoProc(Mutex<Vec<String>>);

impl Proc for NoProc {
    fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
        crate::lock(&self.0).push(format!("spawned {}", spec.prog));
        Err(format!("a scenario spawns no process: {}", spec.prog))
    }
}

#[derive(Debug)]
pub struct Report {
    pub profile: String,
    pub lines: Vec<String>,
    pub calls: Vec<Call>,
    /// What each inverted expectation said when it failed, as it must.
    pub controls: Vec<String>,
}

pub fn run(sc: &Scenario, profile: &Profile, seed: u64) -> Result<Report, String> {
    run_with(sc, profile, seed, &|p, repos| pass(p, repos))
}

/// [`run`] with `steps` in place of the registered ones: a test of the harness itself.
pub fn run_steps(
    sc: &Scenario,
    profile: &Profile,
    seed: u64,
    steps: &[Step],
) -> Result<Report, String> {
    run_with(sc, profile, seed, &|p, repos| pass_with(p, repos, steps))
}

type Pass<'a> = dyn Fn(&Ports, &[String]) + 'a;

fn run_with(sc: &Scenario, profile: &Profile, seed: u64, go: &Pass) -> Result<Report, String> {
    let at = &profile.name;
    let clock = VirtualClock::new(sc.run.epoch * 1000);
    let store = Contract::open(
        MemBackend::new(),
        state_store::Ports::virtual_clock(&clock),
        Config {
            min_lock_ttl_ms: 1000,
        },
    )
    .expect("a fresh in-memory store opens");
    for (i, s) in sc.store.iter().enumerate() {
        let data = serde_json::to_value(&s.data).expect("a TOML value is JSON");
        let obj = Object::new(data);
        let obj = match s.ttl_secs {
            Some(t) => obj.with_ttl(t),
            None => obj,
        };
        store
            .create(
                &s.key,
                obj,
                &Request::new(&format!("seed-{i}"), "scenario", "seed"),
            )
            .map_err(|e| format!("store seed {}: {e}", s.key))?;
    }
    let seeded = store.head().expect("the in-memory store has a head");
    let vm = FakeVm::new(sc.vm.clone(), profile.clone(), clock.clone());
    let verbs = FakeVerbs::new(sc.verbs.clone(), vm.clone(), clock.clone())
        .map_err(|e| format!("scenario: {e}"))?;
    let (rng, log, proc_) = (
        Mutex::new(SeededRng::new(seed)),
        MemLog::default(),
        NoProc::default(),
    );
    let ports = Ports {
        clock: &clock,
        proc_: &proc_,
        rng: &rng,
        store: &store,
        verbs: &verbs,
        log: &log,
    };
    for i in 0..sc.run.passes {
        if i > 0 {
            let jitter = ports.next_u64() % (sc.run.jitter_secs * 1000 + 1);
            clock.advance(sc.run.every_secs * 1000 + jitter);
        }
        go(&ports, &sc.repos);
    }

    let mut broken: Vec<String> = Vec::new();
    let now = runtime::ports::Clock::now_ms(&clock);
    for r in vm.runs().iter().filter(|r| r.ends <= now && !r.collected) {
        broken.push(format!("invariant every-pass-has-a-verdict violated under {at}: pass {} finished at {} with no recorded verdict", r.identity, utc_stamp(r.ends)));
    }
    let faults: Vec<String> = vm
        .faults()
        .into_iter()
        .chain(verbs.faults())
        .chain(crate::lock(&proc_.0).iter().cloned())
        .collect();
    if !faults.is_empty() {
        broken.push(format!(
            "invariant no-unscripted-fault violated under {at}: {}",
            faults.join("; ")
        ));
    }

    let (outcomes, calls) = (log.outcomes(), verbs.calls());
    let writes = store.head().expect("the in-memory store has a head") - seeded;
    let mut controls = Vec::new();
    for e in &sc.expects {
        let got = match &e.count {
            Count::Calls(v) => calls
                .iter()
                .filter(|c| v.is_none_or(|v| c.verb == v))
                .count() as u64,
            Count::Outcomes(k) => outcomes
                .iter()
                .filter(|o| k.is_none_or(|k| o.kind.word() == k))
                .count() as u64,
            Count::StoreWrites => writes,
            Count::VmRuns => vm.runs().len() as u64,
            Count::LockHoldMs => calls.iter().map(|c| c.lock_hold_ms).max().unwrap_or(0),
        };
        let unmet = format!(
            "expectation {} unmet under {at}: counted {got}, expected {}",
            e.name, e.equals
        );
        match (got == e.equals, e.inverted) {
            (true, false) => {}
            (false, false) => broken.push(unmet),
            (false, true) => controls.push(unmet),
            (true, true) => broken.push(format!("inverted expectation {} held under {at}: counted {got}, so the harness cannot see what it counts", e.name)),
        }
    }
    if !broken.is_empty() {
        return Err(broken.join("\n"));
    }
    Ok(Report {
        profile: at.clone(),
        lines: outcomes.iter().map(|o| o.line()).collect(),
        calls,
        controls,
    })
}

/// The scenario under every profile on one seed; every profile's
/// failure is reported, not just the first.
pub fn run_all(text: &str, seed: u64) -> Result<Vec<Report>, String> {
    let sc = parse(text)?;
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for p in profiles().expect("the shipped profiles parse") {
        match run(&sc, &p, seed) {
            Ok(r) => ok.push(r),
            Err(e) => bad.push(e),
        }
    }
    if bad.is_empty() {
        Ok(ok)
    } else {
        Err(bad.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "description = \"t\"\nrepos = [\"a\", \"b\"]\n[run]\nepoch = 1800000000\npasses = 2\nevery_secs = 30\n";

    fn profile() -> Profile {
        profiles().unwrap().remove(1)
    }

    fn go(extra: &str) -> Result<Report, String> {
        run(&parse(&format!("{BASE}{extra}")).unwrap(), &profile(), 3)
    }

    #[test]
    fn the_shipped_profiles_are_default_then_reference_minimum_and_a_missing_one_is_named() {
        let p = profiles().unwrap();
        assert_eq!(
            p.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            PROFILE_NAMES
        );
        assert_eq!(
            (p[1].runner_slots, p[1].vm_vcpus, p[1].vm_mem_mib),
            (1, 2, 4096)
        );
        assert!(
            p[1].note.contains("goal") && p[1].note.contains("round-controller/tiny-hardware"),
            "{}",
            p[1].note
        );
        assert_eq!(parse_profiles("[default]\nrunner_slots = 1\nvm_vcpus = 1\nvm_mem_mib = 1\nduration_factor = 1.0\npass_cap_secs = 1\n").unwrap_err(), "profiles: no profile reference-minimum");
        assert!(parse_profiles("[default]\nslots = 1\n")
            .unwrap_err()
            .contains("slots"));
    }

    #[test]
    fn the_loader_refuses_an_unknown_key_by_name_at_any_depth() {
        assert!(parse(&format!("{BASE}pases = 3\n"))
            .unwrap_err()
            .contains("pases"));
        assert!(parse(&format!(
            "{BASE}[[vm]]\nidentity = \"h\"\nverdict = \"green\"\nduration_secs = 1\nwall = 2\n"
        ))
        .unwrap_err()
        .contains("wall"));
        assert!(parse(&format!(
            "{BASE}[[expect]]\nname = \"n\"\ncount = \"calls\"\nequal = 1\n"
        ))
        .unwrap_err()
        .contains("equal"));
    }

    #[test]
    fn an_expectation_must_count_something_the_harness_counts() {
        for bad in ["calls:preempt", "outcomes:exploded", "seconds", "vm-runs:x"] {
            let e = parse(&format!(
                "{BASE}[[expect]]\nname = \"n1\"\ncount = \"{bad}\"\nequals = 0\n"
            ))
            .unwrap_err();
            assert!(
                e.contains("expectation n1 counts") && e.contains(bad),
                "{e}"
            );
        }
    }

    #[test]
    fn every_count_is_counted() {
        let expect = |n: &str, c: &str, v: u64| {
            format!("[[expect]]\nname = \"{n}\"\ncount = \"{c}\"\nequals = {v}\n")
        };
        let text = [
            expect("c", "calls", 0),
            expect("co", "calls:open", 0),
            expect("o", "outcomes", 4),
            expect("ov", "outcomes:visited", 4),
            expect("w", "store-writes", 0),
            expect("v", "vm-runs", 0),
            expect("l", "lock-hold-ms", 0),
        ]
        .concat();
        let r = go(&format!("[[store]]\nkey = \"batches/a/b1\"\ndata = {{ state = \"OPEN\" }}\n[[store]]\nkey = \"telemetry/x\"\ndata = 1\nttl_secs = 60\n{text}")).unwrap();
        assert_eq!(r.lines.len(), 4);
        assert!(
            r.lines[2].starts_with("2027-01-15T08:00:30Z visited steps repo=a"),
            "{:?}",
            r.lines
        );
    }

    #[test]
    fn an_unmet_expectation_and_an_inverted_one_that_holds_are_each_named() {
        let e = go("[[expect]]\nname = \"wrong\"\ncount = \"outcomes\"\nequals = 9\n[[expect]]\nname = \"flipped\"\ncount = \"outcomes\"\nequals = 4\ninverted = true\n").unwrap_err();
        assert!(
            e.contains("expectation wrong unmet under reference-minimum: counted 4, expected 9"),
            "{e}"
        );
        assert!(
            e.contains("inverted expectation flipped held under reference-minimum"),
            "{e}"
        );
    }

    #[test]
    fn an_inverted_expectation_that_fails_is_a_control_naming_itself() {
        let r =
            go("[[expect]]\nname = \"never\"\ncount = \"outcomes\"\nequals = 0\ninverted = true\n")
                .unwrap();
        assert_eq!(
            r.controls,
            ["expectation never unmet under reference-minimum: counted 4, expected 0"]
        );
    }

    fn opening(p: &Ports, repo: &str) {
        let _ = p.verbs.open(&crate::ports::OpenReq {
            repo: repo.into(),
            members: vec![],
            name: None,
        });
    }

    fn certifying(p: &Ports, repo: &str) {
        let _ = p.verbs.certify(repo, "b1");
    }

    fn certified(p: &Ports, repo: &str) {
        let h = p.verbs.certify(repo, "b1").unwrap();
        assert_eq!(h.poll(), Some(Ok(crate::ports::Certified::Green)));
    }

    fn spawning(p: &Ports, _: &str) {
        let _ = p.proc_.run(&ProcSpec {
            prog: "git".into(),
            args: vec![],
            stdin: vec![],
            deadline: std::time::Duration::from_secs(1),
        });
    }

    fn writing(p: &Ports, repo: &str) {
        let t = state_store::Txn::new().put(
            &format!("batches/{repo}/x"),
            Object::new(serde_json::json!(1)),
        );
        p.store
            .txn(
                &t,
                &Request::new(&format!("w-{repo}-{}", p.clock.now_ms()), "t", "w"),
            )
            .unwrap();
    }

    #[test]
    fn a_step_runs_per_repo_per_pass_and_its_writes_and_calls_are_counted() {
        let sc = parse(&format!(
            "{BASE}[[expect]]\nname = \"w\"\ncount = \"store-writes\"\nequals = 4\n"
        ))
        .unwrap();
        run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "write",
                run: writing,
            }],
        )
        .unwrap();
        let open = |hold: u64| {
            format!("[[verb]]\nverb = \"open\"\nok = {{ batch = \"b1\", head = \"h1\" }}\nlock_hold_ms = {hold}\n")
        };
        let expect = "[[expect]]\nname = \"opens\"\ncount = \"calls:open\"\nequals = 4\n[[expect]]\nname = \"hold\"\ncount = \"lock-hold-ms\"\nequals = 70\n";
        let sc = parse(&format!(
            "{BASE}{}{}{}{}{expect}",
            open(5),
            open(70),
            open(5),
            open(5)
        ))
        .unwrap();
        let r = run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "open",
                run: opening,
            }],
        )
        .unwrap();
        assert_eq!(
            r.calls.iter().map(|c| c.repo.as_str()).collect::<Vec<_>>(),
            ["a", "b", "a", "b"]
        );
        let sc = parse(&format!("{BASE}{}{expect}", open(70))).unwrap();
        let e = run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "open",
                run: opening,
            }],
        )
        .unwrap_err();
        assert!(
            e.contains("unscripted call: queue round open b -"),
            "a scripted answer is used once: {e}"
        );
    }

    #[test]
    fn a_pass_whose_verdict_was_read_keeps_the_invariant_and_every_run_is_counted() {
        let sc = parse(&format!("{BASE}[[vm]]\nidentity = \"b1\"\nverdict = \"green\"\nduration_secs = 0\n[[expect]]\nname = \"runs\"\ncount = \"vm-runs\"\nequals = 4\n")).unwrap();
        run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "certify",
                run: certified,
            }],
        )
        .unwrap();
    }

    #[test]
    fn an_unscripted_spawn_and_an_uncollected_pass_break_the_invariants() {
        let sc = parse(&format!(
            "{BASE}[[vm]]\nidentity = \"b1\"\nverdict = \"green\"\nduration_secs = 1\n"
        ))
        .unwrap();
        let e = run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "spawn",
                run: spawning,
            }],
        )
        .unwrap_err();
        assert!(
            e.contains(
                "invariant no-unscripted-fault violated under reference-minimum: spawned git"
            ),
            "{e}"
        );
        let e = run_steps(
            &sc,
            &profile(),
            1,
            &[Step {
                name: "certify",
                run: certifying,
            }],
        )
        .unwrap_err();
        assert!(e.contains("invariant every-pass-has-a-verdict violated under reference-minimum: pass b1 finished at"), "{e}");
    }

    #[test]
    fn a_seed_the_store_refuses_is_named() {
        let e = run(
            &parse(&format!(
                "{BASE}[[store]]\nkey = \"telemetry/x\"\ndata = 1\n"
            ))
            .unwrap(),
            &profile(),
            1,
        )
        .unwrap_err();
        assert!(e.starts_with("store seed telemetry/x:"), "{e}");
        let e = run(
            &parse(&format!("{BASE}[[verb]]\nverb = \"land\"\n")).unwrap(),
            &profile(),
            1,
        )
        .unwrap_err();
        assert!(e.contains("exactly one of ok and refuse"), "{e}");
    }

    #[test]
    fn run_all_reports_every_profiles_failure() {
        let e = run_all(
            &format!("{BASE}[[expect]]\nname = \"n\"\ncount = \"calls\"\nequals = 1\n"),
            1,
        )
        .unwrap_err();
        assert!(
            e.contains("under default") && e.contains("under reference-minimum"),
            "{e}"
        );
        assert_eq!(run_all(BASE, 1).unwrap().len(), 2);
        assert!(run_all("x", 1).is_err());
    }

    #[test]
    fn the_seed_is_spira_seed_or_the_default() {
        assert_eq!(
            (harness_seed(Some("9")), harness_seed(None)),
            (9, DEFAULT_SEED)
        );
    }
}
