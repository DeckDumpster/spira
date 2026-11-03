use std::collections::BTreeMap;
use std::sync::Mutex;

use batcher::core::Member as RoundMember;
use batcher::attrib::Job;
use batcher::drive::{MainEnd, MainKind, MainPoll, RoundRunner};
use lifecycle::batch::{BatchRow, BatchState};
use runtime::ports::{Clock, VirtualClock};
use serde_json::json;
use state_store::{Object, Request, Txn};

use super::*;
use crate::ledger::{record_attempt, FaultPolicy, StoreLedger};
use crate::ports::{CertifyEnd, Child, Mark, Signal, VerbRefusal};
use crate::probe::Sleep;
use crate::resources::Round;
use crate::run::RunEnv;
use crate::sim::bench::Bench;

const POLICY: FaultPolicy = FaultPolicy {
    backoff_secs: 60,
    retries: 2,
};

#[derive(Default)]
struct FakeFacts {
    repos: Option<Result<Vec<String>, String>>,
    trees: BTreeMap<(String, String), String>,
    unreadable_tree: Option<String>,
    names: BTreeMap<(String, String), Vec<String>>,
    open: BTreeMap<String, String>,
    unreadable_queue: Option<String>,
    submitted: BTreeMap<String, usize>,
}

impl FakeFacts {
    fn two() -> FakeFacts {
        let mut f = FakeFacts {
            repos: Some(Ok(vec!["alpha".into(), "beta".into()])),
            ..FakeFacts::default()
        };
        f.tree("alpha", DECL, "ci_entry = \"bash ci.sh\"\n");
        f.tree("beta", GATE_STEPS, "# the gate\nstep make check\nstep make lint\n");
        f.submitted.insert("alpha".into(), 2);
        f.submitted.insert("beta".into(), 1);
        f
    }

    fn tree(&mut self, repo: &str, path: &str, text: &str) {
        self.trees.insert((repo.into(), path.into()), text.into());
    }
}

impl Facts for FakeFacts {
    fn round_repos(&self) -> Result<Vec<String>, String> {
        self.repos.clone().unwrap_or_else(|| Ok(vec![]))
    }
    fn at_landing(&self, repo: &str, path: &str) -> Result<Option<String>, String> {
        if self.unreadable_tree.as_deref() == Some(repo) {
            return Err("git cat-file exited Some(128)".into());
        }
        Ok(self.trees.get(&(repo.to_string(), path.to_string())).cloned())
    }
    fn names_at_landing(&self, repo: &str, dir: &str) -> Result<Vec<String>, String> {
        self.names
            .get(&(repo.to_string(), dir.to_string()))
            .cloned()
            .ok_or_else(|| format!("git ls-tree exited Some(128): no {dir} in {repo}"))
    }
    fn open_batch(&self, repo: &str) -> Result<Option<String>, String> {
        if self.unreadable_queue.as_deref() == Some(repo) {
            return Err("queue batch show exited Some(1): the store is down".into());
        }
        Ok(self.open.get(repo).cloned())
    }
    fn submitted(&self, repo: &str) -> Result<usize, String> {
        self.submitted
            .get(repo)
            .copied()
            .ok_or_else(|| format!("spira-lc list exited Some(2): no answer for {repo}"))
    }
}

/// The lane, and git's landing ref at the commit it names.
fn seed_lane(b: &Bench, repo: &str, landing: &str, digest: &str) {
    b.seed(
        &format!("lanes/{repo}"),
        json!({ "landing_ref": landing, "image_digest": digest, "harness_version": "h1" }),
    );
    b.git.advance(repo, landing);
}

fn lane_of(b: &Bench, repo: &str) -> Lane {
    lane(&b.ports(), repo).unwrap().unwrap()
}

fn attempt(class: Class) -> Attempt {
    Attempt {
        class,
        initiated_by: InitiatedBy::Judge,
        sub_passes: vec![],
        evidence: String::new(),
        at: 0,
    }
}

fn record(b: &Bench, id: &PassIdentity, class: Class) {
    record_attempt(&b.ports(), id, "seed", &attempt(class), &POLICY).unwrap();
}

fn m(id: &str, tip: &str) -> Member {
    Member {
        id: id.into(),
        tip: tip.into(),
    }
}

