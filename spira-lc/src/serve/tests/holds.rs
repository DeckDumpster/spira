use super::*;

/// The store's socket is absent when the client calls, and comes up 300 ms later: the write
/// waits for it and is applied once.
#[test]
fn a_hold_issued_before_the_socket_exists_is_applied_once_when_it_is_bound() {
    let (w, c) = (World::new(), Content::default());
    filed(&w, "sp-a");
    let dir = testkit::TempDir::new("lc-serve-late");
    let path = dir.join("sock");
    let at = path.to_str().unwrap().to_string();
    let asked = std::thread::spawn(move || crate::client::forward_with(&at, &["hold", "sp-a", "wait", "snooze-until:9999999999", "groomer"].map(String::from), None));
    std::thread::sleep(std::time::Duration::from_millis(300)); // clock-safe: the outage the client must wait through
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let (server, _) = listener.accept().unwrap();
    let mut reader = std::io::BufReader::new(server.try_clone().unwrap());
    let (req, _) = super::read_request(&mut reader).unwrap().unwrap();
    let (mut m, mut b) = (&w, &w);
    let ans = super::answer(req, &mut m, &mut b, &c, &|| Err("no run dir here".to_string()));
    super::reply_then_settle(&server, &ans, || {}, &mut |id, reason| crate::reconcile_content::settle_owed(&mut b, id, reason));
    drop(server);
    let (code, _, err) = asked.join().unwrap();
    assert_eq!(code, 0, "{err}");
    assert_eq!(over(&w, &c, &["held", "sp-a", "wait"], None).0, 0);
    let history = over(&w, &c, &["history", "sp-a"], None).1;
    assert_eq!(history.matches("\"event\": \"Hold\"").count(), 1, "{history}");
}

#[test]
fn hold_held_and_unhold_are_answered_by_the_socket_with_the_clis_exit_codes() {
    let (w, c) = (World::new(), Content::default());
    filed(&w, "sp-a");
    assert_eq!(over(&w, &c, &["held", "sp-a", "wait"], None).0, 1, "not held yet");
    let (code, _, err) = over(&w, &c, &["hold", "sp-a", "wait", "snooze-until:9999999999", "groomer"], None);
    assert_eq!(code, 0, "{err}");
    assert_eq!(over(&w, &c, &["held", "sp-a", "wait"], None).0, 0);
    assert_eq!(over(&w, &c, &["holds", "sp-a"], None).1.trim(), "wait");
    assert_eq!(over(&w, &c, &["unhold", "sp-a", "wait", "groomer"], None).0, 0);
    assert_eq!(over(&w, &c, &["held", "sp-a", "wait"], None).0, 1);
    let (code, _, err) = over(&w, &c, &["hold", "sp-a", "nonsense"], None);
    assert_ne!(code, 0);
    assert!(!err.is_empty(), "a refusal keeps its text, on stderr: {err}");
}
