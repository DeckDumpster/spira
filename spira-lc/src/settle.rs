//! The follow-on writes a content write owes inside lc-serve: the claim inputs of the beads it
//! changed and the planning watermark. They are not the request's: its 3 s deadline may have
//! passed, and a store write begun past it is refused unstarted. A settler thread writes them
//! each under a deadline of its own and keeps a refused one owed until it is written.

use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::callers::{Answer, Bd, Machine, NO_ROW};
use crate::claim_sync::{self, ONLY_IF_UNSET};

const RETRY_AFTER: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Owed {
    claims: BTreeSet<String>,
    only_if_unset: BTreeSet<String>,
    stamp: bool,
}

impl Owed {
    fn is_empty(&self) -> bool {
        self.claims.is_empty() && self.only_if_unset.is_empty() && !self.stamp
    }
}

#[derive(Default)]
pub struct Settler {
    owed: Mutex<Owed>,
    wake: Condvar,
}

static SETTLER: OnceLock<Arc<Settler>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static TEST: std::cell::RefCell<Option<Arc<Settler>>> = const { std::cell::RefCell::new(None) };
}

/// Installs the settler a serving process queues to; call before any request is served.
pub fn install() {
    let _ = SETTLER.set(Arc::new(Settler::default()));
}

fn current() -> Option<Arc<Settler>> {
    #[cfg(test)]
    if let Some(s) = TEST.with(|t| t.borrow().clone()) {
        return Some(s);
    }
    SETTLER.get().cloned()
}

/// Installs a settler for this thread only, so a test sees lc-serve's owed closes while the rest close inline.
#[cfg(test)]
pub(crate) fn install_for_test() -> Arc<Settler> {
    let s = Arc::new(Settler::default());
    TEST.with(|t| *t.borrow_mut() = Some(Arc::clone(&s)));
    s
}

pub fn has_settler() -> bool {
    current().is_some()
}

/// Writes the claim inputs of `ids` now, under the caller's deadline. Inside lc-serve a write
/// the store refused is owed to the settler and not reported; outside it the inline answer stands.
pub fn sync_claims(ids: &[String], only_if_unset: bool, m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let flag = || only_if_unset.then(|| ONLY_IF_UNSET.to_string());
    let Some(s) = current() else {
        return claim_sync::run(&flag().into_iter().chain(ids.iter().cloned()).collect::<Vec<_>>(), m, bd);
    };
    let mut refused = Vec::new();
    for id in ids {
        let args: Vec<String> = flag().into_iter().chain([id.clone()]).collect();
        let ans = claim_sync::run(&args, m, bd);
        if ans.code != 0 && ans.code != NO_ROW {
            refused.push(id.clone());
        }
    }
    if !refused.is_empty() {
        let mut g = s.owed.lock().unwrap_or_else(|e| e.into_inner());
        let into = if only_if_unset { &mut g.only_if_unset } else { &mut g.claims };
        into.extend(refused);
        s.wake.notify_one();
    }
    Answer::default()
}

/// Queues one watermark advance; `false` when no settler is installed.
pub fn owe_stamp() -> bool {
    let Some(s) = current() else { return false };
    s.owed.lock().unwrap_or_else(|e| e.into_inner()).stamp = true;
    s.wake.notify_one();
    true
}

/// Owed writes after the reply are not request work; they keep the receiver's 3 s bound.
const SETTLE_DEADLINE: Duration = Duration::from_secs(3);

fn fresh_deadline() -> state_store::phases::DeadlineScope {
    state_store::phases::deadline_scoped(Instant::now() + SETTLE_DEADLINE)
}

fn count_refused(what: &str) {
    use spira_otel::metrics::{MetricsSink, Point};
    let Ok(endpoint) = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT") else { return };
    MetricsSink::new(Some(&endpoint), "spira-lc", Duration::from_millis(250)).push(&[Point::Count { name: "spira_lc_settle_refused", value: 1 }], &[("write", what)]);
}

impl Settler {
    /// One pass: writes what is owed, each write under its own deadline, and keeps a refused
    /// one owed. A bead with no record is settled: there is nothing to write onto.
    pub fn pass(&self, m: &mut dyn Machine, bd: &mut dyn Bd, stamp: &mut dyn FnMut() -> Result<u64, String>) {
        let taken = std::mem::take(&mut *self.owed.lock().unwrap_or_else(|e| e.into_inner()));
        let mut back = Owed::default();
        for (ids, only_if_unset) in [(taken.claims, false), (taken.only_if_unset, true)] {
            for id in ids {
                let mut args = vec![id.clone()];
                if only_if_unset {
                    args.insert(0, ONLY_IF_UNSET.to_string());
                }
                let _deadline = fresh_deadline();
                let ans = claim_sync::run(&args, m, bd);
                if ans.code != 0 && ans.code != NO_ROW {
                    count_refused("claim-inputs");
                    if only_if_unset { &mut back.only_if_unset } else { &mut back.claims }.insert(id);
                }
            }
        }
        if taken.stamp {
            let _deadline = fresh_deadline();
            if stamp().is_err() {
                count_refused("watermark");
                back.stamp = true;
            }
        }
        let mut g = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        g.claims.extend(back.claims);
        g.only_if_unset.extend(back.only_if_unset);
        g.stamp |= back.stamp;
    }

