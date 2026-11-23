//! The Margin reader's client: the only code in Spira that knows the reader's routes.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;

mod http;
mod fake;
#[cfg(any(test, feature = "record"))]
pub mod scenario;

pub use http::{ChangesRequest, HttpMargin};
pub use fake::FakeMargin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const URL_KEY: &str = "SPIRA_MARGIN_URL";
pub const TOKEN_FILE_KEY: &str = "SPIRA_MARGIN_TOKEN_FILE";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doc {
    pub path: String,
    pub kind: String,
    pub text: String,
}

/// Where a collection's documents came from: a repository's web URL and the commit read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub repository: String,
    pub commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRef {
    pub id: String,
    pub version: u64,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub path: String,
    pub first: u32,
    pub last: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentRef {
    pub thread: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionInfo {
    /// `open`, `published` or `closed`.
    pub state: String,
    /// The reader's last change to the collection while it stands published; none otherwise.
    pub published_at_ms: Option<u64>,
    pub title: String,
    pub documents: Vec<DocRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub created_at_ms: u64,
}

/// The author name the reader gives the operator; every agent has a bare name of its own.
pub const READER: &str = "reader";

impl Comment {
    pub fn by_agent(&self) -> bool {
        self.author != READER
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    /// The document version the thread stands on.
    pub document: DocRef,
    pub anchor: Anchor,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Changes {
    pub through: u64,
    pub changes: Vec<Value>,
}

pub trait Margin {
    fn post_collection(&self, title: &str, cover: &str, docs: &[Doc], source: Option<&Source>) -> Result<Collection, String>;
    fn post_comment(&self, collection: &str, anchor: &Anchor, body: &str, held: bool, in_reply_to: Option<&str>) -> Result<CommentRef, String>;
    fn post_version(&self, document: &str, text: &str) -> Result<u64, String>;
    fn collection(&self, id: &str) -> Result<CollectionInfo, String>;
    fn thread(&self, collection: &str, id: &str) -> Result<Thread, String>;
    /// Every thread of the collection, held ones included.
    fn threads(&self, collection: &str) -> Result<Vec<Thread>, String>;
    fn document_text(&self, document: &str, version: u64) -> Result<String, String>;
    fn close(&self, collection: &str) -> Result<(), String>;
    fn changes(&self, after: u64, hold_secs: u64) -> Result<Changes, String>;
}

/// Stands in for a reader whose config is missing: every call is refused with the config's own words,
/// so a verb that never reaches the reader still runs.
pub struct Unconfigured(pub String);

impl Margin for Unconfigured {
    fn post_collection(&self, _: &str, _: &str, _: &[Doc], _: Option<&Source>) -> Result<Collection, String> {
        Err(self.0.clone())
    }
    fn post_comment(&self, _: &str, _: &Anchor, _: &str, _: bool, _: Option<&str>) -> Result<CommentRef, String> {
        Err(self.0.clone())
    }
    fn post_version(&self, _: &str, _: &str) -> Result<u64, String> {
        Err(self.0.clone())
    }
    fn collection(&self, _: &str) -> Result<CollectionInfo, String> {
        Err(self.0.clone())
    }
    fn thread(&self, _: &str, _: &str) -> Result<Thread, String> {
        Err(self.0.clone())
    }
    fn threads(&self, _: &str) -> Result<Vec<Thread>, String> {
        Err(self.0.clone())
    }
    fn document_text(&self, _: &str, _: u64) -> Result<String, String> {
        Err(self.0.clone())
    }
    fn close(&self, _: &str) -> Result<(), String> {
        Err(self.0.clone())
    }
    fn changes(&self, _: u64, _: u64) -> Result<Changes, String> {
        Err(self.0.clone())
    }
}
