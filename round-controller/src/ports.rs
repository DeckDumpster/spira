//! The ports every effect goes through (law-new-subsystems-are-simulation-ready). A scenario
//! hands [`Ports`] fakes and a virtual clock; the binary hands it the real queue and wall clock.

use std::collections::BTreeMap;
use std::sync::Mutex;

use runtime::ports::{Clock, Proc, Rng};
use state_store::{Backend, Contract, Fence, Item, Request, Selector, Txn, TxnResult};

use crate::death::{Units, Vms};
use crate::liveness::LeaseSettings;
use crate::orphans::Launch;
use crate::outcome::Log;
use crate::slots::SlotSettings;

pub struct Ports<'a> {
    pub clock: &'a dyn Clock,
    pub proc_: &'a dyn Proc,
    pub rng: &'a Mutex<dyn Rng>,
    pub store: &'a dyn Store,
    pub verbs: &'a dyn RoundVerbs,
    pub log: &'a dyn Log,
    pub units: &'a dyn Units,
    pub vms: &'a dyn Vms,
    pub launcher: &'a dyn Launch,
    pub leases: &'a LeaseSettings,
    pub slots: &'a SlotSettings,
}

impl Ports<'_> {
    pub fn next_u64(&self) -> u64 {
        crate::lock(self.rng).next_u64()
    }
}

/// The state store's client surface: the contract's verbs the controller calls.
pub trait Store {
    fn get(&self, key: &str) -> state_store::Result<Option<Item>>;
    fn list(&self, prefixes: &[&str], sel: &Selector) -> state_store::Result<(Vec<Item>, u64)>;
    fn txn(&self, t: &Txn, req: &Request) -> state_store::Result<TxnResult>;
    fn lock(&self, resource: &str, ttl_ms: u64, req: &Request) -> state_store::Result<Fence>;
    fn renew(&self, f: &Fence, req: &Request) -> state_store::Result<()>;
    fn release(&self, f: &Fence, req: &Request) -> state_store::Result<()>;
}

impl<B: Backend> Store for Contract<B> {
    fn get(&self, key: &str) -> state_store::Result<Option<Item>> {
        Contract::get(self, key)
    }
    fn list(&self, prefixes: &[&str], sel: &Selector) -> state_store::Result<(Vec<Item>, u64)> {
        Contract::list(self, prefixes, sel)
    }
    fn txn(&self, t: &Txn, req: &Request) -> state_store::Result<TxnResult> {
        Contract::txn(self, t, req)
    }
    fn lock(&self, resource: &str, ttl_ms: u64, req: &Request) -> state_store::Result<Fence> {
        Contract::lock(self, resource, ttl_ms, req)
    }
    fn renew(&self, f: &Fence, req: &Request) -> state_store::Result<()> {
        Contract::renew(self, f, req)
    }
    fn release(&self, f: &Fence, req: &Request) -> state_store::Result<()> {
        Contract::release(self, f, req)
    }
}

/// The `queue round` verbs the controller drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verb {
    Open,
    Status,
    Certify,
    Eject,
    Land,
    Abandon,
    PassStart,
}

impl Verb {
    pub const ALL: [Verb; 7] = [
        Verb::Open,
        Verb::Status,
        Verb::Certify,
        Verb::Eject,
        Verb::Land,
        Verb::Abandon,
        Verb::PassStart,
    ];

    /// The verb's name on `queue round`'s command line.
    pub fn word(self) -> &'static str {
        match self {
            Verb::Open => "open",
            Verb::Status => "status",
            Verb::Certify => "certify",
            Verb::Eject => "eject",
            Verb::Land => "land",
            Verb::Abandon => "abandon",
            Verb::PassStart => "pass-start",
        }
    }

    pub fn from_word(w: &str) -> Option<Verb> {
        Verb::ALL.into_iter().find(|v| v.word() == w)
    }
}

/// A verb that did not do its act. `code` is `None` when the verb never ran to an exit (it could
/// not be spawned, or it passed its deadline); `text` is its stderr, whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerbRefusal {
    pub verb: Verb,
    pub code: Option<i32>,
    pub text: String,
}

pub type Fields = BTreeMap<String, String>;

