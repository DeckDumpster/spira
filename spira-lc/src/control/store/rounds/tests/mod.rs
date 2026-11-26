use std::cell::Cell;

use lifecycle::batch::{BatchEvent, BatchEventKind, BatchState};

use lifecycle::bead::BeadState;

use lifecycle::delivery::DeliveryState;

use runtime::ports::VirtualClock;

use state_store::{Backend, Config, Contract, MemBackend, Ports, Request};

use state_store_sqlite::SqliteBackend;

use super::{batch_key, Cascade, Cut, Planned, Ran, Store, Turned, OPEN_BATCH_STATES};

use crate::control::store::counting::Counting;

use crate::control::keys;

use crate::control::records::Audit;

use crate::control::state::{self, Call};

mod abandon;
mod batches_of;
mod create_bead;
mod cut;
mod delivery_entered;
mod dwell;
mod eject;
mod feed;
mod landing;
mod list_asks;
mod list_batches;
mod list_claim_view;
mod list_live;
mod list_page;
mod queue_fields;
mod readiness;
mod requeue_orphans;
mod stack;

const AT: i64 = 1_767_225_600;

const T1: &str = "1111111111111111111111111111111111111111";

const T2: &str = "2222222222222222222222222222222222222222";

struct Rig<B: Backend = MemBackend> {
    s: Store<B>,
    n: Cell<u64>,
}

fn clock() -> VirtualClock {
    VirtualClock::new(AT as u64 * 1_000)
}

impl Rig {
    fn new() -> Self {
        Rig::on(MemBackend::new(), &clock())
    }
}

impl Rig<SqliteBackend> {
}

