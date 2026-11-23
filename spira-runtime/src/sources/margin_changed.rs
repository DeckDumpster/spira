//! The `margin.changed` source: a long-poll held open on the reader's `GET /api/changes?after=&wait=`,
//! each answer with changes one memory-only event, a failing reader retried on a capped backoff.

use std::path::{Path, PathBuf};
use std::time::Duration;

use margin_client::{HttpMargin, Margin};
use runtime::host::Submit;
use runtime::sources::{on_thread, Running, Source};
use runtime::{EventKind, MARGIN_CHANGED, MARGIN_STREAM, MARGIN_UNREACHABLE};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub const NAME: &str = "margin.changed";
pub const HOLD_SECS: u64 = 30;
pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(1);

/// The body of a `margin.unreachable` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unreachable {
    pub since_ms: u64,
    pub error: String,
}

/// Where the sequence a restart resumes from is kept, and the marks that move it.
pub fn dir(run_dir: &Path) -> PathBuf {
    run_dir.join("margin")
}

fn resume_file(run_dir: &Path) -> PathBuf {
    dir(run_dir).join("through")
}

fn done_dir(run_dir: &Path) -> PathBuf {
    dir(run_dir).join("done")
}

/// The highest sequence whose deliveries all succeeded; zero before any.
pub fn resume_point(run_dir: &Path) -> u64 {
    std::fs::read_to_string(resume_file(run_dir)).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0)
}

/// Marks `seq` delivered and moves the resume point as far as `batch` (the actionable sequences of one
/// answer, in order) allows: to just before its first undelivered one, or to `through` when every one is done.
pub fn record_done(run_dir: &Path, seq: u64, batch: &[u64], through: u64) -> Result<(), String> {
    let done = done_dir(run_dir);
    std::fs::create_dir_all(&done).map_err(|e| format!("{}: {e}", done.display()))?;
    std::fs::write(done.join(seq.to_string()), "").map_err(|e| format!("{}: {e}", done.display()))?;
    let at = resume_point(run_dir);
    let floor = batch.iter().find(|s| **s > at && !done.join(s.to_string()).exists()).map_or(through, |first| first.saturating_sub(1));
    if floor <= at {
        return Ok(());
    }
    let (tmp, to) = (dir(run_dir).join("through.tmp"), resume_file(run_dir));
    std::fs::write(&tmp, floor.to_string()).and_then(|()| std::fs::rename(&tmp, &to)).map_err(|e| format!("{}: {e}", to.display()))?;
    for s in batch.iter().filter(|s| **s <= floor) {
        let _ = std::fs::remove_file(done.join(s.to_string()));
    }
    Ok(())
}

pub struct MarginChanged {
    /// `None`: the reader is not configured, and the source parks.
    pub client: Option<HttpMargin>,
    pub run_dir: PathBuf,
    pub hold_secs: u64,
    pub backoff_first: Duration,
    pub backoff_cap: Duration,
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64) // clock-safe: stamps an event on a source thread, not the loop
}

impl MarginChanged {
    pub fn configured(cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<Self, String> {
        let run_dir = PathBuf::from(cfg("SPIRA_RUN")?);
        let client = match cfg(margin_client::URL_KEY)?.trim() {
            "" => None,
            _ => Some(HttpMargin::configured(cfg)?),
        };
        Ok(MarginChanged { client, run_dir, hold_secs: HOLD_SECS, backoff_first: BACKOFF_FIRST, backoff_cap: BACKOFF_CAP })
    }
}

impl Source for MarginChanged {
    fn name(&self) -> &str {
        NAME
    }

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) -> Running {
        on_thread(NAME, move || self.serve(tx))
    }
}

