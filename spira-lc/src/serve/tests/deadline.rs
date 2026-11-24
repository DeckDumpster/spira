use super::*;

#[test_id(trztch4)]
#[test]
fn a_write_held_past_its_request_deadline_answers_cannot_tell_and_never_commits() {
    let _env = testkit::env_read();
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

/// law-calls-have-deadlines: a store call is abandoned at the configured default; only the brokered batch verbs keep the longer bound.
#[test_id(tr82uyg)]
#[test]
fn a_store_call_is_abandoned_at_the_configured_default_and_only_batch_verbs_wait_longer() {
    let _env = testkit::env_read();
    let d = |v: &str| dl(&[v.to_string(), "x".to_string()]);
    for verb in ["show", "list", "event", "fact", "cut", "land", "stats", "content", "audits", "ops-view", ""] {
        assert_eq!(d(verb), std::time::Duration::from_millis(1000), "{verb}");
    }
    let watch = |wait: &[&str]| dl(&["watch", "p/", "--from-rev", "0"].iter().chain(wait).map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(watch(&["--wait-ms", "2000"]), std::time::Duration::from_millis(2500));
    assert_eq!(watch(&[]), std::time::Duration::from_millis(30_500));
    assert_eq!(watch(&["--wait-ms", "30000"]), std::time::Duration::from_millis(30_500));
    assert_eq!(watch(&["--wait-ms", "99999"]), std::time::Duration::from_millis(30_500));
    assert_eq!(watch(&["--wait-ms", "x"]), std::time::Duration::from_millis(30_500));
    assert_eq!(d("work"), crate::db::QUERY_DEADLINE);
    let w = |v: &str| dl(&["work".to_string(), "sp-x".to_string(), v.to_string()]);
    for verb in ["show", "note", "done"] {
        assert_eq!(w(verb), crate::db::QUERY_DEADLINE, "{verb}");
    }
    for verb in ["submit", "file-followup", "split", "ack", "blocked", "superseded-by", "groom", "queue", "forge", "ask", "file"] {
        assert!(crate::work::exec_verb(verb), "{verb}: a program-running verb is recorded for the scheduler");
        assert_eq!(w(verb), crate::db::QUERY_DEADLINE, "{verb}: recording is a plain store write, so it keeps the store-call deadline");
    }
    for verb in ["read", "list", "search", "fence", "note-on", "label-add", "label-remove"] {
        assert!(!crate::work::exec_verb(verb), "{verb}");
        assert_eq!(w(verb), super::TOOL_DEADLINE, "{verb}: answered in the service, with the bound it had");
    }
}

fn watch_door(held: Duration) -> Arc<super::Door> {
    let work: super::Work = Arc::new(move |_| {
        std::thread::sleep(held); // clock-safe: the deadline under test is a real recv_timeout; the sleep only outlasts it
        crate::callers::Answer { code: 0, stdout: "idle".into(), stderr: String::new(), cert_log: None, owed: Default::default() }
    });
    let deadlines = super::Deadlines { default: Duration::from_millis(1000) };
    Arc::new(super::Door { ledger: super::Ledger::new(Duration::from_secs(60)), work, personas: Default::default(), deadlines })
}

#[test_id(tlv64af)]
#[test]
fn a_watch_holding_two_seconds_is_not_cut_by_a_one_second_request_cutoff() {
    let _env = testkit::env_read();
    let door = watch_door(Duration::from_secs(2));
    let (code, out) = send(&door, &["watch", "p/", "--from-rev", "0", "--wait-ms", "2000"], None);
    assert_eq!((code, out.as_str()), (0, "idle"));
}

#[test_id(tqvecu8)]
#[test]
fn a_zero_wait_watch_is_cut_at_the_margin() {
    let _env = testkit::env_read();
    let door = watch_door(Duration::from_millis(1500));
    let got = send(&door, &["watch", "p/", "--from-rev", "0", "--wait-ms", "0"], None);
    assert_eq!(got, (2, "cannot tell: deadline".to_string()));
}

#[test_id(t5bw0o8)]
#[test]
fn deadlines_come_from_config_and_bad_config_refuses_naming_its_key() {
    let _env = testkit::env_read();
    let ok = super::deadlines_from(config(Some("800"))).unwrap();
    assert_eq!(ok.default, Duration::from_millis(800));
    for deadline in [None, Some("4000"), Some("0")] {
        let err = super::deadlines_from(config(deadline)).unwrap_err();
        assert!(err.contains("spira.lc_request_deadline_ms"), "{err}");
    }
}

#[test_id(tnfn8dy)]
#[test]
fn a_composed_verb_keeps_the_configured_default_and_only_its_sweeps_are_batch_jobs() {
    let _env = testkit::env_read();
    let d = |v: &str| dl(&[v.to_string(), "x".to_string()]);
    for verb in ["close", "reopen", "reconcile-closed", "close-ask", "hold", "unhold", "drop", "disposition", "held", "reply", "content"] {
        assert_eq!(d(verb), std::time::Duration::from_millis(1000), "{verb}");
    }
    for verb in crate::composed::SWEEPS {
        assert_eq!(d(verb), crate::db::QUERY_DEADLINE, "{verb}");
    }
}

#[test_id(tm9q4t5)]
#[test]
fn a_write_the_server_cut_at_its_deadline_is_resent_under_its_id_and_applied_once() {
    let _env = testkit::env_read();
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

#[test_id(tfgnsak)]
#[test]
fn a_test_started_serve_runs_under_the_ceiling_the_complete_fixture_carries() {
    let dir = testkit::TempDir::new("complete-deadline");
    let toml = spira_config::process::fixture_toml(dir.path(), &[]);
    let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
    let read = spira_config::process::fresh(&["SPIRA_LC_REQUEST_DEADLINE_MS".to_string()]).unwrap();
    let got = super::deadlines_from(|k| read.get(k).cloned().ok_or_else(|| format!("{k} missing"))).unwrap();
    assert_eq!(got.default, Duration::from_millis(super::MAX_CONFIGURED_MS));
}

#[test_id(tsf6mti)]
#[test]
fn work_outliving_its_deadline_holds_its_runner_and_the_next_call_runs_on_another_thread() {
    let _env = testkit::env_read();
    let (hold_tx, hold_rx) = std::sync::mpsc::channel::<()>();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let out = super::within(Duration::from_millis(20), move || {
        started_tx.send(std::thread::current().id()).unwrap();
        let _ = hold_rx.recv();
    });
    assert!(out.is_none());
    let held = started_rx.recv().unwrap();
    let other = super::within(Duration::from_secs(5), || std::thread::current().id());
    assert_ne!(other.expect("answered in time"), held, "a runner still on abandoned work is not reused");
    drop(hold_tx);
}

#[test_id(tsttgjb)]
#[test]
fn work_that_panics_answers_none_and_the_pool_still_answers() {
    let _env = testkit::env_read();
    assert!(super::within(Duration::from_secs(5), || -> u8 { panic!("work failed") }).is_none());
    assert_eq!(super::within(Duration::from_secs(5), || 7), Some(7));
}
