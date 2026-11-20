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
    pub fn count(&self) -> usize {
        self.calls.borrow().len()
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
        if let Some((_, j)) = self.canned.borrow().iter().rev().find(|(s, _)| ep.ends_with(s.as_str())) {
            return GhOut { code: 0, stdout: j.clone().into_bytes() };
        }
        if ep.ends_with("git/blobs") {
            self.blobs.set(self.blobs.get() + 1);
        }
        GhOut { code: 0, stdout: reply(&ep, self.blobs.get()).into_bytes() }
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
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let mut out = Vec::new();
    for chunk in s.as_bytes().chunks(4) {
        let n = chunk.iter().filter(|c| **c != b'=').fold(0u32, |a, c| (a << 6) | u32::from(val(*c))) << (6 * (4 - chunk.iter().filter(|c| **c != b'=').count()));
        let bytes = chunk.iter().filter(|c| **c != b'=').count() - 1;
        for i in 0..bytes {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    out
}
