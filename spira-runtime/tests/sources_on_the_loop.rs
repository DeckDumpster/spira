//! The scheduler's sources and the submission sessions run on the loop: no `source-*` thread, and
//! a session adds none.

#[macro_use]
extern crate spira_test_id;

use std::sync::Arc;
use std::time::Duration;

use runtime::host::Submit;
use runtime::ports::{VirtualClock, WallClock};
use runtime::units::ScriptedUnits;
use spira_runtime::sources::smtp::test_client::{plain, setup, Client};
use spira_runtime::sources::{real, smtp};
use tokio::sync::mpsc;

fn comms() -> Vec<String> {
    std::fs::read_dir(std::path::Path::new(system_paths::PROC_SELF).join("task")).unwrap().flatten().filter_map(|t| std::fs::read_to_string(t.path().join("comm")).ok()).map(|c| c.trim().to_string()).collect()
}

#[test_id(tc0d2p3)]
#[tokio::test(flavor = "current_thread")]
async fn no_source_has_a_thread_and_twenty_concurrent_tls_sessions_add_none() {
    let planted = {
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let t = std::thread::Builder::new().name("source-probe".into()).spawn(move || {
            let _ = stop_rx.recv();
        }).unwrap();
        let seen = tokio::time::timeout(Duration::from_secs(10), async {
            while !comms().iter().any(|c| c.starts_with("source-")) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        drop(stop_tx);
        t.join().unwrap();
        seen
    };
    assert!(planted, "the matcher must see a thread named as a source's was");
    let gone = tokio::time::timeout(Duration::from_secs(10), async {
        while comms().iter().any(|c| c.starts_with("source-")) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(gone.is_ok(), "the planted thread is gone");

    let dir = testkit::TempDir::new("sources-on-the-loop");
    let mail = dir.path().join("mail");
    let cfg = move |k: &str| -> Result<String, String> {
        Ok(match k {
            "SPIRA_MAIL_READERS" => "concierge=inbox-append.sh".into(),
            "SPIRA_MAIL" => mail.display().to_string(),
            _ => String::new(),
        })
    };
    let views = Arc::new(store_view::Projection::new(spira_store::lc_call::LcClient::socket("no-such-socket"), Duration::from_secs(1), Arc::new(std::time::Instant::now)));
    let store = Some((views, Arc::new(spira_runtime::store::ChangeTable::default()), "no-such-socket".into()));
    let units = Arc::new(ScriptedUnits::new(Arc::new(VirtualClock::new(0))));
    let sources = real(&cfg, units, Arc::new(WallClock), store).unwrap();
    assert_eq!(sources.names().len(), 5, "all five sources: {:?}", sources.names());
    let (tx, _rx) = mpsc::channel::<Submit>(8);
    let _running = sources.start(tx);
    let warm = runtime::sources::on_pool("warm", Arc::new(|| 1)).await.unwrap();
    assert_eq!(warm, 1);
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    let named = comms();
    assert!(!named.iter().any(|c| c.starts_with("source-")), "no source owns a thread: {named:?}");
    let before = named.len();

    // The live source reads the process's own config, so the sessions are driven through `smtp::run`, the loop the source runs.
    let b = setup("sources-on-the-loop-sessions");
    let addr = b.cfg.socket();
    let (stx, mut srx) = mpsc::channel::<Submit>(8);
    let answering = tokio::spawn(async move {
        while let Some(s) = srx.recv().await {
            if let Some(reply) = s.reply {
                let _ = reply.send(Ok(()));
            }
        }
    });
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(smtp::run(Some(b.cfg.clone()), Some(b.delivery.clone()), stx, async { let _ = stop_rx.await; }));

    let mut clients = Vec::new();
    for _ in 0..20 {
        clients.push(tokio::spawn(async move {
            let mut c = Client::connect(addr).await;
            assert!(c.line().await.starts_with("220"));
            c
        }));
    }
    let mut open = Vec::new();
    for c in clients {
        open.push(c.await.unwrap());
    }
    let during = comms();
    assert_eq!(during.len(), before, "twenty open sessions add no thread: {during:?}");

    let mut done = Vec::new();
    for (n, mut c) in open.into_iter().enumerate() {
        done.push(tokio::spawn(async move {
            c.say(&format!("AUTH PLAIN {}", plain("ryan", "hunter2"))).await;
            assert!(c.line().await.starts_with("235"));
            if n % 4 == 0 {
                for (cmd, want) in [("MAIL FROM:<me@example.org>", "250"), ("RCPT TO:<concierge@spira>", "250"), ("DATA", "354")] {
                    c.say(cmd).await;
                    assert!(c.line().await.starts_with(want), "{cmd}");
                }
                c.say(&format!("Message-ID: <load{n}@x>\r\n\r\nhello\r\n.")).await;
                assert!(c.line().await.starts_with("250 2.0.0 queued"), "delivered and journaled");
            }
            c.say("QUIT").await;
            assert!(c.line().await.starts_with("221"));
        }));
    }
    for d in done {
        d.await.unwrap();
    }
    assert_eq!(comms().len(), before, "after the sessions: no thread either");
    stop_tx.send(()).unwrap();
    server.await.unwrap();
    answering.await.unwrap();
}
