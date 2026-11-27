//! Publishes a harness fact to the MQTT broker. Every component calls [`Publisher`] in-process,
//! to any well-formed topic, stating the [`Class`] of what it publishes.
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

pub use publisher::{check_topic, configured_seed, configured_socket, Log, Options, Publisher, Republish, CONNECT_DEADLINE, MAX_PAYLOAD};
pub use socket_watch::SocketWatch;
pub use topics::Class;

/// `raw` as one topic segment: `/`, `+` and `#`, which would split it or make it a filter,
/// become `-`.
pub fn segment(raw: &str) -> String {
    raw.replace(['/', '+', '#'], "-")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PubError {
    /// The configuration does not name the broker.
    Config(String),
    BadName(String),
    /// The topic is empty or holds a wildcard or NUL.
    BadTopic(String),
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
            PubError::BadTopic(_) => "bad_topic",
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
            PubError::Config(_) | PubError::BadName(_) | PubError::BadTopic(_) | PubError::TooLarge { .. }
        )
    }
}

impl std::fmt::Display for PubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PubError::Config(why) => write!(f, "{why}"),
            PubError::BadName(name) => write!(f, "{name:?} is not a publisher name: one topic segment, no /, + or #"),
            PubError::BadTopic(topic) => write!(f, "{topic:?} is not a topic to publish to: it must be non-empty, with no +, # or NUL"),
            PubError::TooLarge { topic, bytes } => write!(f, "the payload for {topic:?} is {bytes} bytes, over {MAX_PAYLOAD}"),
            PubError::QueueFull { topic } => write!(f, "the publish queue is full; {topic:?} was not queued"),
            PubError::Unreachable { why } => write!(f, "the broker is unreachable: {why}"),
            PubError::Timeout => write!(f, "the broker did not answer within the deadline"),
            PubError::Undelivered(n) => write!(f, "{n} message(s) never reached the broker"),
        }
    }
}

impl std::error::Error for PubError {}

#[cfg(test)]
mod tests {
    #[test_id(txp79kp)]
    #[test]
    fn a_segment_never_splits_its_topic_or_reads_as_a_filter() {
        assert_eq!(crate::segment("incident:a/b+c#d e"), "incident:a-b-c-d e");
    }
}
