//! The retained `round/<repo>/<batch>` topic: each open round's current event, kept under
//! `run/rounds/<batch>.event` so any process that connects can republish it. A round's ending
//! is published and then cleared from the broker, so only open rounds stay retained there.

use serde_json::{json, Value};
use spira_pub::{PubError, Publisher};
use std::cell::OnceCell;
use std::fs;
use std::path::{Path, PathBuf};

/// The event kinds after which a round is no longer open.
const ENDING: [&str; 3] = ["land", "abandon", "discard"];

type Make = Box<dyn Fn() -> Result<Publisher, PubError>>;

pub fn payload(kind: &str, by: &str, at: u64, fields: &[(&str, String)]) -> Value {
    let fields: serde_json::Map<String, Value> = fields.iter().map(|(k, v)| (k.to_string(), Value::from(v.clone()))).collect();
    json!({"kind": kind, "by": by, "at": at, "fields": fields})
}

/// One open round's kept event.
#[derive(Debug, Clone, PartialEq)]
struct Kept {
    repo: String,
    batch: String,
    event: Value,
}

impl Kept {
    fn topic(&self) -> String {
        format!("round/{}/{}", self.repo, self.batch)
    }
}

fn kept_path(run: &Path, batch: &str) -> PathBuf {
    run.join("rounds").join(format!("{batch}.event"))
}

fn kept(run: &Path) -> Vec<Kept> {
    let mut paths: Vec<PathBuf> = fs::read_dir(run.join("rounds"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "event"))
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|p| {
            let v: Value = serde_json::from_str(&fs::read_to_string(p).ok()?).ok()?;
            Some(Kept { repo: v["repo"].as_str()?.into(), batch: v["batch"].as_str()?.into(), event: v["event"].clone() })
        })
        .collect()
}

fn keep(run: &Path, k: &Kept) {
    let text = json!({"repo": k.repo, "batch": k.batch, "event": k.event}).to_string();
    let written = fs::create_dir_all(run.join("rounds"))
        .map_err(|e| e.to_string())
        .and_then(|()| crate::records::write_atomic(&kept_path(run, &k.batch), &text).map_err(|e| e.to_string()));
    if let Err(e) = written {
        eprintln!("queue.sh: round {}'s event was not kept, so no reconnect republishes it: {e}", k.batch);
    }
}

/// Forgets the kept rounds that ended with no event of their own: those their repo's listing
/// no longer holds. A listing that cannot be read forgets nothing.
fn forget_ended(run: &Path, open: &dyn Fn(&str) -> Result<Vec<String>, String>) {
    let mut listed: std::collections::BTreeMap<String, Option<Vec<String>>> = Default::default();
    for k in kept(run) {
        let ids = listed.entry(k.repo.clone()).or_insert_with(|| open(&k.repo).ok());
        if ids.as_ref().is_some_and(|ids| !ids.contains(&k.batch)) {
            let _ = fs::remove_file(kept_path(run, &k.batch));
        }
    }
}

/// The process's one publisher, named `queue`, made at its first round event.
pub struct RoundBus {
    make: Option<Make>,
    publisher: OnceCell<Option<Publisher>>,
}

impl RoundBus {
    /// On the broker the configuration names.
    pub fn configured() -> RoundBus {
        RoundBus::with(|| Publisher::new("queue"))
    }

    /// On the publisher `make` builds.
    pub fn with(make: impl Fn() -> Result<Publisher, PubError> + 'static) -> RoundBus {
        RoundBus { make: Some(Box::new(make)), publisher: OnceCell::new() }
    }

    /// Publishes nothing.
    pub fn off() -> RoundBus {
        RoundBus { make: None, publisher: OnceCell::new() }
    }

    fn publisher(&self, run: &Path) -> Option<&Publisher> {
        let make = self.make.as_ref()?;
        self.publisher
            .get_or_init(|| {
                let publisher = make().map_err(|e| eprintln!("queue.sh: no round is published: {e}")).ok()?;
                let run = run.to_path_buf();
                publisher.on_connect(move |again| {
                    for k in kept(&run) {
                        if let Err(e) = again.publish(&k.topic(), &k.event) {
                            eprintln!("queue.sh: round {} was not republished: {e}", k.batch);
                        }
                    }
                });
                Some(publisher)
            })
            .as_ref()
    }

    /// Waits until the broker has taken everything announced.
    #[cfg(all(test, feature = "integration"))]
    pub(crate) fn flush(&self, deadline: std::time::Duration) -> Result<(), PubError> {
        self.publisher.get().and_then(Option::as_ref).expect("a round was published").flush(deadline)
    }

    /// For a round that ended with no event of its own: publishes its ending, when this host
    /// still keeps an event for it as open.
    pub fn close(&self, run: &Path, repo: &str, batch: &str, kind: &str, event: Value, open: &dyn Fn(&str) -> Result<Vec<String>, String>) {
        if kept_path(run, batch).exists() {
            self.announce(run, repo, batch, kind, event, open);
        }
    }

    /// Publishes `event` as round `batch`'s current one. `open` lists a repo's open rounds. A
    /// failed publish is logged by the publisher and changes nothing here.
    pub fn announce(&self, run: &Path, repo: &str, batch: &str, kind: &str, event: Value, open: &dyn Fn(&str) -> Result<Vec<String>, String>) {
        // Gone before the connect, so the republish never repeats this round's previous event.
        let _ = fs::remove_file(kept_path(run, batch));
        forget_ended(run, open);
        let now = Kept { repo: repo.into(), batch: batch.into(), event };
        let ended = ENDING.contains(&kind);
        if let Some(p) = self.publisher(run) {
            let _ = p.publish(&now.topic(), &now.event);
            if ended {
                let _ = p.clear(&now.topic());
            }
        }
        if !ended {
            keep(run, &now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tbfuou6)]
    #[test]
    fn a_round_event_is_its_kind_actor_instant_and_fields() {
        let fields = [("red", "test-a.sh,test-b.sh".to_string()), ("reason", "it broke here".to_string())];
        assert_eq!(
            payload("red", "batcher", 1000, &fields),
            json!({"kind": "red", "by": "batcher", "at": 1000, "fields": {"red": "test-a.sh,test-b.sh", "reason": "it broke here"}})
        );
    }

    #[test_id(tvt693k)]
    #[test]
    fn a_kept_round_is_forgotten_only_when_its_repos_listing_answers_without_it() {
        let dir = testkit::TempDir::new("round-bus");
        for (repo, batch) in [("r", "open-1"), ("r", "gone-2"), ("unread", "u-3")] {
            keep(&dir, &Kept { repo: repo.into(), batch: batch.into(), event: json!({}) });
        }
        forget_ended(&dir, &|repo| match repo {
            "r" => Ok(vec!["open-1".into()]),
            _ => Err("no store".into()),
        });
        let left: Vec<String> = kept(&dir).into_iter().map(|k| k.batch).collect();
        assert_eq!(left, ["open-1", "u-3"]);
    }

    #[test_id(tdr8it3)]
    #[test]
    fn an_ending_event_keeps_nothing_to_republish() {
        let dir = testkit::TempDir::new("round-bus-end");
        let bus = RoundBus::off();
        let none = |_: &str| Err("unread".to_string());
        bus.announce(&dir, "r", "b1", "open", json!({"kind": "open"}), &none);
        assert_eq!(kept(&dir).len(), 1);
        bus.announce(&dir, "r", "b1", "land", json!({"kind": "land"}), &none);
        assert!(kept(&dir).is_empty());
    }
}
