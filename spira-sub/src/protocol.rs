//! The control socket: one JSON request line, one JSON reply line. After a `follow`'s or a
//! `receive`'s reply the socket carries deliveries until either side closes it: as plain lines
//! to a `follow`, and to a `receive` as one [`Delivery`] a line, each answered with an `ack`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    Add { filter: String, note: String, once: bool, batch_s: Option<u64> },
    At { at_ms: u64, note: String },
    Drop { id: String },
    Set { id: String, batch_s: Option<u64> },
    List,
    Follow,
    Receive,
    /// Said on the `receive` stream that carried the delivery.
    Ack { seq: u64 },
}

/// One delivery on a `receive` stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    /// Rises within one `run` process; what an ack names.
    pub seq: u64,
    /// The subscriptions it carries something of.
    pub subs: Vec<String>,
    /// What `follow` would print: a batch's header and lines are joined by newlines.
    pub text: String,
}

impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Request::Add { .. } => "add",
            Request::At { .. } => "at",
            Request::Drop { .. } => "drop",
            Request::Set { .. } => "set",
            Request::List => "list",
            Request::Follow => "follow",
            Request::Receive => "receive",
            Request::Ack { .. } => "ack",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    /// The command's output, and what the caller should know about it.
    Done { out: String, warning: Option<String> },
    /// The request itself is wrong: asking again cannot help.
    Refused(String),
    Failed(String),
}
