//! `HttpMargin`: the reader over HTTP with a bearer token read from a file at each use.

use crate::{Anchor, Changes, Collection, CollectionInfo, CommentRef, Doc, Margin, Thread, TOKEN_FILE_KEY, URL_KEY};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::time::Duration;

pub const CALL_SECS: u64 = 5;
const API: &str = "/api"; // not a path: the reader's route prefix

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

impl Margin for HttpMargin {
    fn post_collection(&self, title: &str, cover: &str, docs: &[Doc]) -> Result<Collection, String> {
        self.quick("POST", &format!("{API}/collections"), Some(&json!({"title": title, "cover": cover, "documents": docs})))
    }

    fn post_comment(&self, collection: &str, anchor: &Anchor, body: &str, held: bool, in_reply_to: Option<&str>) -> Result<CommentRef, String> {
        let payload = json!({"document": anchor.document, "version": anchor.version, "first": anchor.first, "last": anchor.last, "body": body, "held": held, "in_reply_to": in_reply_to});
        self.quick("POST", &format!("{API}/collections/{collection}/comments"), Some(&payload))
    }

    fn post_version(&self, document: &str, text: &str) -> Result<u64, String> {
        let v: Value = self.quick("POST", &format!("{API}/documents/{document}/versions"), Some(&json!({"text": text})))?;
        v.get("version").and_then(Value::as_u64).ok_or_else(|| "the reader's reply to a new version has no `version`".to_string())
    }

    fn collection(&self, id: &str) -> Result<CollectionInfo, String> {
        self.quick("GET", &format!("{API}/collections/{id}"), None)
    }

    fn thread(&self, id: &str) -> Result<Thread, String> {
        self.quick("GET", &format!("{API}/threads/{id}"), None)
    }

    fn document_text(&self, document: &str, version: u64) -> Result<String, String> {
        let v: Value = self.quick("GET", &format!("{API}/documents/{document}/versions/{version}"), None)?;
        v.get("text").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "the reader's reply to a document read has no `text`".to_string())
    }

    fn close(&self, collection: &str) -> Result<(), String> {
        self.quick::<Value>("POST", &format!("{API}/collections/{collection}/close"), Some(&json!({}))).map(|_| ())
    }

    fn changes(&self, after: u64, hold_secs: u64) -> Result<Changes, String> {
        self.call("GET", &format!("{API}/changes?after={after}&hold={hold_secs}"), None, self.timeout + Duration::from_secs(hold_secs))
    }
}

#[cfg(test)]
mod tests;
