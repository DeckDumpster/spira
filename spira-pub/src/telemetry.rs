//! The library's own metrics: publishes, failures by reason and queue depth. One thread pushes
//! them, woken only by a sample, so a slow store costs a publisher nothing and an idle one
//! does nothing.

use std::collections::BTreeMap;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::Duration;

use spira_otel::metrics::{MetricsSink, Point};

pub const PUBLISHES: &str = "spira_pub_publishes";
pub const FAILURES: &str = "spira_pub_failures";
pub const QUEUE_DEPTH: &str = "spira_pub_queue_depth";

const BACKLOG: usize = 1024;
/// What dropping a publisher may add to wait for its last samples to be pushed.
const DRAIN_BOUND: Duration = Duration::from_millis(500);

pub enum Sample {
    Published { depth: usize },
    Failed { reason: &'static str, count: usize },
}

pub struct Telemetry {
    tx: Option<SyncSender<Sample>>,
    done: Option<std::sync::Mutex<Receiver<()>>>,
}

impl Telemetry {
    pub fn start(sink: Option<MetricsSink>, publisher: &str) -> Telemetry {
        let Some(sink) = sink else { return Telemetry { tx: None, done: None } };
        let (tx, rx) = sync_channel::<Sample>(BACKLOG);
        let (done_tx, done_rx) = sync_channel(1);
        let publisher = publisher.to_string();
        let spawned = std::thread::Builder::new().name("spira-pub-otel".into()).spawn(move || {
            while let Ok(first) = rx.recv() {
                push(&sink, &publisher, std::iter::once(first).chain(rx.try_iter()));
            }
            let _ = done_tx.send(());
        });
        match spawned {
            Ok(_) => Telemetry { tx: Some(tx), done: Some(done_rx.into()) },
            Err(_) => Telemetry { tx: None, done: None },
        }
    }

    /// A full backlog drops the sample.
    pub fn record(&self, sample: Sample) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(sample);
        }
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(done) = self.done.take().and_then(|d| d.into_inner().ok()) {
            let _ = done.recv_timeout(DRAIN_BOUND);
        }
    }
}

fn push(sink: &MetricsSink, publisher: &str, samples: impl Iterator<Item = Sample>) {
    let mut published = 0u64;
    let mut depth = None;
    let mut failed: BTreeMap<&str, u64> = BTreeMap::new();
    for s in samples {
        match s {
            Sample::Published { depth: d } => {
                published += 1;
                depth = Some(d);
            }
            Sample::Failed { reason, count } => *failed.entry(reason).or_default() += count as u64,
        }
    }
    if let Some(depth) = depth {
        sink.push(
            &[Point::Count { name: PUBLISHES, value: published }, Point::Gauge { name: QUEUE_DEPTH, value: depth as f64 }],
            &[("publisher", publisher)],
        );
    }
    for (reason, value) in failed {
        sink.push(&[Point::Count { name: FAILURES, value }], &[("publisher", publisher), ("reason", reason)]);
    }
}
