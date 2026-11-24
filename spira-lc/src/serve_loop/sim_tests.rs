//! The loop over `SimNet`: events arrive in an order a seed draws, so each seed is one
//! interleaving of connects, chunks and closes the real socket would hand out by luck.

use super::*;
use crate::serve::{Ledger, Work};
use ports::{Net, Peer, SeededRng, SimClient, SimNet, VirtualClock};
use std::sync::atomic::AtomicBool;

const ME: Peer = Peer { pid: Some(1), uid: Some(1000) };

fn answer_of(argv: &[String]) -> Vec<u8> {
    crate::serve::encode_response(0, &format!("ran {}", argv.join(" ")), "", false)
}

fn door(work: Work) -> Arc<Door> {
    Arc::new(Door { ledger: Ledger::new(Duration::from_secs(60)), work, personas: Default::default(), deadlines: crate::serve::Deadlines { default: Duration::from_secs(30) } })
}

fn echo() -> Work {
    Arc::new(|req| Answer::out(0, format!("ran {}", req.argv.join(" "))))
}

async fn read_all(mut client: SimClient) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 7];
    loop {
        match std::future::poll_fn(|cx| client.poll_read(cx, &mut buf)).await {
            Ok(0) | Err(_) => return got,
            Ok(n) => got.extend_from_slice(&buf[..n]),
        }
    }
}

/// Runs `serve` on a sim network, `script` having queued the clients' events, and returns what
/// each client read and how many connections the loop cut when it stopped.
fn run(seed: u64, work: Work, script: impl FnOnce(&Arc<SimNet>) -> Vec<SimClient>) -> (Vec<Vec<u8>>, usize) {
    let net = SimNet::new(VirtualClock::new(0), SeededRng::new(seed));
    let listener = net.listen("loop").unwrap();
    let clients = script(&net);
    let shared = Arc::new(Shared::new(door(work), Pool::new(2, 8), None));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    let stopping = Arc::new(AtomicBool::new(false));
    local.block_on(&runtime, async {
        let flag = stopping.clone();
        let serving = tokio::task::spawn_local(async move { serve(listener, shared, &flag, Duration::from_secs(5), || {}).await });
        let stepping = tokio::task::spawn_local(async move {
            while net.step() {
                tokio::task::yield_now().await;
            }
        });
        let readers: Vec<_> = clients.into_iter().map(|c| tokio::task::spawn_local(read_all(c))).collect();
        let mut read = Vec::new();
        for r in readers {
            read.push(r.await.unwrap());
        }
        stepping.await.unwrap();
        stopping.store(true, Ordering::SeqCst);
        (read, serving.await.unwrap())
    })
}

#[test_id(tnql80l)]
#[test]
fn every_client_that_stays_gets_one_whole_answer_whatever_the_chunks_and_the_loop_ends_empty() {
    std::thread::scope(|scope| {
        for seed in 0..64 {
            scope.spawn(move || one_seed(seed));
        }
    });
}

fn one_seed(seed: u64) {
    {
        let mut expected = Vec::new();
        let (read, cut) = run(seed, echo(), |net| {
            (0..8)
                .map(|n| {
                    let client = net.connect("loop", ME);
                    let argv: Vec<String> = if n % 2 == 0 { vec!["show".into(), format!("sp-{n}")] } else { vec!["list".into(), "--live".into()] };
                    let line = format!("{}\n", serde_json::to_string(&argv).unwrap());
                    match n % 4 {
                        3 => {
                            client.send(&line.as_bytes()[..line.len() / 2]);
                            client.close();
                            expected.push(None);
                        }
                        _ => {
                            client.send(line.as_bytes());
                            expected.push(Some(answer_of(&argv)));
                        }
                    }
                    client
                })
                .collect()
        });
        assert_eq!(cut, 0, "seed {seed}: no connection was left to cut");
        for (n, (got, want)) in read.iter().zip(&expected).enumerate() {
            match want {
                Some(want) => assert_eq!(got, want, "seed {seed} client {n}"),
                None => assert!(!String::from_utf8_lossy(got).contains("ran "), "seed {seed} client {n} closed early and was served: {got:?}"),
            }
        }
    }
}

