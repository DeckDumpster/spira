//! The readiness, claim, birth and sweep verbs, driven through the verbs a caller sends, on the
//! in-memory backend and a virtual clock.

use runtime::ports::VirtualClock;
use serde_json::{json, Value};
use spira_config::edges::gate;
use state_store::{Config, Contract, MemBackend, Ports};

use lifecycle::bead::BeadState;

use super::run as readiness_verb;
use crate::control::keys;
use crate::control::state::{self, Call};
use crate::control::store::Store;

const FLOOR_MS: u64 = 1_000;
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
const TIP2: &str = "1111111111111111111111111111111111111111";
const STAMP: &str = "2026-11-01T12:00:00Z";

struct Rig {
    store: Store<MemBackend>,
    clock: VirtualClock,
    n: std::cell::Cell<u32>,
}

impl Rig {
    fn new() -> Self {
        let clock = VirtualClock::new(1_000_000);
        let c = Contract::open(
            MemBackend::new(),
            Ports::virtual_clock(&clock),
            Config {
                min_lock_ttl_ms: FLOOR_MS,
            },
        )
        .unwrap();
        Rig {
            store: Store::new(c, FLOOR_MS),
            clock,
            n: std::cell::Cell::new(0),
        }
    }

    fn now(&self) -> i64 {
        (self.clock_ms() / 1_000) as i64
    }

    fn clock_ms(&self) -> u64 {
        use runtime::ports::Clock;
        self.clock.now_ms()
    }

    fn call(&self, argv: &[&str]) -> (i32, String) {
        self.n.set(self.n.get() + 1);
        let request = format!("t/{}", self.n.get());
        let call = Call {
            now: self.now(),
            request: &request, inputs: &crate::control::claimable::FIXED,
        };
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        if super::VERBS.contains(&argv[0]) {
            readiness_verb(&self.store, &args, &call)
        } else {
            state::run(&self.store, &args, &call)
        }
    }

    fn ok(&self, argv: &[&str]) -> String {
        let (code, out) = self.call(argv);
        assert_eq!(code, 0, "{argv:?}: {out}");
        out
    }

    fn row(&self, id: &str) -> Value {
        let out = self.ok(&["show", id]);
        serde_json::from_str::<Value>(&out).unwrap()["bead"].clone()
    }

    fn state(&self, id: &str) -> String {
        self.row(id)["state"].as_str().unwrap().to_string()
    }

    fn event(&self, id: &str, kind: Value) -> (i32, String) {
        let row = self.row(id);
        self.call(&[
            "event",
            "bead",
            id,
            "--expect",
            row["state"].as_str().unwrap(),
            "--version",
            row["version"].as_str().unwrap(),
            "--actor",
            "dwarf-1",
            "--kind",
            &kind.to_string(),
        ])
    }

    fn applied(&self, id: &str, kind: Value) {
        let (code, out) = self.event(id, kind);
        assert_eq!(code, 0, "{id}: {out}");
    }

    fn claim(&self, id: &str) -> (i32, String) {
        self.claim_stacked(id, json!({}))
    }

    fn claim_stacked(&self, id: &str, stack: Value) -> (i32, String) {
        let lease = self.now() + 60;
        let depth = u32::from(stack.as_object().is_some_and(|s| !s.is_empty()));
        self.event(
            id,
            json!({ "Claim": { "holder": "dwarf-1", "lease_until": lease, "stack": stack,
                "stack_depth": depth, "stack_max_depth": 4 } }),
        )
    }

    /// A record taken to CERTIFIED at `tip`.
    fn certified(&self, id: &str, tip: &str) {
        self.ok(&["create-bead", id, "--submitted-tip", tip]);
        self.applied(id, json!({ "GatePass": { "tip": tip, "gate_key": "k" } }));
        assert_eq!(self.state(id), "CERTIFIED");
    }

    fn mod_rev(&self, key: &str) -> u64 {
        self.store
            .contract()
            .get(key)
            .unwrap()
            .unwrap()
            .mod_rev
            .main
    }