impl<B: Backend> Rig<B> {
    fn on(b: B, clock: &VirtualClock) -> Self {
        let c = Contract::open(
            b,
            Ports::virtual_clock(clock),
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

    fn with_refs(self, pairs: &[(&str, &str)]) -> Self {
        let held: std::collections::BTreeMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Rig { s: self.s.with_refs(std::sync::Arc::new(move |id: &str| Ok(held.get(id).cloned()))), n: self.n }
    }

    fn req(&self) -> Request {
        self.n.set(self.n.get() + 1);
        Request::new(&format!("r{}", self.n.get()), "test", "t")
    }

    fn lc(&self, argv: &[&str]) -> (i32, String) {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let req = self.req();
        state::run(
            &self.s,
            &args,
            &Call {
                now: AT,
                request: &req.id, inputs: &crate::control::claimable::FIXED,
            },
        )
    }

    fn ok(&self, argv: &[&str]) {
        let (rc, out) = self.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
    }

    fn certified(&self, id: &str, tip: &str) {
        self.certified_with(
            id,
            tip,
            r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#,
        );
    }

    fn certified_with(&self, id: &str, tip: &str, claim: &str) {
        self.submitted_with(id, tip, claim);
        let pass = format!(r#"{{"GatePass":{{"tip":"{tip}","gate_key":"k1"}}}}"#);
        self.bead_event(id, "SUBMITTED", "2", &pass);
    }

    fn submitted_with(&self, id: &str, tip: &str, claim: &str) {
        self.ok(&["create-bead", id, "--repo", "spira", "--ready"]);
        let submit = format!(r#"{{"Submit":{{"tip":"{tip}"}}}}"#);
        self.bead_event(id, "READY", "0", claim);
        self.bead_event(id, "WORKING", "1", &submit);
    }

    fn bead_event(&self, id: &str, expect: &str, version: &str, kind: &str) {
        self.ok(&[
            "event",
            "bead",
            id,
            "--expect",
            expect,
            "--version",
            version,
            "--actor",
            "test",
            "--kind",
            kind,
        ]);
    }

    fn cut_spec<'a>(&self, members: &'a [(String, String)]) -> Cut<'a> {
        Cut {
            batch_id: "b-1",
            repo: "spira",
            head: "H",
            base: "B",
            parent: None,
            members,
            actor: "test",
            at: AT,
        }
    }

    fn state(&self, id: &str) -> BeadState {
        self.s.record(id).unwrap().unwrap().value.row.state
    }

    fn audits_at(&self, rev: u64) -> (bool, Vec<Audit>) {
        let e = self
            .s
            .contract()
            .get(&state_store::events_key(rev))
            .unwrap()
            .unwrap();
        let refused = e.value.data["refused"].as_bool().unwrap();
        let branch = if refused { "else" } else { "then" };
        (
            refused,
            serde_json::from_value(e.value.data["evidence"][branch].clone()).unwrap(),
        )
    }
}

fn two() -> Vec<(String, String)> {
    vec![("sp-a".into(), T1.into()), ("sp-b".into(), T2.into())]
}

fn batch_event<B: Backend>(r: &Rig<B>, expect: BatchState, version: u64, kind: BatchEventKind) {
    let ev = BatchEvent {
        expect,
        version,
        kind,
        actor: "test".into(),
    };
    let ran = r.s.batch_event("b-1", &ev, AT, &r.req()).unwrap();
    assert!(
        matches!(&ran, Ran::Done { applied, .. } if applied == &[true]),
        "{ran:?}"
    );
}

fn green_round<B: Backend>(r: &Rig<B>) {
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    batch_event(
        r,
        BatchState::Open,
        2,
        BatchEventKind::CiStarted { run: "r1".into() },
    );
    batch_event(r, BatchState::CiRunning, 3, BatchEventKind::Green);
}

fn fast_forward(version: u64) -> BatchEvent {
    BatchEvent {
        expect: BatchState::Green,
        version,
        kind: BatchEventKind::FastForward { sha: "M1".into() },
        actor: "test".into(),
    }
}

const T3: &str = "3333333333333333333333333333333333333333";

const CAP: usize = state_store::limits::MAX_CUT_MEMBERS;

fn ids(n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|i| (format!("sp-m{i:02}"), T1.to_string()))
        .collect()
}

fn batch_ev(expect: BatchState, version: u64, kind: BatchEventKind) -> BatchEvent {
    BatchEvent {
        expect,
        version,
        kind,
        actor: "test".into(),
    }
}

fn round_state(r: &Rig) -> BatchState {
    r.s.batch("b-1").unwrap().unwrap().1.value.row.state
}

fn open_round<B: Backend>(r: &Rig<B>) {
    r.certified("sp-a", T1);
    let one = vec![("sp-a".to_string(), T1.to_string())];
    let p = r.s.plan_cut(&r.cut_spec(&one)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
}

fn claim_view_of(r: &Rig, id: &str) -> serde_json::Value {
    let (rc, out) = r.lc(&["list", "--ids", id]);
    assert_eq!(rc, 0, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    rows[0]["claim_view"].clone()
}

fn many_rounds<B: Backend>(r: &Rig<B>, from: usize, to: usize, open: &[usize]) {
    for i in from..to {
        r.certified(&format!("sp-{i:03}"), T1);
    }
    rounds_of_certified(r, from, to, open);
}

/// `sp-{from..to}` certified in a few writes, each a copy of one bead certified through the verbs.
fn cloned_certified<B: Backend>(r: &Rig<B>, from: usize, to: usize) {
    let template = format!("sp-tmpl-{from}");
    r.certified(&template, T1);
    let copied: Vec<(String, String)> = [keys::lifecycle(&template), format!("gates/{template}")]
        .into_iter()
        .map(|key| {
            let raw = String::from_utf8(r.s.contract().get(&key).unwrap().unwrap().value.encode()).unwrap();
            (key, raw)
        })
        .collect();
    let ids: Vec<usize> = (from..to).collect();
    for chunk in ids.chunks(50) {
        let mut t = state_store::Txn::new();
        for (key, raw) in &copied {
            for i in chunk {
                let id = format!("sp-{i:03}");
                t = t.put(&key.replace(&template, &id), state_store::Object::decode(raw.replace(&template, &id).as_bytes()).unwrap());
            }
        }
        assert!(r.s.contract().txn(&t, &r.req()).unwrap().succeeded);
    }
}

/// Rounds `b-{from..to}`, one per already-certified `sp-{i}`, cut `100 * i` seconds before `AT`, all but `open` ended.
fn rounds_of_certified<B: Backend>(r: &Rig<B>, from: usize, to: usize, open: &[usize]) {
    use lifecycle::batch::BatchState;
    for i in from..to {
        let (id, round) = (format!("sp-{i:03}"), format!("b-{i:03}"));
        let members = vec![(id, T1.to_string())];
        let cut = Cut { batch_id: &round, at: AT - 100 * i as i64, ..r.cut_spec(&members) };
        let p = r.s.plan_cut(&cut).unwrap();
        r.s.run_planned(p, &r.req(), "cut").unwrap();
        if !open.contains(&i) {
            let key = keys::batch("spira", &round);
            let (_, mut rec) = r.s.batch(&round).unwrap().unwrap();
            rec.value.row.state = BatchState::Landed;
            let put = state_store::Txn::new().put(&key, state_store::Object::new(crate::control::records::encode(&rec.value).unwrap()));
            r.s.contract().txn(&put, &r.req()).unwrap();
        }
    }
}

fn end_round<B: Backend>(r: &Rig<B>, round: &str, state: BatchState) {
    let (_, mut rec) = r.s.batch(round).unwrap().unwrap();
    rec.value.row.state = state;
    let put = state_store::Txn::new().put(&keys::batch("spira", round), state_store::Object::new(crate::control::records::encode(&rec.value).unwrap()));
    r.s.contract().txn(&put, &r.req()).unwrap();
}

fn ranges_of_rounds(reads: &[(&'static str, String, usize)]) -> usize {
    reads.iter().filter(|(k, p, _)| *k == "range" && p.starts_with(keys::BATCHES)).count()
}

fn set_round_fields(r: &Rig, fields: serde_json::Value) {
    let key = keys::batch("spira", "b-1");
    let item = r.s.contract().get(&key).unwrap().unwrap();
    let mut value = item.value.clone();
    value.data["round"] = fields;
    let t = state_store::Txn::new()
        .when(state_store::Compare::ModRev(key.clone(), Some(item.mod_rev)))
        .put(&key, value);
    assert!(r.s.contract().txn(&t, &r.req()).unwrap().succeeded);
}
