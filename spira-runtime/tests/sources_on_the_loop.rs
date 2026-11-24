//! The scheduler's sources and the submission sessions run on the loop: no `source-*` thread, and
//! a session adds none.

#[macro_use]
extern crate spira_test_id;

use std::sync::Arc;
use std::time::Duration;

use runtime::host::Submit;
use runtime::ports::{VirtualClock, WallClock};
use runtime::units::ScriptedUnits;
use spira_runtime::sources::{real, smtp};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

fn comms() -> Vec<String> {
    std::fs::read_dir(std::path::Path::new(system_paths::PROC_SELF).join("task")).unwrap().flatten().filter_map(|t| std::fs::read_to_string(t.path().join("comm")).ok()).map(|c| c.trim().to_string()).collect()
}

struct Client(tokio_rustls::client::TlsStream<TcpStream>);

impl Client {
    async fn connect(addr: std::net::SocketAddr) -> Client {
        let tcp = loop {
            match TcpStream::connect(addr).await {
                Ok(t) => break t,
                Err(_) => tokio::task::yield_now().await,
            }
        };
        let name = tokio_rustls::rustls::pki_types::ServerName::IpAddress(addr.ip().into());
        Client(tokio_rustls::TlsConnector::from(spira_submission::probe_config()).connect(name, tcp).await.unwrap())
    }

    /// Reads until the reply's last line (`NNN text`, not `NNN-text`) has arrived.
    async fn reply(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut got = String::new();
            loop {
                let mut chunk = [0u8; 1024];
                let n = self.0.read(&mut chunk).await.unwrap();
                assert!(n > 0, "the server closed the connection: {got:?}");
                got.push_str(&String::from_utf8_lossy(&chunk[..n]));
                if got.ends_with("\r\n") && got.lines().last().is_some_and(|l| l.as_bytes().get(3) == Some(&b' ')) {
                    return got;
                }
            }
        })
        .await
        .expect("the server answered")
    }

    async fn say(&mut self, text: &str) -> String {
        self.0.write_all(format!("{text}\r\n").as_bytes()).await.unwrap();
        self.reply().await
    }
}

fn plain(user: &str, pass: &str) -> String {
    let raw = format!("\0{user}\0{pass}");
    let t = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for ch in raw.as_bytes().chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= ch.len() { t[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[test_id(t4qzb4b)]
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
    assert!(!comms().iter().any(|c| c.starts_with("source-")), "the planted thread is gone");

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

    let (listener, pw) = (std::net::TcpListener::bind("127.0.0.1:0").unwrap(), dir.path().join("pw"));
    let addr = listener.local_addr().unwrap();
    drop(listener);
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&pw, "hunter2\n").unwrap();
        std::fs::set_permissions(&pw, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let fixtures = testkit::manifest_dir().join("../spira-submission/tests/fixtures");
    let scfg = spira_submission::Config {
        addr: addr.ip(),
        port: addr.port(),
        cert: fixtures.join("test-cert.pem"),
        key: fixtures.join("test-key.pem"),
        password_file: pw,
        user: "ryan".into(),
        from: "me@example.org".into(),
        rcpt_domain: "spira".into(),
        sendmail: vec![],
    };
    let (stx, _srx) = mpsc::channel::<Submit>(8);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(smtp::run(Some(scfg), None, stx, async { let _ = stop_rx.await; }));

    let mut clients = Vec::new();
    for _ in 0..20 {
        clients.push(tokio::spawn(async move {
            let mut c = Client::connect(addr).await;
            assert!(c.reply().await.starts_with("220"));
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
    for mut c in open {
        done.push(tokio::spawn(async move {
            assert!(c.say("EHLO c").await.starts_with("250"));
            assert!(c.say(&format!("AUTH PLAIN {}", plain("ryan", "hunter2"))).await.starts_with("235"));
            assert!(c.say("QUIT").await.starts_with("221"));
        }));
    }
    for d in done {
        d.await.unwrap();
    }
    assert_eq!(comms().len(), before, "after the sessions: no thread either");
    stop_tx.send(()).unwrap();
    server.await.unwrap();
}
