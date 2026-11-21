//! The simulated forge serves the review repository: what `cr seal`, `cr reply` and `cr findings --post`
//! call, and the operator's side through `sim ghctl`.

use serde_json::{json, Value};
use spira_sim::gh;
use std::path::Path;

const REPO: &str = "repos/fixture-owner/fixture-reviews";

fn call(dir: &Path, cwd: &Path, method: &str, ep: &str, body: Option<Value>) -> Value {
    let mut args: Vec<String> = vec!["api".into(), "--method".into(), method.into(), if ep.is_empty() { REPO.to_string() } else { format!("{REPO}/{ep}") }];
    if let Some(b) = body {
        let f = dir.join(format!("input-{}", std::process::id()));
        std::fs::write(&f, b.to_string()).unwrap();
        args.extend(["--input".into(), f.display().to_string()]);
    }
    let out = gh::run(dir, cwd, &args).unwrap();
    assert_eq!(out.code, 0, "{method} {ep}: {}", out.stderr);
    serde_json::from_slice(&out.stdout).unwrap()
}

fn ctl(dir: &Path, args: &[&str]) -> String {
    gh::ctl(dir, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
}

#[test_id(tv6xu2a)]
#[test]
fn a_review_pull_request_is_opened_commented_on_replied_to_closed_and_read_back() {
    let t = testkit::TempDir::new("gh-review");
    let (dir, cwd) = (t.path(), t.path());

    let info = call(dir, cwd, "GET", "", None);
    let branch = info["default_branch"].as_str().unwrap().to_string();
    let base = call(dir, cwd, "GET", &format!("branches/{branch}"), None);
    let (base_sha, base_tree) = (base["commit"]["sha"].clone(), base["commit"]["commit"]["tree"]["sha"].clone());
    let blob = call(dir, cwd, "POST", "git/blobs", Some(json!({"content": "eA==", "encoding": "base64"})));
    let tree = call(dir, cwd, "POST", "git/trees", Some(json!({"base_tree": base_tree, "tree": [{"path": "a.rs", "sha": blob["sha"]}]})));
    let commit = call(dir, cwd, "POST", "git/commits", Some(json!({"message": "m", "tree": tree["sha"], "parents": [base_sha]})));
    call(dir, cwd, "POST", "git/refs", Some(json!({"ref": "refs/heads/review/001", "sha": commit["sha"]})));
    let got = call(dir, cwd, "GET", "git/ref/heads/review/001", None);
    assert_eq!(got["object"]["sha"], commit["sha"]);
    let pr = call(dir, cwd, "POST", "pulls", Some(json!({"title": "t", "head": "review/001", "base": branch, "body": "cover"})));
    let n = pr["number"].as_u64().unwrap();
    assert!(pr["html_url"].as_str().unwrap().ends_with(&format!("pull/{n}")));

    let id: u64 = ctl(dir, &["pr-review-comment", &n.to_string(), "a.rs", "1", "why this?"]).trim().parse().unwrap();
    ctl(dir, &["pr-review", &n.to_string(), "COMMENTED"]);

    let comments = call(dir, cwd, "GET", &format!("pulls/{n}/comments?per_page=100&page=1"), None);
    let c = &comments.as_array().unwrap()[0];
    assert_eq!((c["id"].as_u64(), c["path"].as_str(), c["line"].as_u64(), c["user"]["type"].as_str()), (Some(id), Some("a.rs"), Some(1), Some("User")));
    let reviews = call(dir, cwd, "GET", &format!("pulls/{n}/reviews"), None);
    assert_eq!(reviews[0]["state"], "COMMENTED");

    let reply = call(dir, cwd, "POST", &format!("pulls/{n}/comments/{id}/replies"), Some(json!({"body": "because"})));
    assert!(reply["html_url"].as_str().unwrap().contains("discussion_r"));
    let comments = call(dir, cwd, "GET", &format!("pulls/{n}/comments"), None);
    assert_eq!(comments[1]["in_reply_to_id"].as_u64(), Some(id));
    assert_eq!(comments[1]["user"]["type"], "Bot");

    call(dir, cwd, "POST", &format!("pulls/{n}/reviews"), Some(json!({"event": "REQUEST_CHANGES", "comments": [{"path": "a.rs", "line": 1, "side": "RIGHT", "body": "x"}]})));
    let reviews = call(dir, cwd, "GET", &format!("pulls/{n}/reviews"), None);
    assert_eq!(reviews[1]["state"], "CHANGES_REQUESTED");
    let comments = call(dir, cwd, "GET", &format!("pulls/{n}/comments"), None);
    assert_eq!((comments[2]["body"].as_str(), comments[2]["pull_request_review_id"].clone()), (Some("x"), reviews[1]["id"].clone()));
    let nested = call(dir, cwd, "POST", &format!("pulls/{n}/comments/{}/replies", reply["id"]), Some(json!({"body": "again"})));
    assert_eq!(nested["in_reply_to_id"].as_u64(), Some(id));
    let dup = ["api", "--method", "POST", &format!("{REPO}/pulls")].map(String::from).to_vec();
    let f = dir.join("dup");
    std::fs::write(&f, json!({"title": "t", "head": "review/001", "base": "main"}).to_string()).unwrap();
    let dup = [dup, vec!["--input".into(), f.display().to_string()]].concat();
    assert_eq!(gh::run(dir, cwd, &dup).unwrap().code, 1);
    ctl(dir, &["pr-review", &n.to_string(), "approved", "--author", "sam"]);
    let reviews = call(dir, cwd, "GET", &format!("pulls/{n}/reviews"), None);
    assert_eq!((reviews[2]["state"].as_str(), reviews[2]["user"]["login"].as_str()), (Some("APPROVED"), Some("sam")));
    assert!(gh::ctl(dir, &["pr-review".to_string(), n.to_string(), "bogus".to_string()]).is_err());
    let said: u64 = ctl(dir, &["pr-comment", &n.to_string(), "published"]).trim().parse().unwrap();
    assert_eq!(call(dir, cwd, "GET", &format!("issues/comments/{said}"), None)["user"]["login"], "operator");
    let issue = call(dir, cwd, "POST", &format!("issues/{n}/comments"), Some(json!({"body": "closeout"})));
    assert_eq!(call(dir, cwd, "GET", &format!("issues/{n}/comments"), None)[1]["body"], "closeout");
    let one = call(dir, cwd, "GET", &format!("issues/comments/{}", issue["id"]), None);
    assert!(one["issue_url"].as_str().unwrap().ends_with(&format!("{n}")));

    let missing = ["api", &format!("{REPO}/issues/comments/99999")].map(String::from).to_vec();
    assert_eq!(gh::run(dir, cwd, &missing).unwrap().code, 1);
    assert_eq!(call(dir, cwd, "GET", &format!("pulls/{n}"), None)["state"], "open");
    ctl(dir, &["pr-close", &n.to_string()]);
    let closed = call(dir, cwd, "GET", &format!("pulls/{n}"), None);
    assert_eq!((closed["state"].as_str(), closed["closed_at"].is_string()), (Some("closed"), true));
}

#[test_id(tknaoo1)]
#[test]
fn a_pull_request_whose_branch_was_never_pushed_is_refused_and_a_ref_is_made_once() {
    let t = testkit::TempDir::new("gh-review-refs");
    let dir = t.path();
    let args: Vec<String> = ["api", "--method", "POST", &format!("{REPO}/pulls")].iter().map(|s| s.to_string()).collect();
    let f = dir.join("in");
    std::fs::write(&f, json!({"title": "t", "head": "nope", "base": "main"}).to_string()).unwrap();
    let mut with = args.clone();
    with.extend(["--input".into(), f.display().to_string()]);
    assert_eq!(gh::run(dir, dir, &with).unwrap().code, 1);

    call(dir, dir, "POST", "git/refs", Some(json!({"ref": "refs/heads/x", "sha": "a"})));
    std::fs::write(&f, json!({"ref": "refs/heads/x", "sha": "a"}).to_string()).unwrap();
    let mut again: Vec<String> = ["api", "--method", "POST", &format!("{REPO}/git/refs")].iter().map(|s| s.to_string()).collect();
    again.extend(["--input".into(), f.display().to_string()]);
    assert_eq!(gh::run(dir, dir, &again).unwrap().code, 1);
}