    fn publish(&self, entries: Value) {
        let body = json!({ "computed_at": STAMP, "planning_watermark": "w-7", "entries": entries });
        self.ok(&["publish-readiness", "--json", &body.to_string()]);
    }

    fn stamp_planning(&self, seq: u64) {
        use state_store::{Object, Request, Txn};
        let txn = Txn::new().put(keys::PLANNING_WATERMARK, Object::new(json!({ "seq": seq })));
        self.store
            .contract()
            .txn(&txn, &Request::new(&format!("stamp/{seq}"), "test", "plan-stamp"))
            .unwrap();
    }

    fn judged_at(&self, id: &str, watermark: &str, entry: Value) {
        let mut entry = entry;
        entry["id"] = json!(id);
        let body = json!({ "computed_at": STAMP, "planning_watermark": watermark, "entries": [entry] });
        self.ok(&["publish-readiness", "--json", &body.to_string()]);
    }

    fn judged_claimable(&self, id: &str, gates: Value) {
        self.publish(json!([{ "id": id, "claimable": true, "gates": gates }]));
    }
}

fn certified_gate(tip: &str) -> Value {
    serde_json::to_value(gate(BeadState::Certified, Some(tip), false)).unwrap()
}

#[test_id(tf0pxh8)]
#[test]
fn a_blockers_hold_express_or_phase_since_judgement_does_not_conflict_the_claim() {
    let r = Rig::new();
    r.certified("sp-b", TIP);
    r.ok(&["create-bead", "sp-w", "--ready"]);
    r.ok(&["create-bead", "sp-a", "--ready"]);
    assert_eq!(r.claim("sp-w").0, 0);
    let unfinished = serde_json::to_value(gate(BeadState::Working, None, false)).unwrap();
    r.judged_claimable(
        "sp-a",
        json!({ "sp-b": certified_gate(TIP), "sp-w": unfinished }),
    );
    r.applied(
        "sp-b",
        json!({ "Hold": { "kind": "Wait", "cause": "no-progress-backoff" } }),
    );
    r.applied("sp-b", json!("Express"));
    r.applied("sp-w", json!({ "Phase": { "phase": "Building" } }));
    let (code, out) = r.claim("sp-a");
    assert_eq!(code, 0, "{out}");
    assert_eq!(r.state("sp-a"), "WORKING");
}

#[test_id(t7j32s9)]
#[test]
fn a_claim_records_the_edge_set_and_planning_watermark_it_was_judged_against() {
    let r = Rig::new();
    r.certified("sp-b", TIP);
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.judged_claimable(
        "sp-a",
        json!({ "sp-b": certified_gate(TIP), "sp-gone": null }),
    );
    assert_eq!(r.claim("sp-a").0, 0);
    let changes = r.store.history("sp-a", None).unwrap();
    let claim = changes.iter().find(|c| c.audit.event == "Claim").unwrap();
    let judged = &claim.audit.evidence["Claim"]["judged"];
    assert_eq!(judged["planning_watermark"], "w-7");
    assert_eq!(judged["edges"]["sp-b"], certified_gate(TIP));
    assert_eq!(judged["edges"]["sp-gone"], Value::Null);
}

#[test_id(tnmik04)]
#[test]
fn a_stacked_claim_conflicts_when_its_prerequisite_moved_off_the_stacked_tip() {
    let r = Rig::new();
    r.certified("sp-b", TIP);
    r.ok(&["create-bead", "sp-a", "--ready"]);
    let (code, out) = r.claim_stacked("sp-a", json!({ "sp-b": TIP2 }));
    assert_eq!(code, crate::REFUSED, "{out}");
    assert!(out.contains("blocker sp-b moved"), "{out}");
    let (code, out) = r.claim_stacked("sp-a", json!({ "sp-b": TIP }));
    assert_eq!(code, 0, "{out}");
}

