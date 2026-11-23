//! An in-memory reader that records every post. Cloning shares the state, so a test keeps a handle.

use crate::{Anchor, Changes, Collection, CollectionInfo, Comment, CommentRef, Doc, DocRef, Margin, Thread, AGENT_PREFIX};
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

#[derive(Default)]
struct State {
    collections: Vec<(String, String, String, Vec<Doc>)>,
    texts: BTreeMap<(String, u64), String>,
    threads: BTreeMap<String, Thread>,
    comments: Vec<PostedComment>,
    published_at: BTreeMap<String, u64>,
    closed: Vec<String>,
    fail_comment_number: Option<usize>,
    comment_attempts: usize,
    down: bool,
}

#[derive(Clone, Default)]
pub struct FakeMargin(Rc<RefCell<State>>);

pub const BASE: &str = "https://margin.invalid";

impl FakeMargin {
    pub fn collections_posted(&self) -> usize {
        self.0.borrow().collections.len()
    }
    /// `(title, cover, docs)` of the collection posted `n`th, from zero.
    pub fn collection_posted(&self, n: usize) -> (String, String, Vec<Doc>) {
        let s = self.0.borrow();
        let c = &s.collections[n];
        (c.2.clone(), c.1.clone(), c.3.clone())
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
    pub fn publish(&self, collection: &str, at_ms: u64) {
        self.0.borrow_mut().published_at.insert(collection.to_string(), at_ms);
    }
    /// A comment by `author` on the thread `thread`, as the reader would hold it.
    pub fn reply_as(&self, thread: &str, author: &str, body: &str, at_ms: u64) {
        let mut s = self.0.borrow_mut();
        let t = s.threads.get_mut(thread).unwrap_or_else(|| panic!("no thread {thread}"));
        let id = format!("{thread}-r{}", t.comments.len());
        t.comments.push(Comment { id, author: author.to_string(), body: body.to_string(), created_at_ms: at_ms });
    }
    /// A thread `author` started on `collection`, as the reader would hold it.
    pub fn thread_by(&self, collection: &str, anchor: Anchor, author: &str, body: &str, at_ms: u64) -> String {
        let mut s = self.0.borrow_mut();
        let id = format!("{collection}-his{}", s.threads.len());
        s.threads.insert(id.clone(), Thread { anchor, comments: vec![Comment { id: format!("{id}-0"), author: author.into(), body: body.into(), created_at_ms: at_ms }] });
        id
    }
    /// A thread the operator started.
    pub fn his_thread(&self, collection: &str, anchor: Anchor, body: &str, at_ms: u64) -> String {
        self.thread_by(collection, anchor, "ryan", body, at_ms)
    }
    pub fn agent_author() -> String {
        format!("{AGENT_PREFIX}concierge")
    }
    fn gate(&self, route: &str) -> Result<(), String> {
        if self.0.borrow().down {
            return Err(format!("the reader at {BASE} did not answer {route}: connection refused"));
        }
        Ok(())
    }
}

impl Margin for FakeMargin {
    fn post_collection(&self, title: &str, cover: &str, docs: &[Doc]) -> Result<Collection, String> {
        self.gate("POST /api/collections")?;
        let mut s = self.0.borrow_mut();
        let id = format!("col{}", s.collections.len() + 1);
        let mut refs = Vec::new();
        for (i, d) in docs.iter().enumerate() {
            let doc_id = format!("{id}-d{}", i + 1);
            s.texts.insert((doc_id.clone(), 1), d.text.clone());
            refs.push(DocRef { id: doc_id, version: 1, path: d.path.clone() });
        }
        s.collections.push((id.clone(), cover.to_string(), title.to_string(), docs.to_vec()));
        Ok(Collection { url: format!("{BASE}/c/{id}"), id, documents: refs })
    }

    fn post_comment(&self, collection: &str, anchor: &Anchor, body: &str, held: bool, in_reply_to: Option<&str>) -> Result<CommentRef, String> {
        self.gate("POST /api/collections/{id}/comments")?;
        let mut s = self.0.borrow_mut();
        s.comment_attempts += 1;
        if s.fail_comment_number == Some(s.comment_attempts) {
            return Err(format!("the reader at {BASE} did not answer POST /api/collections/{collection}/comments: timed out"));
        }
        let n = s.comments.len() + 1;
        s.comments.push(PostedComment { collection: collection.to_string(), anchor: anchor.clone(), body: body.to_string(), held, in_reply_to: in_reply_to.map(str::to_string) });
        let thread = in_reply_to.map_or_else(|| format!("{collection}-t{n}"), str::to_string);
        let id = format!("{collection}-c{n}");
        let author = Self::agent_author();
        s.threads.entry(thread.clone()).or_insert_with(|| Thread { anchor: anchor.clone(), comments: vec![] }).comments.push(Comment { id: id.clone(), author, body: body.to_string(), created_at_ms: 0 });
        Ok(CommentRef { url: format!("{BASE}/c/{collection}#{id}"), id, thread })
    }

    fn post_version(&self, document: &str, text: &str) -> Result<u64, String> {
        self.gate("POST /api/documents/{id}/versions")?;
        let mut s = self.0.borrow_mut();
        let next = s.texts.keys().filter(|(d, _)| d == document).map(|(_, v)| *v).max().unwrap_or(0) + 1;
        s.texts.insert((document.to_string(), next), text.to_string());
        Ok(next)
    }

    fn collection(&self, id: &str) -> Result<CollectionInfo, String> {
        self.gate("GET /api/collections/{id}")?;
        let s = self.0.borrow();
        let Some((cid, _, title, docs)) = s.collections.iter().find(|c| c.0 == id) else { return Err(format!("the reader at {BASE} refused GET /api/collections/{{id}}: HTTP 404: no collection {id}")) };
        let published = s.published_at.get(id).copied();
        let state = if s.closed.iter().any(|c| c == id) { "closed" } else if published.is_some() { "published" } else { "open" };
        let documents = docs.iter().enumerate().map(|(i, d)| DocRef { id: format!("{cid}-d{}", i + 1), version: 1, path: d.path.clone() }).collect();
        let threads = s.threads.keys().filter(|k| k.starts_with(&format!("{id}-"))).cloned().collect();
        Ok(CollectionInfo { state: state.into(), published_at_ms: published, title: title.clone(), documents, threads })
    }

    fn thread(&self, id: &str) -> Result<Thread, String> {
        self.gate("GET /api/threads/{id}")?;
        self.0.borrow().threads.get(id).cloned().ok_or_else(|| format!("the reader at {BASE} refused GET /api/threads/{{id}}: HTTP 404: no thread {id}"))
    }

    fn document_text(&self, document: &str, version: u64) -> Result<String, String> {
        self.gate("GET /api/documents/{id}/versions/{v}")?;
        self.0.borrow().texts.get(&(document.to_string(), version)).cloned().ok_or_else(|| format!("no document {document} at version {version}"))
    }

    fn close(&self, collection: &str) -> Result<(), String> {
        self.gate("POST /api/collections/{id}/close")?;
        self.0.borrow_mut().closed.push(collection.to_string());
        Ok(())
    }

    fn changes(&self, after: u64, _hold_secs: u64) -> Result<Changes, String> {
        self.gate("GET /api/changes")?;
        Ok(Changes { through: after, changes: vec![] })
    }
}
