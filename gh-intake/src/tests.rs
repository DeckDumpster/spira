//! Unit tests over `logic::run`, driven entirely through the `ports` traits — no network,
//! no real `bd`, no real `mail.sh`. Replaces the `GH_INTAKE_LIB=1` seam test-gh-intake.sh
//! used to source the bash functions directly (dispatch.md row "GitHub triage/dedup").

use crate::logic::{run, Config};
use crate::ports::{Bd, Http, Mail, Repo, Seen, Stamp};
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
            "body": String::from_utf8_lossy(body),
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
    fn list_asks(&self, _which: spira_config::nonwork::Which, _label: &str) -> Option<serde_json::Value> {
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
    /// (registered name, forge slug)
    registered: Vec<(&'static str, &'static str)>,
}
impl Repo for FakeRepo {
    fn root_with_git(&self, _name: &str) -> Option<String> {
        self.root.clone()
    }
    fn forge_slug(&self, name: &str) -> Option<String> {
        self.registered.iter().find(|(n, _)| *n == name).map(|(_, s)| s.to_string())
    }
    fn all_names(&self) -> Vec<String> {
        self.registered.iter().map(|(n, _)| n.to_string()).collect()
    }
    fn landref(&self, _repo_path: &str) -> Option<String> {
        None
    }
    fn landrefs(&self, _repo_path: &str) -> Vec<String> {
        Vec::new()
    }
}

