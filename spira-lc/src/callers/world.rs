//! Both planes in memory: the control store on `MemBackend` with a virtual clock answering the
//! state verbs exactly as lc-serve does, and a planning plane that answers `plan items|children`
//! and the `Bd` port, recording every bd status write it is handed.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use runtime::ports::VirtualClock;
use serde_json::{json, Value};
use state_store::{Config, Contract, MemBackend, Ports};

use super::{Bd, Machine};
use crate::control::state::{self, Call};
use crate::control::store::Store;

pub const AT: i64 = 1_767_225_600;

#[derive(Clone)]
pub struct Item {
    pub kind: String,
    pub status: String,
    pub parent: Option<String>,
    pub labels: Vec<String>,
}

pub struct World {
    pub store: Store<MemBackend>,
    n: Cell<u64>,
    pub plan: RefCell<BTreeMap<String, Item>>,
    /// Every bd status write, as `close <id>` or `reopen <id>`.
    pub writes: RefCell<Vec<String>>,
}

impl World {
    pub fn new() -> Self {
        let c = Contract::open(
            MemBackend::new(),
            Ports::virtual_clock(&VirtualClock::new(AT as u64 * 1_000)),
            Config { min_lock_ttl_ms: 1_000 },
        )
        .unwrap();
        World { store: Store::new(c, 1_000), n: Cell::new(0), plan: RefCell::new(BTreeMap::new()), writes: RefCell::new(Vec::new()) }
    }

    /// A bead in the planning plane, open.
    pub fn file(&self, id: &str, kind: &str, parent: Option<&str>, labels: &[&str]) {
        let item = Item { kind: kind.into(), status: "open".into(), parent: parent.map(str::to_string), labels: labels.iter().map(|l| l.to_string()).collect() };
        self.plan.borrow_mut().insert(id.into(), item);
    }

    pub fn status(&self, id: &str) -> String {
        self.plan.borrow()[id].status.clone()
    }

    pub fn lc(&self, argv: &[&str]) -> (i32, String) {
        let mut m = self;
        m.call(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    pub fn ok(&self, argv: &[&str]) -> String {
        let (rc, out) = self.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        out
    }

    /// End `id`'s record as DROPPED, from whatever live state it is in.
    pub fn drop(&self, id: &str) {
        let shown: Value = serde_json::from_str(&self.ok(&["show", id])).unwrap();
        let (state, version) = (shown["bead"]["state"].as_str().unwrap().to_string(), shown["bead"]["version"].as_str().unwrap().to_string());
        let kind = serde_json::to_string(&lifecycle::bead::BeadEventKind::Drop { reason: lifecycle::reason::DropReason::Unwanted }).unwrap();
        self.ok(&["event", "bead", id, "--expect", &state, "--version", &version, "--actor", "test", "--kind", &kind]);
    }

    fn item_json(id: &str, i: &Item) -> Value {
        json!({"id": id, "title": format!("title of {id}"), "issue_type": i.kind, "status": i.status, "parent": i.parent, "labels": i.labels})
    }

    fn plan_answer(&self, args: &[String]) -> (i32, String) {
        let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
        let open = args.iter().any(|a| a == "--open");
        let ids: Option<Vec<String>> = flag("--ids").map(|s| s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect());
        let kind = flag("--type");
        let plan = self.plan.borrow();
        let keep = |id: &String, i: &Item| {
            (!open || i.status != "closed") && kind.as_deref().is_none_or(|k| i.kind == k) && {
                match args.get(1).map(String::as_str) {
                    Some("children") => ids.as_ref().is_some_and(|p| i.parent.as_ref().is_some_and(|x| p.contains(x))),
                    _ => ids.as_ref().is_none_or(|want| want.contains(id)),
                }
            }
        };
        let items: Vec<Value> = plan.iter().filter(|(id, i)| keep(id, i)).map(|(id, i)| Self::item_json(id, i)).collect();
        (0, json!({"watermark": 0, "items": items}).to_string())
    }
}

impl Machine for &World {
    fn call(&mut self, args: &[String]) -> (i32, String) {
        if args.first().map(String::as_str) == Some("plan") {
            return self.plan_answer(args);
        }
        // One world holds both planes, so they never skew: the repair sweeps' guard passes.
        if args.first().map(String::as_str) == Some(crate::skew::CHECK_VERB) {
            return (0, format!("{}: the planes agree", crate::skew::CHECK_VERB));
        }
        if args.first().map(String::as_str) == Some("create-bead") {
            let mut read = |ids: &[&str]| {
                let argv: Vec<String> = ["plan", "items", "--ids", &ids.join(",")].iter().map(|s| s.to_string()).collect();
                let (_, out) = self.plan_answer(&argv);
                Ok(serde_json::from_str::<Value>(&out).unwrap()["items"].as_array().cloned().unwrap_or_default())
            };
            if let Some(refused) = crate::cutover::refuse_no_record_type(&args[1..], &mut read) {
                return refused;
            }
        }
        self.n.set(self.n.get() + 1);
        let request = format!("world/{}", self.n.get());
        let call = Call { now: AT, request: &request };
        match args.first().map(String::as_str) {
            Some(v) if crate::control::verbs::VERBS.contains(&v) => crate::control::verbs::run(&self.store, args),
            _ => state::run(&self.store, args, &call),
        }
    }
}

impl Bd for &World {
    fn issue_type(&mut self, id: &str) -> Result<String, String> {
        self.plan.borrow().get(id).map(|i| i.kind.clone()).ok_or_else(|| format!("{id}: no such bead"))
    }
    fn close(&mut self, id: &str, _reason: &str) -> Result<(), String> {
        self.writes.borrow_mut().push(format!("close {id}"));
        self.plan.borrow_mut().get_mut(id).ok_or("no such bead")?.status = "closed".into();
        Ok(())
    }
    fn closed(&mut self, ids: &[String]) -> Result<Vec<(String, String)>, String> {
        let plan = self.plan.borrow();
        Ok(ids.iter().filter(|i| plan.get(*i).is_some_and(|x| x.status == "closed")).map(|i| (i.clone(), String::new())).collect())
    }
    fn known(&mut self, ids: &[String]) -> Result<Vec<String>, String> {
        let plan = self.plan.borrow();
        Ok(ids.iter().filter(|i| plan.contains_key(*i)).cloned().collect())
    }
    fn open_among(&mut self, ids: &[String]) -> Result<Vec<String>, String> {
        let plan = self.plan.borrow();
        Ok(ids.iter().filter(|i| plan.get(*i).is_some_and(|x| x.status != "closed")).cloned().collect())
    }
    fn reopen(&mut self, id: &str) -> Result<(), String> {
        self.writes.borrow_mut().push(format!("reopen {id}"));
        self.plan.borrow_mut().get_mut(id).ok_or("no such bead")?.status = "open".into();
        Ok(())
    }
    fn work_beads(&mut self, id: &str) -> Result<Vec<String>, String> {
        let plan = self.plan.borrow();
        Ok(plan.get(id).map(|i| i.labels.iter().filter_map(|l| l.strip_prefix("work-bead:").map(str::to_string)).collect()).unwrap_or_default())
    }
    fn external(&mut self, _id: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn blocking_dependents(&mut self, _id: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    fn planning_watermark(&mut self) -> Result<Option<u64>, String> {
        Ok(None)
    }
}
