//! The goodput pass's assembler over the section verbs: every document it builds comes from one
//! `section snapshot` against the control store, written to through the verbs a component uses.

use reconciler_engine::assemble::{assemble, Assembled, Derived, Snapshot, SnapshotSource};
use reconciler_engine::pass_record::{PublishLatency, PASS_KEY};
use spira_desired_state::section::Freshness;
use spira_desired_state::store::format_rfc3339;

use super::*;

const SEC: u64 = 1_000;

/// The production shape: one `section snapshot` call.
struct InMemory<'a, B: Backend>(&'a Rig<B>);

impl<B: Backend> SnapshotSource for InMemory<'_, B> {
    fn snapshot(&self) -> Result<Snapshot, String> {
        let (code, v) = self.0.call(&["section", "snapshot"], None);
        if code != 0 {
            return Err(v.to_string());
        }
        Snapshot::parse(&v)
    }
}

/// The shape a watch-and-replay assembler has: each document from its own read.
struct TwoCall<'a, B: Backend>(&'a Rig<B>);

impl<B: Backend> TwoCall<'_, B> {
    fn read(&self) -> Snapshot {
        let mut v = self.0.ok(&["section", "list"], None);
        v["intents"] = json!([]);
        v["pass"] = Value::Null;
        v["knobs"] = json!({});
        Snapshot::parse(&v).unwrap()
    }
}

impl<B: Backend> Rig<B> {
    fn assembled(&self) -> Assembled {
        assemble(&InMemory(self).snapshot().unwrap(), self.clock.now_ms())
    }

    fn write_pass(&self, n: u64) {
        let rec = json!({ "n": n, "list_rev": self.head(), "at": NOW, "holder": "goodput-pass", "mode": "shadow" });
        let t = Txn::new().put(PASS_KEY, Object::new(rec).with_ttl(600));
        assert!(self.store.txn(&t, &Request::new(&format!("pass/{n}"), "reconciler", "t")).unwrap().succeeded);
    }

    fn stamp(&self) -> String {
        format_rfc3339(self.clock.now_ms() / 1000)
    }
}

fn names<T>(m: &std::collections::BTreeMap<String, T>) -> Vec<String> {
    m.keys().cloned().collect()
}

fn rev(v: &Option<String>) -> u64 {
    v.as_deref().unwrap().parse().unwrap()
}

#[test]
fn both_documents_the_intents_and_the_pass_record_are_read_at_one_list_rev() {
    let r = rig_on(Interleave { inner: MemBackend::new(), fired: AtomicBool::new(true) });
    r.publish(&status("a", 0, NOW));
    let token = r.lock();
    r.ok(&["intent", "put", "intents/Probe/a/1", "--token", &token, "--actor", "reconciler"], Some(&intent("1", &token, json!(1), json!(2))));
    r.write_pass(4);

    let two = TwoCall(&r);
    let first = two.read();
    r.apply_spec(&spec("a", 4.0));
    let second = two.read();
    let current = assemble(&first, r.clock.now_ms()).current;
    let desired = assemble(&second, r.clock.now_ms()).desired;
    assert_ne!(current.list_rev, desired.list_rev, "two reads straddle the write");
    assert_ne!(
        current.sections["Probe/a"].metadata.resource_version,
        desired.sections["Probe/a"].metadata.resource_version,
        "so the two documents describe Probe/a at different revisions"
    );

    let before = r.head();
    r.store.backend().fired.store(false, Ordering::SeqCst);
    let a = r.assembled();
    assert!(r.head() > before, "a section and an intent were committed while the snapshot was listing");
    assert_eq!((a.list_rev, a.current.list_rev, a.desired.list_rev), (before, before, before));
    assert_eq!(names(&a.current.sections), ["Probe/a"], "the late section is past the snapshot");
    assert_eq!(names(&a.desired.sections), ["Probe/a"]);
    assert_eq!(a.current.sections["Probe/a"].metadata, a.desired.sections["Probe/a"].metadata);
    assert!(rev(&a.current.sections["Probe/a"].metadata.resource_version) <= before);
    assert_eq!(a.intents.len(), 1, "the late intent is past the snapshot");
    assert!(a.undecodable.is_empty(), "nothing late was listed at all: {:?}", a.undecodable);
    assert!(a.intents.iter().all(|i| i.mod_rev <= before));
    assert_eq!(a.prev.as_ref().map(|p| p.n), Some(4));
    assert_eq!(a.n, 5);
}

