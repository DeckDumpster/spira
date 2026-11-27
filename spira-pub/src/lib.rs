//! Publishes a harness fact to the MQTT broker. Every component calls [`Publisher`] in-process;
//! [`topics`] is the table of what may be published.
//!
//! A publish never blocks past [`CONNECT_DEADLINE`] and is never retried: it fails, is logged,
//! and the caller carries on. A state topic recovers on the next publish or reconnect.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;

pub mod cli;
pub mod link;
mod publisher;
mod socket_watch;
mod telemetry;
pub mod topics;

pub use publisher::{Log, Options, Publisher, Republish, CONNECT_DEADLINE, MAX_PAYLOAD};
pub use socket_watch::SocketWatch;
pub use topics::Class;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PubError {
    /// The configuration does not name the broker.
    Config(String),
    BadName(String),
    UndeclaredFamily { topic: String },
    WrongClass { topic: String, declared: Class, asked: Class },
    TooLarge { topic: String, bytes: usize },
    QueueFull { topic: String },
    /// The connect attempt failed.
    Unreachable { why: String },
    /// The broker did not answer within the deadline.
    Timeout,
    /// This many accepted messages never reached the broker.
    Undelivered(usize),
}

impl PubError {
    /// The failure's metric label.
    pub fn reason(&self) -> &'static str {
        match self {
            PubError::Config(_) => "config",
            PubError::BadName(_) => "bad_name",
            PubError::UndeclaredFamily { .. } => "undeclared_family",
            PubError::WrongClass { .. } => "wrong_class",
            PubError::TooLarge { .. } => "too_large",
            PubError::QueueFull { .. } => "queue_full",
            PubError::Unreachable { .. } => "unreachable",
            PubError::Timeout => "timeout",
            PubError::Undelivered(_) => "undelivered",
        }
    }

    /// Whether the publish was refused before anything was sent: trying again cannot help.
    pub fn is_refusal(&self) -> bool {
        matches!(
            self,
            PubError::Config(_) | PubError::BadName(_) | PubError::UndeclaredFamily { .. } | PubError::WrongClass { .. } | PubError::TooLarge { .. }
        )
    }
}

impl std::fmt::Display for PubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PubError::Config(why) => write!(f, "{why}"),
            PubError::BadName(name) => write!(f, "{name:?} is not a publisher name: one topic segment, no /, + or #"),
            PubError::UndeclaredFamily { topic } => write!(f, "topic {topic:?} belongs to no family in spira/topics.toml"),
            PubError::WrongClass { topic, declared, asked } => {
                write!(f, "topic {topic:?} is declared {}, not {}", declared.name(), asked.name())
            }
            PubError::TooLarge { topic, bytes } => write!(f, "the payload for {topic:?} is {bytes} bytes, over {MAX_PAYLOAD}"),
            PubError::QueueFull { topic } => write!(f, "the publish queue is full; {topic:?} was not queued"),
            PubError::Unreachable { why } => write!(f, "the broker is unreachable: {why}"),
            PubError::Timeout => write!(f, "the broker did not answer within the deadline"),
            PubError::Undelivered(n) => write!(f, "{n} message(s) never reached the broker"),
        }
    }
}

impl std::error::Error for PubError {}
