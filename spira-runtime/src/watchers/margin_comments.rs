//! Carries the operator's comments in the Margin reader to the Concierge's mailbox, and his Publish
//! to `cr unseal`. It runs on the `margin.changed` source's events; its only timer is the sweep that
//! asks again for an act that failed. `decide` does no IO.

use std::collections::BTreeMap;

use margin_client::{Changes, READER};
use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answered_spec, carried, iso8601, INBOX_APPEND};
use crate::sources::margin_changed::Unreachable;

pub const NAME: &str = "margin-comments";
pub const STREAM: &str = "task.margin-comments";
pub const BOOT: EventKindTag = "margin-comments.boot";
pub const STATE_TAG: EventKindTag = "margin-comments.state";
pub const DELIVER: &str = "margin.deliver";
pub const PUBLISH: &str = "margin.publish";

const DELIVER_DEADLINE_MS: u64 = 60_000;
pub const PUBLISH_CALL_SECS: u64 = 100;
const PUBLISH_DEADLINE_MS: u64 = (PUBLISH_CALL_SECS + 20) * 1000;
const INBOX_DEADLINE_MS: u64 = 10_000;
pub const MAX_ATTEMPTS: u32 = 20;

/// The input of `margin.deliver`: one comment of the operator's, and where it sits in its answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deliver {
    pub collection: String,
    pub thread: String,
    pub comment: String,
    pub seq: u64,
    pub batch: Vec<u64>,
    pub through: u64,
}

/// The input of `margin.publish`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publish {
    pub collection: String,
    pub seq: u64,
    pub batch: Vec<u64>,
    pub through: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failed {
    pub spec: EffectSpec,
    pub attempts: u32,
}

/// Carried between events as the body of the latest state event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    /// Epoch milliseconds the current run of unanswered feeds began; `None` while the feed answers.
    pub unreachable: Option<u64>,
    /// Acts that failed and wait for the next sweep, by key.
    pub failed: BTreeMap<String, Failed>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarginComments {
    pub sweep_ms: u64,
}

impl MarginComments {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("margin-comments/boot", STREAM, EventKind::new(BOOT, ""))
    }
}