fn round(repo: &str, batch: &str, state: BatchState, head: &str, base: &str, members: Vec<Member>) -> serde_json::Value {
    let mut row = BatchRow::cut(batch, repo, head, base);
    row.state = state;
    if matches!(state, BatchState::Landed | BatchState::Deploying) {
        row.reason = Some(head.into());
    }
    serde_json::to_value(Round {
        row,
        members,
        tested_base: Some(base.into()),
        last_lease_token: None,
    })
    .unwrap()
}

/// Both repos ready: alpha by a base probe, beta by a landed round green on the current image.
fn ready(b: &Bench) {
    seed_lane(b, "alpha", "a1", "sha256:img2");
    seed_lane(b, "beta", "b2", "sha256:img2");
    record(b, &base_identity("alpha", &lane_of(b, "alpha")), Class::Green);
    let members = vec![m("sp-x", "t1")];
    b.seed("batches/beta/k1", round("beta", "k1", BatchState::Landed, "b2", "b1", members.clone()));
    let id = PassIdentity {
        members,
        base: "b1".into(),
        ..base_identity("beta", &lane_of(b, "beta"))
    };
    record(b, &id, Class::Green);
}

fn row<'a>(r: &'a Readiness, scope: &str) -> &'a Row {
    r.row(scope).unwrap_or_else(|| panic!("no row {scope}: {}", r.render()))
}

fn why<'a>(r: &'a Readiness, scope: &str, check: &str) -> &'a Check {
    row(r, scope).check(check).unwrap()
}

#[test]
fn every_row_green_exits_0_and_names_each_check() {
    let b = Bench::new();
    ready(&b);
    let r = readiness(&b.ports(), &FakeFacts::two());
    assert_eq!(r.code(), 0, "{}", r.render());
    let text = r.render();
    assert!(text.contains("alpha green ci=green base=green submitted=green"), "{text}");
    assert!(text.contains("beta green ci=green base=green submitted=green"), "{text}");
    assert!(text.contains("global green repos=green rounds=green open-batches=green"), "{text}");
    assert!(text.ends_with("readiness=green\n"), "{text}");
    assert!(why(&r, "alpha", "ci").why.contains("bash ci.sh"));
    assert!(why(&r, "beta", "ci").why.contains("2 steps"));
    assert!(why(&r, "alpha", "base").why.contains("base probe"));
    assert!(why(&r, "beta", "base").why.contains("round k1 landed"));
}

#[test]
fn a_repo_with_no_ci_entry_and_no_gate_steps_is_a_red_row() {
    let b = Bench::new();
    ready(&b);
    let mut f = FakeFacts::two();
    f.trees.clear();
    f.tree("alpha", DECL, "[tier.unit]\ncmd = \"make\"\n");
    f.tree("beta", GATE_STEPS, "# no step at all\n");
    let r = readiness(&b.ports(), &f);
    assert_eq!(r.code(), 1);
    for repo in ["alpha", "beta"] {
        let c = why(&r, repo, "ci");
        assert!(!c.green && c.why.contains("no ci_entry"), "{c:?}");
    }
    assert!(r.render().contains("alpha red ci=red base=green submitted=green"));
}

#[test]
fn a_declaration_or_tree_that_cannot_be_read_is_red_naming_why() {
    let b = Bench::new();
    ready(&b);
    let mut f = FakeFacts::two();
    f.tree("beta", DECL, "ci_entry = [");
    f.unreadable_tree = Some("alpha".into());
    let r = readiness(&b.ports(), &f);
    assert!(why(&r, "alpha", "ci").why.contains("cannot read"));
    assert!(why(&r, "beta", "ci").why.contains("does not parse"));
    assert_eq!(r.code(), 1);
}

#[test]
fn a_red_base_on_the_current_digest_is_a_red_row() {
    let b = Bench::new();
    ready(&b);
    record(
        &b,
        &base_identity("alpha", &lane_of(&b, "alpha")),
        Class::Red { step: "all".into() },
    );
    let r = readiness(&b.ports(), &FakeFacts::two());
    let c = why(&r, "alpha", "base");
    assert!(!c.green && c.why.contains("was red"), "{c:?}");
    assert_eq!(r.code(), 1);
}

