use super::*;
use crate::{Anchor, Doc, Margin, TOKEN_FILE_KEY, URL_KEY};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};
use testkit::TempDir;

const TOKEN: &str = "s3cret-agent-token";

/// Serves one request with `reply` as its JSON body and hands back the raw request text.
fn serve_once(reply: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = s.read(&mut buf).unwrap();
            seen.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&seen).to_string();
            if let Some(at) = text.find("\r\n\r\n") {
                let want: usize = text.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                if seen.len() >= at + 4 + want {
                    break;
                }
            }
        }
        write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        String::from_utf8_lossy(&seen).to_string()
    });
    (base, h)
}

fn client(dir: &TempDir, base: &str) -> HttpMargin {
    let file = dir.path().join("token");
    std::fs::write(&file, TOKEN).unwrap();
    HttpMargin { base: base.to_string(), token_file: file.to_str().unwrap().to_string() }
}

#[test_id(tmrg001)]
#[test]
fn post_collection_sends_the_token_file_text_as_a_bearer_and_parses_the_reply() {
    let dir = TempDir::new("margin-bearer");
    let (base, seen) = serve_once(r#"{"id":"c1","url":"http://r/c/c1","documents":[{"id":"d1","version":1,"path":"a.rs"}]}"#);
    let docs = [Doc { path: "a.rs".into(), kind: "code".into(), text: "fn a() {}\n".into() }];
    let got = client(&dir, &base).post_collection("T", "cover", &docs).unwrap();
    assert_eq!((got.id.as_str(), got.url.as_str(), got.documents[0].path.as_str()), ("c1", "http://r/c/c1", "a.rs"));
    let req = seen.join().unwrap();
    assert!(req.starts_with("POST /api/collections "), "{req}");
    assert!(req.to_ascii_lowercase().contains(&format!("authorization: bearer {TOKEN}").to_ascii_lowercase()), "{req}");
    assert!(req.contains(r#""text":"fn a() {}\n""#), "{req}");
}

#[test_id(tmrg002)]
#[test]
fn an_empty_url_key_or_a_missing_token_file_is_refused_by_name_without_the_token() {
    let cfg = |url: &'static str, file: String| move |k: &str| Ok::<_, String>(if k == URL_KEY { url.to_string() } else { file.clone() });
    let e = HttpMargin::configured(cfg("", "/x".into())).err().unwrap();
    assert!(e.contains(URL_KEY), "{e}");
    let dir = TempDir::new("margin-missing");
    let missing = dir.path().join("absent").to_str().unwrap().to_string();
    let m = HttpMargin::configured(cfg("http://127.0.0.1:1", missing.clone())).unwrap();
    let e = m.close("c1").unwrap_err();
    assert!(e.contains(TOKEN_FILE_KEY) && e.contains(&missing), "{e}");
    let e = HttpMargin::configured(cfg("http://r", String::new())).err().unwrap();
    assert!(e.contains(TOKEN_FILE_KEY), "{e}");
    let (base, _h) = serve_once("{}");
    let e = client(&dir, &base).thread("t").unwrap_err();
    assert!(!e.contains(TOKEN), "{e}");
}

#[test_id(tmrg003)]
#[test]
fn a_reader_that_accepts_and_never_answers_fails_post_comment_within_six_seconds_naming_url_and_route() {
    let dir = TempDir::new("margin-silent");
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let (release, released) = std::sync::mpsc::channel::<()>();
    let hold = std::thread::spawn(move || {
        let held = l.accept();
        let _ = released.recv();
        drop(held);
    });
    let started = Instant::now();
    let a = Anchor { document: "d1".into(), version: 1, first: 1, last: 1 };
    let e = client(&dir, &base).post_comment("c1", &a, "b", true, None).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(6), "took {:?}", started.elapsed());
    assert!(e.contains(&base) && e.contains("POST /api/collections/c1/comments") && !e.contains(TOKEN), "{e}");
    release.send(()).unwrap();
    hold.join().unwrap();
}

#[test_id(tmrg004)]
#[test]
fn a_held_comment_is_sent_with_its_anchor_and_the_reply_parent() {
    let dir = TempDir::new("margin-comment");
    let (base, seen) = serve_once(r#"{"id":"k1","thread":"t1","url":"http://r/c/c1#k1"}"#);
    let a = Anchor { document: "d1".into(), version: 2, first: 3, last: 5 };
    let got = client(&dir, &base).post_comment("c1", &a, "body", true, Some("t0")).unwrap();
    assert_eq!(got.thread, "t1");
    let req = seen.join().unwrap();
    for want in [r#""held":true"#, r#""first":3"#, r#""last":5"#, r#""version":2"#, r#""in_reply_to":"t0""#] {
        assert!(req.contains(want), "{want} missing in {req}");
    }
}