/// `key=value` lines of a verb's stdout; every other line is prose and ignored.
pub fn fields(stdout: &str) -> Fields {
    stdout
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| {
            !k.is_empty()
                && k.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        })
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn need(verb: Verb, f: &Fields, key: &str) -> Result<String, VerbRefusal> {
    f.get(key)
        .filter(|v| !v.is_empty())
        .cloned()
        .ok_or_else(|| VerbRefusal {
            verb,
            code: Some(0),
            text: format!("queue round {} answered without {key}=", verb.word()),
        })
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Member {
    pub id: String,
    pub tip: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReq {
    pub repo: String,
    pub members: Vec<Member>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub batch: String,
    pub head: String,
    pub members: String,
}

impl Opened {
    pub fn from_fields(f: &Fields) -> Result<Self, VerbRefusal> {
        Ok(Opened {
            batch: need(Verb::Open, f, "batch")?,
            head: need(Verb::Open, f, "head")?,
            members: f.get("members").cloned().unwrap_or_default(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// The repo's non-terminal batches, in the order `round status` lists them.
    pub open: Vec<String>,
}

impl Status {
    pub fn from_fields(f: &Fields) -> Self {
        let open = f
            .get("open_batches")
            .map(|b| b.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        Status { open }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Certified {
    Green,
    Red { suites: Vec<String> },
}

impl Certified {
    /// A red names its suites on `red=`; without one the run certified green. A red naming no
    /// suite is no verdict at all.
    pub fn from_fields(f: &Fields) -> Result<Self, VerbRefusal> {
        let Some(r) = f.get("red") else {
            return Ok(Certified::Green);
        };
        let suites: Vec<String> = r
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if suites.is_empty() {
            return Err(VerbRefusal {
                verb: Verb::Certify,
                code: Some(0),
                text: "queue round certify answered red= naming no suite".into(),
            });
        }
        Ok(Certified::Red { suites })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EjectReq {
    pub repo: String,
    pub batch: String,
    pub id: String,
    pub reason: String,
    pub suites: Vec<String>,
    pub red: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ejected {
    pub head: String,
    pub members: String,
}

impl Ejected {
    pub fn from_fields(f: &Fields) -> Result<Self, VerbRefusal> {
        Ok(Ejected {
            head: need(Verb::Eject, f, "head")?,
            members: f.get("members").cloned().unwrap_or_default(),
        })
    }
}

/// The verbs whose only answer is that they did it: land, abandon, pass-start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    pub said: String,
}

/// A long verb running elsewhere: `None` until it has answered.
pub trait Pending<T> {
    fn poll(&self) -> Option<Result<T, VerbRefusal>>;
}

pub trait RoundVerbs {
    fn open(&self, r: &OpenReq) -> Result<Opened, VerbRefusal>;
    fn status(&self, repo: &str) -> Result<Status, VerbRefusal>;
    fn certify(&self, repo: &str, batch: &str) -> Result<Box<dyn Pending<Certified>>, VerbRefusal>;
    fn eject(&self, r: &EjectReq) -> Result<Ejected, VerbRefusal>;
    fn land(&self, repo: &str, batch: &str) -> Result<Done, VerbRefusal>;
    fn abandon(&self, repo: &str, batch: &str, reason: &str) -> Result<Done, VerbRefusal>;
    fn pass_start(&self, repo: &str, batch: &str) -> Result<Done, VerbRefusal>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contract_is_a_store_through_the_port() {
        use runtime::ports::VirtualClock;
        use state_store::{Config, MemBackend, Object};
        let clock = VirtualClock::new(0);
        let c = Contract::open(
            MemBackend::new(),
            state_store::Ports::virtual_clock(&clock),
            Config {
                min_lock_ttl_ms: 1000,
            },
        )
        .unwrap();
        let s: &dyn Store = &c;
        let req = |id: &str| Request::new(id, "t", "k");
        assert!(
            s.txn(
                &Txn::new().put("batches/r/b1", Object::new(serde_json::json!(1))),
                &req("w")
            )
            .unwrap()
            .succeeded
        );
        assert_eq!(
            s.get("batches/r/b1").unwrap().unwrap().value.data,
            serde_json::json!(1)
        );
        assert_eq!(
            s.list(&["batches/"], &Selector::Everything)
                .unwrap()
                .0
                .len(),
            1
        );
        let f = s.lock("round/r/b1", 5000, &req("l")).unwrap();
        s.renew(&f, &req("n")).unwrap();
        s.release(&f, &req("x")).unwrap();
        assert!(s.renew(&f, &req("n2")).is_err());
    }

    #[test]
    fn every_verb_round_trips_its_word_and_an_unknown_word_is_none() {
        for v in Verb::ALL {
            assert_eq!(Verb::from_word(v.word()), Some(v));
        }
        assert_eq!(Verb::from_word("preempt"), None);
    }

    #[test]
    fn fields_are_key_value_lines_and_prose_is_ignored() {
        let f = fields("queue.sh round eject: ejected a\nhead=h2\nmembers=b:1\nNot A=key\n=x\n");
        assert_eq!(f.len(), 2);
        assert_eq!((f["head"].as_str(), f["members"].as_str()), ("h2", "b:1"));
    }

    #[test]
    fn a_reply_missing_its_key_is_a_refusal_naming_the_key() {
        let e = Opened::from_fields(&fields("batch=b1\n")).unwrap_err();
        assert_eq!((e.verb, e.code), (Verb::Open, Some(0)));
        assert!(e.text.contains("head="), "{}", e.text);
        assert!(Ejected::from_fields(&fields("members=a\n")).is_err());
        let o = Opened::from_fields(&fields("batch=b1\nhead=h1\n")).unwrap();
        assert_eq!(
            (o.batch.as_str(), o.head.as_str(), o.members.as_str()),
            ("b1", "h1", "")
        );
    }

    #[test]
    fn status_lists_open_batches_and_none_is_empty() {
        assert_eq!(
            Status::from_fields(&fields("rounds=2\nopen_batches=b1 b2\n")).open,
            vec!["b1", "b2"]
        );
        assert!(Status::from_fields(&fields("round=none\n")).open.is_empty());
    }

    #[test]
    fn a_certify_with_red_suites_is_red_and_without_is_green() {
        assert_eq!(
            Certified::from_fields(&fields("red=a,b\n")),
            Ok(Certified::Red {
                suites: vec!["a".into(), "b".into()]
            })
        );
        assert_eq!(
            Certified::from_fields(&fields("verdict line\n")),
            Ok(Certified::Green)
        );
        let e = Certified::from_fields(&fields("red=,\n")).unwrap_err();
        assert!(e.text.contains("naming no suite"), "{}", e.text);
    }
}
