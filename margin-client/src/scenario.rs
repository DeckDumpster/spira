//! The one session the recorder runs against a real reader and the tests replay from its recording.

use crate::{Anchor, Changes, Collection, CollectionInfo, CommentRef, Doc, HttpMargin, Margin, Thread};
use serde_json::{json, Value};

/// What each step of the session returned.
pub struct Seen {
    pub collection: Collection,
    pub open: CollectionInfo,
    pub held: CommentRef,
    pub reply: CommentRef,
    pub his_on_first_version: String,
    pub agent_thread: Thread,
    pub second_version: u64,
    pub all: Vec<Thread>,
    pub first_text: String,
    pub second_text: String,
    pub published: CollectionInfo,
    pub changes: Changes,
    pub closed: CollectionInfo,
}

pub const FIRST: &str = "fn one() {}\nfn two() {}\nfn three() {}\n";
pub const SECOND: &str = "fn zero() {}\nfn one() {}\nfn two() {}\nfn three() {}\n";

/// A reader-side call, which carries no bearer: the operator's browser makes it.
fn browser(base: &str, method: &str, route: &str, body: Option<&Value>) -> Result<String, String> {
    let req = ureq::request(method, &format!("{base}{route}"));
    let sent = match body {
        Some(b) => req.set("Content-Type", "application/json").send_string(&b.to_string()),
        None => req.call(),
    };
    sent.map_err(|e| format!("{method} {route}: {e}"))?.into_string().map_err(|e| e.to_string())
}

pub fn run(m: &HttpMargin) -> Result<Seen, String> {
    let docs = [Doc { path: "src/a.rs".into(), kind: "code".into(), text: FIRST.into() }, Doc { path: "src/b.rs".into(), kind: "code".into(), text: "fn b() {}\n".into() }];
    let collection = m.post_collection("Scenario", "cover words", &docs, None)?;
    let id = collection.id.as_str();
    let open = m.collection(id)?;
    let a = open.documents.iter().find(|d| d.path == "src/a.rs").ok_or("no src/a.rs")?.id.clone();
    let anchor = Anchor { path: "src/a.rs".into(), first: 2, last: 3 };
    let held = m.post_comment(id, &anchor, "held note", true, None)?;
    let reply = m.post_comment(id, &anchor, "held answer", true, Some(&held.thread))?;

    let read: Value = serde_json::from_str(&browser(&m.base, "GET", &format!("/api/collections/{id}"), None)?).map_err(|e| e.to_string())?; // not a path: the reader's route
    let first_version = read["documents"].as_array().and_then(|d| d.iter().find(|d| d["path"] == "src/a.rs")).map(|d| d["version_id"].clone()).ok_or("the collection has no version_id")?;
    let agent_thread = m.thread(id, &held.thread)?;
    let second_version = m.post_version(&a, SECOND)?;
    let his = json!({"version_id": first_version, "first_line": 1, "last_line": 1, "body": "his note on the first version"});
    browser(&m.base, "POST", &format!("/c/{id}/threads"), Some(&his))?; // not a path: the reader's route
    let all = m.threads(id)?;
    let his_on_first_version = all.iter().find(|t| t.comments.iter().any(|c| !c.by_agent())).map(|t| t.id.clone()).ok_or("his thread is not listed")?;
    let first_text = m.document_text(&a, 1)?;
    let second_text = m.document_text(&a, 2)?;

    browser(&m.base, "POST", &format!("/c/{id}/publish"), Some(&json!({})))?; // not a path: the reader's route
    let published = m.collection(id)?;
    let changes = m.changes(0, 0)?;
    m.close(id)?;
    let closed = m.collection(id)?;
    Ok(Seen { collection, open, held, reply, his_on_first_version, agent_thread, second_version, all, first_text, second_text, published, changes, closed })
}