#[test_id(tn88kf3)]
#[test]
fn the_gate_is_written_with_a_transition_that_moves_it_and_never_by_one_that_does_not() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-b", "--submitted-tip", TIP]);
    let gate_key = keys::gate("sp-b");
    let born = r.mod_rev(&gate_key);
    r.applied("sp-b", json!("Express"));
    assert_eq!(r.mod_rev(&gate_key), born, "Express moved the gate");
    r.applied(
        "sp-b",
        json!({ "GatePass": { "tip": TIP, "gate_key": "k" } }),
    );
    let certified = r.mod_rev(&gate_key);
    assert!(certified > born);
    r.applied("sp-b", json!("Deliver"));
    assert_eq!(r.state("sp-b"), "IN_DELIVERY");
    assert_eq!(
        r.mod_rev(&gate_key),
        certified,
        "CERTIFIED to IN_DELIVERY is stackable both sides: the gate stays"
    );
}

#[test_id(trb5kiv)]
#[test]
fn publishing_readiness_covers_submitted_and_certified_records_and_moves_no_lifecycle_record() {
    let r = Rig::new();
    r.certified("sp-c", TIP);
    r.ok(&["create-bead", "sp-s", "--submitted-tip", TIP]);
    // Live blockers: `show` and `list` name only the prerequisites whose record still lives.
    r.ok(&["create-bead", "sp-x", "--ready"]);
    r.ok(&["create-bead", "sp-y", "--ready"]);
    let before = (
        r.mod_rev(&keys::lifecycle("sp-c")),
        r.mod_rev(&keys::lifecycle("sp-s")),
    );
    r.publish(json!([
        { "id": "sp-c", "blocked_by": ["sp-x"], "gates": { "sp-x": { "class": "idle" } } },
        { "id": "sp-s", "blocked_by": ["sp-y"] },
        { "id": "sp-ghost", "claimable": true },
    ]));
    assert_eq!(
        (
            r.mod_rev(&keys::lifecycle("sp-c")),
            r.mod_rev(&keys::lifecycle("sp-s"))
        ),
        before
    );
    assert_eq!(r.row("sp-c")["blocked_by"], json!(["sp-x"]));
    assert_eq!(r.row("sp-s")["blocked_by"], json!(["sp-y"]));
    let listed: Value = serde_json::from_str(&r.ok(&["list", "--state", "CERTIFIED"])).unwrap();
    assert_eq!(listed[0]["blocked_by"], json!(["sp-x"]));
    let c = r.store.readiness("sp-c").unwrap().unwrap().value;
    assert_eq!(
        (c.claimable, c.judged_at_planning_watermark.as_deref()),
        (false, Some("w-7"))
    );
    assert!(r.store.readiness("sp-ghost").unwrap().is_none());
}

#[test_id(tp6aekd)]
#[test]
fn a_record_from_filing_is_unclaimable_until_promoted_and_ready_born_records_are_claimable() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-f"]);
    assert_eq!(r.state("sp-f"), "OPEN");
    let (code, out) = r.claim("sp-f");
    assert_eq!(code, crate::REFUSED, "{out}");
    assert_eq!(r.state("sp-f"), "OPEN");
    r.ok(&["promote-filed", "sp-f"]);
    assert_eq!(r.state("sp-f"), "READY");
    r.ok(&["promote-filed", "sp-f"]);
    assert_eq!(r.state("sp-f"), "READY", "a second promote leaves it");
    assert_eq!(r.claim("sp-f").0, 0);
    r.ok(&["create-bead", "sp-r", "--ready"]);
    assert_eq!(r.state("sp-r"), "READY");
    assert_eq!(r.call(&["promote-filed", "sp-none"]).0, crate::CANNOT_TELL);
}

fn sweep(r: &Rig, extra: &[&str]) -> Value {
    let mut argv = vec!["sweep-open", "--grace-secs", "300"];
    argv.extend_from_slice(extra);
    serde_json::from_str(&r.ok(&argv)).unwrap()
}

#[test_id(trjzmh3)]
#[test]
fn an_open_record_past_the_grace_is_promoted_when_its_bead_is_open_and_left_inside_it() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-o"]);
    r.clock.advance(299_000);
    assert_eq!(sweep(&r, &["--open", "sp-o"])["promoted"], json!([]));
    assert_eq!(r.state("sp-o"), "OPEN");
    r.clock.advance(1_000);
    assert_eq!(sweep(&r, &["--open", "sp-o"])["promoted"], json!(["sp-o"]));
    assert_eq!(r.state("sp-o"), "READY");
}

