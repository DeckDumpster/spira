//! An in-memory reader that records every post. Cloning shares the state, so a test keeps a handle.

use crate::{Anchor, Changes, Collection, CollectionInfo, Comment, CommentRef, Doc, DocRef, Margin, Source, Thread, READER};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedComment {
    pub collection: String,
    pub anchor: Anchor,
    pub body: String,
    pub held: bool,
    pub in_reply_to: Option<String>,
}

struct Stored {
    id: String,
    cover: String,
    title: String,
    docs: Vec<Doc>,
    source: Option<Source>,
}

#[derive(Default)]
struct State {
    collections: Vec<Stored>,
    texts: BTreeMap<(String, u64), String>,
    threads: Vec<(String, Thread)>,
    comments: Vec<PostedComment>,
    published_at: BTreeMap<String, u64>,
    closed: Vec<String>,
    fail_comment_number: Option<usize>,
    comment_attempts: usize,
    down: bool,
}

impl State {
    fn doc_id(collection: &str, index: usize) -> String {
        format!("{collection}-d{}", index + 1)
    }

    fn latest(&self, doc: &str) -> u64 {
        self.texts.keys().filter(|(d, _)| d == doc).map(|(_, v)| *v).max().unwrap_or(1)
    }

    fn doc_at(&self, collection: &str, path: &str) -> Option<DocRef> {
        let c = self.collections.iter().find(|c| c.id == collection)?;
        let i = c.docs.iter().position(|d| d.path == path)?;
        let id = Self::doc_id(collection, i);
        Some(DocRef { version: self.latest(&id), id, path: path.to_string() })
    }
}

#[derive(Clone, Default)]
pub struct FakeMargin(Rc<RefCell<State>>);

pub const BASE: &str = "https://margin.invalid";
pub const AGENT: &str = "concierge";

fn not_found(route: &str, what: &str) -> String {
    format!("the reader at {BASE} refused {route}: HTTP 404: {what}")
}

impl FakeMargin {
    pub fn collections_posted(&self) -> usize {
        self.0.borrow().collections.len()
    }
    /// `(title, cover, docs)` of the collection posted `n`th, from zero.
    pub fn collection_posted(&self, n: usize) -> (String, String, Vec<Doc>) {
        let s = self.0.borrow();
        let c = &s.collections[n];
        (c.title.clone(), c.cover.clone(), c.docs.clone())
    }
    /// The source the collection posted `n`th carried, if any.
    pub fn source_posted(&self, n: usize) -> Option<Source> {
        self.0.borrow().collections[n].source.clone()
    }
    pub fn comments_posted(&self) -> Vec<PostedComment> {
        self.0.borrow().comments.clone()
    }
    pub fn closed(&self) -> Vec<String> {
        self.0.borrow().closed.clone()
    }
    /// The `n`th comment post of this fake's life (from 1) fails once; later posts succeed.
    pub fn fail_comment_number(&self, n: usize) {
        self.0.borrow_mut().fail_comment_number = Some(n);
    }
    pub fn go_down(&self) {
        self.0.borrow_mut().down = true;
    }
    pub fn come_up(&self) {
        self.0.borrow_mut().down = false;
    }
    pub fn publish(&self, collection: &str, at_ms: u64) {
        self.0.borrow_mut().published_at.insert(collection.to_string(), at_ms);
    }
    /// A comment by `author` on the thread `thread`, as the reader would hold it.
    pub fn reply_as(&self, thread: &str, author: &str, body: &str, at_ms: u64) {
        let mut s = self.0.borrow_mut();
        let t = s.threads.iter_mut().find(|(_, t)| t.id == thread).map(|(_, t)| t).unwrap_or_else(|| panic!("no thread {thread}"));
        let id = format!("{thread}-r{}", t.comments.len());
        t.comments.push(Comment { id, author: author.to_string(), body: body.to_string(), created_at_ms: at_ms });
    }
    /// A thread `author` started on `collection`, as the reader would hold it.
    pub fn thread_by(&self, collection: &str, anchor: Anchor, author: &str, body: &str, at_ms: u64) -> String {
        let mut s = self.0.borrow_mut();
        let id = format!("{collection}-his{}", s.threads.len());
        let document = s.doc_at(collection, &anchor.path).unwrap_or_else(|| panic!("no document {} in {collection}", anchor.path));
        let first = Comment { id: format!("{id}-0"), author: author.into(), body: body.into(), created_at_ms: at_ms };
        s.threads.push((collection.to_string(), Thread { id: id.clone(), document, anchor, comments: vec![first] }));
        id
    }
    /// A thread the operator started.
    pub fn his_thread(&self, collection: &str, anchor: Anchor, body: &str, at_ms: u64) -> String {
        self.thread_by(collection, anchor, READER, body, at_ms)
    }
    pub fn agent_author() -> String {
        AGENT.to_string()
    }
    fn gate(&self, route: &str) -> Result<(), String> {
        if self.0.borrow().down {
            return Err(format!("the reader at {BASE} did not answer {route}: connection refused"));
        }
        Ok(())
    }
}