#[derive(Default)]
struct FakeSeen {
    held: RefCell<HashMap<String, Vec<u64>>>,
}
impl Seen for FakeSeen {
    fn load(&self, slug: &str) -> Option<Vec<u64>> {
        self.held.borrow().get(slug).cloned()
    }
    fn save(&self, slug: &str, numbers: &[u64]) {
        self.held.borrow_mut().insert(slug.to_string(), numbers.to_vec());
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
    fn send_question(&self, _from: &str, _subject: &str, _default: &str, _bead_id: &str, _body: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

struct FakeStamp {
    now: i64,
    last: RefCell<Option<i64>>,
}
impl Stamp for FakeStamp {
    fn now(&self) -> i64 {
        self.now
    }
    fn last_ok(&self) -> Option<i64> {
        *self.last.borrow()
    }
    fn mark_ok(&self, at: i64) {
        *self.last.borrow_mut() = Some(at);
    }
}

fn stamp() -> FakeStamp {
    FakeStamp { now: 1_000_000, last: RefCell::new(None) }
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

/// A repository intake has already read once, so the first-pass rule is not in play.
fn seen_of(cfg: &Config) -> FakeSeen {
    let seen = FakeSeen::default();
    seen.save(&cfg.repo, &[]);
    seen
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
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

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
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

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
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

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
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

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
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

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
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

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
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

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
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    assert!(bd.beads.borrow().iter().any(|b| b["external_ref"] == "github:acme/widgets#101"), "second page was fetched");
    assert_eq!(http.calls.borrow().iter().filter(|c| c.contains("page=1") || c.contains("page=2")).count(), 2);
}

#[test]
fn an_unresolvable_repo_map_entry_is_refused() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    let bd = FakeBd::default();
    let repo = FakeRepo { root: None, registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

    assert_eq!(report.code, 1);
    assert!(report.err.iter().any(|l| l.contains("does not resolve to a checkout")));
}

#[test]
fn a_store_reporting_zero_beads_is_refused_not_treated_as_empty() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    let bd = FakeBd::default(); // no seed — genuinely zero, or the query is broken
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

    assert_eq!(report.code, 1);
    assert!(report.err.iter().any(|l| l.contains("the store reports zero beads")));
}

#[test]
fn no_configured_repo_and_none_registered_is_a_real_no_op() {
    let cfg = Config { repo: String::new(), ..base_cfg() };
    let http = FakeHttp::default();
    let bd = FakeBd::default();
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

    assert_eq!(report.code, 0);
    assert!(report.out.iter().any(|l| l.contains("nothing to ingest")));
    assert!(http.calls.borrow().is_empty(), "nothing was fetched");
}

#[test]
fn the_api_refusing_a_page_is_named_not_swallowed() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(&issues_url(&cfg, 1), 404, r#"{"message":"Not Found"}"#);
    let bd = FakeBd::default();
    bd.seed("seed", "sp-seed", &["spira"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

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

    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

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

const RATE_BODY: &str = r#"{"message":"API rate limit exceeded for 203.0.113.9. (But here's the good news: Authenticated requests get a higher rate limit.)"}"#;

fn rate_limited_world(status: u16) -> (Config, FakeHttp, FakeBd, FakeRepo, FakeMail) {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(&issues_url(&cfg, 1), status, RATE_BODY);
    http.set(&format!("{}/rate_limit", cfg.api), 200, r#"{"resources":{"core":{"limit":60,"remaining":0,"reset":1792773140}}}"#);
    let bd = FakeBd::default();
    bd.seed("some-other-ref", "sp-existing", &["spira"]);
    (cfg, http, bd, FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] }, FakeMail::default())
}

#[test]
fn a_rate_limit_refusal_is_a_logged_deferral_not_a_failure() {
    for status in [403u16, 429] {
        let (cfg, http, bd, repo, mail) = rate_limited_world(status);
        let st = stamp();
        *st.last.borrow_mut() = Some(st.now - 600);
        let report = run(&http, &bd, &repo, &mail, &st, &seen_of(&cfg), &cfg);
        assert_eq!(report.code, 0, "status {status}: {:?}", report.err);
        let log = report.out.join("\n");
        assert!(log.contains("deferred") && log.contains("1792773140"), "{log}");
        assert_eq!(*st.last.borrow(), Some(st.now - 600), "a deferral is not a success");
    }
}

#[test]
fn refusals_past_the_bound_alarm_naming_the_rate_limit_and_the_last_success() {
    let (cfg, http, bd, repo, mail) = rate_limited_world(403);
    let st = stamp();
    let last = st.now - crate::logic::RATE_LIMIT_ALARM_SECS - 1;
    *st.last.borrow_mut() = Some(last);
    let report = run(&http, &bd, &repo, &mail, &st, &seen_of(&cfg), &cfg);
    assert_eq!(report.code, 1);
    let err = report.err.join("\n");
    assert!(err.contains("rate limit") && err.contains(&last.to_string()), "{err}");
}

#[test]
fn a_first_ever_refusal_seeds_the_clock_and_does_not_alarm() {
    let (cfg, http, bd, repo, mail) = rate_limited_world(429);
    let st = stamp();
    let report = run(&http, &bd, &repo, &mail, &st, &seen_of(&cfg), &cfg);
    assert_eq!(report.code, 0);
    assert_eq!(*st.last.borrow(), Some(st.now));
}

#[test]
fn a_completed_poll_marks_success() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(&issues_url(&cfg, 1), 200, "[]");
    let bd = FakeBd::default();
    bd.seed("some-other-ref", "sp-existing", &["spira"]);
    let st = stamp();
    run(&http, &bd, &FakeRepo { root: Some("/r".to_string()), registered: vec![] }, &FakeMail::default(), &st, &seen_of(&cfg), &cfg);
    assert_eq!(*st.last.borrow(), Some(st.now));
}

fn issue_json(number: u64, assoc: &str) -> String {
    format!(r#"{{"number":{number},"title":"issue {number}","body":"b","user":{{"login":"u"}},"author_association":"{assoc}","labels":[]}}"#)
}

fn two_repo_world() -> (Config, FakeHttp, FakeBd, FakeRepo) {
    let cfg = Config { repo: String::new(), ..base_cfg() };
    let http = FakeHttp::default();
    let bd = FakeBd::default();
    bd.seed("some-other-ref", "sp-existing", &["spira"]);
    let repo = FakeRepo {
        root: None,
        registered: vec![("widgets", "acme/widgets"), ("gadgets", "acme/gadgets")],
    };
    (cfg, http, bd, repo)
}

fn url_for(cfg: &Config, slug: &str) -> String {
    format!("{}/repos/{slug}/issues?state=open&per_page=100&page=1", cfg.api)
}

#[test]
fn an_issue_on_a_second_registered_repo_becomes_a_bead_tagged_with_that_repo() {
    let (cfg, http, bd, repo) = two_repo_world();
    http.set(&url_for(&cfg, "acme/widgets"), 200, "[]");
    http.set(&url_for(&cfg, "acme/gadgets"), 200, &format!("[{}]", issue_json(7, "MEMBER")));
    let seen = FakeSeen::default();
    seen.save("acme/widgets", &[]);
    seen.save("acme/gadgets", &[]);

    let report = run(&http, &bd, &repo, &FakeMail::default(), &stamp(), &seen, &cfg);

    assert_eq!(report.code, 0, "{:?}", report.err);
    let beads = bd.beads.borrow();
    let made = beads.iter().find(|b| b["external_ref"] == "github:acme/gadgets#7").expect("bead filed");
    assert!(made["labels"].as_array().unwrap().iter().any(|l| l == "repo:gadgets"), "{made}");
}

#[test]
fn the_first_read_of_a_repo_records_its_open_issues_and_files_nothing() {
    let (cfg, http, bd, repo) = two_repo_world();
    http.set(&url_for(&cfg, "acme/widgets"), 200, "[]");
    http.set(&url_for(&cfg, "acme/gadgets"), 200, &format!("[{},{}]", issue_json(3, "MEMBER"), issue_json(4, "NONE")));
    let seen = FakeSeen::default();

    let before = bd.beads.borrow().len();
    let report = run(&http, &bd, &repo, &FakeMail::default(), &stamp(), &seen, &cfg);

    assert_eq!(report.code, 0, "{:?}", report.err);
    assert_eq!(bd.beads.borrow().len(), before, "a backlog is recorded, never filed");
    assert_eq!(seen.load("acme/gadgets"), Some(vec![3, 4]));
    assert_eq!(seen.load("acme/widgets"), Some(vec![]), "a quiet repo is still marked read");

    http.set(&url_for(&cfg, "acme/gadgets"), 200, &format!("[{},{},{}]", issue_json(3, "MEMBER"), issue_json(4, "NONE"), issue_json(9, "MEMBER")));
    let report = run(&http, &bd, &repo, &FakeMail::default(), &stamp(), &seen, &cfg);
    assert_eq!(report.code, 0, "{:?}", report.err);
    let beads = bd.beads.borrow();
    assert!(beads.iter().any(|b| b["external_ref"] == "github:acme/gadgets#9"), "a new issue after the first pass is filed");
    assert!(!beads.iter().any(|b| b["external_ref"] == "github:acme/gadgets#3"), "a seen issue stays unfiled");
}

#[test]
fn a_rate_limit_on_one_repo_defers_the_rest_of_the_pass() {
    let (cfg, http, bd, repo) = two_repo_world();
    http.set(&url_for(&cfg, "acme/widgets"), 429, RATE_BODY);
    http.set(&format!("{}/rate_limit", cfg.api), 200, r#"{"resources":{"core":{"reset":1792773140}}}"#);
    let st = stamp();
    *st.last.borrow_mut() = Some(st.now - 600);
    let report = run(&http, &bd, &repo, &FakeMail::default(), &st, &FakeSeen::default(), &cfg);
    assert_eq!(report.code, 0, "{:?}", report.err);
    assert!(!http.calls.borrow().iter().any(|u| u.contains("acme/gadgets")));
}

#[test]
fn github_slug_reads_every_remote_shape() {
    use crate::model::github_slug;
    for u in ["git@github.com:acme/widgets.git", "https://github.com/acme/widgets", "ssh://git@github.com/acme/widgets.git", "https://github.com/acme/widgets/\n"] {
        assert_eq!(github_slug(u).as_deref(), Some("acme/widgets"), "{u}");
    }
    for u in ["/srv/git/widgets.git", "git@gitlab.com:acme/widgets.git", "https://github.com/acme", "https://github.com/a/b/c"] {
        assert_eq!(github_slug(u), None, "{u}");
    }
}

#[test]
fn contributor_with_hidden_org_membership_is_named_unverifiable() {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(
        &issues_url(&cfg, 1),
        200,
        r#"[{"number":7,"title":"a","body":"x","user":{"login":"hidden1"},"labels":[],"author_association":"CONTRIBUTOR"},
            {"number":8,"title":"b","body":"x","user":{"login":"hidden1"},"labels":[],"author_association":"CONTRIBUTOR"}]"#,
    );
    http.set(&format!("{}/orgs/acme/members/hidden1", cfg.api), 404, "");
    let bd = FakeBd::default();
    bd.seed("seed", "sp-seed", &["spira"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let mail = FakeMail::default();

    let report = run(&http, &bd, &repo, &mail, &stamp(), &seen_of(&cfg), &cfg);

    assert_eq!(report.code, 0, "stderr: {:?}", report.err);
    assert!(report.out.iter().chain(report.err.iter()).any(|l| l.contains("private member, unverifiable") && l.contains("hidden1")));
    let notes: Vec<_> = mail.sent.borrow().iter().filter(|(s, _)| s.contains("unverifiable")).cloned().collect();
    assert_eq!(notes.len(), 1, "once per author");
    assert!(notes[0].1.contains("make their org membership public"));
}

fn bot_issue(login: &str, ty: &str, labels: &str) -> String {
    format!(r#"[{{"number":5,"title":"acceptance red","body":"b","user":{{"login":"{login}","type":"{ty}"}},"author_association":"NONE","labels":[{labels}]}}]"#)
}

fn run_bot_issue(login: &str, ty: &str, labels: &str) -> FakeBd {
    let cfg = base_cfg();
    let http = FakeHttp::default();
    http.set(&issues_url(&cfg, 1), 200, &bot_issue(login, ty, labels));
    let bd = FakeBd::default();
    bd.seed("some-other-ref", "sp-existing", &["spira"]);
    let repo = FakeRepo { root: Some("/repos/widgets".to_string()), registered: vec![] };
    let report = run(&http, &bd, &repo, &FakeMail::default(), &stamp(), &seen_of(&cfg), &cfg);
    assert_eq!(report.code, 0, "{:?}", report.err);
    bd
}

fn work_beads(bd: &FakeBd) -> Vec<serde_json::Value> {
    bd.beads.borrow().iter().filter(|b| b["external_ref"] == "github:acme/widgets#5").cloned().collect()
}

#[test]
fn own_bot_on_an_acceptance_issue_becomes_a_work_bead_with_provenance() {
    let bd = run_bot_issue("github-actions[bot]", "Bot", r#"{"name":"spira:acceptance"}"#);
    let made = work_beads(&bd);
    assert_eq!(made.len(), 1);
    assert!(made[0]["body"].as_str().unwrap().contains("Admitted: the repo's own github-actions[bot]"));
}

#[test]
fn own_bot_without_the_acceptance_label_is_not_trusted() {
    let bd = run_bot_issue("github-actions[bot]", "Bot", "");
    assert!(work_beads(&bd).is_empty());
}

#[test]
fn another_bot_with_the_acceptance_label_is_not_trusted() {
    let bd = run_bot_issue("dependabot[bot]", "Bot", r#"{"name":"spira:acceptance"}"#);
    assert!(work_beads(&bd).is_empty());
}

#[test]
fn a_user_named_like_the_bot_is_not_trusted() {
    let bd = run_bot_issue("github-actions[bot]", "User", r#"{"name":"spira:acceptance"}"#);
    assert!(work_beads(&bd).is_empty());
}
