use super::*;

#[test_id(t8tubc5)]
#[test]
fn an_event_sent_twice_under_one_request_id_advances_the_bead_once() {
    let r = rig(false, three_seconds());
    filed(&r.world.lock().unwrap(), "sp-a");
    let argv = drop_argv(&r, "sp-a");
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let before = history_rows(&r, "sp-a");
    let first = send(&r.door, &argv, Some("req-1"));
    let second = send(&r.door, &argv, Some("req-1"));
    assert_eq!(first.0, 0, "{first:?}");
    assert_eq!(second, first, "the retry gets the first answer");
    assert_eq!(history_rows(&r, "sp-a"), before + 1, "one transition");
}

#[test_id(tet9v2i)]
#[test]
fn a_store_txn_retried_under_the_clients_id_after_a_restart_applies_once() {
    let r = rig(false, three_seconds());
    filed(&r.world.lock().unwrap(), "sp-a");
    let argv = drop_argv(&r, "sp-a");
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let before = history_rows(&r, "sp-a");
    let first = send(&r.door, &argv, Some("req-1"));
    let restarted = Arc::new(super::Door { ledger: super::Ledger::new(Duration::from_secs(60)), work: Arc::clone(&r.door.work), personas: Default::default(), deadlines: three_seconds() });
    let again = send(&restarted, &argv, Some("req-1"));
    assert_eq!(first.0, 0);
    assert_eq!(again.0, 0, "{again:?}");
    assert_eq!(history_rows(&r, "sp-a"), before + 1);
}

#[test_id(tv17p33)]
#[test]
fn a_content_note_sent_twice_under_one_request_id_writes_one_note() {
    let r = rig(false, three_seconds());
    let argv = ["content", "note", "sp-a", "x"];
    let first = send(&r.door, &argv, Some("req-1"));
    let second = send(&r.door, &argv, Some("req-1"));
    assert_eq!(first.0, 0, "{first:?}");
    assert_eq!(second, first);
    assert_eq!(r.notes.mutations(), ["note sp-a x"]);
}

#[test_id(tqlozt4)]
#[test]
fn without_a_request_id_two_identical_notes_are_two_writes() {
    let r = rig(false, three_seconds());
    let argv = ["content", "note", "sp-a", "x"];
    send(&r.door, &argv, None);
    send(&r.door, &argv, None);
    assert_eq!(r.notes.mutations().len(), 2);
}

#[test_id(thh7wmu)]
#[test]
fn a_read_sent_twice_under_one_request_id_is_answered_fresh_each_time() {
    let r = rig(false, three_seconds());
    filed(&r.world.lock().unwrap(), "sp-a");
    let argv = drop_argv(&r, "sp-a");
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let before = send(&r.door, &["show", "sp-a"], Some("req-1"));
    assert_eq!(send(&r.door, &argv, None).0, 0);
    let after = send(&r.door, &["show", "sp-a"], Some("req-1"));
    assert_ne!(before.1, after.1, "the second read ran, not the first one's answer");
}

#[test_id(t47oupb)]
#[test]
fn a_second_arrival_waits_only_to_its_own_deadline_and_the_worker_still_records_its_answer() {
    let ledger = super::Ledger::new(Duration::from_secs(60));
    let (started, started_rx) = std::sync::mpsc::channel::<()>();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let first = {
        let ledger = ledger.clone();
        std::thread::spawn(move || {
            ledger.answer_in_time("r", Duration::from_secs(10), move || {
                started.send(()).unwrap();
                release_rx.recv().unwrap();
                crate::callers::Answer::out(0, "real".to_string())
            })
        })
    };
    started_rx.recv().unwrap();
    assert!(ledger.answer_in_time("r", Duration::from_millis(30), || panic!("a second arrival never runs the work")).is_none());
    release.send(()).unwrap();
    assert_eq!(first.join().unwrap().unwrap().stdout, "real");
    let replay = ledger.answer_in_time("r", Duration::from_secs(1), || panic!("a recorded answer is replayed, not run again")).unwrap();
    assert_eq!(replay.stdout, "real");
}

#[test_id(tpeuxp7)]
#[test]
fn an_empty_or_non_string_request_id_is_a_bad_request() {
    for head in [r#"{"argv":["list"],"request_id":""}"#, r#"{"argv":["list"],"request_id":7}"#] {
        let mut reader = std::io::BufReader::new(format!("{head}\n").into_bytes().leak() as &[u8]);
        let (_, msg) = super::read_request(&mut reader).unwrap_err();
        assert!(msg.starts_with("bad request:") && msg.contains("request_id"), "{msg}");
    }
}

#[test_id(tysvjej)]
#[test]
fn a_retry_after_a_deadline_gets_the_workers_real_answer_and_the_note_exists_once() {
    let r = rig(true, fifty_ms());
    let argv = ["content", "note", "sp-a", "x"];
    let first = send(&r.door, &argv, Some("req-1"));
    assert_eq!(first, (2, "cannot tell: deadline".to_string()), "the worker is parked past the deadline");
    let door = Arc::clone(&r.door);
    let retry = std::thread::spawn(move || loop {
        let answer = send(&door, &argv, Some("req-1"));
        if answer.0 != 2 {
            break answer;
        }
    });
    r.notes.release();
    let second = retry.join().unwrap();
    assert_eq!(second.0, 0, "{second:?}");
    let third = send(&r.door, &argv, Some("req-1"));
    assert_eq!(third.0, 0, "{third:?}");
    assert_eq!(r.notes.mutations(), ["note sp-a x"]);
}