impl Margin for FakeMargin {
    fn post_collection(&self, title: &str, cover: &str, docs: &[Doc], source: Option<&Source>) -> Result<Collection, String> {
        self.gate("POST /api/collections")?;
        let mut s = self.0.borrow_mut();
        let id = format!("col{}", s.collections.len() + 1);
        for (i, d) in docs.iter().enumerate() {
            s.texts.insert((State::doc_id(&id, i), 1), d.text.clone());
        }
        s.collections.push(Stored { id: id.clone(), cover: cover.to_string(), title: title.to_string(), docs: docs.to_vec(), source: source.cloned() });
        Ok(Collection { url: format!("{BASE}/c/{id}"), id })
    }

    fn post_comment(&self, collection: &str, anchor: &Anchor, body: &str, held: bool, in_reply_to: Option<&str>) -> Result<CommentRef, String> {
        let route = match in_reply_to {
            Some(_) => "POST /api/threads/{tid}/comments",
            None => "POST /api/collections/{id}/threads",
        };
        self.gate(route)?;
        let mut s = self.0.borrow_mut();
        s.comment_attempts += 1;
        if s.fail_comment_number == Some(s.comment_attempts) {
            return Err(format!("the reader at {BASE} did not answer {route}: timed out"));
        }
        let n = s.comments.len() + 1;
        let author = Self::agent_author();
        let thread = match in_reply_to {
            Some(tid) => {
                let comment = Comment { id: format!("{collection}-c{n}"), author, body: body.to_string(), created_at_ms: 0 };
                let t = s.threads.iter_mut().find(|(_, t)| t.id == tid).map(|(_, t)| t).ok_or_else(|| not_found(route, "not found"))?;
                t.comments.push(comment);
                tid.to_string()
            }
            None => {
                let document = s.doc_at(collection, &anchor.path).ok_or_else(|| not_found(route, "not found"))?;
                let id = format!("{collection}-t{n}");
                let comment = Comment { id: format!("{collection}-c{n}"), author, body: body.to_string(), created_at_ms: 0 };
                s.threads.push((collection.to_string(), Thread { id: id.clone(), document, anchor: anchor.clone(), comments: vec![comment] }));
                id
            }
        };
        s.comments.push(PostedComment { collection: collection.to_string(), anchor: anchor.clone(), body: body.to_string(), held, in_reply_to: in_reply_to.map(str::to_string) });
        Ok(CommentRef { url: format!("{BASE}/c/{collection}#thread-{thread}"), thread })
    }

    fn post_version(&self, document: &str, text: &str) -> Result<u64, String> {
        self.gate("POST /api/documents/{doc_id}/versions")?;
        let mut s = self.0.borrow_mut();
        let next = s.latest(document) + 1;
        s.texts.insert((document.to_string(), next), text.to_string());
        Ok(next)
    }

    fn collection(&self, id: &str) -> Result<CollectionInfo, String> {
        self.gate("GET /api/collections/{id}")?;
        let s = self.0.borrow();
        let Some(c) = s.collections.iter().find(|c| c.id == id) else { return Err(not_found("GET /api/collections/{id}", "not found")) };
        let published = s.published_at.get(id).copied();
        let state = if s.closed.iter().any(|c| c == id) { "closed" } else if published.is_some() { "published" } else { "open" };
        let documents = c.docs.iter().enumerate().map(|(i, d)| {
            let id = State::doc_id(&c.id, i);
            DocRef { version: s.latest(&id), id, path: d.path.clone() }
        });
        Ok(CollectionInfo { state: state.into(), published_at_ms: if state == "published" { published } else { None }, title: c.title.clone(), documents: documents.collect() })
    }

    fn thread(&self, collection: &str, id: &str) -> Result<Thread, String> {
        self.gate("GET /api/threads/{tid}")?;
        self.0.borrow().threads.iter().find(|(c, t)| c == collection && t.id == id).map(|(_, t)| t.clone()).ok_or_else(|| not_found("GET /api/threads/{tid}", "not found"))
    }

    fn threads(&self, collection: &str) -> Result<Vec<Thread>, String> {
        self.gate("GET /api/collections/{id}/threads")?;
        let s = self.0.borrow();
        if !s.collections.iter().any(|c| c.id == collection) {
            return Err(not_found("GET /api/collections/{id}/threads", "not found"));
        }
        Ok(s.threads.iter().filter(|(c, _)| c == collection).map(|(_, t)| t.clone()).collect())
    }

    fn document_text(&self, document: &str, version: u64) -> Result<String, String> {
        self.gate("GET /api/documents/{doc_id}/versions/{n}")?;
        self.0.borrow().texts.get(&(document.to_string(), version)).cloned().ok_or_else(|| not_found("GET /api/documents/{doc_id}/versions/{n}", "not found"))
    }

    fn close(&self, collection: &str) -> Result<(), String> {
        self.gate("PATCH /api/collections/{id}")?;
        self.0.borrow_mut().closed.push(collection.to_string());
        Ok(())
    }

    fn changes(&self, after: u64, _hold_secs: u64) -> Result<Changes, String> {
        self.gate("GET /api/changes")?;
        Ok(Changes { through: after, changes: vec![] })
    }
}