#[test_id(tb76edm)]
#[test]
fn an_open_record_whose_bead_is_gone_is_named_for_escalation_exactly_once() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-g"]);
    r.ok(&["create-bead", "sp-u"]);
    r.clock.advance(301_000);
    let first = sweep(&r, &["--gone", "sp-g"]);
    assert_eq!(
        first["escalate"],
        json!(["sp-g"]),
        "sp-u has no answer and is left"
    );
    let marked = sweep(&r, &["--gone", "sp-g", "--escalated", "sp-g"]);
    assert_eq!(marked["escalated"], json!(["sp-g"]));
    let again = sweep(&r, &["--gone", "sp-g"]);
    assert_eq!(
        (again["escalate"].clone(), again["escalated"].clone()),
        (json!([]), json!([]))
    );
    assert_eq!(
        (r.state("sp-g"), r.state("sp-u")),
        ("OPEN".into(), "OPEN".into())
    );
}

#[test_id(tc5dtob)]
#[test]
fn an_expired_claim_returns_to_the_state_it_was_claimed_from_once_its_holder_is_stopped() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-ready", "--ready"]);
    r.ok(&["create-bead", "sp-rework", "--submitted-tip", TIP]);
    r.applied(
        "sp-rework",
        json!({ "GateRed": { "tip": TIP, "reason": "suites-failed" } }),
    );
    r.ok(&["create-bead", "sp-live", "--ready"]);
    for id in ["sp-ready", "sp-rework"] {
        assert_eq!(r.claim(id).0, 0, "{id}");
    }
    r.clock.advance(61_000);
    assert_eq!(r.claim("sp-live").0, 0);
    let out: Value =
        serde_json::from_str(&r.ok(&["expire-claims", "--stopped", "sp-rework,sp-live"])).unwrap();
    assert_eq!(
        out["returned"],
        json!(["sp-rework"]),
        "sp-live's lock is live; sp-ready's holder was not stopped"
    );
    assert_eq!(r.state("sp-rework"), "REWORK");
    assert_eq!(r.state("sp-ready"), "WORKING");
    r.ok(&["expire-claims", "--stopped", "sp-ready"]);
    assert_eq!(r.state("sp-ready"), "READY");
    assert_eq!(r.state("sp-live"), "WORKING");
}

#[test_id(tsyk02c)]
#[test]
fn a_coordination_parent_claimed_after_judgement_conflicts_its_childs_claim() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-p", "--ready"]);
    r.ok(&["create-bead", "sp-c", "--ready"]);
    let idle = serde_json::to_value(gate(BeadState::Ready, None, false)).unwrap();
    r.judged_claimable("sp-c", json!({ "sp-p": idle }));
    assert_eq!(r.claim("sp-p").0, 0);
    let (code, out) = r.claim("sp-c");
    assert_eq!(code, crate::REFUSED, "{out}");
    assert!(out.contains("blocker sp-p moved"), "{out}");
}

#[test_id(tx4j3ml)]
#[test]
fn a_record_judged_not_claimable_is_refused_its_claim_and_nothing_is_written() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.publish(json!([{ "id": "sp-a", "claimable": false, "blocked_by": ["sp-x"] }]));
    let before = r.mod_rev(&keys::lifecycle("sp-a"));
    let (code, out) = r.claim("sp-a");
    assert_eq!(code, crate::REFUSED, "{out}");
    assert!(
        out.contains("judged not claimable (blocked by sp-x)"),
        "{out}"
    );
    assert_eq!(r.mod_rev(&keys::lifecycle("sp-a")), before);
}

#[test_id(t6jw9jk)]
#[test]
fn a_refusal_with_no_blockers_names_the_verdict_the_record_was_judged_with() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.judged_at("sp-a", "7", json!({ "claimable": false, "verdict": "occupied" }));
    let (code, out) = r.claim("sp-a");
    assert_eq!(code, crate::REFUSED, "{out}");
    assert!(out.contains("occupied") && !out.contains("blocked by )"), "{out}");
}

