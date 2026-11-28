//! The forge client's token travels in the request and stays out of its debug output.

use std::io::{Read, Write};
use std::net::TcpListener;

use spira_runtime::forge::Forge;
#[test_id(tfwe5qw)]
#[test]
fn the_forge_token_rides_the_request_and_is_not_in_the_debug_output() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", l.local_addr().unwrap());
    let seen = std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut buf = [0u8; 2048];
        let n = c.read(&mut buf).unwrap();
        let body = r#"{"state":"closed"}"#;
        let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
        String::from_utf8_lossy(&buf[..n]).to_lowercase()
    });
    let forge = Forge { api, token: "sekrit-token".into(), ..Forge::default() };
    assert_eq!(forge.open_pr("acme/widgets", 7), Some(None));
    assert!(seen.join().unwrap().contains("authorization: bearer sekrit-token"));
    assert!(!format!("{forge:?}").contains("sekrit"));
}