#[test]
fn a_base_green_only_on_another_image_or_another_base_is_red() {
    let b = Bench::new();
    seed_lane(&b, "alpha", "a1", "sha256:img2");
    let old_image = PassIdentity {
        image_digest: "sha256:img1".into(),
        ..base_identity("alpha", &lane_of(&b, "alpha"))
    };
    record(&b, &old_image, Class::Green);
    b.seed("batches/alpha/k0", round("alpha", "k0", BatchState::Landed, "a0", "z", vec![m("sp-y", "t")]));
    let r = readiness(&b.ports(), &FakeFacts::two());
    let c = why(&r, "alpha", "base");
    assert!(!c.green && c.why.contains("--probe-base alpha"), "{c:?}");
    let c = why(&r, "beta", "base");
    assert!(!c.green && c.why.contains("no lane record at lanes/beta"), "{c:?}");
}

#[test]
fn a_landed_round_not_green_on_the_current_image_does_not_make_the_base_green() {
    let b = Bench::new();
    ready(&b);
    seed_lane_replace(&b, "beta", "b2", "sha256:img3");
    let r = readiness(&b.ports(), &FakeFacts::two());
    assert!(!why(&r, "beta", "base").green, "{}", r.render());
}

fn seed_lane_replace(b: &Bench, repo: &str, landing: &str, digest: &str) {
    let key = format!("lanes/{repo}");
    b.store
        .txn(
            &Txn::new().put(
                &key,
                Object::new(json!({ "landing_ref": landing, "image_digest": digest, "harness_version": "h1" })),
            ),
            &Request::new(&format!("lane-{landing}-{digest}"), "t", "t"),
        )
        .unwrap();
    b.git.advance(repo, landing);
}

#[test]
fn a_repo_with_no_submitted_bead_or_an_unreadable_count_is_a_red_row() {
    let b = Bench::new();
    ready(&b);
    let mut f = FakeFacts::two();
    f.submitted.insert("alpha".into(), 0);
    f.submitted.remove("beta");
    let r = readiness(&b.ports(), &f);
    let c = why(&r, "alpha", "submitted");
    assert_eq!((c.green, c.why.as_str()), (false, "no SUBMITTED bead"));
    assert!(why(&r, "beta", "submitted").why.contains("cannot read"));
    assert!(r.render().contains("alpha red ci=green base=green submitted=red"));
    assert_eq!(r.code(), 1);
}

#[test]
fn a_non_terminal_round_reds_the_global_row_and_names_it() {
    let b = Bench::new();
    ready(&b);
    b.seed("batches/beta/k2", round("beta", "k2", BatchState::Attributing, "b3", "b2", vec![m("sp-z", "t")]));
    b.seed("batches/beta/k2/progress", json!({ "pass": 1 }));
    let r = readiness(&b.ports(), &FakeFacts::two());
    let c = why(&r, GLOBAL, "rounds");
    assert_eq!((c.green, c.why.as_str()), (false, "1 non-terminal: beta/k2 ATTRIBUTING"));
    assert!(row(&r, "alpha").green() && row(&r, "beta").green());
    assert_eq!(r.code(), 1);
}

#[test]
fn an_open_batch_or_an_unreadable_one_reds_the_global_row() {
    let b = Bench::new();
    ready(&b);
    let mut f = FakeFacts::two();
    f.open.insert("alpha".into(), "deck-1".into());
    f.open.insert("beta".into(), "spira-9".into());
    let r = readiness(&b.ports(), &f);
    let c = why(&r, GLOBAL, "open-batches");
    assert_eq!((c.green, c.why.as_str()), (false, "alpha/deck-1 beta/spira-9"));
    assert_eq!(r.code(), 1);
    let mut f = FakeFacts::two();
    f.unreadable_queue = Some("beta".into());
    let r = readiness(&b.ports(), &f);
    let c = why(&r, GLOBAL, "open-batches");
    assert!(!c.green && c.why.contains("cannot tell whether beta has an open batch"), "{}", c.why);
    assert_eq!(r.code(), 1);
}

#[test]
fn no_round_repo_or_an_unreadable_registry_is_red() {
    let b = Bench::new();
    let r = readiness(&b.ports(), &FakeFacts::default());
    assert!(!why(&r, GLOBAL, "repos").green);
    assert_eq!(r.code(), 1);
    let f = FakeFacts {
        repos: Some(Err("no repository registry is declared".into())),
        ..FakeFacts::default()
    };
    let r = readiness(&b.ports(), &f);
    assert!(why(&r, GLOBAL, "repos").why.contains("no repository registry"));
    assert_eq!(r.code(), 1);
}