#[test]
fn a_section_that_stops_publishing_reads_fresh_until_max_age_then_stale_and_stays_in_both_documents() {
    let r = rig();
    r.publish(&status("a", 0, NOW));
    r.apply_spec(&spec("a", 4.0));
    for at in [0, 5 * MIN, 15 * MIN] {
        r.clock.advance_to(T0 + at);
        let a = r.assembled();
        assert_eq!(a.current.sections["Probe/a"].freshness, Freshness::Fresh, "at +{}s", at / SEC);
        assert_eq!(a.desired.sections["Probe/a"].freshness, Freshness::Fresh);
    }
    r.clock.advance(SEC);
    let a = r.assembled();
    let Freshness::Stale { age_secs, reason } = a.current.sections["Probe/a"].freshness.clone() else {
        panic!("Probe/a is still fresh past max_age");
    };
    assert_eq!(age_secs, Some(15 * 60 + 1));
    assert!(reason.contains("max_age"), "{reason}");
    let d = &a.desired.sections["Probe/a"];
    assert_eq!(d.freshness, a.current.sections["Probe/a"].freshness, "the desired document keeps it, with its reason");
    assert_eq!(d.spec["depth_max"].value, 4.0);
    let age = &a.ages["Probe/a"];
    assert_eq!((age.age_secs, &age.freshness), (Some(901), &d.freshness));
    assert_eq!(age.publish_latency, PublishLatency::Late { target_secs: 300, over_secs: 601 });
}

#[test]
fn a_spec_generation_ahead_of_observed_generation_reads_not_caught_up_and_fresh_at_once() {
    let r = rig();
    r.publish(&status("a", 0, NOW));
    r.apply_spec(&spec("a", 4.0));
    let a = r.assembled();
    assert_eq!((a.ages["Probe/a"].caught_up, &a.ages["Probe/a"].freshness), (false, &Freshness::Fresh));
    assert_eq!(a.desired.sections["Probe/a"].metadata.generation, Some(1));
    assert_eq!(a.current.sections["Probe/a"].status.observed_generation, 0);

    r.clock.advance(MIN);
    r.publish(&status("a", 1, &r.stamp()));
    assert!(r.assembled().ages["Probe/a"].caught_up, "once observed, it has caught up");
}

#[test]
fn an_unacked_intent_past_its_horizon_marks_its_target_section_until_it_is_acked() {
    let r = rig();
    r.publish(&status("a", 0, NOW));
    r.publish(&status("b", 0, NOW));
    let token = r.lock();
    let key = "intents/Probe/a/1";
    r.ok(&["intent", "put", key, "--token", &token, "--actor", "reconciler"], Some(&intent("1", &token, json!(1), json!(2))));
    let derived = |r: &Rig<MemBackend>, s: &str| r.assembled().current.sections[s].derived.clone();

    for at in [MIN, 10 * MIN] {
        r.clock.advance_to(T0 + at);
        assert_eq!(derived(&r, "Probe/a"), [], "inside its 10m horizon at +{}s", at / SEC);
    }
    r.clock.advance(SEC);
    assert_eq!(derived(&r, "Probe/a"), [Derived::IntentsUnacknowledged { n: 1 }]);
    assert_eq!(derived(&r, "Probe/b"), [], "only its target section is marked");

    r.ok(&["intent", "ack", key, "--actor", "probe", "--result", "applied"], None);
    assert_eq!(derived(&r, "Probe/a"), [], "an acked intent is judged, not counted");
}

#[test]
fn a_fresh_process_assembles_what_an_uninterrupted_pass_does_at_the_same_revision() {
    let backend = MemBackend::new();
    let r = rig_on(backend.clone());
    r.publish(&status("a", 0, NOW));
    r.publish(&status("b", 0, NOW));
    r.apply_spec(&spec("a", 4.0));
    let first = r.assembled();
    assert_eq!((first.n, first.prev.as_ref()), (1, None), "with no pass record the passes number from 1");

    let token = r.lock();
    r.ok(&["intent", "put", "intents/Probe/a/1", "--token", &token, "--actor", "reconciler"], Some(&intent("1", &token, json!(1), json!(2))));
    r.write_pass(7);
    r.clock.advance(11 * MIN);
    let uninterrupted = r.assembled();

    let restarted = rig_on(backend);
    restarted.clock.advance_to(r.clock.now_ms());
    let again = restarted.assembled();
    assert_eq!(again, uninterrupted);
    assert_eq!(again.n, 8);
    assert_eq!(again.current.sections["Probe/a"].derived, [Derived::IntentsUnacknowledged { n: 1 }]);
}
