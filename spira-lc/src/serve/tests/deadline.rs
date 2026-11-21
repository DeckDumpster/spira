use super::*;

#[test]
fn unpublished_is_served_without_the_store_and_waits_longer_than_a_store_call() {
    let (reply, code) = ask(None, "[\"unpublished\"]\n");
    assert_eq!(code, 2, "{reply}");
    assert!(reply.contains("unpublished: usage"), "the verb answered, not the store: {reply}");
    let argv = |v: &str| vec![v.to_string()];
    assert!(dl(&argv("unpublished")) > dl(&argv("show")));
    assert!(dl(&argv("unpublished")) < std::time::Duration::from_secs(5), "the server answers before the 5 s client gives up");
}

#[test]
fn a_write_held_past_its_request_deadline_answers_cannot_tell_and_never_commits() {
    use state_store::{contract::{Config, Contract, Ports}, MemBackend, Object, Request, Txn};
    use std::{sync::Arc, time::{Duration, Instant}};
    let c = Arc::new(Contract::open(MemBackend::new(), Ports::virtual_clock(&runtime::ports::VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap());
    let worker = Arc::clone(&c);
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let (done, done_rx) = std::sync::mpsc::channel();
    let limit = Duration::from_millis(30);
    let received = Instant::now();
    let out = super::within(limit, move || {
        let _deadline = super::bind_deadline(received, limit);
        let _ = release_rx.recv();
        let r = worker.txn(&Txn::new().put("late", Object::new(serde_json::json!(1))), &Request::new("1", "t", "w"));
        let _ = done.send(state_store::wire::answer(&r));
    });
    assert!(out.is_none());
    drop(release);
    let (code, text) = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((code, text.as_str()), (2, r#"{"cannot_tell":"deadline"}"#));
    assert!(c.get("late").unwrap().is_none(), "the abandoned write never commits");
}

/// law-calls-have-deadlines: a store call is abandoned at the configured default or its declared
/// slow deadline; only the brokered batch verbs keep the longer bound.
#[test]
fn a_store_call_is_abandoned_at_the_configured_default_and_only_declared_and_batch_verbs_wait_longer() {
    let d = |v: &str| dl(&[v.to_string(), "x".to_string()]);
    for verb in ["show", "list", "event", "fact", "cut", "land", "stats", ""] {
        assert_eq!(d(verb), std::time::Duration::from_millis(1000), "{verb}");
    }
    assert_eq!(d("ops-view"), std::time::Duration::from_secs(3));
    let watch = |wait: &[&str]| dl(&["watch", "p/", "--from-rev", "0"].iter().chain(wait).map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(watch(&["--wait-ms", "2000"]), std::time::Duration::from_millis(2500));
    assert_eq!(watch(&[]), std::time::Duration::from_secs(3));
    assert_eq!(watch(&["--wait-ms", "9999"]), std::time::Duration::from_secs(3));
    assert_eq!(watch(&["--wait-ms", "x"]), std::time::Duration::from_secs(3));
    assert_eq!(d("work"), crate::db::QUERY_DEADLINE);
    let w = |v: &str| dl(&["work".to_string(), "sp-x".to_string(), v.to_string()]);
    for verb in ["show", "note", "done", "blocked", "superseded-by"] {
        assert_eq!(w(verb), crate::db::QUERY_DEADLINE, "{verb}");
    }
    for verb in ["submit", "file-followup", "split"] {
        assert_eq!(w(verb), super::TOOL_DEADLINE, "{verb}");
    }
}

fn watch_door(held: Duration) -> Arc<super::Door> {
    let work: super::Work = Arc::new(move |_| {
        std::thread::sleep(held); // clock-safe: the deadline under test is a real recv_timeout; the sleep only outlasts it
        crate::callers::Answer { code: 0, stdout: "idle".into(), stderr: String::new(), cert_log: None, owed: Default::default() }
    });
    let deadlines = super::Deadlines { default: Duration::from_millis(1000), slow: Default::default() };
    Arc::new(super::Door { ledger: super::Ledger::new(Duration::from_secs(60)), work, deadlines })
}

#[test]
fn a_watch_holding_two_seconds_is_not_cut_by_a_one_second_request_cutoff() {
    let door = watch_door(Duration::from_secs(2));
    let (code, out) = send(&door, &["watch", "p/", "--from-rev", "0", "--wait-ms", "2000"], None);
    assert_eq!((code, out.as_str()), (0, "idle"));
}

#[test]
fn a_zero_wait_watch_is_cut_at_the_margin() {
    let door = watch_door(Duration::from_millis(1500));
    let got = send(&door, &["watch", "p/", "--from-rev", "0", "--wait-ms", "0"], None);
    assert_eq!(got, (2, "cannot tell: deadline".to_string()));
}

#[test]
fn deadlines_come_from_config_and_bad_config_refuses_naming_its_key() {
    let ok = super::deadlines_from(config(Some("1000"), "")).unwrap();
    assert_eq!((ok.default, ok.slow.len()), (Duration::from_millis(1000), 0), "an empty slow list is accepted");
    let ok = super::deadlines_from(config(Some("800"), "list=3000@sp-a, rc=2000@sp-b")).unwrap();
    assert_eq!(ok.slow["rc"], (Duration::from_millis(2000), "sp-b".to_string()));
    for (deadline, slow, names) in [
        (None, "", "spira.lc_request_deadline_ms"),
        (Some("4000"), "", "spira.lc_request_deadline_ms"),
        (Some("1000"), "list=3000", "spira.lc_slow_verb_deadlines"),
        (Some("1000"), "list=fast@sp-a", "spira.lc_slow_verb_deadlines"),
        (Some("1000"), "list=4000@sp-a", "spira.lc_slow_verb_deadlines"),
    ] {
        let err = super::deadlines_from(config(deadline, slow)).unwrap_err();
        assert!(err.contains(names), "{err}");
    }
    let err = super::deadlines_from(config(Some("1000"), "list=fast@sp-a")).unwrap_err();
    assert!(err.contains("list=fast@sp-a"), "the entry is named: {err}");
}

#[test]
fn a_composed_verb_keeps_the_configured_default_and_only_its_sweeps_are_batch_jobs() {
    let d = |v: &str| dl(&[v.to_string(), "x".to_string()]);
    for verb in ["close", "reopen", "reconcile-closed", "close-ask", "hold", "unhold", "drop", "disposition", "held", "reply", "content"] {
        assert_eq!(d(verb), std::time::Duration::from_millis(1000), "{verb}");
    }
    for verb in crate::composed::SWEEPS {
        assert_eq!(d(verb), crate::db::QUERY_DEADLINE, "{verb}");
    }
}

#[test]
fn a_write_the_server_cut_at_its_deadline_is_resent_under_its_id_and_applied_once() {
    use std::io::{BufRead, Read, Write};
    let dir = testkit::TempDir::new("lc-serve-resend");
    let path = dir.join("sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let r = rig(true, fifty_ms());
    let (door, notes) = (Arc::clone(&r.door), Arc::clone(&r.notes));
    let ids = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let seen = Arc::clone(&ids);
    std::thread::spawn(move || {
        let mut first = true;
        for stream in listener.incoming().flatten() {
            let mut head = String::new();
            std::io::BufReader::new(&stream).read_line(&mut head).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(head.trim()).unwrap();
            seen.lock().unwrap().push(parsed["request_id"].as_str().map(str::to_string));
            let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
            (&client).write_all(head.as_bytes()).unwrap();
            let mut got = Vec::new();
            std::thread::scope(|scope| {
                scope.spawn(|| super::serve_one(server, None, std::time::Instant::now(), &door));
                (&client).read_to_end(&mut got).unwrap();
            });
            if first {
                notes.release();
                let id = parsed["request_id"].as_str().unwrap();
                assert!(
                    door.ledger.await_done(id, Duration::from_secs(5)),
                    "the released write never recorded its answer for {id}"
                );
            }
            first = false;
            (&stream).write_all(&got).unwrap();
            notes.release();
        }
    });
    let (code, out, err) = crate::client::forward_with(path.to_str().unwrap(), &["content", "note", "sp-a", "x"].map(String::from), None);
    assert_eq!(code, 0, "{out}{err}");
    let ids = ids.lock().unwrap().clone();
    assert_eq!(ids.len(), 2, "cut once, then re-sent: {ids:?}");
    assert!(ids[0].is_some() && ids[0] == ids[1], "both sends carry one request id: {ids:?}");
    assert_eq!(r.notes.mutations().len(), 1, "the write is applied once");
}