fn say(at_ms: u64, m: &str) -> Command {
    let now = at_ms / 1000;
    Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:inbox:{now}:{:016x}", fnv1a(m)), format!("{} [watch:{NAME}] {m}", iso8601(now)), INBOX_DEADLINE_MS))
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// The change kinds that carry a new comment: a thread's first comment is logged as the thread's creation.
const NEW_COMMENT_KINDS: [&str; 2] = ["thread_created", "comment_created"];
const PUBLISHED_KIND: &str = "published";

fn actions(changes: &Changes) -> Vec<Command> {
    let field = |c: &serde_json::Value, k: &str| c[k].as_str().map(str::to_string);
    let wanted = |c: &serde_json::Value| match c["kind"].as_str() {
        Some(k) if NEW_COMMENT_KINDS.contains(&k) => c["author"].as_str() == Some(READER),
        Some(PUBLISHED_KIND) => true,
        _ => false,
    };
    let acts: Vec<(u64, &serde_json::Value)> = changes.changes.iter().filter(|c| wanted(c)).filter_map(|c| c["seq"].as_u64().map(|s| (s, c))).collect();
    let batch: Vec<u64> = acts.iter().map(|(s, _)| *s).collect();
    let through = changes.through;
    let mut cmds = vec![];
    for (seq, c) in acts {
        let spec = if c["kind"].as_str() == Some(PUBLISHED_KIND) {
            field(c, "collection_id").map(|collection| EffectSpec::keyed(PUBLISH, format!("{NAME}:publish:{collection}"), json(&Publish { collection, seq, batch: batch.clone(), through }), PUBLISH_DEADLINE_MS))
        } else {
            match (field(c, "collection_id"), field(c, "thread_id"), field(c, "comment_id")) {
                (Some(collection), Some(thread), Some(comment)) => Some(EffectSpec::keyed(DELIVER, format!("{NAME}:comment:{comment}"), json(&Deliver { collection, thread, comment, seq, batch: batch.clone(), through }), DELIVER_DEADLINE_MS)),
                _ => None,
            }
        };
        cmds.extend(spec.map(Command::Effect));
    }
    cmds
}

/// The new carry and the commands for `ev`; `answered` is the spec `ev` is the outcome of, if it is one.
pub fn decide(carry: &Carry, ev: &Event, answered: Option<&EffectSpec>) -> (Carry, Vec<Command>) {
    let mut next = carry.clone();
    let mut cmds = vec![];
    match ev.kind.tag.as_str() {
        BOOT => {
            for (key, f) in std::mem::take(&mut next.failed) {
                if f.attempts >= MAX_ATTEMPTS {
                    cmds.push(say(ev.at, &format!("margin act {key} failed {} times and is dropped until the next restart reads it again", f.attempts)));
                } else {
                    cmds.push(Command::Effect(f.spec.clone()));
                    next.failed.insert(key, f);
                }
            }
        }
        MARGIN_CHANGED => {
            next.unreachable = None;
            if let Ok(changes) = serde_json::from_str::<Changes>(&ev.kind.body) {
                cmds.extend(actions(&changes));
            }
        }
        MARGIN_UNREACHABLE => {
            if let (None, Ok(u)) = (carry.unreachable, serde_json::from_str::<Unreachable>(&ev.kind.body)) {
                next.unreachable = Some(u.since_ms);
                cmds.push(say(ev.at, &format!("margin feed unreachable since {}: {}", iso8601(u.since_ms / 1000), u.error)));
            }
        }
        EFFECT_SUCCEEDED | EFFECT_FAILED | EFFECT_TIMED_OUT => {
            let Some(spec) = answered.filter(|s| s.name == DELIVER || s.name == PUBLISH) else { return (next, cmds) };
            if ev.kind.tag == EFFECT_SUCCEEDED {
                next.failed.remove(&spec.key);
            } else {
                let attempts = carry.failed.get(&spec.key).map_or(0, |f| f.attempts) + 1;
                next.failed.insert(spec.key.clone(), Failed { spec: spec.clone(), attempts });
            }
        }
        _ => {}
    }
    (next, cmds)
}

impl Handler for MarginComments {
    fn interests(&self) -> &[EventKindTag] {
        &[BOOT, MARGIN_CHANGED, MARGIN_UNREACHABLE, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let own = StreamKey(STREAM.into());
        let carry = carried::<Carry>(view, &own, STATE_TAG);
        let answered = answered_spec(view, ev);
        let (next, mut cmds) = decide(&carry, ev, answered.as_ref());
        if next != carry {
            cmds.push(Command::EmitTo { stream: own, kind: EventKind::new(STATE_TAG, &json(&next)) });
        }
        if ev.kind.tag == BOOT {
            cmds.push(Command::Timer { at: ev.at + self.sweep_ms, kind: EventKind::new(BOOT, "") });
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(tag: &str, body: String) -> Event {
        Event { id: EventId("e".into()), stream: StreamKey(MARGIN_STREAM.into()), kind: EventKind::new(tag, &body), at: 1_000_000, cause: None, correlation: CorrelationId("c".into()) }
    }

    fn comment(seq: u64, id: &str, author: &str) -> serde_json::Value {
        json!({"seq": seq, "kind": "comment_created", "collection_id": "col1", "thread_id": "t1", "comment_id": id, "author": author, "visibility": "now", "body": "x"})
    }

    fn changed(changes: Vec<serde_json::Value>) -> Event {
        event(MARGIN_CHANGED, serde_json::to_string(&Changes { through: 9, changes }).unwrap())
    }

    fn effects(cmds: &[Command]) -> Vec<(String, String)> {
        cmds.iter().filter_map(|c| if let Command::Effect(s) = c { Some((s.name.clone(), s.key.clone())) } else { None }).collect()
    }

    #[test_id(ttu5nhc)]
    #[test]
    fn two_comments_by_the_operator_and_one_by_an_agent_are_two_deliveries_keyed_by_comment() {
        let ev = changed(vec![comment(3, "c-1", "reader"), comment(4, "c-2", "concierge"), comment(5, "c-3", "reader")]);
        let (_, cmds) = decide(&Carry::default(), &ev, None);
        assert_eq!(effects(&cmds), [(DELIVER.to_string(), "margin-comments:comment:c-1".to_string()), (DELIVER.to_string(), "margin-comments:comment:c-3".to_string())]);
        let Command::Effect(first) = &cmds[0] else { unreachable!() };
        let d: Deliver = serde_json::from_str(&first.input).unwrap();
        assert_eq!((d.seq, d.batch, d.through), (3, vec![3, 5], 9));
    }

    #[test_id(tul4gty)]
    #[test]
    fn a_publish_change_is_one_publish_keyed_by_collection() {
        let ev = changed(vec![json!({"seq": 7, "kind": "published", "collection_id": "col1"}), json!({"seq": 8, "kind": "collection_updated", "collection_id": "col1"})]);
        assert_eq!(effects(&decide(&Carry::default(), &ev, None).1), [(PUBLISH.to_string(), "margin-comments:publish:col1".to_string())]);
    }

    #[test_id(tiszg4b)]
    #[test]
    fn an_unreachable_feed_is_said_once_per_run_and_again_after_a_good_answer() {
        let down = |since| event(MARGIN_UNREACHABLE, serde_json::to_string(&Unreachable { since_ms: since, error: "connection refused".into() }).unwrap());
        let (c1, said1) = decide(&Carry::default(), &down(1_000), None);
        let (c2, said2) = decide(&c1, &down(2_000), None);
        assert_eq!((said1.len(), said2.len()), (1, 0));
        let Command::Effect(line) = &said1[0] else { unreachable!() };
        assert!(line.input.contains("margin feed unreachable since 1970-01-01T00:00:01Z: connection refused"), "{}", line.input);
        let (c3, _) = decide(&c2, &changed(vec![]), None);
        assert_eq!(c3.unreachable, None);
        assert_eq!(decide(&c3, &down(3_000), None).1.len(), 1);
    }

    #[test_id(tuvprkw)]
    #[test]
    fn a_failed_delivery_is_asked_again_on_the_sweep_until_it_lands() {
        let ev = changed(vec![comment(3, "c-1", "reader")]);
        let Command::Effect(spec) = decide(&Carry::default(), &ev, None).1.remove(0) else { unreachable!() };
        let (failed, _) = decide(&Carry::default(), &event(EFFECT_FAILED, "boom".into()), Some(&spec));
        assert_eq!(failed.failed[&spec.key].attempts, 1);
        let (still, again) = decide(&failed, &event(BOOT, String::new()), None);
        assert_eq!(effects(&again), [(DELIVER.to_string(), spec.key.clone())]);
        let (done, _) = decide(&still, &event(EFFECT_SUCCEEDED, String::new()), Some(&spec));
        assert!(done.failed.is_empty());
    }
}
