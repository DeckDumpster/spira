//! A scripted review repository behind `forge::ports::Gh`: records `(args, stdin)` and answers
//! canned JSON per call. Also included by `tests/review_flow.rs`, so it uses only `forge` and `std`.

#![allow(dead_code)]

use forge::ports::{Gh, GhOut};
use std::cell::{Cell, RefCell};
use std::path::Path;

pub const OWNER: &str = "fixture-owner";
pub const NAME: &str = "fixture-reviews";

#[derive(Default)]
pub struct FakeGh {
    pub calls: RefCell<Vec<(Vec<String>, Vec<u8>)>>,
    fail_on: RefCell<Vec<String>>,
    canned: RefCell<Vec<(String, String)>>,
    blobs: Cell<u32>,
    ledger: RefCell<Option<(String, u32)>>,
    conflict: RefCell<Option<String>>,
}

impl FakeGh {
    /// Every call whose endpoint ends with `suffix` exits 1 until `heal` is called.
    pub fn fail(&self, suffix: &str) {
        self.fail_on.borrow_mut().push(suffix.to_string());
    }
    pub fn heal(&self) {
        self.fail_on.borrow_mut().clear();
    }
    /// Calls whose endpoint ends with `suffix` answer `json`.
    pub fn answer(&self, suffix: &str, json: &str) {
        self.canned.borrow_mut().push((suffix.to_string(), json.to_string()));
    }
    pub fn posted(&self, suffix: &str) -> Vec<serde_json::Value> {
        self.calls
            .borrow()
            .iter()
            .filter(|(a, _)| a.iter().any(|x| x == "POST") && endpoint(a).ends_with(suffix))
            .map(|(_, s)| serde_json::from_slice(s).expect("payloads are JSON"))
            .collect()
    }
    /// Seeds `ledger.jsonl` on the default branch with `text`.
    pub fn with_ledger_text(self, text: &str) -> FakeGh {
        *self.ledger.borrow_mut() = Some((text.to_string(), 1));
        self
    }
    /// The ledger text the forge holds now.
    pub fn ledger_text(&self) -> String {
        self.ledger.borrow().as_ref().expect("a ledger was seeded").0.clone()
    }
    /// The next ledger write fails as a conflict, and `line` is appended by someone else first.
    pub fn conflict_next_ledger_put(&self, line: &str) {
        *self.conflict.borrow_mut() = Some(format!("{line}\n"));
    }
    pub fn put(&self, suffix: &str) -> Vec<serde_json::Value> {
        self.calls
            .borrow()
            .iter()
            .filter(|(a, _)| a.iter().any(|x| x == "PUT") && endpoint(a).ends_with(suffix))
            .map(|(_, s)| serde_json::from_slice(s).expect("payloads are JSON"))
            .collect()
    }
    /// Calls other than the ledger's own reads and writes.
    pub fn count(&self) -> usize {
        self.calls.borrow().iter().filter(|(a, _)| !endpoint(a).ends_with("contents/ledger.jsonl")).count()
    }
}

fn endpoint(args: &[String]) -> &str {
    args.iter().find(|a| a.starts_with("repos/")).map_or("", String::as_str)
}

fn reply(ep: &str, n: u32) -> String {
    let base = format!("repos/{OWNER}/{NAME}");
    if ep == base {
        r#"{"default_branch":"main"}"#.into()
    } else if ep.ends_with("branches/main") {
        r#"{"commit":{"sha":"base-head","commit":{"tree":{"sha":"base-tree"}}}}"#.into()
    } else if ep.ends_with("git/blobs") {
        format!(r#"{{"sha":"blob-{n}"}}"#)
    } else if ep.ends_with("git/trees") {
        r#"{"sha":"new-tree"}"#.into()
    } else if ep.ends_with("git/commits") {
        r#"{"sha":"new-commit"}"#.into()
    } else if ep.ends_with("git/refs") {
        r#"{"ref":"refs/heads/x"}"#.into()
    } else if ep.ends_with("pulls") {
        format!(r#"{{"number":7,"html_url":"https://forge.invalid/{OWNER}/{NAME}/pull/7"}}"#)
    } else if ep.ends_with("reviews") {
        r#"{"id":1}"#.into()
    } else {
        "{}".into()
    }
}

impl Gh for FakeGh {
    fn call(&self, repo: Option<&Path>, args: &[&str]) -> GhOut {
        self.call_merged(repo, args, None)
    }
    fn call_merged(&self, _repo: Option<&Path>, args: &[&str], input: Option<&[u8]>) -> GhOut {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.calls.borrow_mut().push((args.clone(), input.unwrap_or_default().to_vec()));
        let ep = endpoint(&args).to_string();
        if self.fail_on.borrow().iter().any(|s| ep.ends_with(s.as_str())) {
            return GhOut { code: 1, stdout: b"HTTP 500".to_vec() };
        }
        if ep.ends_with("contents/ledger.jsonl") {
            return self.ledger_call(&args, input);
        }
        if let Some((_, j)) = self.canned.borrow().iter().rev().find(|(s, _)| ep.ends_with(s.as_str())) {
            return GhOut { code: 0, stdout: j.clone().into_bytes() };
        }
        if ep.ends_with("git/blobs") {
            self.blobs.set(self.blobs.get() + 1);
        }
        GhOut { code: 0, stdout: reply(&ep, self.blobs.get()).into_bytes() }
    }
}

impl FakeGh {
    fn ledger_call(&self, args: &[String], input: Option<&[u8]>) -> GhOut {
        use base64::Engine;
        let mut held = self.ledger.borrow_mut();
        let Some((text, version)) = held.as_mut() else {
            return GhOut { code: 1, stdout: b"gh: Not Found (HTTP 404)".to_vec() };
        };
        if args.iter().any(|a| a == "PUT") {
            if let Some(other) = self.conflict.borrow_mut().take() {
                text.push_str(&other);
                *version += 1;
                return GhOut { code: 1, stdout: b"gh: is at a different sha (HTTP 409)".to_vec() };
            }
            let body: serde_json::Value = serde_json::from_slice(input.unwrap_or_default()).expect("payloads are JSON");
            if body["sha"] != format!("ledger-sha-{version}") {
                return GhOut { code: 1, stdout: b"gh: sha does not match (HTTP 409)".to_vec() };
            }
            *text = String::from_utf8(unbase64(body["content"].as_str().unwrap())).unwrap();
            *version += 1;
            return GhOut { code: 0, stdout: b"{}".to_vec() };
        }
        let packed = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        GhOut { code: 0, stdout: serde_json::json!({"content": packed, "sha": format!("ledger-sha-{version}")}).to_string().into_bytes() }
    }
}

/// The handle a test hands to `Ports` while keeping its own to inspect.
pub struct Shared(pub std::rc::Rc<FakeGh>);

impl Gh for Shared {
    fn call(&self, repo: Option<&Path>, args: &[&str]) -> GhOut {
        self.0.call(repo, args)
    }
    fn call_merged(&self, repo: Option<&Path>, args: &[&str], input: Option<&[u8]>) -> GhOut {
        self.0.call_merged(repo, args, input)
    }
}

pub fn unbase64(s: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).expect("blob content is base64")
}