    fn wait_owed(&self) {
        let mut g = self.owed.lock().unwrap_or_else(|e| e.into_inner());
        while g.is_empty() {
            g = self.wake.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn owes(&self) -> bool {
        !self.owed.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }
}

/// The settler thread lc-serve runs.
pub fn run() {
    let Some(s) = SETTLER.get().cloned() else { return };
    loop {
        s.wait_owed();
        s.pass(&mut crate::bd::StoreMachine, &mut crate::bd::LiveBd, &mut crate::plan::stamp_served);
        if s.owes() {
            std::thread::sleep(RETRY_AFTER);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callers::world::World;

    /// Installs a settler for this thread only, so a test queues while the rest write inline.
    fn installed() -> Arc<Settler> {
        install_for_test()
    }

    /// The world, whose request deadline passes once the write it served has landed.
    struct Expires<'w>(&'w World, Vec<state_store::phases::DeadlineScope>);

    impl Machine for Expires<'_> {
        fn call(&mut self, args: &[String]) -> (i32, String) {
            let mut w = self.0;
            let done = w.call(args);
            self.1.push(state_store::phases::deadline_scoped(Instant::now()));
            done
        }
    }

    struct NoContent;

    impl crate::composed::Content for NoContent {
        fn read(&self, _: &[String]) -> Result<String, crate::composed::ReadError> {
            Ok(String::new())
        }
        fn write(&self, _: &[&str]) -> Result<String, String> {
            Ok(String::new())
        }
        fn write_stdin(&self, _: &[String], _: Option<&str>) -> Result<String, String> {
            Ok(String::new())
        }
        fn delete(&self, _: &[String]) -> Result<(), String> {
            Ok(())
        }
        fn ask_label(&self) -> Result<String, String> {
            Ok("ask".into())
        }
        fn registry(&self) -> spira_config::repos::Registry {
            Default::default()
        }
    }

    fn claim_of(w: &World, id: &str) -> serde_json::Value {
        let rows: Vec<serde_json::Value> = serde_json::from_str(&w.ok(&["list", "--live"])).unwrap();
        rows.into_iter().find(|r| r["bead_id"] == id).unwrap()["claim"].clone()
    }

    #[test]
    fn a_claim_sync_owed_past_the_requests_deadline_is_written_by_the_settler_under_its_own() {
        let w = World::new();
        w.file("sp-new", "task", Some("sp-e"), &["incident"]);
        let settler = installed();
        let req = crate::serve::Request { argv: ["create-bead", "sp-new", "--repo", "spira", "--ready"].map(String::from).to_vec(), stdin: None, caller: Default::default(), task: None, request_id: None };
        let (mut m, mut b) = (Expires(&w, Vec::new()), &w);
        let ans = crate::serve::answer(req, &mut m, &mut b, &NoContent, &|| Err("no run dir here".to_string()));
        assert_eq!((ans.code, ans.stderr.as_str()), (0, ""), "{ans:?}");
        assert_eq!(claim_of(&w, "sp-new"), serde_json::Value::Null, "owed, not yet written");
        settler.pass(&mut &w, &mut &w, &mut || Ok(1));
        assert_eq!(claim_of(&w, "sp-new"), serde_json::json!({"blocks": [], "parent": "sp-e", "labels": ["incident"]}));
        assert!(!settler.owes());
    }

    #[test]
    fn a_content_writes_claim_sync_owed_past_the_requests_deadline_is_written_by_the_settler() {
        let w = World::new();
        w.file("sp-a", "task", Some("sp-e"), &["x"]);
        w.ok(&["create-bead", "sp-a", "--ready"]);
        let settler = installed();
        let args = ["label", "add", "sp-a", "x"].map(String::from).to_vec();
        let (mut m, mut b) = (Expires(&w, Vec::new()), &w);
        m.call(&["list".to_string()]);
        let report = crate::content::after(&args, "", &mut m, &mut b, 0);
        assert_eq!(report, "");
        assert_eq!(claim_of(&w, "sp-a"), serde_json::Value::Null);
        settler.pass(&mut &w, &mut &w, &mut || Ok(1));
        assert_eq!(claim_of(&w, "sp-a")["labels"], serde_json::json!(["x"]));
    }

    #[test]
    fn a_dep_add_through_the_content_door_has_written_the_claim_inputs_when_it_answers() {
        let w = World::new();
        w.file("sp-a", "task", Some("sp-e"), &[]);
        w.file("sp-b", "task", Some("sp-e"), &[]);
        w.ok(&["create-bead", "sp-a", "--ready"]);
        w.ok(&["create-bead", "sp-b", "--ready"]);
        w.plan.borrow_mut().get_mut("sp-a").unwrap().dependencies.push(("sp-b".to_string(), "blocks".to_string()));
        let settler = installed();
        crate::content::after(&["dep", "add", "sp-a", "sp-b"].map(String::from), "", &mut &w, &mut &w, 0);
        assert_eq!(claim_of(&w, "sp-a")["blocks"], serde_json::json!(["sp-b"]));
        assert!(!settler.owes());
    }

    #[test]
    fn a_create_bead_has_written_its_claim_inputs_when_it_answers() {
        let w = World::new();
        w.file("sp-new", "task", Some("sp-e"), &["incident"]);
        let settler = installed();
        let req = crate::serve::Request { argv: ["create-bead", "sp-new", "--repo", "spira", "--ready"].map(String::from).to_vec(), stdin: None, caller: Default::default(), task: None, request_id: None };
        let ans = crate::serve::answer(req, &mut &w, &mut &w, &NoContent, &|| Err("no run dir here".to_string()));
        assert_eq!((ans.code, ans.stderr.as_str()), (0, ""), "{ans:?}");
        assert_eq!(claim_of(&w, "sp-new")["parent"], "sp-e");
        assert!(!settler.owes());
    }

    #[test]
    fn a_refused_claim_sync_stays_owed_and_the_next_pass_writes_it() {
        let w = World::new();
        w.file("sp-a", "task", Some("sp-e"), &[]);
        w.ok(&["create-bead", "sp-a", "--ready"]);
        let settler = installed();
        struct Refuses<'w>(&'w World, bool);
        impl Machine for Refuses<'_> {
            fn call(&mut self, args: &[String]) -> (i32, String) {
                if std::mem::take(&mut self.1) {
                    return (crate::callers::CANNOT_TELL, "cannot tell: Deadline".into());
                }
                let mut w = self.0;
                w.call(args)
            }
        }
        let mut m = Refuses(&w, true);
        let ans = sync_claims(&["sp-a".to_string()], false, &mut m, &mut &w);
        assert_eq!((ans.code, ans.stderr.as_str()), (0, ""), "owed, not lost");
        assert!(settler.owes() && claim_of(&w, "sp-a") == serde_json::Value::Null);
        settler.pass(&mut m, &mut &w, &mut || Ok(1));
        settler.pass(&mut m, &mut &w, &mut || Ok(1));
        assert!(!settler.owes());
        assert_eq!(claim_of(&w, "sp-a")["parent"], "sp-e");
    }

