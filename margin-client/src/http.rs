//! `HttpMargin`: the reader over HTTP with a bearer token read from a file at each use.

use crate::{Anchor, Changes, Collection, CollectionInfo, Comment, CommentRef, Doc, DocRef, Margin, Source, Thread, TOKEN_FILE_KEY, URL_KEY};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use serde_json::{json, Value};
use std::time::Duration;

pub const CALL_SECS: u64 = 5;
const API: &str = "/api"; // not a path: the reader's route prefix

#[derive(Clone, Debug)]
pub struct HttpMargin {
    pub base: String,
    pub token_file: String,
    /// What a request/response call waits for the reader's answer.
    pub timeout: Duration,
}

impl HttpMargin {
    /// Reads the reader's address and token file through `cfg`; an empty key is refused by name.
    pub fn configured(cfg: impl Fn(&str) -> Result<String, String>) -> Result<HttpMargin, String> {
        let get = |k: &str| -> Result<String, String> {
            let v = cfg(k)?;
            if v.trim().is_empty() {
                return Err(format!("{k} is empty; name it in the config, it has no default"));
            }
            Ok(v.trim().to_string())
        };
        Ok(HttpMargin { base: get(URL_KEY)?.trim_end_matches('/').to_string(), token_file: get(TOKEN_FILE_KEY)?, timeout: Duration::from_secs(CALL_SECS) })
    }

    fn token(&self) -> Result<String, String> {
        let text = std::fs::read_to_string(&self.token_file).map_err(|e| format!("{TOKEN_FILE_KEY}: cannot read {}: {e}", self.token_file))?;
        let t = text.trim();
        if t.is_empty() {
            return Err(format!("{TOKEN_FILE_KEY}: {} is empty", self.token_file));
        }
        Ok(t.to_string())
    }

    fn call<T: DeserializeOwned>(&self, method: &str, route: &str, body: Option<&Value>, timeout: Duration) -> Result<T, String> {
        let token = self.token()?;
        let url = format!("{}{route}", self.base);
        let silent = |why: String| format!("the reader at {} did not answer {method} {}: {why}", self.base, route.split('?').next().unwrap_or(route));
        let req = ureq::AgentBuilder::new().timeout(timeout).build().request(method, &url).set("Authorization", &format!("Bearer {token}")).set("Accept", "application/json");
        let sent = match body {
            Some(b) => req.set("Content-Type", "application/json").send_string(&b.to_string()),
            None => req.call(),
        };
        match sent {
            Ok(r) => r.into_string().map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<T>(&t).map_err(|e| e.to_string())).map_err(|e| format!("the reader at {} answered {method} {route} with a reply that is not the expected JSON: {e}", self.base)),
            Err(ureq::Error::Status(code, r)) => {
                let said = r.into_string().unwrap_or_default();
                Err(format!("the reader at {} refused {method} {}: HTTP {code}: {}", self.base, route.split('?').next().unwrap_or(route), said.trim().chars().take(300).collect::<String>()))
            }
            Err(e) => Err(silent(e.to_string().replace(&token, "<token>"))),
        }
    }

    fn quick<T: DeserializeOwned>(&self, method: &str, route: &str, body: Option<&Value>) -> Result<T, String> {
        self.call(method, route, body, self.timeout)
    }
}

const PAGE: usize = 200;
const MAX_WAIT_SECS: u64 = 30;

#[derive(Deserialize)]
struct Created {
    id: String,
    url: String,
}

#[derive(Deserialize)]
struct WireDoc {
    id: String,
    version_id: i64,
    path: String,
    version: u64,
}

#[derive(Deserialize)]
struct WireCollection {
    title: String,
    state: String,
    updated_at: u64,
    documents: Vec<WireDoc>,
}

#[derive(Deserialize)]
struct WireComment {
    id: String,
    author: String,
    body: String,
    created_at: u64,
}

#[derive(Deserialize)]
struct WireThread {
    id: String,
    version_id: i64,
    first_line: u32,
    last_line: u32,
    comments: Vec<WireComment>,
}

