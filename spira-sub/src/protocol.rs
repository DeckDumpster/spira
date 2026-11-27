//! The control socket: one JSON request line, one JSON reply line. After a `follow`'s reply the
//! socket carries delivery lines until either side closes it.

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