#[test_id(tvz6rrq)]
#[test]
fn a_request_one_byte_at_a_time_is_one_whole_message() {
    let argv = vec!["show".to_string(), "sp-slow".to_string()];
    let (read, cut) = run(5, echo(), |net| {
        let client = net.connect("loop", ME);
        client.send_in(format!("{}\n", serde_json::to_string(&argv).unwrap()).as_bytes(), 1);
        vec![client]
    });
    assert_eq!((read, cut), (vec![answer_of(&argv)], 0));
}

#[test_id(t3yz8yy)]
#[test]
fn a_client_that_resets_leaves_no_connection_behind() {
    let (_, cut) = run(9, echo(), |net| {
        let client = net.connect("loop", ME);
        client.send(b"[\"show\",");
        client.reset();
        vec![client]
    });
    assert_eq!(cut, 0);
}

#[test_id(tiok0p4)]
#[test]
fn the_peer_the_port_names_is_the_caller_the_work_sees() {
    let seen: Work = Arc::new(|_| Answer::out(0, format!("caller {}", crate::slow::current_caller())));
    let (read, _) = run(2, seen, |net| {
        let named = net.connect("loop", Peer { pid: Some(std::process::id() as i32), uid: Some(1000) });
        let nameless = net.connect("loop", Peer { pid: None, uid: None });
        for c in [&named, &nameless] {
            c.send(b"[\"show\",\"sp-x\"]\n");
        }
        vec![named, nameless]
    });
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    assert!(text(&read[1]).ends_with("caller -"), "no pid, no caller: {}", text(&read[1]));
    assert!(!text(&read[0]).ends_with("caller -"), "a named pid is looked up: {}", text(&read[0]));
}

async fn exchange_over(dial: &ports::SimDial, argv: &[&str]) -> Result<spira_store::lc_call::Reply, spira_store::lc_call::Fail> {
    use ports::Dial;
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let mut stream = dial.dial().unwrap();
    let mut exchange = spira_store::lc_call::Exchange::new(spira_store::lc_call::encode_request(&argv, None, None, None)?);
    std::future::poll_fn(|cx| exchange.poll_exchange(stream.as_mut(), cx)).await
}

#[test_id(tvyg9pl)]
#[test]
fn the_stores_client_gets_the_whole_reply_from_the_loop_a_byte_at_a_time() {
    let net = SimNet::new(VirtualClock::new(0), SeededRng::new(3));
    let listener = net.listen("loop").unwrap();
    let shared = Arc::new(Shared::new(door(echo()), Pool::new(2, 8), None));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    let stopping = Arc::new(AtomicBool::new(false));
    let dial = ports::SimDial { net: net.clone(), path: "loop".into(), peer: ME, chunk: Some(1) };
    let replies = local.block_on(&runtime, async {
        let flag = stopping.clone();
        let serving = tokio::task::spawn_local(async move { serve(listener, shared, &flag, Duration::from_secs(5), || {}).await });
        let stepping = tokio::task::spawn_local(async move {
            loop {
                net.step();
                tokio::task::yield_now().await;
            }
        });
        let shown = exchange_over(&dial, &["show", "sp-1"]).await;
        let listed = exchange_over(&dial, &["list", "--live"]).await;
        stepping.abort();
        stopping.store(true, Ordering::SeqCst);
        serving.await.unwrap();
        (shown, listed)
    });
    let whole = |out: &str| Ok(spira_store::lc_call::Reply { code: 0, out: out.into(), err: String::new(), read_ended: false });
    assert_eq!(replies, (whole("ran show sp-1"), whole("ran list --live")));
}
