//! The `store.changed` source: the store's one watch, held as a socket on the scheduler's loop.
//! Each answer it applies becomes a memory-only event naming the keys that changed; applying runs
//! on the offload pool, and no thread belongs to the source.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use runtime::host::Submit;
use runtime::sources::{on_pool, Running, Source};
use runtime::StoreChanged as Body;
use spira_store::lc_call::{as_task, call_wait, connect_failure, encode_request, Fail, LcError, ReplyHead};
use store_view::{Polled, Projection, RETRY};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::store::ChangeTable;

pub const NAME: &str = "store.changed";
const TASK: &str = "store-watch";

pub struct StoreChanged {
    pub views: Arc<Projection>,
    pub table: Arc<ChangeTable>,
    pub socket: PathBuf,
}

impl Source for StoreChanged {
    fn name(&self) -> &str {
        NAME
    }

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running {
        Box::pin(watch(self.views, self.table, self.socket, tx))
    }
}

enum Prev {
    First,
    Answer(Option<Result<String, LcError>>),
}

struct Step {
    polled: Option<Result<Polled, LcError>>,
    argv: Option<Vec<String>>,
}

async fn step(views: &Arc<Projection>, prev: Prev) -> Result<Step, String> {
    let (views, prev) = (views.clone(), Arc::new(Mutex::new(Some(prev))));
    let done = on_pool(NAME, Arc::new(move || {
        let prev = prev.lock().unwrap_or_else(|e| e.into_inner()).take().expect("a step runs once");
        as_task(TASK, || {
            let polled = match prev {
                Prev::First => None,
                Prev::Answer(from) => Some(views.apply_watch(from)),
            };
            Step { polled, argv: views.watch_argv() }
        })
    }))
    .await;
    done.map_err(|e| e.to_string())
}

fn into_error(f: Fail) -> LcError {
    match f {
        Fail::NotRunning(m) => LcError::NotRunning(m),
        f => LcError::Fault(f.into_text()),
    }
}

async fn exchange(socket: &Path, argv: &[String]) -> Result<String, LcError> {
    let wait = call_wait(argv);
    let call = async {
        let stream = UnixStream::connect(socket).await.map_err(|e| into_error(connect_failure(socket, &e)))?; // loop-safe: tokio's non-blocking socket
        let request = encode_request(argv, None, Some(TASK), None).map_err(into_error)?;
        let (read, mut write) = stream.into_split();
        let lost = |why: &str| LcError::Fault(format!("cannot tell: the lifecycle service took the request but its reply was lost ({why})"));
        write.write_all(&request).await.map_err(|_| lost("write"))?;
        let mut reader = BufReader::with_capacity(1 << 20, read);
        let mut head = String::new();
        if !matches!(reader.read_line(&mut head).await, Ok(n) if n > 0) {
            return Err(lost("no reply"));
        }
        let head = ReplyHead::parse(&head).map_err(into_error)?;
        let mut body = vec![0u8; head.body_len()];
        reader.read_exact(&mut body).await.map_err(|_| lost("truncated reply"))?;
        head.reply(body).map_err(into_error)?.into_answer()
    };
    match tokio::time::timeout(wait, call).await {
        Ok(answer) => answer,
        Err(_) => Err(LcError::Fault(format!("cannot tell: no answer on {} within {}s", socket.display(), wait.as_secs()))),
    }
}

/// Whether the source goes on: false once the host's channel has closed.
async fn observe(polled: &Polled, table: &ChangeTable, tx: &mpsc::Sender<Submit>) -> bool {
    let (rev, body) = match polled {
        Polled::Quiet { rev } => {
            table.settle(*rev);
            return true;
        }
        Polled::Applied { through, keys } => (*through, Body { rev: *through, keys: keys.clone(), reload: false }),
        Polled::Reloaded { rev } => (*rev, Body { rev: *rev, keys: Vec::new(), reload: true }),
    };
    table.set(rev);
    tx.send(body.submit()).await.is_ok()
}

