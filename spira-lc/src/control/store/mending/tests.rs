use std::cell::Cell;

use lifecycle::mending::MendingState;
use runtime::ports::VirtualClock;
use state_store::{Config, Contract, MemBackend, Ports, Request};

use super::Store;
use crate::control::keys;
use crate::control::state::{self, Call};

const AT: i64 = 1_767_225_600;
const T1: &str = "1111111111111111111111111111111111111111";
const T2: &str = "2222222222222222222222222222222222222222";
const FACTS: &str =
    r#"{"changes":[],"member_files":[],"required_checks":[],"checked":[],"ask":null}"#;

struct Rig {
    s: Store<MemBackend>,
    n: Cell<u64>,
}

impl Rig {
    fn new() -> Self {
        let c = Contract::open(
            MemBackend::new(),
            Ports::virtual_clock(&VirtualClock::new(AT as u64 * 1_000)),
            Config {
                min_lock_ttl_ms: 1_000,
            },
        )
        .unwrap();
        Rig {
            s: Store::new(c, 1_000),
            n: Cell::new(0),
        }
    }

    fn lc(&self, argv: &[&str]) -> (i32, String) {
        self.n.set(self.n.get() + 1);
        let req = Request::new(&format!("r{}", self.n.get()), "test", "t");
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        state::run(
            &self.s,
            &args,
            &Call {
                now: AT,
                request: &req.id,
            },
        )
    }

    fn ok(&self, argv: &[&str]) -> String {
        let (rc, out) = self.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        out
    }

    fn event(&self, machine: &str, key: &str, expect: &str, version: u64, kind: &str) -> String {
        let v = version.to_string();
        self.ok(&[
            "event",
            machine,
            key,
            "--expect",
            expect,
            "--version",
            &v,
            "--actor",
            "test",
            "--kind",
            kind,
        ])
    }

    fn round_version(&self) -> u64 {
        self.s.batch("b-1").unwrap().unwrap().1.value.row.version
    }

    /// A round of sp-a at T1, its first pass running suites.
    fn suites(&self) {
        self.ok(&["create-bead", "sp-a", "--repo", "spira"]);
        let claim = r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#;
        self.event("bead", "sp-a", "READY", 0, claim);
        self.event(
            "bead",
            "sp-a",
            "WORKING",
            1,
            &format!(r#"{{"Submit":{{"tip":"{T1}"}}}}"#),
        );
        self.ok(&[
            "cut",
            "b-1",
            "--repo",
            "spira",
            "--head",
            "H",
            "--base",
            "B",
            "--members",
            &format!("sp-a:{T1}"),
            "--actor",
            "test",
        ]);
        let v = self.round_version();
        self.event(
            "batch",
            "b-1",
            "OPEN",
            v,
            r#"{"PassStarted":{"n":1,"head":"H"}}"#,
        );
        self.event(
            "batch",
            "b-1",
            "CI_RUNNING",
            v + 1,
            r#"{"SuitesStarted":{"n":1}}"#,
        );
    }

    fn fail(&self, suite: &str) -> String {
        let kind = format!(r#"{{"Failed":{{"suite":"{suite}","n":1}}}}"#);
        self.event("batch", "b-1", "CI_RUNNING", self.round_version(), &kind)
    }

    fn row_state(&self, suite: &str) -> MendingState {
        self.s.mending("b-1", 1, suite).unwrap().unwrap().row.state
    }

    fn pickup(&self, suite: &str, deadline: &str) {
        self.ok(&[
            "mending",
            "pickup",
            "b-1",
            "--pass",
            "1",
            "--suite",
            suite,
            "--deadline-secs",
            deadline,
            "--actor",
            "mender",
        ]);
    }

    fn mend(&self, suite: &str, members: &str) -> (i32, String) {
        self.lc(&[
            "mending",
            "mend",
            "b-1",
            "--pass",
            "1",
            "--suite",
            suite,
            "--members",
            members,
            "--facts",
            FACTS,
            "--fences",
            "[]",
            "--actor",
            "mender",
        ])
    }
}

#[test]
fn a_failed_suite_opens_its_mending_row_in_the_events_txn_and_is_recorded_once() {
    let r = Rig::new();
    r.suites();
    assert_eq!(r.fail("test-a.sh"), "");
    assert_eq!(r.row_state("test-a.sh"), MendingState::Waiting);
    let row =
        r.s.contract()
            .get(&keys::mending("b-1#1#test-a.sh"))
            .unwrap()
            .unwrap();
    let round = r.s.batch("b-1").unwrap().unwrap().1;
    assert_eq!(row.mod_rev.main, round.mod_rev.main, "one txn");
    let v = r.round_version();
    assert_eq!(
        r.fail("test-a.sh"),
        "already recorded: test-a.sh failed in pass 1"
    );
    assert_eq!(r.round_version(), v, "a second Failed writes nothing");
}

#[test]
fn a_mend_moves_the_branch_the_next_mender_builds_on_and_a_stale_one_returns_the_failure() {
    let r = Rig::new();
    r.suites();
    r.fail("test-a.sh");
    r.fail("test-b.sh");
    r.pickup("test-a.sh", "600");
    assert_eq!(
        r.mend("test-a.sh", &format!("sp-a:{T1}:{T2}")),
        (0, "MENDED".into())
    );
    assert_eq!(
        r.ok(&["mending", "tips", "--beads", "sp-a,sp-x"]),
        format!(r#"{{"sp-a":"{T2}","sp-x":null}}"#)
    );
    r.pickup("test-b.sh", "600");
    let (rc, out) = r.mend("test-b.sh", &format!("sp-a:{T1}:{T2}"));
    assert_eq!(rc, 3, "{out}");
    assert!(
        out.contains("StaleTip") && out.ends_with("the failure is returned"),
        "{out}"
    );
    assert_eq!(r.row_state("test-b.sh"), MendingState::Returned);
}

#[test]
fn an_overdue_mend_expires_on_sweep_and_a_waiting_failure_of_an_ended_round_is_returned() {
    let r = Rig::new();
    r.suites();
    r.fail("test-a.sh");
    r.fail("test-b.sh");
    r.pickup("test-a.sh", "0");
    assert_eq!(r.ok(&["mending", "sweep", "--actor", "sweeper"]), "1");
    assert_eq!(r.row_state("test-a.sh"), MendingState::Returned);
    assert_eq!(
        r.ok(&["mending", "close-ended", "--actor", "concierge"]),
        "0",
        "the round is live"
    );
    let v = r.round_version();
    r.event(
        "batch",
        "b-1",
        "CI_RUNNING",
        v,
        r#"{"Abandon":{"reason":"stopped"}}"#,
    );
    assert_eq!(
        r.ok(&["mending", "close-ended", "--actor", "concierge"]),
        "1"
    );
    assert_eq!(r.row_state("test-b.sh"), MendingState::Returned);
    let listed: serde_json::Value =
        serde_json::from_str(&r.ok(&["mending", "list", "--batch", "b-1"])).unwrap();
    let states: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["RETURNED", "RETURNED"]);
}
