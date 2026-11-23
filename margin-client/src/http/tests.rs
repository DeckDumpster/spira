use super::*;
use crate::{scenario, Anchor, Comment, Doc, Margin, Source, READER, TOKEN_FILE_KEY, URL_KEY};
use serde::Deserialize;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;
use testkit::TempDir;

const TOKEN: &str = "s3cret-agent-token";

fn read_request(s: &mut std::net::TcpStream) -> String {
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = s.read(&mut buf).unwrap();
        seen.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&seen).to_string();
        if let Some(at) = text.find("\r\n\r\n") {
            let want: usize = text.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
            if seen.len() >= at + 4 + want {
                return text;
            }
        }
    }
}

/// Serves one request with `reply` as its JSON body and hands back the raw request text.
fn serve_once(reply: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let seen = read_request(&mut s);
        write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        seen
    });
    (base, h)
}

fn client(dir: &TempDir, base: &str) -> HttpMargin {
    let file = dir.path().join("token");
    std::fs::write(&file, TOKEN).unwrap();
    HttpMargin { base: base.to_string(), token_file: file.to_str().unwrap().to_string(), timeout: Duration::from_secs(CALL_SECS) }
}

const RECORDING: &str = include_str!("../../fixtures/scenario.json");

#[derive(Deserialize, Clone)]
struct Exchange {
    method: String,
    path: String,
    query: String,
    body: Value,
    status: u16,
    reply: String,
}

fn recording() -> Vec<Exchange> {
    serde_json::from_str(RECORDING).unwrap()
}

/// Serves `script` in order, one request each, and hands back every way a request differed from its recording.
fn replay(script: Vec<Exchange>) -> (String, std::thread::JoinHandle<(Vec<String>, usize)>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let h = std::thread::spawn(move || {
        let mut differed = Vec::new();
        let mut bearers = 0;
        for want in script {
            let (mut s, _) = l.accept().unwrap();
            let raw = read_request(&mut s);
            bearers += usize::from(raw.to_ascii_lowercase().contains(&format!("authorization: bearer {TOKEN}").to_ascii_lowercase()));
            let (head, body) = raw.split_once("\r\n\r\n").unwrap();
            let mut first = head.lines().next().unwrap().split(' ');
            let (method, target) = (first.next().unwrap(), first.next().unwrap());
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            let body: Value = if body.is_empty() { Value::Null } else { serde_json::from_str(body).unwrap() };
            if (method, path, query, &body) != (want.method.as_str(), want.path.as_str(), want.query.as_str(), &want.body) {
                differed.push(format!("sent {method} {path}?{query} {body}; the recording has {} {}?{} {}", want.method, want.path, want.query, want.body));
            }
            write!(s, "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", want.status, want.reply.len(), want.reply).unwrap();
        }
        (differed, bearers)
    });
    (base, h)
}

fn exchange(method: &str, path: &str, query: &str, body: Value, reply: &str) -> Exchange {
    Exchange { method: method.into(), path: path.into(), query: query.into(), body, status: 200, reply: reply.into() }
}

#[test_id(t7odd0c)]
#[test]
fn posting_a_collection_sends_the_cover_as_cover_note_and_reads_only_id_and_url() {
    let dir = TempDir::new("margin-collection");
    let (base, served) = replay(recording()[..1].to_vec());
    let docs = [Doc { path: "src/a.rs".into(), kind: "code".into(), text: scenario::FIRST.into() }, Doc { path: "src/b.rs".into(), kind: "code".into(), text: "fn b() {}\n".into() }];
    let got = client(&dir, &base).post_collection("Scenario", "cover words", &docs, None).unwrap();
    assert_eq!((got.id.as_str(), got.url.as_str()), ("dd7ca9c501e39498", "http://127.0.0.1:18931/c/dd7ca9c501e39498"));
    assert_eq!(served.join().unwrap().0, Vec::<String>::new());
}

#[test_id(tsrcjsn)]
#[test]
fn a_source_reaches_the_request_json() {
    let dir = TempDir::new("margin-source");
    let src = Source { repository: "https://github.com/acme/demo".into(), commit: "c0ffee".into() };
    let mut want = recording()[0].clone();
    want.body["source"] = serde_json::json!({"repository": "https://github.com/acme/demo", "commit": "c0ffee"});
    let (base, served) = replay(vec![want]);
    let docs = [Doc { path: "src/a.rs".into(), kind: "code".into(), text: scenario::FIRST.into() }, Doc { path: "src/b.rs".into(), kind: "code".into(), text: "fn b() {}\n".into() }];
    client(&dir, &base).post_collection("Scenario", "cover words", &docs, Some(&src)).unwrap();
    assert_eq!(served.join().unwrap().0, Vec::<String>::new());
}

