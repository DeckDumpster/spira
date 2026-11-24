//! `call_request` binds a `work` request to the persona of the uid its stream's peer reports.

use super::*;
use ports::{Net, Peer, SeededRng, SimNet, VirtualClock};
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

fn accepted_as(uid: u32) -> Peer {
    let net = SimNet::new(VirtualClock::new(0), SeededRng::new(1));
    let mut listener = net.listen("lc").unwrap();
    let _client = net.connect("lc", Peer { pid: None, uid: Some(uid) });
    while net.step() {}
    let Poll::Ready(Ok(stream)) = listener.poll_accept(&mut Context::from_waker(Waker::noop())) else { panic!("no connection was delivered") };
    stream.peer()
}

fn called(peer: Peer, argv: &[&str]) -> (i32, String, Vec<Vec<String>>) {
    let dir = testkit::TempDir::new("spira-lc-personas");
    let file = dir.path().join("lc.personas");
    std::fs::write(&file, "1001 builder\n").unwrap();
    let ran = Arc::new(Mutex::new(Vec::new()));
    let log = ran.clone();
    let work: Work = Arc::new(move |req| {
        log.lock().unwrap().push(req.argv.clone());
        Answer::out(0, "ran".to_string())
    });
    let door = Door { ledger: Ledger::new(Duration::from_secs(60)), work, deadlines: test_deadlines(), personas: file };
    let req = Request { argv: argv.iter().map(|s| s.to_string()).collect(), stdin: None, caller: Default::default(), task: None, request_id: None };
    let now = Instant::now();
    let called = call_request(req, peer, &door, now, now, Cut::Within);
    let seen = ran.lock().unwrap().clone();
    (called.ans.code, called.ans.stdout, seen)
}

#[test_id(t1jfbcx)]
#[test]
fn a_work_request_naming_another_persona_than_its_peers_is_refused_and_not_run() {
    let (code, out, ran) = called(accepted_as(1001), &["work", "sp-1", "show", "--actor", "czar"]);
    assert_eq!(code, crate::REFUSED, "{out}");
    assert!(out.contains("uid 1001 is persona builder, not czar"), "{out}");
    assert!(ran.is_empty(), "the work closure was run: {ran:?}");
}

#[test_id(t9egm2u)]
#[test]
fn a_work_request_from_a_mapped_peer_runs_as_its_persona() {
    let (code, _, ran) = called(accepted_as(1001), &["work", "sp-1", "show"]);
    assert_eq!(code, 0);
    assert_eq!(ran, vec![["work", "sp-1", "show", "--actor", "builder"].map(String::from).to_vec()]);
}