#[test_id(t17vew4)]
#[test]
fn a_rejudge_mark_without_blockers_names_rejudge_in_the_refusal() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.judged_at("sp-a", "7", json!({ "claimable": true }));
    r.ok(&["mark-rejudge", "sp-a"]);
    let (code, out) = r.claim("sp-a");
    assert_eq!(code, crate::REFUSED, "{out}");
    assert!(out.contains("rejudge") && !out.contains("blocked by )"), "{out}");
}

#[test_id(t2vwmz0)]
#[test]
fn a_record_judged_before_verdicts_were_kept_is_refused_with_no_reason_recorded() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.judged_at("sp-a", "7", json!({ "claimable": false }));
    let (code, out) = r.claim("sp-a");
    assert_eq!(code, crate::REFUSED, "{out}");
    assert!(out.contains("(no reason recorded)"), "{out}");
}

#[test_id(tdd16uv)]
#[test]
fn a_record_with_no_gate_gets_one_on_its_next_transition() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    let drop = state_store::Txn::new().delete(&keys::gate("sp-a"));
    let req = state_store::Request::new("drop-gate", "test", "t");
    assert!(r.store.contract().txn(&drop, &req).unwrap().succeeded);
    assert!(r
        .store
        .contract()
        .get(&keys::gate("sp-a"))
        .unwrap()
        .is_none());
    r.applied("sp-a", json!("Express"));
    assert!(r
        .store
        .contract()
        .get(&keys::gate("sp-a"))
        .unwrap()
        .is_some());
}

#[test_id(t8xalrn)]
#[test]
fn an_unrelated_planning_write_after_judgement_does_not_refuse_the_claim() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.stamp_planning(7);
    r.judged_at("sp-a", "7", json!({ "claimable": true }));
    r.stamp_planning(8);
    let (code, out) = r.claim("sp-a");
    assert_eq!(code, 0, "{out}");
    assert_eq!(r.state("sp-a"), "WORKING");
}

#[test_id(tpnzqzo)]
#[test]
fn an_edge_added_through_the_door_refuses_the_claim_naming_the_blocker_before_any_publish() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.ok(&["create-bead", "sp-b", "--ready"]);
    r.judged_at("sp-a", "7", json!({ "claimable": true }));
    r.ok(&["mark-rejudge", "sp-a", "--blocked-by", "sp-b"]);
    let (code, out) = r.claim("sp-a");
    assert_eq!(code, crate::REFUSED, "{out}");
    assert!(out.contains("judged not claimable") && out.contains("sp-b"), "{out}");
    assert_eq!(r.state("sp-a"), "READY");
    r.judged_at("sp-a", "8", json!({ "claimable": true }));
    assert_eq!(r.claim("sp-a").0, 0);
}

#[test_id(tg5f5ug)]
#[test]
fn a_mark_moves_the_readiness_revision_and_leaves_the_record_judged_but_not_claimable() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.judged_at("sp-a", "7", json!({ "claimable": true }));
    let before = r.store.readiness("sp-a").unwrap().unwrap().mod_rev;
    r.ok(&["mark-rejudge", "sp-a"]);
    assert_ne!(r.store.readiness("sp-a").unwrap().unwrap().mod_rev, before);
    let marked = r.store.readiness("sp-a").unwrap().unwrap().value;
    assert!(marked.judged_for_claim && !marked.claimable);
}

#[test_id(tklf12u)]
#[test]
fn a_mark_on_a_record_no_pass_has_judged_leaves_it_claimable() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.ok(&["mark-rejudge", "sp-a", "--blocked-by", "sp-b"]);
    assert!(r.store.readiness("sp-a").unwrap().is_none());
    assert_eq!(r.claim("sp-a").0, 0);
}