#[derive(Deserialize)]
struct Page<T> {
    items: Vec<T>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct WireVersion {
    id: i64,
    number: u64,
}

#[derive(Deserialize)]
struct WireChanges {
    changes: Vec<Value>,
    last_seq: u64,
}

/// Turns a thread's `version_id` into the document and version it stands on.
struct Resolver<'a> {
    reader: &'a HttpMargin,
    docs: Vec<WireDoc>,
    versions: HashMap<String, Vec<WireVersion>>,
}

impl Resolver<'_> {
    fn doc_ref(&mut self, version_id: i64) -> Result<DocRef, String> {
        if let Some(d) = self.docs.iter().find(|d| d.version_id == version_id) {
            return Ok(DocRef { id: d.id.clone(), version: d.version, path: d.path.clone() });
        }
        for d in &self.docs {
            if !self.versions.contains_key(&d.id) {
                let all = self.reader.paged::<WireVersion>(&format!("{API}/documents/{}/versions", d.id))?;
                self.versions.insert(d.id.clone(), all);
            }
            if let Some(v) = self.versions[&d.id].iter().find(|v| v.id == version_id) {
                return Ok(DocRef { id: d.id.clone(), version: v.number, path: d.path.clone() });
            }
        }
        Err(format!("the reader holds no document version {version_id} in this collection"))
    }

    fn thread(&mut self, t: WireThread) -> Result<Thread, String> {
        let document = self.doc_ref(t.version_id)?;
        let anchor = Anchor { path: document.path.clone(), first: t.first_line, last: t.last_line };
        let comments = t.comments.into_iter().map(|c| Comment { id: c.id, author: c.author, body: c.body, created_at_ms: c.created_at * 1000 }).collect();
        Ok(Thread { id: t.id, document, anchor, comments })
    }
}

impl HttpMargin {
    /// Every item of a listing route, following `next` until the reader names none.
    fn paged<T: DeserializeOwned>(&self, route: &str) -> Result<Vec<T>, String> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let at = after.as_ref().map(|a| format!("&after={a}")).unwrap_or_default();
            let page: Page<T> = self.quick("GET", &format!("{route}?limit={PAGE}{at}"), None)?;
            out.extend(page.items);
            match page.next {
                Some(n) => after = Some(n),
                None => return Ok(out),
            }
        }
    }

    fn resolver(&self, collection: &str) -> Result<Resolver<'_>, String> {
        let c: WireCollection = self.quick("GET", &format!("{API}/collections/{collection}"), None)?;
        Ok(Resolver { reader: self, docs: c.documents, versions: HashMap::new() })
    }
}

impl Margin for HttpMargin {
    fn post_collection(&self, title: &str, cover: &str, docs: &[Doc], source: Option<&Source>) -> Result<Collection, String> {
        let mut body = json!({"title": title, "cover_note": cover, "documents": docs});
        if let Some(s) = source {
            body["source"] = json!(s);
        }
        let made: Created = self.quick("POST", &format!("{API}/collections"), Some(&body))?;
        Ok(Collection { id: made.id, url: made.url })
    }

    fn post_comment(&self, collection: &str, anchor: &Anchor, body: &str, held: bool, in_reply_to: Option<&str>) -> Result<CommentRef, String> {
        let thread: WireThread = match in_reply_to {
            Some(tid) => self.quick("POST", &format!("{API}/threads/{tid}/comments"), Some(&json!({"body": body, "held": held})))?,
            None => self.quick("POST", &format!("{API}/collections/{collection}/threads"), Some(&json!({"path": anchor.path, "first_line": anchor.first, "last_line": anchor.last, "body": body, "held": held})))?,
        };
        Ok(CommentRef { url: format!("{}/c/{collection}#thread-{}", self.base, thread.id), thread: thread.id })
    }

    fn post_version(&self, document: &str, text: &str) -> Result<u64, String> {
        let v: WireVersion = self.quick("POST", &format!("{API}/documents/{document}/versions"), Some(&json!({"text": text})))?;
        Ok(v.number)
    }

