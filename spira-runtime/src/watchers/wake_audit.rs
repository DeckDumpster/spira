//! WAKE-MISSED lines for the Concierge inbox: one per cause, not one per task. The engine reports
//! each run that found work on the sweep alone; this folds the reports and, on each tick, says
//! what is new. `source`: the store.changed source lost a change, once per process. `reads`: a
//! row's declared reads did not name what it decides from, each row named once per process.

use std::collections::BTreeMap;

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

/// What the audit holds since the process started; memory-only, so a restart pages again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    /// The newest finding of each task.
    pub found: BTreeMap<String, SweepFound>,
    pub source_paged: bool,
    pub named: Vec<String>,
}

fn counted(f: &SweepFound, with_reads: bool) -> String {
    if with_reads {
        format!("{} ({}; reads {})", f.task, f.count, f.reads.join(", "))
    } else {
        format!("{} ({})", f.task, f.count)
    }
}

fn source_line(lost: &[&SweepFound]) -> String {
    let rev = lost.iter().map(|f| f.rev).max().unwrap_or(0);
    let tasks: Vec<_> = lost.iter().map(|f| counted(f, false)).collect();
    format!(
        "WAKE-MISSED store.changed: the source lost a change up to rev {rev}; found on the one-minute sweep by {}. Fix the store.changed source (spira-runtime/src/sources/store_changed.rs).",
        tasks.join(", ")
    )
}

