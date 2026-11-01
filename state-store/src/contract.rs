//! `Contract<B>`: every verb of the state-store design, implemented once above any [`Backend`].

use std::sync::Arc;

use runtime::ports::{Clock, VirtualClock};
use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::{Item, Object, Result, Rev, LOCKS_PREFIX};

pub struct Ports {
    /// TTLs and locks run on this; `MonotonicClock` in a server, `VirtualClock` in a test.
    pub mono: Arc<dyn Clock>,
    /// Only stamps `deletionTimestamp`, which is for display.
    pub wall: Arc<dyn Clock>,
}

impl Ports {
    pub fn virtual_clock(c: &VirtualClock) -> Self {
        Ports {
            mono: Arc::new(c.clone()),
            wall: Arc::new(c.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// A lock asked for with a shorter TTL is refused.
    pub min_lock_ttl_ms: u64,
}

/// Every mutating call carries one: its id makes a retry idempotent, and the rest is the audit
/// record written to `events/<main>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub actor: String,
    pub kind: String,
    #[serde(default)]
    pub evidence: serde_json::Value,
}

impl Request {
    pub fn new(id: &str, actor: &str, kind: &str) -> Self {
        Request {
            id: id.into(),
            actor: actor.into(),
            kind: kind.into(),
            evidence: serde_json::Value::Null,
        }
    }

    pub fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(0, |v| v.len())
    }
}

/// Equality compares; `None` means the key is absent.
#[derive(Clone, Debug, PartialEq)]
pub enum Compare {
    ModRev(String, Option<Rev>),
    CreateRev(String, Option<Rev>),
    Value(String, Option<Object>),
}

impl Compare {
    pub fn key(&self) -> &str {
        match self {
            Compare::ModRev(k, _) | Compare::CreateRev(k, _) | Compare::Value(k, _) => k,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Put(String, Object),
    /// Marks the key for deletion instead while it has finalizers.
    Delete(String),
}

impl Op {
    pub fn key(&self) -> &str {
        match self {
            Op::Put(k, _) | Op::Delete(k) => k,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Txn {
    pub compares: Vec<Compare>,
    pub ops: Vec<Op>,
    pub else_ops: Vec<Op>,
}

impl Txn {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn when(mut self, c: Compare) -> Self {
        self.compares.push(c);
        self
    }

    pub fn put(mut self, key: &str, v: Object) -> Self {
        self.ops.push(Op::Put(key.into(), v));
        self
    }

    pub fn delete(mut self, key: &str) -> Self {
        self.ops.push(Op::Delete(key.into()));
        self
    }

    pub fn or_put(mut self, key: &str, v: Object) -> Self {
        self.else_ops.push(Op::Put(key.into(), v));
        self
    }

    pub fn fenced(self, f: &Fence) -> Self {
        self.when(f.compare())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxnResult {
    /// The txn's main revision; for a refusal that stored nothing, the head it was judged at.
    pub rev: u64,
    pub succeeded: bool,
    /// A retry of a request already committed: `rev` and `succeeded` are the original's.
    pub already_applied: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub mod_rev: Rev,
    /// The key already held an equal value; nothing was written.
    pub existed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deleted {
    Removed {
        rev: u64,
    },
    /// Finalizers remain: `deletionTimestamp` is set and the GC controller removes the key once
    /// they are cleared.
    Pending {
        rev: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Selector {
    Everything,
    /// The value at this JSON pointer into `data` equals this.
    Eq(String, serde_json::Value),
}

/// Where the next page of a list resumes: the same snapshot, the same prefix, after this key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub list_rev: u64,
    pub prefix: usize,
    pub after: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListPage {
    pub items: Vec<Item>,
    pub list_rev: u64,
    pub next: Option<Cursor>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub key: String,
    pub rev: Rev,
    /// `None` for a delete.
    pub value: Option<Object>,
    pub create_rev: Option<Rev>,
    pub prev_mod_rev: Option<Rev>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Watched {
    /// Every event of each txn after `from_rev` under the prefix; resume from `through`.
    Events { events: Vec<Event>, through: u64 },
    /// Nothing under the prefix changed up to `progress_rev` before the wait elapsed.
    Progress { progress_rev: u64 },
}

/// A lock holder's proof: `create_rev(locks/<resource>) == token`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fence {
    pub resource: String,
    pub token: Rev,
}

impl Fence {
    pub fn key(&self) -> String {
        format!("{LOCKS_PREFIX}{}", self.resource)
    }

    pub fn compare(&self) -> Compare {
        Compare::CreateRev(self.key(), Some(self.token))
    }
}

pub struct Contract<B: Backend> {
    backend: B,
    ports: Ports,
    cfg: Config,
}

impl<B: Backend> Contract<B> {
    pub fn open(backend: B, ports: Ports, cfg: Config) -> Result<Self> {
        let _ = (&backend, &ports, cfg);
        todo!()
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn now_ms(&self) -> u64 {
        self.ports.mono.now_ms()
    }

    pub fn head(&self) -> Result<u64> {
        todo!()
    }

    pub fn get(&self, _key: &str) -> Result<Option<Item>> {
        todo!()
    }

    pub fn get_at(&self, _key: &str, _at: u64) -> Result<Option<Item>> {
        todo!()
    }

    pub fn list(&self, _prefixes: &[&str], _sel: &Selector) -> Result<(Vec<Item>, u64)> {
        todo!()
    }

    pub fn list_page(
        &self,
        _prefixes: &[&str],
        _sel: &Selector,
        _limit: usize,
        _cursor: Option<&Cursor>,
    ) -> Result<ListPage> {
        todo!()
    }

    pub fn create(&self, _key: &str, _value: Object, _req: &Request) -> Result<Created> {
        todo!()
    }

    pub fn txn(&self, _t: &Txn, _req: &Request) -> Result<TxnResult> {
        todo!()
    }

    pub fn delete(&self, _key: &str, _expect: Option<Rev>, _req: &Request) -> Result<Deleted> {
        todo!()
    }

    pub fn watch(&self, _prefix: &str, _from_rev: u64, _wait_ms: u64) -> Result<Watched> {
        todo!()
    }

    pub fn lock(&self, _resource: &str, _ttl_ms: u64, _req: &Request) -> Result<Fence> {
        let _ = self.cfg;
        todo!()
    }

    pub fn renew(&self, _f: &Fence, _req: &Request) -> Result<()> {
        todo!()
    }

    pub fn release(&self, _f: &Fence, _req: &Request) -> Result<()> {
        todo!()
    }

    pub fn compact(&self, _rev: u64) -> Result<()> {
        todo!()
    }

    pub fn wake_watchers(&self) {
        todo!()
    }
}
