//! For one fixture forge, pr-notify.sh and the pr-notify handler say the same lines and mail the
//! operator the same notes. The script is the oracle: it runs unmodified against a stubbed `gh`
//! and `mail`; the handler runs on a virtual clock against an HTTP stub of the forge. Both are
//! fed from one description of the world, so a disagreement is a disagreement about the same facts.

use std::io::{Read, Write};
use std::net::TcpListener;

use spira_runtime::forge::Forge;
#[test]
fn the_forge_token_rides_the_request_and_is_not_in_the_debug_output() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", l.local_addr().unwrap());
    let seen = std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut buf = [0u8; 2048];
        let n = c.read(&mut buf).unwrap();
        let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]");
        String::from_utf8_lossy(&buf[..n]).to_lowercase()
    });
    let forge = Forge { api, token: "sekrit-token".into(), ..Forge::default() };
    assert_eq!(forge.open_prs("acme/widgets"), Some(vec![]));
    assert!(seen.join().unwrap().contains("authorization: bearer sekrit-token"));
    assert!(!format!("{forge:?}").contains("sekrit"));
}

#[test]
fn e2e_a_forge_without_a_credential_answers_nothing_and_is_never_called() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", l.local_addr().unwrap());
    l.set_nonblocking(true).unwrap();
    let forge = Forge { api, gh: "/nonexistent/gh".into(), ..Forge::default() };
    assert_eq!(forge.open_prs("acme/widgets"), None);
    assert_eq!(forge.stray_branches("acme/widgets", "main", &[]), None);
    assert!(l.accept().is_err(), "an anonymous call reached the forge");
}
