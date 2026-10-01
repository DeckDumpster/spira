//! Unit tests over `logic::run`, driven entirely through the `ports` traits — no network,
//! no real `bd`, no real `mail.sh`. Replaces the `GH_INTAKE_LIB=1` seam test-gh-intake.sh
//! used to source the bash functions directly (dispatch.md row "GitHub triage/dedup").

use crate::logic::{run, Config};
use crate::ports::{Bd, Http, Mail, Repo};
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Default)]
struct FakeHttp {
    // url -> (status, body)
    responses: RefCell<HashMap<String, (u16, Vec<u8>)>>,
    calls: RefCell<Vec<String>>,
}

impl FakeHttp {
    fn set(&self, url: &str, status: u16, body: &str) {
        self.responses.borrow_mut().insert(url.to_string(), (status, body.as_bytes().to_vec()));
    }
}

impl Http for FakeHttp {
    fn get(&self, url: &str, _timeout_secs: u64) -> Result<(u16, Vec<u8>), String> {
        self.calls.borrow_mut().push(url.to_string());
        self.responses
            .borrow()
            .get(url)
            .cloned()
            .ok_or_else(|| format!("no fixture response for {url}"))
    }
}

#[derive(Default)]
struct FakeBd {
    beads: RefCell<Vec<serde_json::Value>>,
    next_id: RefCell<u32>,
    notes: RefCell<Vec<(String, String)>>,
    closes: RefCell<Vec<(String, String)>>,
    fail_create: RefCell<bool>,
    /// Simulates a labelling defect in `bd` itself: when set, `create()` stores these
    /// labels instead of the ones it was asked to write.
    force_labels: RefCell<Option<Vec<String>>>,
}

impl FakeBd {
    fn seed(&self, external_ref: &str, id: &str, labels: &[&str]) {
        self.beads.borrow_mut().push(json!({
            "id": id,
            "external_ref": external_ref,
            "labels": labels,
        }));
    }
}

impl Bd for FakeBd {
    fn list_all_json(&self) -> Option<serde_json::Value> {
        Some(serde_json::Value::Array(self.beads.borrow().clone()))
    }

    fn create(&self, _title: &str, external_ref: &str, labels: &str, priority: &str, body: &[u8]) -> bool {
        if *self.fail_create.borrow() {
            return false;
        }
        let mut n = self.next_id.borrow_mut();
        *n += 1;
        let id = format!("sp-fake{n}");
        let stored_labels: Vec<String> = match self.force_labels.borrow().clone() {
            Some(l) => l,
            None => labels.split(',').map(String::from).collect(),
        };
        self.beads.borrow_mut().push(json!({
            "id": id,
            "external_ref": external_ref,
            "labels": stored_labels,
            "priority": priority,
            "body_len": body.len(),
        }));
        true
    }

    fn note(&self, id: &str, text: &str) -> bool {
        self.notes.borrow_mut().push((id.to_string(), text.to_string()));
        true
    }

    fn close(&self, id: &str, reason: &str) -> bool {
        self.closes.borrow_mut().push((id.to_string(), reason.to_string()));
        true
    }

    // The closeout-side methods (sp-j3fim) are exercised through `closeout.rs`'s own
    // fakes, not through the ingest side's — these are never called from `logic::run`.
    fn show_json(&self, _id: &str) -> Option<serde_json::Value> {
        None
    }
    fn list_by_label(&self, _status: &str, _label: &str) -> Option<serde_json::Value> {
        None
    }
    fn dep_remove(&self, _id: &str, _other: &str) -> bool {
        false
    }
    fn dep_relate(&self, _from: &str, _to: &str) -> bool {
        false
    }
}

struct FakeRepo {
    root: Option<String>,
}
impl Repo for FakeRepo {
    fn root_with_git(&self, _name: &str) -> Option<String> {
        self.root.clone()
    }
    fn all_names(&self) -> Vec<String> {
        Vec::new()
    }
    fn landref(&self, _repo_path: &str) -> Option<String> {
        None
    }
    fn landrefs(&self, _repo_path: &str) -> Vec<String> {
        Vec::new()
    }
}