#[test]
fn a_store_that_cannot_be_read_is_red_never_green() {
    let b = Bench::unserved();
    let r = readiness(&b.ports(), &FakeFacts::two());
    assert_eq!(r.code(), 1);
    for repo in ["alpha", "beta"] {
        let c = why(&r, repo, "base");
        assert!(!c.green && c.why.contains("no spira-lc door"), "{c:?}");
    }
    assert!(why(&r, GLOBAL, "rounds").why.contains("cannot read the rounds"));
}

#[test]
fn an_undecodable_round_or_summary_is_red_naming_its_key() {
    let b = Bench::new();
    seed_lane(&b, "alpha", "a1", "sha256:img2");
    b.seed("batches/alpha/bad", json!({ "state": "NOT-A-STATE" }));
    let r = readiness(&b.ports(), &FakeFacts::two());
    assert!(why(&r, "alpha", "base").why.contains("batches/alpha/bad"));
    assert!(why(&r, GLOBAL, "rounds").why.contains("batches/alpha/bad"));
    let b = Bench::new();
    seed_lane(&b, "alpha", "a1", "sha256:img2");
    b.seed(&summary_key(&base_identity("alpha", &lane_of(&b, "alpha"))), json!({ "class": 7 }));
    let r = readiness(&b.ports(), &FakeFacts::two());
    assert!(why(&r, "alpha", "base").why.contains("verdicts/alpha/"));
}

#[test]
fn a_lane_the_landing_ref_moved_past_or_a_ref_git_cannot_read_is_red() {
    let b = Bench::new();
    ready(&b);
    b.git.advance("alpha", "a2");
    b.git.break_repo("beta", "git rev-parse exited Some(128)");
    let r = readiness(&b.ports(), &FakeFacts::two());
    let c = why(&r, "alpha", "base");
    assert!(!c.green && c.why.contains("names a1") && c.why.contains("at a2"), "{c:?}");
    let c = why(&r, "beta", "base");
    assert!(!c.green && c.why.contains("cannot read beta's landing ref"), "{c:?}");
}

#[test]
fn a_faulted_base_probe_leaves_the_landed_round_to_answer() {
    let b = Bench::new();
    ready(&b);
    record(
        &b,
        &base_identity("beta", &lane_of(&b, "beta")),
        Class::Fault { cause: Cause::Vm },
    );
    let r = readiness(&b.ports(), &FakeFacts::two());
    assert!(why(&r, "beta", "base").why.contains("round k1 landed"), "{}", r.render());
}

/// A batch transition as spira-lc commits one: the record, a history entry under the batch, and
/// the audit as the request's evidence, which the contract keeps at `events/<rev>`.
fn transition(b: &Bench, repo: &str, batch: &str, state: BatchState, head: &str, event: &str, applied: bool) -> u64 {
    let planned = b.store.head().unwrap();
    let id = format!("{batch}-{event}-{planned}");
    let req = Request {
        evidence: json!({
            "then": [{ "machine": "batch", "lc_key": batch, "event": event, "applied": applied }],
            "else": [],
        }),
        ..Request::new(&id, "queue", "batch-event")
    };
    let t = Txn::new()
        .put(&round_key(repo, batch), Object::new(round(repo, batch, state, head, "base", vec![])))
        .put(&format!("history/{batch}/{planned:020}/{id}"), Object::new(json!({})));
    b.store.txn(&t, &req).unwrap();
    b.store.head().unwrap()
}

/// A landing through spira-lc: the ref moves at the FastForward, and the release follows.
fn landed(b: &Bench, repo: &str, batch: &str, head: &str) -> u64 {
    let at = transition(b, repo, batch, BatchState::Deploying, head, "FastForward", true);
    transition(b, repo, batch, BatchState::Landed, head, "Deployed", true);
    at
}

/// The controller's record of a landing it found already under the ref: one write, no audit.
fn recorded_landed(b: &Bench, repo: &str, batch: &str, head: &str) -> u64 {
    let t = Txn::new().put(
        &round_key(repo, batch),
        Object::new(round(repo, batch, BatchState::Landed, head, "base", vec![])),
    );
    b.store
        .txn(&t, &Request::new(&format!("rc-{batch}"), "round-controller", "land"))
        .unwrap();
    b.store.head().unwrap()
}