    #[test]
    fn stamps_owed_together_advance_the_watermark_once_and_a_refused_one_stays_owed() {
        let settler = installed();
        for _ in 0..3 {
            assert!(owe_stamp());
        }
        let mut stamps = 0;
        let mut fail = true;
        let mut stamp = || {
            stamps += 1;
            if std::mem::take(&mut fail) { Err("cannot tell: plan stamp: Deadline".to_string()) } else { Ok(stamps) }
        };
        let w = World::new();
        settler.pass(&mut &w, &mut &w, &mut stamp);
        assert!(settler.owes());
        settler.pass(&mut &w, &mut &w, &mut stamp);
        assert!(!settler.owes());
        assert_eq!(stamps, 2);
    }

    #[test]
    fn a_watermark_owed_past_the_requests_deadline_advances_the_control_stores_seq_in_the_settler() {
        use state_store::{Config, Contract, MemBackend, Object, Ports};
        let clock = runtime::ports::VirtualClock::new(1_767_225_600_000);
        let store = crate::control::store::Store::new(Contract::open(MemBackend::default(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap(), 1_000);
        let key = crate::control::keys::PLANNING_WATERMARK;
        let req = state_store::Request::new("seed", "t", "seed");
        store.contract().txn(&state_store::Txn::new().put(key, Object::new(serde_json::json!({ "seq": 7 }))), &req).unwrap();
        let seq = || store.contract().get(key).unwrap().unwrap().value.data["seq"].as_u64().unwrap();
        let settler = installed();
        let expired = state_store::phases::deadline_scoped(Instant::now());
        crate::plan::stamp_after_write();
        assert!(crate::plan::stamp_control(&store).is_err(), "inline, the stamp is refused past the deadline");
        drop(expired);
        assert_eq!(seq(), 7);
        let w = World::new();
        settler.pass(&mut &w, &mut &w, &mut || crate::plan::stamp_control(&store));
        assert_eq!(seq(), 8);
    }
}
