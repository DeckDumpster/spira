//! WAKE-MISSED lines for the Concierge inbox: a task whose run found work on the sweep alone, once
//! per task per process start. The engine reports each such run; this only says it.

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{iso8601, INBOX_APPEND};

pub const STREAM: &str = WAKE_AUDIT_STREAM;
pub const TICK: EventKindTag = "wake-audit.tick";
pub const STATE: EventKindTag = "wake-audit.state";
pub const NAME: &str = "wake-audit";
const APPEND_DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeAudit {
    pub interval_ms: u64,
}

/// The tasks already paged since the process started; memory-only, so a restart pages again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub paged: Vec<String>,
}

fn line(f: &SweepFound) -> String {
    format!(
        "WAKE-MISSED {}: found work on its one-minute sweep alone ({} so far); the store moved to rev {} and no store.changed woke it. Declared reads: {}. Fix the row's reads in spira-runtime/src/tasks*.rs, or the store.changed source if the reads are right.",
        f.task,
        f.count,
        f.rev,
        if f.reads.is_empty() { "none".into() } else { f.reads.join(", ") }
    )
}

impl WakeAudit {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("wake-audit/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn carry(view: &Projection, stream: &StreamKey) -> Carry {
        view.events(stream).iter().rev().find(|e| e.kind.tag == STATE).and_then(|e| serde_json::from_str(&e.kind.body).ok()).unwrap_or_default()
    }
}

impl Handler for WakeAudit {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, WAKE_SWEEP_FOUND]
    }

    fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return vec![Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") }];
        }
        let Ok(found) = serde_json::from_str::<SweepFound>(&ev.kind.body) else { return vec![] };
        let mut carry = Self::carry(view, &ev.stream);
        if carry.paged.contains(&found.task) {
            return vec![];
        }
        carry.paged.push(found.task.clone());
        vec![
            Command::Note(EventKind::new(STATE, &serde_json::to_string(&carry).unwrap_or_default())),
            Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:{}", found.task), format!("{} [watch:{NAME}] {}", iso8601(ev.at / 1000), line(&found)), APPEND_DEADLINE_MS)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::taskrig::TaskRig;

    fn found(task: &str, count: u64) -> EventKind {
        let f = SweepFound { task: task.into(), count, rev: 42, reads: vec!["lifecycle/".into(), "readiness/".into()] };
        EventKind::new(WAKE_SWEEP_FOUND, &serde_json::to_string(&f).unwrap())
    }

    #[test_id(tifpk5s)]
    #[test]
    fn the_first_miss_of_each_task_pages_once_and_names_what_the_fix_needs() {
        let mut rig = TaskRig::new(1, WakeAudit { interval_ms: 60_000 }.registration()).unwrap();
        let (_, stream, boot) = WakeAudit::boot();
        rig.tick(stream, boot).unwrap();
        assert!(rig.requested().is_empty(), "the boot tick asks nothing");
        for (task, count) in [("a", 1), ("a", 2), ("b", 1)] {
            rig.deliver(stream, found(task, count)).unwrap();
        }
        let asked: Vec<_> = rig.requested().iter().map(|r| (r.name.as_str(), r.key.as_str())).collect();
        assert_eq!(asked, [(INBOX_APPEND, "wake-audit:a"), (INBOX_APPEND, "wake-audit:b")]);
        let text = &rig.requested()[0].input;
        for part in ["WAKE-MISSED a:", "(1 so far)", "rev 42", "lifecycle/, readiness/"] {
            assert!(text.contains(part), "{part} missing from {text}");
        }
    }

    #[test_id(tn3wq7a)]
    #[test]
    fn a_restart_pages_a_task_again_and_the_boot_tick_arms_the_next_tick() {
        let mut rig = TaskRig::new(1, WakeAudit { interval_ms: 60_000 }.registration()).unwrap();
        let (_, stream, boot) = WakeAudit::boot();
        rig.tick(stream, boot.clone()).unwrap();
        assert_eq!(rig.engine().next_timer_at(), Some(60_000));
        rig.deliver(stream, found("a", 1)).unwrap();
        rig.reopen(WakeAudit { interval_ms: 60_000 }.registration()).unwrap();
        rig.tick(stream, boot).unwrap();
        rig.deliver(stream, found("a", 2)).unwrap();
        let keys: Vec<_> = rig.requested().iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["wake-audit:a", "wake-audit:a"]);
    }

    #[test_id(tr8kc2m)]
    #[test]
    fn a_task_with_no_declared_reads_is_named_as_having_none() {
        let f = SweepFound { task: "a".into(), count: 1, rev: 3, reads: vec![] };
        assert!(line(&f).contains("Declared reads: none."), "{}", line(&f));
    }
}