/// Runs until `tx` closes. A watch is sent only after the last answer is applied and its event
/// sent, so a full channel holds the next watch back and it resumes from the revision held.
async fn watch(views: Arc<Projection>, table: Arc<ChangeTable>, socket: PathBuf, tx: mpsc::Sender<Submit>) {
    let _watching = views.hold_watching();
    let mut prev = Prev::First;
    loop {
        let Step { polled, argv } = match step(&views, prev).await {
            Ok(step) => step,
            Err(why) => {
                prev = Prev::Answer(Some(Err(LcError::Fault(format!("the store views' step failed: {why}")))));
                let _ = on_pool(NAME, Arc::new(move || eprintln!("spira-runtime: source {NAME}: {why}"))).await;
                continue;
            }
        };
        match polled {
            Some(Ok(polled)) => {
                if !observe(&polled, &table, &tx).await {
                    return;
                }
            }
            Some(Err(_)) => {
                tokio::select! {
                    _ = tx.closed() => return,
                    _ = tokio::time::sleep(RETRY) => {} // clock-safe: paces a retry of a store that did not answer on the source's own task
                }
            }
            None => {}
        }
        prev = Prev::Answer(match argv {
            None => None,
            Some(argv) => Some(tokio::select! {
                _ = tx.closed() => return,
                answer = exchange(&socket, &argv) => answer,
            }),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use runtime::changes::Changes;
    use runtime::STORE_STREAM;
    use serde_json::json;
    use spira_store::lc_call::LcClient;
    use tokio::net::UnixListener;

    use super::*;

    enum Served {
        Out(String),
        Hold,
        CloseMidReply,
    }

    struct Listener {
        _dir: testkit::TempDir,
        path: PathBuf,
        watches: Arc<AtomicUsize>,
        reached_hold: mpsc::UnboundedReceiver<Vec<String>>,
    }

    fn serve(name: &str, script: impl Fn(usize, &[String]) -> Served + Send + Sync + 'static) -> Listener {
        let dir = testkit::TempDir::new(name);
        let path = dir.path().join("lc.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (held, reached_hold) = mpsc::unbounded_channel();
        let watches = Arc::new(AtomicUsize::new(0));
        let (script, counted) = (Arc::new(script), watches.clone());
        tokio::spawn(async move {
            loop {
                let Ok((conn, _)) = listener.accept().await else { return };
                let (script, counted, held) = (script.clone(), counted.clone(), held.clone());
                tokio::spawn(async move {
                    let (read, mut write) = conn.into_split();
                    let mut line = String::new();
                    BufReader::new(read).read_line(&mut line).await.unwrap();
                    let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
                    let argv: Vec<String> = serde_json::from_value(request.get("argv").cloned().unwrap_or(request)).unwrap();
                    let nth = if argv[0] == "watch" { counted.fetch_add(1, Ordering::SeqCst) } else { 0 };
                    match script(nth, &argv) {
                        Served::Out(out) => {
                            write.write_all(format!("{}\n{out}", json!({ "exit_code": 0, "len": out.len() })).as_bytes()).await.unwrap();
                            let _ = tokio::io::copy(&mut tokio::io::empty(), &mut write).await;
                        }
                        Served::Hold => {
                            let _ = held.send(argv);
                            std::future::pending::<()>().await;
                        }
                        Served::CloseMidReply => {
                            write.write_all(format!("{}\nabc", json!({ "exit_code": 0, "len": 100 })).as_bytes()).await.unwrap();
                        }
                    }
                });
            }
        });
        Listener { _dir: dir, path, watches, reached_hold }
    }

    fn views_on(path: &Path) -> Arc<Projection> {
        Arc::new(Projection::new(LcClient::socket(path), Duration::from_secs(30), Arc::new(Instant::now)))
    }

    async fn load(views: &Arc<Projection>) {
        let views = views.clone();
        tokio::task::spawn_blocking(move || views.load().unwrap()).await.unwrap();
    }

    fn load_script(lists: &AtomicUsize, argv: &[String]) -> Option<Served> {
        match argv[0].as_str() {
            "rc" => Some(Served::Out(json!({ "ok": { "items": [], "list_rev": if lists.fetch_add(1, Ordering::SeqCst) == 0 { 7 } else { 12 } } }).to_string())),
            "list" => Some(Served::Out("[]".into())),
            _ => None,
        }
    }

    #[test_id(tky05vt)]
    #[tokio::test]
    async fn the_source_submits_one_event_per_applied_answer_and_keeps_the_table_at_the_last_revision() {
        let lists = AtomicUsize::new(0);
        let mut server = serve("applied", move |nth, argv| {
            load_script(&lists, argv).unwrap_or_else(|| match nth {
                0 => Served::Out(json!({ "events": [{ "key": "lifecycle/sp-a", "rev": 9, "value": {} }, { "key": "readiness/sp-a", "rev": 9, "value": {} }], "through": 9 }).to_string()),
                1 => Served::Out(json!({ "too_old": { "compacted": 11 } }).to_string()),
                2 => Served::Out(json!({ "progress_rev": 15 }).to_string()),
                _ => Served::Hold,
            })
        });
        let views = views_on(&server.path);
        load(&views).await;
        assert_eq!(server.watches.load(Ordering::SeqCst), 0, "installing the views opens no watch");
        let table = Arc::new(ChangeTable::default());
        assert_eq!(table.rev(), None);
        let (tx, mut rx) = mpsc::channel(4);
        let source = Box::new(StoreChanged { views, table: table.clone(), socket: server.path.clone() });
        tokio::spawn(source.run(tx));
        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        server.reached_hold.recv().await.unwrap();
        assert_eq!((first.stream.as_str(), second.stream.as_str()), (STORE_STREAM, STORE_STREAM));
        assert_eq!(Body::parse(&first.kind.body), Some(Body { rev: 9, keys: vec!["lifecycle/sp-a".into(), "readiness/sp-a".into()], reload: false }));
        assert_eq!(Body::parse(&second.kind.body), Some(Body { rev: 12, keys: vec![], reload: true }));
        assert!(rx.try_recv().is_err(), "a quiet answer submits nothing");
        assert_eq!(table.rev(), Some(12), "a quiet answer moves a table that holds a revision no further: any write anywhere moves the store's head");
    }

    #[test_id(trhwkl6)]
    #[tokio::test]
    async fn the_source_ends_when_the_host_channel_closes() {
        let lists = AtomicUsize::new(0);
        let server = serve("closes", move |_, argv| {
            load_script(&lists, argv).unwrap_or_else(|| Served::Out(json!({ "events": [{ "key": "lifecycle/sp-a", "rev": 4, "value": {} }], "through": 4 }).to_string()))
        });
        let views = views_on(&server.path);
        load(&views).await;
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let source = Box::new(StoreChanged { views, table: Arc::new(ChangeTable::default()), socket: server.path.clone() });
        source.run(tx).await;
    }

    fn comms() -> Vec<String> {
        std::fs::read_dir(std::path::Path::new(system_paths::PROC_SELF).join("task")).unwrap().flatten().filter_map(|t| std::fs::read_to_string(t.path().join("comm")).ok()).map(|c| c.trim().to_string()).collect()
    }

    #[cfg(target_os = "linux")]
    #[test_id(ts443rn)]
    #[tokio::test]
    async fn an_unanswered_watch_holds_no_thread_and_leaves_the_loop_running() {
        let planted = {
            let (up, wait) = (std::sync::Arc::new(std::sync::Barrier::new(2)), std::sync::Arc::new(std::sync::Barrier::new(2)));
            let (u, w) = (up.clone(), wait.clone());
            let t = std::thread::Builder::new().name("source-store.changed".into()).spawn(move || {
                u.wait();
                w.wait();
            }).unwrap();
            up.wait();
            let found = comms().iter().any(|c| c == "source-store.ch");
            wait.wait();
            t.join().unwrap();
            found
        };
        assert!(planted, "the matcher must see a thread named as the source's was");
        let lists = AtomicUsize::new(0);
        let mut server = serve("loop", move |_, argv| load_script(&lists, argv).unwrap_or(Served::Hold));
        let views = views_on(&server.path);
        load(&views).await;
        let (tx, _rx) = mpsc::channel(1);
        let source = Box::new(StoreChanged { views, table: Arc::new(ChangeTable::default()), socket: server.path.clone() });
        let running = tokio::spawn(source.run(tx));
        let argv = server.reached_hold.recv().await.unwrap();
        assert_eq!(argv[0], "watch");
        let ran = tokio::spawn(async { 1 + 1 }).await.unwrap();
        assert_eq!(ran, 2, "another task runs while the watch is unanswered");
        assert!(!running.is_finished());
        assert!(!comms().iter().any(|c| c == "source-store.ch"), "{:?}", comms());
    }

    #[test_id(tg8a9qx)]
    #[tokio::test]
    async fn a_reply_cut_short_is_followed_by_a_watch_from_the_same_revision_and_no_event_is_lost() {
        let lists = AtomicUsize::new(0);
        let first = Arc::new(Mutex::new(Vec::<String>::new()));
        let resumed = Arc::new(AtomicUsize::new(0));
        let (seen, same) = (first.clone(), resumed.clone());
        let mut server = serve("cut", move |nth, argv| {
            load_script(&lists, argv).unwrap_or_else(|| match nth {
                0 => {
                    *seen.lock().unwrap() = argv.to_vec();
                    Served::CloseMidReply
                }
                1 => {
                    same.store((*seen.lock().unwrap() == argv) as usize, Ordering::SeqCst);
                    Served::Out(json!({ "events": [{ "key": "lifecycle/sp-a", "rev": 9, "value": {} }], "through": 9 }).to_string())
                }
                _ => Served::Hold,
            })
        });
        let views = views_on(&server.path);
        load(&views).await;
        let (tx, mut rx) = mpsc::channel(4);
        let source = Box::new(StoreChanged { views, table: Arc::new(ChangeTable::default()), socket: server.path.clone() });
        tokio::spawn(source.run(tx));
        let event = rx.recv().await.unwrap();
        assert_eq!(Body::parse(&event.kind.body), Some(Body { rev: 9, keys: vec!["lifecycle/sp-a".into()], reload: false }));
        server.reached_hold.recv().await.unwrap();
        assert_eq!(resumed.load(Ordering::SeqCst), 1, "the watch after the cut one is the same watch");
    }

    #[test_id(t273ici)]
    #[tokio::test]
    async fn a_cut_reply_sets_lost_and_holds_the_revision() {
        let lists = AtomicUsize::new(0);
        let mut server = serve("lost", move |nth, argv| load_script(&lists, argv).unwrap_or(if nth == 0 { Served::CloseMidReply } else { Served::Hold }));
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock = now.clone();
        let views = Arc::new(Projection::new(LcClient::socket(&server.path), Duration::from_secs(1), Arc::new(move || *clock.lock().unwrap())));
        load(&views).await;
        let before = views.watch_argv().unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let source = Box::new(StoreChanged { views: views.clone(), table: Arc::new(ChangeTable::default()), socket: server.path.clone() });
        tokio::spawn(source.run(tx));
        assert_eq!(server.reached_hold.recv().await.unwrap(), before, "the watch after the cut one is from the revision held");
        *now.lock().unwrap() += Duration::from_secs(120);
        let read = views.ready_counts();
        assert!(matches!(&read, Err(LcError::Refused(m)) if m.contains("the watch is lost") && m.contains("truncated reply")), "{read:?}");
    }

    #[test_id(txk5y52)]
    #[tokio::test]
    async fn a_full_channel_holds_the_next_watch_back_until_the_event_is_taken() {
        let lists = AtomicUsize::new(0);
        let mut server = serve("full", move |nth, argv| {
            load_script(&lists, argv).unwrap_or_else(|| match nth {
                0 | 1 => Served::Out(json!({ "events": [{ "key": "lifecycle/sp-a", "rev": 9 + nth, "value": {} }], "through": 9 + nth }).to_string()),
                _ => Served::Hold,
            })
        });
        let views = views_on(&server.path);
        load(&views).await;
        let table = Arc::new(ChangeTable::default());
        let (tx, mut rx) = mpsc::channel(1);
        let source = Box::new(StoreChanged { views, table: table.clone(), socket: server.path.clone() });
        tokio::spawn(source.run(tx));
        while table.rev() != Some(10) {
            tokio::task::yield_now().await;
        }
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(server.watches.load(Ordering::SeqCst), 2, "the third watch waits on the send that the full channel holds");
        rx.recv().await.unwrap();
        rx.recv().await.unwrap();
        server.reached_hold.recv().await.unwrap();
        assert_eq!(server.watches.load(Ordering::SeqCst), 3);
    }

    #[test_id(tkaad2r)]
    #[tokio::test]
    async fn the_source_ends_while_a_watch_is_unanswered_once_the_host_channel_closes() {
        let lists = AtomicUsize::new(0);
        let mut server = serve("cancel", move |_, argv| load_script(&lists, argv).unwrap_or(Served::Hold));
        let views = views_on(&server.path);
        load(&views).await;
        let (tx, rx) = mpsc::channel(1);
        let source = Box::new(StoreChanged { views, table: Arc::new(ChangeTable::default()), socket: server.path.clone() });
        let running = tokio::spawn(source.run(tx));
        server.reached_hold.recv().await.unwrap();
        drop(rx);
        tokio::time::timeout(Duration::from_secs(10), running).await.expect("the source ends with its channel").unwrap();
    }

    #[test_id(ta1cc8w)]
    #[tokio::test]
    async fn a_store_that_is_not_running_marks_the_watch_lost_and_moves_nothing() {
        let lists = AtomicUsize::new(0);
        let server = serve("absent", move |_, argv| load_script(&lists, argv).unwrap_or(Served::Hold));
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock = now.clone();
        let views = Arc::new(Projection::new(LcClient::socket(&server.path), Duration::from_secs(1), Arc::new(move || *clock.lock().unwrap())));
        load(&views).await;
        let before = views.watch_argv().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let source = Box::new(StoreChanged { views: views.clone(), table: Arc::new(ChangeTable::default()), socket: server._dir.path().join("absent.sock") });
        let running = tokio::spawn(source.run(tx));
        *now.lock().unwrap() += Duration::from_secs(120);
        while !matches!(views.ready_counts(), Err(LcError::Refused(m)) if m.contains("the watch is lost: store not running")) {
            tokio::task::yield_now().await;
        }
        assert_eq!(views.watch_argv(), Some(before));
        assert!(rx.try_recv().is_err());
        running.abort();
    }
}