#[test_id(tu5cz8c)]
#[test]
fn a_claim_judged_at_the_planning_watermark_it_finds_goes_through() {
    let r = Rig::new();
    r.ok(&["create-bead", "sp-a", "--ready"]);
    r.stamp_planning(7);
    r.judged_at("sp-a", "7", json!({ "claimable": true }));
    let (code, out) = r.claim("sp-a");
    assert_eq!(code, 0, "{out}");
    assert_eq!(r.state("sp-a"), "WORKING");
}

#[test_id(tejecpz)]
#[test]
fn a_payload_sent_on_stdin_is_published_with_exit_zero() {
    let r = Rig::new();
    let body = json!({ "computed_at": STAMP, "entries": [{ "id": "sp-a", "claimable": true }] }).to_string();
    let argv = crate::cutover::inline_payload_from(vec!["publish-readiness".into()], &mut body.as_bytes());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    r.ok(&argv);
}

#[test_id(tf5n425)]
#[test]
fn the_judged_claimable_set_is_the_ready_records_a_pass_judged_claimable_and_nothing_else() {
    let r = Rig::new();
    for id in ["sp-yes", "sp-no", "sp-unjudged", "sp-taken"] {
        r.ok(&["create-bead", id, "--ready"]);
    }
    r.publish(json!([
        { "id": "sp-yes", "claimable": true },
        { "id": "sp-no", "claimable": false, "blocked_by": ["sp-yes"] },
        { "id": "sp-taken", "claimable": true },
    ]));
    assert_eq!(r.claim("sp-taken").0, 0);
    let ids = |r: &Rig| -> Vec<String> { r.store.judged_claimable().unwrap().0.into_iter().map(|(rec, _)| rec.row.bead_id).collect() };
    assert_eq!(ids(&r), ["sp-yes"]);
    assert_eq!(r.store.judged_claimable().unwrap().1, 1, "sp-unjudged has no judgement; sp-taken is no longer READY");
    let (_, judged) = r.store.judged_claimable().unwrap().0.remove(0);
    assert_eq!(judged.judged_at_planning_watermark.as_deref(), Some("w-7"));
}

struct Fixed {
    run: std::path::PathBuf,
    rowless: Vec<(String, String, String)>,
    fail: bool,
    items_fail: bool,
}

impl crate::control::claimable::Inputs for Fixed {
    fn config(&self) -> Result<crate::control::claimable::Config, String> {
        if self.fail {
            return Err("SPIRA_STACK_MAX_DEPTH is unreadable".into());
        }
        Ok(crate::control::claimable::Config {
            queue_wait: String::new(),
            stack_max: 4,
            incident_label: "incident".into(),
            pilgrim_label: "pilgrimage".into(),
            skuld_label: "draft".into(),
            scope_label: String::new(),
            no_loop_label: String::new(),
        })
    }

    fn statuses(&self, ids: &[String]) -> Result<std::collections::BTreeMap<String, (String, String)>, String> {
        Ok(self.rowless.iter().filter(|(id, ..)| ids.contains(id)).map(|(i, t, s)| (i.clone(), (t.clone(), s.clone()))).collect())
    }

    fn items(&self, ids: &[String]) -> Result<Vec<Value>, String> {
        if self.items_fail {
            return Err("the content store is unreadable".into());
        }
        Ok(ids
            .iter()
            .map(|id| json!({ "id": id, "title": format!("title of {id}"), "priority": 1, "issue_type": "task", "status": "open", "created_at": "2026-01-02T03:04:05Z", "defer_until": "2026-02-03T00:00:00Z", "dependencies": [{ "issue_id": id, "depends_on_id": "sp-w", "type": "blocks" }] }))
            .collect())
    }

    fn run_dir(&self) -> Result<std::path::PathBuf, String> {
        Ok(self.run.clone())
    }

    fn partitions_stamp(&self) -> Result<spira_config::runes::PartitionsStamp, String> {
        Ok(Default::default())
    }

    fn partitions(&self, _: &crate::control::claimable::Config) -> Result<Vec<spira_config::runes::Partition>, String> {
        Ok(Vec::new())
    }
}

impl Rig {
    fn claimable(&self, run: &std::path::Path, extra: &[&str]) -> Value {
        self.claimable_with(Fixed { run: run.into(), rowless: Vec::new(), fail: false, items_fail: false }, extra)
    }

