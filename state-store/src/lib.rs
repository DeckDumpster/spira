//! state-store — the control-plane contract. `Contract<B>` owns every semantic (revisions, txn,
//! history keys, watch, locks with fencing, finalizers and GC, request ids, etcd v3's limits,
//! the telemetry prefix and key TTLs) above a deliberately small [`Backend`], so a backend gets
//! them all by passing [`conformance::run_all`].

pub mod backend;
pub mod clock;
pub mod conformance;
pub mod contract;
pub mod gc;
pub mod limits;
pub mod mem;
pub mod model;
pub mod wire;

use serde::{Deserialize, Serialize};

pub use backend::{Backend, LogEntry, Version, Write};
pub use clock::MonotonicClock;
pub use contract::{
    carries_ttl, Compare, Config, Contract, Created, Deleted, Event, Fence, ListPage, Op, Ports,
    Request, Selector, Txn, TxnResult, Watched,
};
pub use mem::MemBackend;
pub use model::Modeled;

pub const EVENTS_PREFIX: &str = "events/";
pub const REQUESTS_PREFIX: &str = "requests/";
pub const LOCKS_PREFIX: &str = "locks/";
/// Writes under this prefix are regenerable telemetry: revisioned and watched, never audited.
pub const TELEMETRY_PREFIX: &str = "telemetry/";
pub const INTENTS_PREFIX: &str = "intents/";
/// Prefixes whose every put must carry `ttl_secs > 0`.
pub const TTL_PREFIXES: &[&str] = &[TELEMETRY_PREFIX, INTENTS_PREFIX];
pub const REQUEST_TTL_SECS: u64 = 24 * 60 * 60;
/// The longest a watch may wait, inside the socket protocol's 3 s deadline.
pub const MAX_WATCH_WAIT_MS: u64 = 2_500;

/// `main` is the txn's global revision; `sub` orders the keys one txn touches.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Rev {
    pub main: u64,
    pub sub: u32,
}

impl Rev {
    pub fn new(main: u64, sub: u32) -> Self {
        Rev { main, sub }
    }
}

/// The metadata envelope the contract reserves in every value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finalizers: Vec<String>,
    /// Keys of the owners; the key is collected once none of them exists.
    #[serde(
        default,
        rename = "ownerReferences",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub owner_references: Vec<String>,
    /// Wall milliseconds, for display only: nothing is ordered by it.
    #[serde(
        default,
        rename = "deletionTimestamp",
        skip_serializing_if = "Option::is_none"
    )]
    pub deletion_timestamp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Object {
    #[serde(default)]
    pub metadata: Meta,
    #[serde(default)]
    pub data: serde_json::Value,
}

impl Object {
    pub fn new(data: serde_json::Value) -> Self {
        Object {
            metadata: Meta::default(),
            data,
        }
    }

    pub fn with_ttl(mut self, secs: u64) -> Self {
        self.metadata.ttl_secs = Some(secs);
        self
    }

    pub fn with_finalizers(mut self, f: &[&str]) -> Self {
        self.metadata.finalizers = f.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn with_owners(mut self, o: &[&str]) -> Self {
        self.metadata.owner_references = o.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|e| Error::Backend(format!("undecodable value: {e}")))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub key: String,
    pub value: Object,
    pub mod_rev: Rev,
    pub create_rev: Rev,
    /// A live lock's current expiry, at any read revision: advisory wall-clock ms; nothing
    /// fences on it. `None` for any other key and for an expired lock.
    pub lock_expires_at_ms: Option<u64>,
}

/// Each etcd v3 limit, named in the refusal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Limit {
    Ops { max: usize, got: usize },
    Compares { max: usize, got: usize },
    RequestBytes { max: usize, got: usize },
    DuplicateKey { key: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    /// `create` found the key holding a different value.
    Conflict {
        key: String,
        existing: Box<Item>,
    },
    NotFound {
        key: String,
    },
    /// `delete`'s `expect_rev` is not the key's `mod_rev`.
    RevMismatch {
        key: String,
        expected: Rev,
        actual: Rev,
    },
    Limit(Limit),
    TtlRequired {
        key: String,
    },
    /// `ttl_secs` on a key outside the prefixes whose keys expire.
    TtlNotAllowed {
        key: String,
    },
    /// Caller writes may not touch the contract's own prefixes.
    ReservedKey {
        key: String,
    },
    /// The revision asked for has been compacted away: re-list.
    TooOld {
        compacted: u64,
    },
    /// A read at a revision the store has not reached.
    FutureRev {
        rev: u64,
        head: u64,
    },
    Locked {
        resource: String,
        token: Rev,
    },
    /// The fence's lock expired or is held under another token.
    LockLost {
        resource: String,
    },
    LockTtlBelowMinimum {
        min_ms: u64,
        got_ms: u64,
    },
    WaitTooLong {
        max_ms: u64,
        got_ms: u64,
    },
    /// `Backend::commit`'s guard: the head moved since the batch was read.
    Stale {
        expected: u64,
        head: u64,
    },
    Backend(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// The audit history key of the txn at `main`, zero-padded so key order is revision order.
pub fn events_key(main: u64) -> String {
    format!("{EVENTS_PREFIX}{main:020}")
}

pub fn request_key(id: &str) -> String {
    format!("{REQUESTS_PREFIX}{id}")
}