#[derive(Default)]
struct FakeMail {
    sent: RefCell<Vec<(String, String)>>,
}
impl Mail for FakeMail {
    fn send_operator_note(&self, subject: &str, body: &[u8]) -> bool {
        self.sent.borrow_mut().push((subject.to_string(), String::from_utf8_lossy(body).to_string()));
        true
    }
    fn send_question(&self, _from: &str, _subject: &str, _default: &str, _body: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

fn base_cfg() -> Config {
    Config {
        repo: "acme/widgets".to_string(),
        bead_repo: "widgets".to_string(),
        scope: "spira".to_string(),
        lane: "plan".to_string(),
        priority: "1".to_string(),
        api: "https://api.example".to_string(),
        untrusted_label: "gh-untrusted".to_string(),
        dry_run: false,
    }
}

fn issues_url(cfg: &Config, page: u32) -> String {
    format!("{}/repos/{}/issues?state=open&per_page=100&page={page}", cfg.api, cfg.repo)
}

#[test]
fn trusted_author_creates_a_work_bead() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(
        &issues_url(&cfg, 1),
        200,
        r#"[{"number":1,"title":"crash on save","body":"steps to repro","user":{"login":"alice"},"labels":[],"author_association":"OWNER"}]"#,
    );
    let bd = FakeBd::default();
    bd.seed("some-other-ref", "sp-existing", &["spira"]); // non-empty store, unrelated
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    let beads = bd.beads.borrow();
    let created = beads.iter().find(|b| b["external_ref"] == "github:acme/widgets#1").expect("work bead created");
    assert_eq!(created["labels"], json!(["spira", "plan", "repo:widgets"]));
    assert_eq!(created["priority"], "1");
}

#[test]
fn rerun_is_idempotent_and_notes_the_existing_bead() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(
        &issues_url(&cfg, 1),
        200,
        r#"[{"number":1,"title":"crash on save","body":"x","user":{"login":"alice"},"labels":[],"author_association":"OWNER"}]"#,
    );
    let bd = FakeBd::default();
    bd.seed("github:acme/widgets#1", "sp-abc12", &["spira", "plan"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    // No second bead created for the same ref.
    let matching: usize = bd.beads.borrow().iter().filter(|b| b["external_ref"] == "github:acme/widgets#1").count();
    assert_eq!(matching, 1);
    assert_eq!(bd.notes.borrow().len(), 1);
    assert_eq!(bd.notes.borrow()[0].0, "sp-abc12");
}

#[test]
fn untrusted_author_gets_an_untrusted_record_and_a_digest() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(
        &issues_url(&cfg, 1),
        200,
        r#"[{"number":9,"title":"feature request","body":"line one\nline two\nline three\nline four","user":{"login":"random_person"},"labels":[],"author_association":"NONE"}]"#,
    );
    let bd = FakeBd::default();
    bd.seed("seed", "sp-seed", &["spira"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    let beads = bd.beads.borrow();
    let rec = beads.iter().find(|b| b["external_ref"] == "github-untrusted:acme/widgets#9").expect("untrusted record created");
    assert_eq!(rec["labels"], json!(["gh-untrusted"]));
    assert_eq!(rec["priority"], "4");
    assert_eq!(mail.sent.borrow().len(), 1);
    assert!(mail.sent.borrow()[0].0.contains("1 new untrusted"));
}

#[test]
fn pull_requests_are_excluded() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(
        &issues_url(&cfg, 1),
        200,
        r#"[{"number":1,"title":"a PR","body":"x","user":{"login":"alice"},"labels":[],"author_association":"OWNER","pull_request":{}},
            {"number":2,"title":"a real issue","body":"x","user":{"login":"alice"},"labels":[],"author_association":"OWNER"}]"#,
    );
    let bd = FakeBd::default();
    bd.seed("seed", "sp-seed", &["spira"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    let beads = bd.beads.borrow();
    assert!(beads.iter().any(|b| b["external_ref"] == "github:acme/widgets#2"));
    assert!(!beads.iter().any(|b| b["external_ref"] == "github:acme/widgets#1"));
}

#[test]
fn spira_accept_promotes_when_the_actor_has_access() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(
        &issues_url(&cfg, 1),
        200,
        r#"[{"number":5,"title":"promoted","body":"x","user":{"login":"newbie"},"labels":[{"name":"spira:accept"}],"author_association":"NONE"}]"#,
    );
    let events_url = format!("{}/repos/{}/issues/5/events", cfg.api, cfg.repo);
    http.set(&events_url, 200, r#"[{"event":"labeled","label":{"name":"spira:accept"},"actor":{"login":"maintainer1"}}]"#);
    let org_url = format!("{}/orgs/acme/members/maintainer1", cfg.api);
    http.set(&org_url, 204, "");

    let bd = FakeBd::default();
    bd.seed("github-untrusted:acme/widgets#5", "sp-untr5", &[]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    let beads = bd.beads.borrow();
    assert!(beads.iter().any(|b| b["external_ref"] == "github:acme/widgets#5"));
    assert_eq!(bd.closes.borrow().len(), 1, "the untrusted record is closed on promotion");
    assert_eq!(bd.closes.borrow()[0].0, "sp-untr5");
}

#[test]
fn spira_accept_does_not_promote_without_access() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(
        &issues_url(&cfg, 1),
        200,
        r#"[{"number":6,"title":"not promoted","body":"x","user":{"login":"newbie"},"labels":[{"name":"spira:accept"}],"author_association":"NONE"}]"#,
    );
    let events_url = format!("{}/repos/{}/issues/6/events", cfg.api, cfg.repo);
    http.set(&events_url, 200, r#"[{"event":"labeled","label":{"name":"spira:accept"},"actor":{"login":"randomuser"}}]"#);
    let org_url = format!("{}/orgs/acme/members/randomuser", cfg.api);
    http.set(&org_url, 404, "");
    let perm_url = format!("{}/repos/{}/collaborators/randomuser/permission", cfg.api, cfg.repo);
    http.set(&perm_url, 200, r#"{"permission":"read"}"#);

    let bd = FakeBd::default();
    bd.seed("seed", "sp-seed", &["spira"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    let beads = bd.beads.borrow();
    assert!(!beads.iter().any(|b| b["external_ref"] == "github:acme/widgets#6"));
    assert!(beads.iter().any(|b| b["external_ref"] == "github-untrusted:acme/widgets#6"));
}

#[test]
fn dry_run_makes_no_bd_writes() {
    let cfg = Config { dry_run: true, ..base_cfg() };
    let http = FakeHttp::default();
    http.set(
        &issues_url(&cfg, 1),
        200,
        r#"[{"number":1,"title":"x","body":"x","user":{"login":"alice"},"labels":[],"author_association":"OWNER"},
            {"number":2,"title":"y","body":"y","user":{"login":"bob"},"labels":[],"author_association":"NONE"}]"#,
    );
    let bd = FakeBd::default();
    bd.seed("seed", "sp-seed", &["spira"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    assert_eq!(bd.beads.borrow().len(), 1, "only the seed bead — nothing created under --dry-run");
    assert!(mail.sent.borrow().is_empty());
    assert!(report.out.iter().any(|l| l.contains("would create work bead")));
    assert!(report.out.iter().any(|l| l.contains("would record untrusted")));
}

#[test]
fn pagination_follows_a_full_first_page() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    let mut page1: Vec<String> = (1..=100)
        .map(|n| format!(r#"{{"number":{n},"title":"t{n}","body":"b","user":{{"login":"alice"}},"labels":[],"author_association":"OWNER"}}"#))
        .collect();
    http.set(&issues_url(&cfg, 1), 200, &format!("[{}]", page1.join(",")));
    http.set(&issues_url(&cfg, 2), 200, r#"[{"number":101,"title":"last","body":"b","user":{"login":"alice"},"labels":[],"author_association":"OWNER"}]"#);
    page1.clear();

    let bd = FakeBd::default();
    bd.seed("seed", "sp-seed", &["spira"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    assert!(bd.beads.borrow().iter().any(|b| b["external_ref"] == "github:acme/widgets#101"), "second page was fetched");
    assert_eq!(http.calls.borrow().iter().filter(|c| c.contains("page=1") || c.contains("page=2")).count(), 2);
}

#[test]
fn an_unresolvable_repo_map_entry_is_refused() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    let bd = FakeBd::default();
    let repo = FakeRepo { root: None };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 1);
    assert!(report.err.iter().any(|l| l.contains("does not resolve to a checkout")));
}

#[test]
fn a_store_reporting_zero_beads_is_refused_not_treated_as_empty() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    let bd = FakeBd::default(); // no seed — genuinely zero, or the query is broken
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 1);
    assert!(report.err.iter().any(|l| l.contains("the store reports zero beads")));
}

#[test]
fn an_empty_configured_repo_is_a_real_no_op() {
    let cfg = Config { repo: String::new(), ..base_cfg() };
    let http = FakeHttp::default();
    let bd = FakeBd::default();
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 0);
    assert!(report.out.iter().any(|l| l.contains("SPIRA_GH_INTAKE_REPO is not set")));
    assert!(http.calls.borrow().is_empty(), "nothing was fetched");
}

#[test]
fn the_api_refusing_a_page_is_named_not_swallowed() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(&issues_url(&cfg, 1), 404, r#"{"message":"Not Found"}"#);
    let bd = FakeBd::default();
    bd.seed("seed", "sp-seed", &["spira"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 1);
    assert!(report.err.iter().any(|l| l.contains("Not Found")));
}

#[test]
fn a_bead_the_store_silently_dropped_the_scope_label_from_is_caught_by_the_post_check() {
    // Simulates a labelling defect in `bd` itself (not in gh-intake's own call): the store
    // wrote something other than the labels it was asked to. The post-check re-reads the
    // store rather than trusting the write, and must refuse rather than report ok.
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(
        &issues_url(&cfg, 1),
        200,
        r#"[{"number":1,"title":"x","body":"x","user":{"login":"alice"},"labels":[],"author_association":"OWNER"}]"#,
    );
    let bd = FakeBd::default();
    bd.seed("seed", "sp-seed", &["spira"]);
    *bd.force_labels.borrow_mut() = Some(vec!["plan".to_string(), "repo:widgets".to_string()]); // no "spira"

    let repo = FakeRepo { root: Some("/repos/widgets".to_string()) };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &cfg);

    assert_eq!(report.code, 1);
    assert!(report.err.iter().any(|l| l.contains("carry no spira label")));
    assert!(report.err.iter().any(|l| l.contains("github:acme/widgets#1")));
}

#[test]
fn valid_priority_accepts_0_to_4_only() {
    for p in ["0", "1", "2", "3", "4"] {
        assert!(crate::logic::valid_priority(p), "{p} should be valid");
    }
    for p in ["5", "-1", "a", "", "01"] {
        assert!(!crate::logic::valid_priority(p), "{p} should be invalid");
    }
}

#[test]
fn model_association_trust() {
    use crate::model::association_trusted;
    for a in ["OWNER", "MEMBER", "COLLABORATOR"] {
        assert!(association_trusted(a));
    }
    for a in ["CONTRIBUTOR", "FIRST_TIME_CONTRIBUTOR", "FIRST_TIMER", "NONE"] {
        assert!(!association_trusted(a));
    }
}

#[test]
fn model_accept_label_is_exact_token() {
    use crate::model::has_accept_label;
    assert!(has_accept_label(&["spira:accept".to_string()]));
    assert!(!has_accept_label(&["spira:accepted".to_string()]));
    assert!(!has_accept_label(&[]));
}

#[test]
fn model_last_accept_actor_takes_the_most_recent() {
    use crate::model::last_accept_actor;
    let events = r#"[
        {"event":"labeled","label":{"name":"spira:accept"},"actor":{"login":"first"}},
        {"event":"unlabeled","label":{"name":"spira:accept"},"actor":{"login":"first"}},
        {"event":"labeled","label":{"name":"spira:accept"},"actor":{"login":"second"}}
    ]"#;
    assert_eq!(last_accept_actor(events.as_bytes()), "second");
}

#[test]
fn model_permission_grants_access_rules() {
    use crate::model::permission_grants_access;
    for p in ["admin", "maintain", "write"] {
        assert!(permission_grants_access(format!(r#"{{"permission":"{p}"}}"#).as_bytes()));
    }
    assert!(!permission_grants_access(br#"{"permission":"read"}"#));
    assert!(!permission_grants_access(b"not json"));
}