impl MarginChanged {
    fn serve(self, tx: mpsc::Sender<Submit>) {
        let Some(client) = &self.client else {
            while !tx.is_closed() {
                std::thread::sleep(IDLE); // clock-safe: a dormant source's own thread, waiting for shutdown
            }
            return;
        };
        let mut after = resume_point(&self.run_dir);
        let mut failing: Option<Duration> = None;
        let mut heard = false;
        loop {
            if tx.is_closed() {
                return;
            }
            let hold = if heard && failing.is_none() { self.hold_secs } else { 0 };
            match client.changes(after, hold) {
                Ok(answer) => {
                    let said = !heard || failing.is_some();
                    heard = true;
                    failing = None;
                    after = after.max(answer.through);
                    if answer.changes.is_empty() && !said {
                        continue;
                    }
                    let body = serde_json::to_string(&answer).unwrap_or_default();
                    let id = if answer.changes.is_empty() { format!("{MARGIN_CHANGED}/{}/{}", answer.through, wall_ms()) } else { format!("{MARGIN_CHANGED}/{}", answer.through) };
                    if tx.blocking_send(Submit::new(&id, MARGIN_STREAM, EventKind::new(MARGIN_CHANGED, &body))).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let wait = match failing {
                        None => {
                            let body = serde_json::to_string(&Unreachable { since_ms: wall_ms(), error }).unwrap_or_default();
                            let id = format!("{MARGIN_UNREACHABLE}/{after}/{}", wall_ms());
                            if tx.blocking_send(Submit::new(&id, MARGIN_STREAM, EventKind::new(MARGIN_UNREACHABLE, &body))).is_err() {
                                return;
                            }
                            self.backoff_first
                        }
                        Some(last) => (last * 2).min(self.backoff_cap),
                    };
                    failing = Some(wait);
                    std::thread::sleep(wait); // clock-safe: the retry wait on the source's own thread
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use super::*;

    const FIRST: Duration = Duration::from_millis(120);
    const CAP: Duration = Duration::from_millis(240);

    type Seen = Arc<Mutex<Vec<(Instant, String)>>>;

    /// Answers each request with the next scripted `(status, body)`, then stops accepting; returns the base URL and what it saw.
    fn serve(script: Vec<(u16, &'static str)>) -> (String, Seen) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen: Seen = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for (status, body) in script {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or_default().to_string();
                log.lock().unwrap().push((Instant::now(), line));
                write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            std::thread::sleep(Duration::from_secs(30)); // clock-safe: keeps the listener bound while the test reads
        });
        (base, seen)
    }

    fn source(base: &str, dir: &testkit::TempDir) -> Box<MarginChanged> {
        let token = dir.path().join("token");
        std::fs::write(&token, "tok").unwrap();
        let client = HttpMargin { base: base.into(), token_file: token.to_str().unwrap().into(), timeout: Duration::from_millis(400) };
        Box::new(MarginChanged { client: Some(client), run_dir: dir.path().into(), hold_secs: 0, backoff_first: FIRST, backoff_cap: CAP })
    }

    fn start(s: Box<MarginChanged>) -> mpsc::Receiver<Submit> {
        let (tx, rx) = mpsc::channel(8);
        std::thread::spawn(move || s.serve(tx));
        rx
    }

    #[test_id(tk4pitq)]
    #[test]
    fn two_answers_with_changes_submit_two_events_in_order_and_the_first_ask_resumes_from_the_recorded_point() {
        let dir = testkit::TempDir::new("margin-source-two");
        std::fs::create_dir_all(super::dir(dir.path())).unwrap();
        std::fs::write(resume_file(dir.path()), "7\n").unwrap();
        let (base, seen) = serve(vec![(200, r#"{"last_seq":9,"changes":[{"seq":8,"kind":"published","collection_id":"c1"}]}"#), (200, r#"{"last_seq":12,"changes":[{"seq":12,"kind":"published","collection_id":"c2"}]}"#)]);
        let mut rx = start(source(&base, &dir));
        let (a, b) = (rx.blocking_recv().unwrap(), rx.blocking_recv().unwrap());
        assert_eq!((a.stream.as_str(), a.kind.tag.as_str(), b.kind.tag.as_str()), (MARGIN_STREAM, MARGIN_CHANGED, MARGIN_CHANGED));
        assert!(a.kind.body.contains(r#""through":9"#) && b.kind.body.contains(r#""through":12"#), "{} / {}", a.kind.body, b.kind.body);
        let asked: Vec<String> = seen.lock().unwrap().iter().map(|(_, l)| l.clone()).collect();
        assert!(asked[0].contains("after=7"), "{asked:?}");
        assert!(asked[1].contains("after=9"), "{asked:?}");
    }

    #[test_id(t1srdqm)]
    #[test]
    fn a_reader_that_refuses_is_said_once_and_asked_again_no_sooner_than_the_backoff() {
        let dir = testkit::TempDir::new("margin-source-down");
        let (base, seen) = serve(vec![(500, "no"), (500, "no"), (500, "no"), (200, r#"{"last_seq":4,"changes":[]}"#)]);
        let mut rx = start(source(&base, &dir));
        let down = rx.blocking_recv().unwrap();
        let back = rx.blocking_recv().unwrap();
        assert_eq!((down.kind.tag.as_str(), back.kind.tag.as_str()), (MARGIN_UNREACHABLE, MARGIN_CHANGED), "three refusals are one unreachable, and the first good answer is said so the task can reset");
        let u: Unreachable = serde_json::from_str(&down.kind.body).unwrap();
        assert!(u.error.contains("HTTP 500") && u.since_ms > 0, "{u:?}");
        let times: Vec<Instant> = seen.lock().unwrap().iter().map(|(t, _)| *t).collect();
        let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps[0] >= FIRST && gaps[1] >= CAP && gaps[2] >= CAP, "{gaps:?}");
    }

    #[test_id(tvo7x8m)]
    #[test]
    fn two_outages_at_one_through_give_recoveries_with_distinct_ids() {
        let dir = testkit::TempDir::new("margin-source-twice");
        let (base, _) = serve(vec![(200, r#"{"last_seq":4,"changes":[]}"#), (500, "no"), (200, r#"{"last_seq":4,"changes":[]}"#), (500, "no"), (200, r#"{"last_seq":4,"changes":[]}"#)]);
        let mut rx = start(source(&base, &dir));
        let tags: Vec<(String, String)> = (0..5).map(|_| rx.blocking_recv().unwrap()).map(|s| (s.id.clone(), s.kind.tag.to_string())).collect();
        let recoveries: Vec<&String> = tags.iter().filter(|(_, t)| t == MARGIN_CHANGED).map(|(i, _)| i).collect();
        assert_eq!(recoveries.len(), 3, "{tags:?}");
        assert!(recoveries[0] != recoveries[1] && recoveries[1] != recoveries[2] && recoveries[0] != recoveries[2], "{recoveries:?}");
    }

    #[test_id(t4mq567)]
    #[test]
    fn an_empty_first_answer_is_said_and_retries_ask_without_waiting_though_the_hold_is_set() {
        let dir = testkit::TempDir::new("margin-source-first");
        let (base, seen) = serve(vec![(200, r#"{"last_seq":4,"changes":[]}"#), (500, "no"), (200, r#"{"last_seq":4,"changes":[]}"#), (200, r#"{"last_seq":5,"changes":[{"seq":5,"kind":"published","collection_id":"c"}]}"#)]);
        let mut s = source(&base, &dir);
        s.hold_secs = 7;
        let mut rx = start(s);
        let first = rx.blocking_recv().unwrap();
        assert_eq!(first.kind.tag.as_str(), MARGIN_CHANGED, "an empty first answer is the proof the feed is up");
        assert_eq!(rx.blocking_recv().unwrap().kind.tag.as_str(), MARGIN_UNREACHABLE);
        assert_eq!(rx.blocking_recv().unwrap().kind.tag.as_str(), MARGIN_CHANGED);
        assert_eq!(rx.blocking_recv().unwrap().kind.tag.as_str(), MARGIN_CHANGED);
        let asked: Vec<String> = seen.lock().unwrap().iter().map(|(_, l)| l.clone()).collect();
        assert!(asked[0].contains("wait=0") && asked[1].contains("wait=7") && asked[2].contains("wait=0") && asked[3].contains("wait=7"), "{asked:?}");
    }

    #[test_id(tmx57w7)]
    #[test]
    fn after_refusals_a_change_arrives_and_is_asked_with_the_same_after() {
        let dir = testkit::TempDir::new("margin-source-same-after");
        let (base, seen) = serve(vec![(500, "no"), (500, "no"), (200, r#"{"last_seq":3,"changes":[{"seq":3,"kind":"published","collection_id":"c"}]}"#)]);
        let mut s = source(&base, &dir);
        s.hold_secs = 7;
        let mut rx = start(s);
        let (down, back) = (rx.blocking_recv().unwrap(), rx.blocking_recv().unwrap());
        assert_eq!((down.kind.tag.as_str(), back.kind.tag.as_str()), (MARGIN_UNREACHABLE, MARGIN_CHANGED));
        assert!(back.kind.body.contains(r#""through":3"#), "{}", back.kind.body);
        let asked: Vec<String> = seen.lock().unwrap().iter().map(|(_, l)| l.clone()).collect();
        assert!(asked.iter().all(|l| l.contains("after=0")), "{asked:?}");
    }

    #[test_id(tqhabfo)]
    #[test]
    fn the_resume_point_moves_only_to_just_before_the_first_undelivered_change_of_an_answer() {
        let dir = testkit::TempDir::new("margin-resume");
        assert_eq!(resume_point(dir.path()), 0);
        let batch = [3, 5, 8];
        record_done(dir.path(), 5, &batch, 9).unwrap();
        assert_eq!(resume_point(dir.path()), 2, "everything under 3 is delivered or needs nothing; 3 is not delivered yet");
        record_done(dir.path(), 3, &batch, 9).unwrap();
        assert_eq!(resume_point(dir.path()), 7, "8 is the first not delivered");
        record_done(dir.path(), 8, &batch, 9).unwrap();
        assert_eq!(resume_point(dir.path()), 9);
        assert!(!done_dir(dir.path()).join("5").exists(), "a mark under the resume point is dropped");
    }
}
