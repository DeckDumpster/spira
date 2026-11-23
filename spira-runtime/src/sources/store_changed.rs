//! The `store.changed` source: the store's one watch, on the thread that keeps the in-process
//! views current. Each answer it applies becomes a memory-only event naming the keys that changed.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use runtime::host::Submit;
use runtime::sources::{on_thread, Running, Source};
use runtime::StoreChanged as Body;
use store_view::{Polled, Projection};
use tokio::sync::mpsc;

use crate::store::ChangeTable;

pub const NAME: &str = "store.changed";

pub struct StoreChanged {
    pub views: Arc<Projection>,
    pub table: Arc<ChangeTable>,
}

impl Source for StoreChanged {
    fn name(&self) -> &str {
        NAME
    }

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running {
        let (views, table) = (self.views, self.table);
        on_thread(NAME, move || {
        let closed = AtomicBool::new(false);
        spira_store::lc_call::as_task("store-watch", || {
            views.run(&closed, &mut |polled| {
                let (rev, body) = match polled {
                    Polled::Quiet { rev } => return table.settle(*rev),
                    Polled::Applied { through, keys } => (*through, Body { rev: *through, keys: keys.clone(), reload: false }),
                    Polled::Reloaded { rev } => (*rev, Body { rev: *rev, keys: Vec::new(), reload: true }),
                };
                table.set(rev);
                if tx.blocking_send(body.submit()).is_err() {
                    closed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            })
        })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use runtime::changes::Changes;
    use runtime::STORE_STREAM;
    use serde_json::json;
    use spira_store::lc_call::{LcClient, Reply, Transport};

    use super::*;

    struct Double {
        watches: AtomicUsize,
        lists: AtomicUsize,
        reached_end: Mutex<std::sync::mpsc::Sender<()>>,
    }

    impl Transport for Double {
        fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
            let ok = |out: String| Ok(Reply { code: 0, out, err: String::new(), read_ended: false });
            match argv[0].as_str() {
                "rc" => ok(json!({ "ok": { "items": [], "list_rev": if self.lists.fetch_add(1, Ordering::SeqCst) == 0 { 7 } else { 12 } } }).to_string()),
                "list" => ok("[]".into()),
                "watch" => match self.watches.fetch_add(1, Ordering::SeqCst) {
                    0 => ok(json!({ "events": [{ "key": "lifecycle/sp-a", "rev": 9, "value": {} }, { "key": "readiness/sp-a", "rev": 9, "value": {} }], "through": 9 }).to_string()),
                    1 => ok(json!({ "too_old": { "compacted": 11 } }).to_string()),
                    2 => ok(json!({ "progress_rev": 15 }).to_string()),
                    _ => {
                        let _ = self.reached_end.lock().unwrap().send(());
                        loop {
                            std::thread::park();
                        }
                    }
                },
                other => Err(format!("unscripted {other}")),
            }
        }
    }

    #[test_id(tky05vt)]
    #[tokio::test]
    async fn the_source_submits_one_event_per_applied_answer_and_keeps_the_table_at_the_last_revision() {
        let (end_tx, end_rx) = std::sync::mpsc::channel();
        let double = Arc::new(Double { watches: AtomicUsize::new(0), lists: AtomicUsize::new(0), reached_end: Mutex::new(end_tx) });
        struct Shared(Arc<Double>);
        impl Transport for Shared {
            fn exchange(&self, argv: &[String], stdin: Option<&str>, d: Duration) -> Result<Reply, String> {
                self.0.exchange(argv, stdin, d)
            }
        }
        let views = Arc::new(Projection::new(LcClient::over(Arc::new(Shared(double.clone())), true), Duration::from_secs(30), Arc::new(Instant::now)));
        views.load().unwrap();
        assert_eq!(double.watches.load(Ordering::SeqCst), 0, "installing the views opens no watch");
        let table = Arc::new(ChangeTable::default());
        assert_eq!(table.rev(), None);
        let (tx, mut rx) = mpsc::channel(4);
        let source = Box::new(StoreChanged { views, table: table.clone() });
        tokio::spawn(source.run(tx));
        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        end_rx.recv().unwrap();
        assert_eq!((first.stream.as_str(), second.stream.as_str()), (STORE_STREAM, STORE_STREAM));
        assert_eq!(Body::parse(&first.kind.body), Some(Body { rev: 9, keys: vec!["lifecycle/sp-a".into(), "readiness/sp-a".into()], reload: false }));
        assert_eq!(Body::parse(&second.kind.body), Some(Body { rev: 12, keys: vec![], reload: true }));
        assert!(rx.try_recv().is_err(), "a quiet answer submits nothing");
        assert_eq!(table.rev(), Some(12), "a quiet answer moves a table that holds a revision no further: any write anywhere moves the store's head");
    }

    #[test_id(trhwkl6)]
    #[tokio::test]
    async fn the_source_ends_when_the_host_channel_closes() {
        struct Applies;
        impl Transport for Applies {
            fn exchange(&self, argv: &[String], _: Option<&str>, _: Duration) -> Result<Reply, String> {
                let out = match argv[0].as_str() {
                    "rc" => json!({ "ok": { "items": [], "list_rev": 3 } }).to_string(),
                    "list" => "[]".into(),
                    _ => json!({ "events": [{ "key": "lifecycle/sp-a", "rev": 4, "value": {} }], "through": 4 }).to_string(),
                };
                Ok(Reply { code: 0, out, err: String::new(), read_ended: false })
            }
        }
        let views = Arc::new(Projection::new(LcClient::over(Arc::new(Applies), true), Duration::from_secs(30), Arc::new(Instant::now)));
        views.load().unwrap();
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let source = Box::new(StoreChanged { views, table: Arc::new(ChangeTable::default()) });
        source.run(tx).await;
    }
}