    fn claimable_with(&self, inputs: Fixed, extra: &[&str]) -> Value {
        let call = Call { now: self.now(), request: "t/claimable", inputs: &crate::control::claimable::FIXED };
        let mut args = vec!["claimable".to_string()];
        args.extend(extra.iter().map(|s| s.to_string()));
        let (code, out) = crate::control::claimable::run(&self.store, &args, &inputs, &call);
        assert_eq!(code, 0, "{out}");
        serde_json::from_str(&out).unwrap()
    }

    fn inputs(&self, id: &str, blocks: &[&str]) {
        use state_store::Request;
        let inputs = crate::control::records::ClaimInputs {
            blocks: blocks.iter().map(|b| b.to_string()).collect(),
            ..Default::default()
        };
        self.n.set(self.n.get() + 1);
        let request = format!("t/inputs/{}", self.n.get());
        self.store.set_claim_inputs(id, inputs, &Request::new(&request, "test", "inputs")).unwrap();
    }
}

fn verdicts(answer: &Value) -> Vec<(String, String)> {
    answer["beads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["bead_id"].as_str().unwrap().into(), b["verdict"].as_str().unwrap().into()))
        .collect()
}

fn three_ready(r: &Rig) {
    for id in ["sp-a", "sp-b", "sp-c", "sp-w"] {
        r.ok(&["create-bead", id, "--ready", "--repo", "spira"]);
    }
    assert_eq!(r.claim("sp-w").0, 0);
    r.inputs("sp-a", &[]);
    r.inputs("sp-b", &["sp-w"]);
}

#[test_id(trgb0f0)]
#[test]
fn claimable_carries_each_candidates_content_fields_the_ranking_reads() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-content");
    three_ready(&r);
    let answer = r.claimable(run.path(), &[]);
    let beads = answer["beads"].as_array().unwrap();
    assert_eq!(beads.len(), 3);
    for b in beads {
        let id = b["bead_id"].as_str().unwrap();
        let c = &b["content"];
        assert_eq!(c["id"], id);
        assert_eq!(c["title"], format!("title of {id}"));
        for field in ["priority", "issue_type", "status", "created_at", "defer_until", "dependencies"] {
            assert!(!c[field].is_null(), "{id} lacks content field {field}: {c}");
        }
    }
}

#[test_id(t6p7o3r)]
#[test]
fn an_unreadable_content_store_leaves_the_judgement_standing_and_says_why_in_the_reply() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-content-fail");
    three_ready(&r);
    let answer = r.claimable_with(Fixed { run: run.path().into(), rowless: Vec::new(), fail: false, items_fail: true }, &[]);
    assert_eq!(answer["content_error"], "the content store is unreadable");
    assert!(answer["beads"].as_array().unwrap().iter().all(|b| b["content"].is_null()));
    assert_eq!(verdicts(&answer)[0], ("sp-a".to_string(), "claimable".to_string()));
}

#[test_id(tcnqfzy)]
#[test]
fn claimable_answers_every_ready_record_with_its_verdict_and_counts_the_ones_it_cannot_tell() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-verb");
    three_ready(&r);
    let answer = r.claimable(run.path(), &[]);
    assert_eq!(
        verdicts(&answer),
        [("sp-a", "claimable"), ("sp-b", "blocked"), ("sp-c", "cannot-tell")].map(|(a, b)| (a.to_string(), b.to_string()))
    );
    assert_eq!(answer["cannot_tell"], 1);
    assert_eq!(answer["rev"], r.store.contract().head().unwrap());
    assert_eq!(answer["beads"][2]["missing"], json!(["claim"]));
    assert_eq!(answer["beads"][1]["blocker"], "sp-w");
}

#[test_id(tzy1qf3)]
#[test]
fn claimable_names_the_gate_of_each_blocker_it_read() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-gates");
    three_ready(&r);
    let answer = r.claimable(run.path(), &[]);
    let expected = json!({ "sp-w": gate(BeadState::Working, None, false) });
    assert_eq!(answer["beads"][1]["gates"], expected);
}