#[test_id(tz5vx4t)]
#[test]
fn the_recorded_session_replays_request_for_request_and_parses_every_reply() {
    let dir = TempDir::new("margin-session");
    let (base, served) = replay(recording());
    let seen = scenario::run(&client(&dir, &base)).unwrap();
    let (differed, bearers) = served.join().unwrap();
    assert_eq!(differed, Vec::<String>::new());
    let browser_calls = 3;
    assert_eq!(bearers, recording().len() - browser_calls, "every call the client makes carries the token file's bearer; the operator's browser calls carry none");

    assert_eq!(seen.open.state, "open");
    assert_eq!(seen.open.published_at_ms, None);
    assert_eq!(seen.held.url, format!("{base}/c/{}#thread-{}", seen.collection.id, seen.held.thread));
    assert_eq!(seen.reply.thread, seen.held.thread, "a reply answers into the same thread");

    let t = &seen.agent_thread;
    assert_eq!((t.id.as_str(), t.anchor.path.as_str(), t.anchor.first, t.anchor.last), (seen.held.thread.as_str(), "src/a.rs", 2, 3));
    assert_eq!((t.document.path.as_str(), t.document.version), ("src/a.rs", 1));
    assert_eq!(t.comments.len(), 2);
    assert!(t.comments[0].by_agent() && t.comments[0].author == "concierge" && t.comments[0].author != READER, "{:?}", t.comments[0]);
    assert!(t.comments[0].created_at_ms > 0 && t.comments[0].created_at_ms.is_multiple_of(1000), "seconds become ms: {:?}", t.comments[0]);

    assert_eq!(seen.second_version, 2);
    assert_eq!((seen.first_text.as_str(), seen.second_text.as_str()), (scenario::FIRST, scenario::SECOND));

    assert_eq!(seen.all.len(), 2, "held and reader threads are both listed");
    let his = seen.all.iter().find(|t| t.id == seen.his_on_first_version).unwrap();
    assert_eq!((his.document.version, his.anchor.path.as_str()), (1, "src/a.rs"), "a thread on the older version resolves through the versions listing");
    assert!(!his.comments[0].by_agent());
    let carried = seen.all.iter().find(|t| t.id == seen.held.thread).unwrap();
    assert_eq!(carried.document.version, 2);
    assert_eq!(carried.comments.len(), 2, "the held reply is in the held thread");

    assert_eq!(seen.published.state, "published");
    assert!(seen.published.published_at_ms.is_some_and(|ms| ms > 0 && ms.is_multiple_of(1000)));
    assert!(seen.changes.through >= 3 && seen.changes.changes.len() as u64 >= 3, "{:?}", seen.changes);
    assert_eq!(seen.closed.state, "closed");
    assert_eq!(seen.closed.published_at_ms, None);
}

#[test_id(tbsw3yp)]
#[test]
fn only_the_reader_is_not_an_agent() {
    let by = |author: &str| Comment { id: "c".into(), author: author.into(), body: String::new(), created_at_ms: 0 }.by_agent();
    assert_eq!((by("reader"), by("concierge"), by("agent:concierge"), by("ryan")), (false, true, true, true));
}