fn reads_line(fresh: &[&SweepFound]) -> String {
    let rev = fresh.iter().map(|f| f.rev).max().unwrap_or(0);
    let tasks: Vec<_> = fresh.iter().map(|f| counted(f, true)).collect();
    let changed: std::collections::BTreeSet<&str> = fresh.iter().flat_map(|f| f.changed.iter().map(String::as_str)).collect();
    let changed = if changed.is_empty() { "none heard".to_string() } else { changed.into_iter().collect::<Vec<_>>().join(", ") };
    format!("WAKE-MISSED reads: the store moved to rev {rev} and no declared read named the change; found on the one-minute sweep by {}; changed prefixes: {changed}. Fix those rows' reads in spira-runtime/src/tasks*.rs.", tasks.join(", "))
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

    fn note(carry: &Carry) -> Command {
        Command::Note(EventKind::new(STATE, &serde_json::to_string(carry).unwrap_or_default()))
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
        let mut carry = Self::carry(view, &ev.stream);
        if ev.kind.tag == WAKE_SWEEP_FOUND {
            let Ok(found) = serde_json::from_str::<SweepFound>(&ev.kind.body) else { return vec![] };
            carry.found.insert(found.task.clone(), found);
            return vec![Self::note(&carry)];
        }
        let mut out = vec![Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") }];
        let stamp = iso8601(ev.at / 1000);
        let lost: Vec<&SweepFound> = carry.found.values().filter(|f| f.lost).collect();
        let fresh: Vec<&SweepFound> = carry.found.values().filter(|f| !f.lost && !carry.named.contains(&f.task)).collect();
        let (page_source, page_reads) = (!carry.source_paged && !lost.is_empty(), !fresh.is_empty());
        if page_source {
            out.push(Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:source"), format!("{stamp} [watch:{NAME}] {}", source_line(&lost)), APPEND_DEADLINE_MS)));
        }
        if page_reads {
            let key = format!("{NAME}:reads:{}", fresh.iter().map(|f| f.task.as_str()).collect::<Vec<_>>().join(","));
            out.push(Command::Effect(EffectSpec::keyed(INBOX_APPEND, key, format!("{stamp} [watch:{NAME}] {}", reads_line(&fresh)), APPEND_DEADLINE_MS)));
        }
        if page_source || page_reads {
            let named: Vec<String> = fresh.iter().map(|f| f.task.clone()).collect();
            carry.source_paged |= page_source;
            carry.named.extend(named);
            out.push(Self::note(&carry));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::taskrig::TaskRig;

    fn found(task: &str, count: u64, rev: u64, lost: bool) -> EventKind {
        let f = SweepFound { task: task.into(), count, rev, reads: vec!["lifecycle/".into(), "readiness/".into()], lost, changed: vec![] };
        EventKind::new(WAKE_SWEEP_FOUND, &serde_json::to_string(&f).unwrap())
    }

    fn rig() -> TaskRig {
        let mut rig = TaskRig::new(1, WakeAudit { interval_ms: 60_000 }.registration()).unwrap();
        let (_, stream, boot) = WakeAudit::boot();
        rig.tick(stream, boot).unwrap();
        rig
    }

    fn keys(rig: &TaskRig) -> Vec<&str> {
        rig.requested().iter().map(|r| r.key.as_str()).collect()
    }

    #[test_id(tifpk5s)]
    #[test]
    fn a_lost_change_pages_once_for_all_tasks_it_hit_and_never_again() {
        let mut rig = rig();
        assert!(rig.requested().is_empty(), "the boot tick asks nothing");
        let stream = WakeAudit::boot().1;
        for (task, count, rev) in [("a", 1, 5), ("a", 2, 6), ("b", 1, 9), ("c", 4, 7)] {
            rig.deliver(stream, found(task, count, rev, true)).unwrap();
        }
        assert!(rig.requested().is_empty(), "a finding only folds");
        rig.advance(60_000).unwrap();
        assert_eq!(keys(&rig), ["wake-audit:source"]);
        let text = &rig.requested()[0].input;
        for part in ["WAKE-MISSED store.changed:", "up to rev 9", "a (2), b (1), c (4)", "store_changed.rs"] {
            assert!(text.contains(part), "{part} missing from {text}");
        }
        rig.deliver(stream, found("d", 1, 11, true)).unwrap();
        rig.advance(60_000).unwrap();
        assert_eq!(keys(&rig), ["wake-audit:source"]);
    }

    #[test_id(t4rqezv)]
    #[test]
    fn rows_whose_reads_missed_page_once_each_in_one_line_per_minute() {
        let mut rig = rig();
        let stream = WakeAudit::boot().1;
        rig.deliver(stream, found("b", 2, 8, false)).unwrap();
        rig.deliver(stream, found("a", 1, 8, false)).unwrap();
        rig.advance(60_000).unwrap();
        assert_eq!(keys(&rig), ["wake-audit:reads:a,b"]);
        let text = &rig.requested()[0].input;
        for part in ["WAKE-MISSED reads:", "rev 8", "a (1; reads lifecycle/, readiness/), b (2; reads lifecycle/, readiness/)", "tasks*.rs"] {
            assert!(text.contains(part), "{part} missing from {text}");
        }
        rig.deliver(stream, found("a", 2, 9, false)).unwrap();
        rig.deliver(stream, found("b", 3, 9, false)).unwrap();
        rig.advance(60_000).unwrap();
        assert_eq!(rig.requested().len(), 1, "named rows are not named again");
        rig.deliver(stream, found("c", 1, 10, false)).unwrap();
        rig.advance(60_000).unwrap();
        assert_eq!(keys(&rig), ["wake-audit:reads:a,b", "wake-audit:reads:c"]);
        assert!(!rig.requested()[1].input.contains("a ("));
    }

    #[test_id(tq8c2dv)]
    #[test]
    fn the_reads_line_names_the_prefixes_that_changed() {
        let mut rig = rig();
        let stream = WakeAudit::boot().1;
        let f = SweepFound { task: "a".into(), count: 1, rev: 8, reads: vec!["lifecycle/".into()], lost: false, changed: vec!["edges/".into(), "planning/".into()] };
        rig.deliver(stream, EventKind::new(WAKE_SWEEP_FOUND, &serde_json::to_string(&f).unwrap())).unwrap();
        rig.advance(60_000).unwrap();
        let text = &rig.requested()[0].input;
        assert!(text.contains("changed prefixes: edges/, planning/."), "{text}");
    }

    #[test_id(t60s0qn)]
    #[test]
    fn one_of_each_cause_in_a_minute_is_two_lines() {
        let mut rig = rig();
        let stream = WakeAudit::boot().1;
        rig.deliver(stream, found("a", 1, 5, true)).unwrap();
        rig.deliver(stream, found("b", 1, 5, false)).unwrap();
        rig.advance(60_000).unwrap();
        assert_eq!(keys(&rig), ["wake-audit:source", "wake-audit:reads:b"]);
    }

    #[test_id(tgp7vpe)]
    #[test]
    fn an_hour_of_ticks_with_no_finding_requests_nothing() {
        let mut rig = rig();
        let stream = WakeAudit::boot().1;
        rig.advance(3_600_000).unwrap();
        assert!(rig.requested().is_empty());
        rig.deliver(stream, found("a", 1, 5, true)).unwrap();
        rig.advance(60_000).unwrap();
        assert_eq!(rig.requested().len(), 1, "the same tick pages when there is a finding");
    }

    #[test_id(tn3wq7a)]
    #[test]
    fn a_restart_pages_the_same_cause_again_and_the_boot_tick_arms_the_next_tick() {
        let mut rig = rig();
        let (_, stream, boot) = WakeAudit::boot();
        assert_eq!(rig.engine().next_timer_at(), Some(60_000));
        rig.deliver(stream, found("a", 1, 5, true)).unwrap();
        rig.advance(60_000).unwrap();
        rig.reopen(WakeAudit { interval_ms: 60_000 }.registration()).unwrap();
        rig.tick(stream, boot).unwrap();
        rig.deliver(stream, found("a", 2, 6, true)).unwrap();
        rig.advance(60_000).unwrap();
        assert_eq!(keys(&rig), ["wake-audit:source", "wake-audit:source"]);
    }
}