#[test_id(tv8hx00)]
#[test]
fn claimable_writes_nothing() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-readonly");
    three_ready(&r);
    let before = r.store.contract().head().unwrap();
    r.claimable(run.path(), &[]);
    assert_eq!(r.store.contract().head().unwrap(), before);
}

#[test_id(t8w5p9o)]
#[test]
fn a_record_moved_to_working_between_two_calls_leaves_the_second_answer_with_no_pass_between() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-fresh");
    three_ready(&r);
    assert!(verdicts(&r.claimable(run.path(), &[])).iter().any(|(id, _)| id == "sp-a"));
    assert_eq!(r.claim("sp-a").0, 0);
    assert!(!verdicts(&r.claimable(run.path(), &[])).iter().any(|(id, _)| id == "sp-a"));
}

#[test_id(tmewqqz)]
#[test]
fn claimable_narrowed_to_a_repo_lists_only_that_repos_records() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-repo");
    three_ready(&r);
    r.ok(&["create-bead", "sp-o", "--ready", "--repo", "other"]);
    let ids: Vec<String> = verdicts(&r.claimable(run.path(), &["--repo", "other"])).into_iter().map(|(i, _)| i).collect();
    assert_eq!(ids, ["sp-o"]);
}

#[test_id(tn91dob)]
#[test]
fn claimable_reads_the_run_dir_reserved_ids_and_repo_stops() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-run");
    three_ready(&r);
    r.ok(&["create-bead", "sp-s", "--ready", "--repo", "stopped"]);
    r.inputs("sp-s", &[]);
    let stop = spira_config::scoped_stop::RepoStop {
        repo: "stopped".into(),
        lanes: vec!["claims".into()],
        owner: "test".into(),
        reason: "fixture".into(),
        until: "never".into(),
        since: 1,
    };
    spira_config::scoped_stop::put_repo(run.path(), &stop).unwrap();
    let answer = r.claimable(run.path(), &[]);
    assert!(verdicts(&answer).contains(&("sp-s".to_string(), "stopped".to_string())), "{answer}");
}

#[test_id(tyepm59)]
#[test]
fn claimable_judges_a_rowless_gate_by_the_content_stores_status() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-gate");
    three_ready(&r);
    r.inputs("sp-a", &["sp-gate"]);
    let with = |status: &str| Fixed {
        run: run.path().into(),
        rowless: vec![("sp-gate".into(), "gate".into(), status.into())],
        fail: false,
        items_fail: false,
    };
    let verdict_of = |a: &Value| a["beads"][0]["verdict"].clone();
    assert_eq!(verdict_of(&r.claimable_with(with("open"), &[])), "blocked");
    assert_eq!(verdict_of(&r.claimable_with(with("closed"), &[])), "claimable");
}

#[test_id(tzzwwo0)]
#[test]
fn claimable_reads_a_carrier_by_key_and_a_landed_one_releases_its_member() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-carried");
    three_ready(&r);
    r.certified("sp-int", TIP);
    r.store
        .set_carrier("sp-a", Some("sp-int"), false, &state_store::Request::new("t/carry", "test", "carry"))
        .unwrap();
    let answer = r.claimable(run.path(), &[]);
    assert_eq!(answer["beads"][0]["verdict"], "carried");
    assert_eq!(answer["beads"][0]["blocker"], "sp-int");
}

#[test_id(tfy6lqh)]
#[test]
fn claimable_that_cannot_read_its_config_answers_cannot_tell_with_no_list() {
    let r = Rig::new();
    let run = testkit::TempDir::new("claimable-fail");
    three_ready(&r);
    let call = Call { now: r.now(), request: "t/claimable", inputs: &crate::control::claimable::FIXED };
    let inputs = Fixed { run: run.path().into(), rowless: Vec::new(), fail: true, items_fail: false };
    let (code, out) = crate::control::claimable::run(&r.store, &["claimable".to_string()], &inputs, &call);
    assert_eq!(code, crate::CANNOT_TELL);
    assert!(out.starts_with("cannot tell: claimable: ") && !out.contains("beads"), "{out}");
}