    fn collection(&self, id: &str) -> Result<CollectionInfo, String> {
        let c: WireCollection = self.quick("GET", &format!("{API}/collections/{id}"), None)?;
        let published_at_ms = (c.state == "published").then_some(c.updated_at * 1000);
        let documents = c.documents.into_iter().map(|d| DocRef { id: d.id, version: d.version, path: d.path }).collect();
        Ok(CollectionInfo { state: c.state, published_at_ms, title: c.title, documents })
    }

    fn thread(&self, collection: &str, id: &str) -> Result<Thread, String> {
        let t: WireThread = self.quick("GET", &format!("{API}/threads/{id}"), None)?;
        self.resolver(collection)?.thread(t)
    }

    fn threads(&self, collection: &str) -> Result<Vec<Thread>, String> {
        let wire: Vec<WireThread> = self.paged(&format!("{API}/collections/{collection}/threads"))?;
        let mut resolver = self.resolver(collection)?;
        wire.into_iter().map(|t| resolver.thread(t)).collect()
    }

    fn document_text(&self, document: &str, version: u64) -> Result<String, String> {
        let v: Value = self.quick("GET", &format!("{API}/documents/{document}/versions/{version}"), None)?;
        v.get("text").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "the reader's reply to a document read has no `text`".to_string())
    }

    fn close(&self, collection: &str) -> Result<(), String> {
        self.quick::<Value>("PATCH", &format!("{API}/collections/{collection}"), Some(&json!({"state": "closed"}))).map(|_| ())
    }

    fn changes(&self, after: u64, hold_secs: u64) -> Result<Changes, String> {
        let req = self.changes_request(after, hold_secs)?;
        let sent = ureq::AgentBuilder::new().timeout(req.timeout).build().get(&req.url).set("Authorization", &format!("Bearer {}", req.token)).set("Accept", ACCEPT).call();
        match sent {
            Ok(r) => {
                let status = r.status();
                req.reply(status, &r.into_string().map_err(|e| e.to_string())?)
            }
            Err(ureq::Error::Status(code, r)) => req.reply(code, &r.into_string().unwrap_or_default()),
            Err(e) => Err(req.silent(&e.to_string())),
        }
    }
}

const ACCEPT: &str = "application/json";

/// One long-poll of the change feed, ready to send over any transport: where, as whom, and how long to wait.
pub struct ChangesRequest {
    pub base: String,
    pub url: String,
    pub token: String,
    /// The whole exchange's deadline: the call timeout plus the hold.
    pub timeout: Duration,
}

impl ChangesRequest {
    pub const ACCEPT: &'static str = ACCEPT;

    /// The answer's status and body as the changes it carries, or the refusal in today's words.
    pub fn reply(&self, status: u16, body: &str) -> Result<Changes, String> {
        let route = self.route();
        if status >= 400 {
            return Err(format!("the reader at {} refused GET {}: HTTP {status}: {}", self.base, route.split('?').next().unwrap_or(route), body.trim().chars().take(300).collect::<String>()));
        }
        let got: WireChanges = serde_json::from_str(body).map_err(|e| format!("the reader at {} answered GET {route} with a reply that is not the expected JSON: {e}", self.base))?;
        Ok(Changes { through: got.last_seq, changes: got.changes })
    }

    /// A transport failure, with the token redacted.
    pub fn silent(&self, why: &str) -> String {
        let route = self.route();
        format!("the reader at {} did not answer GET {}: {}", self.base, route.split('?').next().unwrap_or(route), why.replace(&self.token, "<token>"))
    }

    fn route(&self) -> &str {
        self.url.strip_prefix(self.base.as_str()).unwrap_or(&self.url)
    }
}

impl HttpMargin {
    /// The request for the changes after `after`, held up to `hold_secs`; reads the token file.
    pub fn changes_request(&self, after: u64, hold_secs: u64) -> Result<ChangesRequest, String> {
        let wait = hold_secs.min(MAX_WAIT_SECS);
        Ok(ChangesRequest { base: self.base.clone(), url: format!("{}{API}/changes?after={after}&wait={wait}", self.base), token: self.token()?, timeout: self.timeout + Duration::from_secs(wait) })
    }
}

#[cfg(test)]
mod tests;