#[test]
fn since_rev_with_one_repo_landed_and_another_not_exits_1_printing_none() {
    let b = Bench::new();
    landed(&b, "beta", "k0", "old");
    let rev = b.store.head().unwrap();
    let at = landed(&b, "alpha", "k1", "a2");
    transition(&b, "beta", "k3", BatchState::Green, "b3", "PassGreen", true);
    let rows = since_rev(&b.ports(), &FakeFacts::two(), rev).unwrap();
    assert_eq!(
        rows[0].1,
        Ok(Some(Landed {
            batch: "k1".into(),
            sha: "a2".into(),
            rev: at
        }))
    );
    let (out, _, code) = render_since(&rows);
    assert_eq!((out.as_str(), code), ("alpha k1 a2\nbeta none\n", 1));
}

#[test]
fn since_rev_with_both_landed_exits_0_naming_each_repos_first_round() {
    let b = Bench::new();
    let rev = b.store.head().unwrap();
    landed(&b, "alpha", "k2", "a3");
    landed(&b, "alpha", "k1", "a2");
    recorded_landed(&b, "beta", "k4", "b4");
    let (out, err, code) = render_since(&since_rev(&b.ports(), &FakeFacts::two(), rev).unwrap());
    assert_eq!((out.as_str(), err.as_str(), code), ("alpha k2 a3\nbeta k4 b4\n", "", 0));
}

#[test]
fn a_landing_before_the_revision_counts_even_when_its_release_is_recorded_after() {
    let b = Bench::new();
    transition(&b, "alpha", "k1", BatchState::Deploying, "a2", "FastForward", true);
    recorded_landed(&b, "beta", "k2", "b2");
    let rev = b.store.head().unwrap();
    transition(&b, "alpha", "k1", BatchState::Landed, "a2", "Deployed", true);
    let (out, _, code) = render_since(&since_rev(&b.ports(), &FakeFacts::two(), rev).unwrap());
    assert_eq!((out.as_str(), code), ("alpha none\nbeta none\n", 1));
}

#[test]
fn a_refused_or_unapplied_fast_forward_is_not_the_landing() {
    let b = Bench::new();
    transition(&b, "alpha", "k1", BatchState::Green, "a2", "FastForward", false);
    let rev = b.store.head().unwrap();
    transition(&b, "alpha", "k1", BatchState::Green, "a2", "LandAborted", true);
    let (out, _, code) = render_since(&since_rev(&b.ports(), &FakeFacts::two(), rev).unwrap());
    assert_eq!((out.as_str(), code), ("alpha none\nbeta none\n", 1), "a GREEN round has not landed");
    let at = landed(&b, "alpha", "k1", "a2");
    let rows = since_rev(&b.ports(), &FakeFacts::two(), rev).unwrap();
    assert_eq!(rows[0].1.as_ref().unwrap().as_ref().map(|l| l.rev), Some(at));
}

#[test]
fn since_rev_that_cannot_read_a_repo_prints_none_and_says_why() {
    let b = Bench::unserved();
    let (out, err, code) = render_since(&since_rev(&b.ports(), &FakeFacts::two(), 0).unwrap());
    assert_eq!((out.as_str(), code), ("alpha none\nbeta none\n", 1));
    assert!(err.contains("alpha: batches/alpha/") && err.contains("no spira-lc door"), "{err}");
    let f = FakeFacts {
        repos: Some(Err("no registry".into())),
        ..FakeFacts::default()
    };
    assert_eq!(since_rev(&b.ports(), &f, 0), Err("no registry".into()));
}