fn thread_json(id: &str, version_id: i64) -> String {
    format!(r#"{{"id":"{id}","version_id":{version_id},"first_line":1,"last_line":1,"comments":[{{"id":"k{id}","author":"concierge","body":"b","visibility":"now","created_at":5}}]}}"#)
}

const ONE_DOC: &str = r#"{"title":"T","state":"open","updated_at":9,"documents":[{"id":"d1","version_id":7,"path":"a.rs","version":3}]}"#;

#[test_id(tgb8mp0)]
#[test]
fn a_thread_listing_of_two_pages_is_followed_by_its_cursor_to_the_end() {
    let dir = TempDir::new("margin-pages");
    let first = format!(r#"{{"items":[{}],"next":"5.t1"}}"#, thread_json("t1", 7));
    let second = format!(r#"{{"items":[{}],"next":null}}"#, thread_json("t2", 7));
    let (base, served) = replay(vec![
        exchange("GET", "/api/collections/c1/threads", "limit=200", Value::Null, &first), // not a path: the reader's route
        exchange("GET", "/api/collections/c1/threads", "limit=200&after=5.t1", Value::Null, &second), // not a path: the reader's route
        exchange("GET", "/api/collections/c1", "", Value::Null, ONE_DOC), // not a path: the reader's route
    ]);
    let got = client(&dir, &base).threads("c1").unwrap();
    assert_eq!(got.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t1", "t2"]);
    assert_eq!(served.join().unwrap().0, Vec::<String>::new());
}

#[test_id(thrnbwf)]
#[test]
fn a_thread_on_a_version_past_the_first_page_of_versions_resolves() {
    let dir = TempDir::new("margin-old-version");
    let (v1, v2) = (r#"{"items":[{"id":1,"number":1,"created_at":1}],"next":"1"}"#, r#"{"items":[{"id":4,"number":2,"created_at":2}],"next":null}"#);
    let (base, served) = replay(vec![
        exchange("GET", "/api/threads/t1", "", Value::Null, &thread_json("t1", 4)), // not a path: the reader's route
        exchange("GET", "/api/collections/c1", "", Value::Null, ONE_DOC), // not a path: the reader's route
        exchange("GET", "/api/documents/d1/versions", "limit=200", Value::Null, v1), // not a path: the reader's route
        exchange("GET", "/api/documents/d1/versions", "limit=200&after=1", Value::Null, v2), // not a path: the reader's route
    ]);
    let t = client(&dir, &base).thread("c1", "t1").unwrap();
    assert_eq!((t.document.id.as_str(), t.document.version, t.anchor.path.as_str()), ("d1", 2, "a.rs"));
    assert_eq!(served.join().unwrap().0, Vec::<String>::new());
}

#[test_id(tbj2fvn)]
#[test]
fn a_thread_whose_version_no_document_holds_is_an_error_not_a_guess() {
    let dir = TempDir::new("margin-unresolved");
    let (base, _served) = replay(vec![
        exchange("GET", "/api/threads/t1", "", Value::Null, &thread_json("t1", 99)), // not a path: the reader's route
        exchange("GET", "/api/collections/c1", "", Value::Null, ONE_DOC), // not a path: the reader's route
        exchange("GET", "/api/documents/d1/versions", "limit=200", Value::Null, r#"{"items":[{"id":1,"number":1,"created_at":1}],"next":null}"#), // not a path: the reader's route
    ]);
    let e = client(&dir, &base).thread("c1", "t1").unwrap_err();
    assert!(e.contains("99"), "{e}");
}

#[test_id(tm2f5y9)]
#[test]
fn a_feed_wait_longer_than_the_reader_allows_is_asked_for_at_thirty_seconds() {
    let dir = TempDir::new("margin-wait");
    let (base, served) = replay(vec![exchange("GET", "/api/changes", "after=4&wait=30", Value::Null, r#"{"changes":[],"last_seq":4}"#)]); // not a path: the reader's route
    let got = client(&dir, &base).changes(4, 90).unwrap();
    assert_eq!((got.through, got.changes.len()), (4, 0));
    assert_eq!(served.join().unwrap().0, Vec::<String>::new());
}

#[test_id(tegpxm2)]
#[test]
fn an_empty_url_key_or_a_missing_token_file_is_refused_by_name_without_the_token() {
    let cfg = |url: &'static str, file: String| move |k: &str| Ok::<_, String>(if k == URL_KEY { url.to_string() } else { file.clone() });
    let e = HttpMargin::configured(cfg("", "token".into())).err().unwrap();
    assert!(e.contains(URL_KEY), "{e}");
    let dir = TempDir::new("margin-missing");
    let missing = dir.path().join("absent").to_str().unwrap().to_string();
    let m = HttpMargin::configured(cfg("http://127.0.0.1:1", missing.clone())).unwrap();
    let e = m.close("c1").unwrap_err();
    assert!(e.contains(TOKEN_FILE_KEY) && e.contains(&missing), "{e}");
    let e = HttpMargin::configured(cfg("http://r", String::new())).err().unwrap();
    assert!(e.contains(TOKEN_FILE_KEY), "{e}");
    let (base, _h) = serve_once("{}");
    let e = client(&dir, &base).thread("c", "t").unwrap_err();
    assert!(!e.contains(TOKEN), "{e}");
}

#[test_id(tt2eidx)]
#[test]
fn a_reader_that_accepts_and_never_answers_fails_post_comment_naming_url_and_route() {
    let dir = TempDir::new("margin-silent");
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let (release, released) = std::sync::mpsc::channel::<()>();
    let hold = std::thread::spawn(move || {
        let held = l.accept();
        let _ = released.recv();
        drop(held);
    });
    let a = Anchor { path: "a.rs".into(), first: 1, last: 1 };
    let mut m = client(&dir, &base);
    m.timeout = Duration::from_millis(300);
    let e = m.post_comment("c1", &a, "b", true, None).unwrap_err();
    assert!(e.contains(&base) && e.contains("POST /api/collections/c1/threads") && !e.contains(TOKEN), "{e}");
    release.send(()).unwrap();
    hold.join().unwrap();
}

#[test_id(t2qs0mm)]
#[test]
fn a_configured_client_waits_at_most_five_seconds_for_an_answer() {
    let m = HttpMargin::configured(|k: &str| Ok::<_, String>(if k == URL_KEY { "http://r".to_string() } else { "token".to_string() })).unwrap();
    assert!(m.timeout <= Duration::from_secs(5), "{:?}", m.timeout);
}

#[test_id(te282j9)]
#[test]
fn the_change_feed_is_asked_with_after_and_wait_and_its_last_seq_is_the_point_to_ask_on_from() {
    let dir = TempDir::new("margin-feed");
    let (base, seen) = serve_once(r#"{"changes":[{"seq":4,"kind":"published","collection_id":"c1","thread_id":null,"comment_id":null,"author":null,"visibility":null}],"last_seq":4}"#);
    let got = client(&dir, &base).changes(3, 0).unwrap();
    assert_eq!((got.through, got.changes.len(), got.changes[0]["kind"].as_str()), (4, 1, Some("published")));
    let req = seen.join().unwrap();
    assert!(req.starts_with("GET /api/changes?after=3&wait=0 "), "{req}");
}

fn changes_request() -> ChangesRequest {
    ChangesRequest { base: "http://reader:1".into(), url: "http://reader:1/api/changes?after=3&wait=7".into(), token: TOKEN.into(), timeout: Duration::from_secs(12) }
}

#[test_id(tbuspkv)]
#[test]
fn a_changes_request_carries_the_after_the_clamped_wait_and_a_deadline_of_call_plus_hold() {
    let dir = TempDir::new("margin-changes-request");
    let token = dir.path().join("token");
    std::fs::write(&token, format!("{TOKEN}\n")).unwrap();
    let reader = HttpMargin { base: "http://reader:1".into(), token_file: token.to_str().unwrap().into(), timeout: Duration::from_secs(5) };
    let req = reader.changes_request(3, 7).unwrap();
    assert_eq!((req.url.as_str(), req.token.as_str(), req.timeout), ("http://reader:1/api/changes?after=3&wait=7", TOKEN, Duration::from_secs(12)));
    assert!(reader.changes_request(3, 999).unwrap().url.ends_with("wait=30"), "the hold is capped");
}

#[test_id(t37e038)]
#[test]
fn a_changes_reply_is_the_feed_on_success_and_the_refusal_in_words_on_a_bad_status_or_body() {
    let req = changes_request();
    let ok = req.reply(200, r#"{"last_seq":9,"changes":[{"seq":8}]}"#).unwrap();
    assert_eq!((ok.through, ok.changes.len()), (9, 1));
    let refused = req.reply(503, " busy ").unwrap_err();
    assert_eq!(refused, "the reader at http://reader:1 refused GET /api/changes: HTTP 503: busy");
    assert!(req.reply(200, "<html>").unwrap_err().contains("not the expected JSON"));
}

#[test_id(tc33gjv)]
#[test]
fn a_changes_request_that_got_no_answer_is_said_with_the_token_redacted() {
    let said = changes_request().silent(&format!("connect: bad header Bearer {TOKEN}"));
    assert!(said.starts_with("the reader at http://reader:1 did not answer GET /api/changes: "), "{said}");
    assert!(said.contains("<token>") && !said.contains(TOKEN), "{said}");
}