#[test]
fn the_modes_are_named_and_mutually_exclusive() {
    assert_eq!(parse(&["--readiness"]), Ok(Mode::Readiness));
    assert_eq!(parse(&["--since-rev", "42"]), Ok(Mode::SinceRev(42)));
    assert_eq!(parse(&["--probe-base", "alpha"]), Ok(Mode::ProbeBase("alpha".into())));
    for bad in [
        &["--readiness", "--since-rev", "4"][..],
        &["--since-rev", "x"],
        &["--since-rev"],
        &["--probe-base", "--readiness"],
        &["42"],
        &[],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_probe_runs_the_selection_or_for_all_the_landing_trees_whole_corpus() {
    let mut f = FakeFacts::two();
    assert_eq!(probe_suites(&f, "alpha", "test-a.sh, test-b.sh"), Ok(vec!["test-a.sh".into(), "test-b.sh".into()]));
    assert!(probe_suites(&f, "alpha", " , ").unwrap_err().contains("names no suite"));
    f.tree("alpha", DECL, "ci_entry = \"bash ci.sh\"\n[tier.unit]\ncmd = \"x\"\n[tier.e2e]\ncmd = \"y\"\n");
    assert_eq!(probe_suites(&f, "alpha", "all"), Ok(vec!["e2e".into(), "unit".into()]));
    f.tree("alpha", HARNESS_IMAGE, "FROM x\n");
    f.names.insert(("alpha".into(), "spira".into()), vec!["test-a.sh".into(), "lib.sh".into(), "test-b.sh".into()]);
    assert_eq!(probe_suites(&f, "alpha", "all"), Ok(vec!["test-a.sh".into(), "test-b.sh".into()]));
    assert_eq!(probe_suites(&f, "beta", "all").unwrap_err(), "git ls-tree exited Some(128): no spira in beta");
    f.names.insert(("beta".into(), "spira".into()), vec!["lib.sh".into()]);
    assert!(probe_suites(&f, "beta", "all").unwrap_err().contains("declares no suite"));
    f.tree("beta", DECL, "tier = 3\n");
    assert!(probe_suites(&f, "beta", "all").unwrap_err().contains("does not parse"));
}

/// A round VM that serves the base run, and is gone once closed unless it lingers.
struct Vm {
    gone: Mutex<bool>,
    lingers: bool,
}

impl Child for Vm {
    fn poll(&self) -> Option<Result<CertifyEnd, VerbRefusal>> {
        None
    }
    fn mark(&self) -> Option<Mark> {
        None
    }
    fn signal(&self, sig: Signal) -> Result<(), String> {
        if sig == Signal::Kill && !self.lingers {
            *crate::lock(&self.gone) = true;
        }
        Ok(())
    }
    fn gone(&self) -> bool {
        *crate::lock(&self.gone)
    }
}

/// How the scripted corpus ends.
#[derive(Clone)]
enum Ends {
    /// Each suite's result, then the end.
    Ran(Vec<(&'static str, bool)>, MainEnd),
    Faults(&'static str),
    NoStart,
}

struct Subs<'a> {
    vm: &'a Vm,
    up: bool,
    ends: Ends,
    after_ms: u64,
    clock: VirtualClock,
    started: Option<(u64, Vec<String>)>,
    members: Option<Vec<RoundMember>>,
    /// The slot record as the store holds it at each poll.
    during: Option<&'a Bench>,
    seen: Vec<serde_json::Value>,
    /// How far each wait moves the clock: past the slot's TTL, the runner stalled.
    step_ms: u64,
}

impl<'a> Subs<'a> {
    fn new(b: &Bench, vm: &'a Vm, ends: Ends, after_ms: u64) -> Subs<'a> {
        Subs {
            vm,
            up: false,
            ends,
            after_ms,
            clock: b.clock.clone(),
            started: None,
            members: None,
            during: None,
            seen: vec![],
            step_ms: 1_000,
        }
    }
}

impl RoundRunner for Subs<'_> {
    fn set_members(&mut self, members: &[RoundMember]) {
        self.members = Some(members.to_vec());
    }
    fn set_base(&mut self, _: &str) {}
    fn start_main(&mut self, kind: MainKind, suites: &[String]) -> Result<(), String> {
        assert_eq!(kind, MainKind::Corpus);
        if matches!(self.ends, Ends::NoStart) {
            return Err("round-vm did not come up".into());
        }
        self.up = true;
        self.started = Some((self.clock.now_ms(), suites.to_vec()));
        Ok(())
    }
    fn poll_main(&mut self) -> Result<MainPoll, String> {
        if let Some(b) = self.during {
            if let Ok(Some(i)) = b.store.get("slots/proxmox/0") {
                self.seen.push(i.value.data);
            }
        }
        let (at, _) = self.started.clone().expect("polled only once started");
        if self.clock.now_ms() < at + self.after_ms {
            return Ok(MainPoll::default());
        }
        match &self.ends {
            Ends::Ran(results, end) => Ok(MainPoll {
                results: results.iter().map(|(s, g)| (s.to_string(), *g)).collect(),
                done: Some(*end),
            }),
            Ends::Faults(why) => Err(why.to_string()),
            Ends::NoStart => unreachable!(),
        }
    }
    fn stop_main(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn launch(&mut self, _: &Job) -> Result<(), String> {
        Err("the probe runs no sub-pass".into())
    }
    fn poll_jobs(&mut self) -> Vec<(u64, batcher::attrib::JobResult)> {
        vec![]
    }
    fn now(&self) -> u64 {
        self.clock.now_ms() / 1000
    }
    fn wait(&mut self) {
        self.clock.advance(self.step_ms);
    }
}

impl SubPasses for Subs<'_> {
    fn vm(&self) -> Option<&dyn Child> {
        self.up.then_some(self.vm as &dyn Child)
    }
    fn close(&mut self) {
        if !self.vm.lingers {
            *crate::lock(&self.vm.gone) = true;
        }
    }
}

struct Advance(VirtualClock);

impl Sleep for Advance {
    fn sleep_ms(&self, ms: u64) {
        self.0.advance(ms);
    }
}

fn vm(lingers: bool) -> Vm {
    Vm {
        gone: Mutex::new(false),
        lingers,
    }
}

const SUITES: [&str; 2] = ["test-a.sh", "test-b.sh"];

fn suites() -> Vec<String> {
    SUITES.map(String::from).to_vec()
}

fn green_run() -> Ends {
    Ends::Ran(vec![("test-a.sh", true), ("test-b.sh", true)], MainEnd::Ran)
}

fn probe_with(b: &Bench, slots: Option<&crate::slots::SlotSettings>, subs: &mut Subs) -> Result<Probed, String> {
    let ledger = StoreLedger { policy: POLICY };
    let base = b.ports();
    let p = Ports {
        ledger: &ledger,
        slots: slots.unwrap_or(base.slots),
        ..base
    };
    let sleep = Advance(b.clock.clone());
    let env = RunEnv {
        sleep: &sleep,
        poll_ms: 1_000,
        teardown_ms: 30_000,
        kill_grace_ms: 5_000,
    };
    let lane = probe_target(&p, "alpha")?;
    probe_base(&p, &env, subs, ("alpha", &lane), &suites(), "probe.scope")
}

fn probe(b: &Bench, subs: &mut Subs) -> Result<Probed, String> {
    probe_with(b, None, subs)
}

#[test]
fn a_green_base_probe_is_recorded_under_the_base_identity_and_readiness_reads_it() {
    let b = Bench::new();
    ready(&b);
    seed_lane_replace(&b, "alpha", "a9", "sha256:img2");
    assert!(!why(&readiness(&b.ports(), &FakeFacts::two()), "alpha", "base").green);
    let v = vm(false);
    let mut subs = Subs::new(&b, &v, green_run(), 120_000);
    subs.during = Some(&b);
    let probed = probe(&b, &mut subs).unwrap();
    assert_eq!((probed.class.clone(), probed.attempt, probed.slot_kept, probed.code()), (Class::Green, 1, false, 0));
    assert_eq!(probed.identity, base_identity("alpha", &lane_of(&b, "alpha")));
    assert_eq!(subs.members, Some(vec![]), "the base tree carries no member");
    assert_eq!(subs.started.as_ref().map(|(_, s)| s.clone()), Some(suites()));
    assert!(
        !subs.seen.is_empty() && subs.seen.iter().all(|s| s["unit"] == "probe.scope" && s["holder"] == PROBE_HOLDER),
        "the slot names its holder's unit while the VM is up: {:?}",
        subs.seen
    );
    let slot = b.store.get("slots/proxmox/0").unwrap().unwrap();
    assert_eq!(slot.value.data["released"], true, "{:?}", slot.value.data);
    assert!(b.store.get("locks/runner-slot/proxmox/0").unwrap().is_none(), "the slot is given back");
    let key = summary_key(&probed.identity);
    let owners = b.store.get(&key).unwrap().unwrap().value.metadata.owner_references;
    assert_eq!(owners, vec!["lanes/alpha"], "kept while the lane lives");
    let r = readiness(&b.ports(), &FakeFacts::two());
    assert!(why(&r, "alpha", "base").green, "{}", r.render());
    assert!(probed.render().starts_with("probe=green repo=alpha base=a9 image=sha256:img2 attempt=1"));
}

#[test]
fn a_red_unfinished_or_faulted_base_probe_is_recorded_and_exits_1() {
    for (ends, class, readiness_says) in [
        (
            Ends::Ran(vec![("test-a.sh", true), ("test-b.sh", false)], MainEnd::Ran),
            Class::Red { step: "test-b.sh".into() },
            "was red",
        ),
        (
            Ends::Ran(vec![], MainEnd::WorkspaceBuild),
            Class::Red { step: "workspace-build".into() },
            "was red",
        ),
        (
            Ends::Ran(vec![("test-a.sh", true)], MainEnd::Ran),
            Class::Fault { cause: Cause::Vm },
            "no green base probe",
        ),
        (
            Ends::Faults("round-vm: harness fault"),
            Class::Fault { cause: Cause::Vm },
            "no green base probe",
        ),
    ] {
        let b = Bench::new();
        seed_lane(&b, "alpha", "a1", "sha256:img2");
        let v = vm(false);
        let mut subs = Subs::new(&b, &v, ends, 0);
        let probed = probe(&b, &mut subs).unwrap();
        assert_eq!((probed.class.clone(), probed.code()), (class.clone(), 1));
        let r = readiness(&b.ports(), &FakeFacts::two());
        let c = why(&r, "alpha", "base");
        assert!(!c.green && c.why.contains(readiness_says), "{c:?}");
    }
}

#[test]
fn a_probe_whose_vm_outlives_its_teardown_keeps_the_slot_to_lapse() {
    let b = Bench::new();
    seed_lane(&b, "alpha", "a1", "sha256:img2");
    let v = vm(true);
    let mut subs = Subs::new(&b, &v, green_run(), 0);
    let probed = probe(&b, &mut subs).unwrap();
    assert!(probed.slot_kept && probed.render().contains("slot=kept"));
    let slot = b.store.get("slots/proxmox/0").unwrap().unwrap();
    assert_ne!(slot.value.data["released"], true, "a slot whose VM may live is not marked released");
}

#[test]
fn a_probe_refuses_a_held_slot_a_stale_lane_or_no_lane_and_gives_back_a_failed_start() {
    let b = Bench::new();
    let v = vm(false);
    let mut subs = Subs::new(&b, &v, green_run(), 0);
    assert!(probe(&b, &mut subs).unwrap_err().contains("no lane record at lanes/alpha"));
    seed_lane(&b, "alpha", "a1", "sha256:img2");
    b.git.advance("alpha", "a2");
    let e = probe(&b, &mut subs).unwrap_err();
    assert!(e.contains("names a1") && e.contains("at a2"), "{e}");
    b.git.advance("alpha", "a1");
    let mut down = Subs::new(&b, &v, Ends::NoStart, 0);
    assert!(probe(&b, &mut down).unwrap_err().contains("did not start"));
    assert!(b.store.get("locks/runner-slot/proxmox/0").unwrap().is_none(), "a failed start gives the slot back");
    let held = grant_for_hand(&b.ports(), "round-recert").unwrap();
    let one_slot = crate::slots::SlotSettings {
        ec2_slots: 0,
        spill: false,
    };
    let e = probe_with(&b, Some(&one_slot), &mut subs).unwrap_err();
    assert!(e.contains("round-recert"), "a refusal names the holder: {e}");
    assert!(subs.started.is_none(), "nothing ran without a slot");
    held.release(&b.ports()).unwrap();
}

#[test]
fn a_probe_that_loses_its_slot_mid_run_records_nothing() {
    let b = Bench::new();
    seed_lane(&b, "alpha", "a1", "sha256:img2");
    let v = vm(false);
    let mut subs = Subs::new(&b, &v, green_run(), 10 * 60_000);
    subs.step_ms = 90_000;
    let e = probe(&b, &mut subs).unwrap_err();
    assert!(e.contains("lost the runner slot"), "{e}");
    let id = base_identity("alpha", &lane_of(&b, "alpha"));
    assert!(b.store.get(&summary_key(&id)).unwrap().is_none());
}
