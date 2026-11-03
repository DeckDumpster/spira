//! The one table of tasks the service hosts. `registrations()` and the boot list in `main.rs`
//! are both read from it, so a task registered is a task booted.

use runtime::event::{EventKind, Registration};
use runtime::heartbeat::{self, Heartbeat};

use crate::watchers::*;
use crate::Settings;

pub struct Task {
    pub name: &'static str,
    /// The first tick, handed to `Engine::start` each time the process starts: (id, stream, kind).
    pub boot: fn() -> (&'static str, &'static str, EventKind),
    pub cadence_ms: fn(&Settings) -> u64,
    /// Every effect or probe name the handler can ask for.
    pub effects: &'static [&'static str],
    pub build: fn(&Settings) -> Registration,
}

impl Task {
    pub fn stream(&self) -> &'static str {
        (self.boot)().1
    }
}

pub static TASKS: &[Task] = &[
    Task { name: heartbeat::NAME, boot: Heartbeat::boot, cadence_ms: |s| s.heartbeat_secs * 1000, effects: &[heartbeat::PING], build: |s| Heartbeat { interval_ms: s.heartbeat_secs * 1000, ping_deadline_ms: s.ping_deadline_ms() }.registration() },
    Task { name: round_duty::NAME, boot: round_duty::RoundDuty::boot, cadence_ms: |s| s.round_duty.interval_ms, effects: &[round_duty::OBSERVE, INBOX_APPEND], build: |s| s.round_duty.clone().registration() },
    Task { name: inbox_keeper::NAME, boot: inbox_keeper::InboxKeeper::boot, cadence_ms: |s| s.inbox_keeper.interval_ms, effects: &[inbox_keeper::OBSERVE, CONCIERGE_WAKE], build: |s| s.inbox_keeper.clone().registration() },
    Task { name: pr_notify::NAME, boot: pr_notify::PrNotify::boot, cadence_ms: |s| s.pr_notify.interval_ms, effects: &[pr_notify::OBSERVE, pr_notify::OPERATOR_NOTE, INBOX_APPEND], build: |s| s.pr_notify.clone().registration() },
    Task { name: asks::NAME, boot: asks::Asks::boot, cadence_ms: |s| s.asks.interval_ms, effects: &[asks::OBSERVE, INBOX_APPEND], build: |s| s.asks.clone().registration() },
    Task { name: design_asks::NAME, boot: design_asks::DesignAsks::boot, cadence_ms: |s| s.design_asks.interval_ms, effects: &[design_asks::OBSERVE, INBOX_APPEND], build: |s| s.design_asks.clone().registration() },
    Task { name: brain_dirty::NAME, boot: brain_dirty::BrainDirty::boot, cadence_ms: |s| s.brain_dirty.interval_ms, effects: &[brain_dirty::OBSERVE, INBOX_APPEND], build: |s| s.brain_dirty.clone().registration() },
    Task { name: supersede_duty::NAME, boot: supersede_duty::SupersedeDuty::boot, cadence_ms: |s| s.supersede_duty.interval_ms, effects: &[supersede_duty::OBSERVE, supersede_duty::ACT, INBOX_APPEND], build: |s| s.supersede_duty.clone().registration() },
    Task { name: bead_sweep::NAME, boot: bead_sweep::BeadSweep::boot, cadence_ms: |s| s.bead_sweep.interval_ms, effects: &[bead_sweep::OBSERVE, bead_sweep::REPAIR, INBOX_APPEND], build: |s| s.bead_sweep.clone().registration() },
    Task { name: publish_backlog::NAME, boot: publish_backlog::PublishBacklog::boot, cadence_ms: |s| s.publish_backlog.interval_ms, effects: &[publish_backlog::OBSERVE, INBOX_APPEND], build: |s| s.publish_backlog.clone().registration() },
    Task { name: repo_events::NAME, boot: repo_events::RepoEvents::boot, cadence_ms: |s| s.repo_events.interval_ms, effects: &[repo_events::OBSERVE, INBOX_APPEND], build: |s| s.repo_events.clone().registration() },
    Task { name: overrides::NAME, boot: overrides::Overrides::boot, cadence_ms: |s| s.overrides.interval_ms, effects: &[overrides::APPLY, INBOX_APPEND], build: |s| s.overrides.clone().registration() },
    Task { name: view::NAME, boot: view::View::boot, cadence_ms: |s| s.view.interval_ms, effects: &[view::OBSERVE, view::FLIP, view::FORGET], build: |s| s.view.clone().registration() },
    Task { name: reconciler::NAME, boot: reconciler::Reconciler::boot, cadence_ms: |s| s.reconciler.interval_ms, effects: &[reconciler::OBSERVE, reconciler::RECORD, reconciler::REMEDY, reconciler::MAIL], build: |s| s.reconciler.clone().registration() },
    Task { name: reconciler_flow::NAME, boot: reconciler_flow::ReconcilerFlow::boot, cadence_ms: |s| s.reconciler_flow.interval_ms, effects: &[reconciler_flow::OBSERVE, reconciler_flow::STATUS, reconciler_flow::MAIL], build: |s| s.reconciler_flow.clone().registration() },
];

/// The boot ticks of every task, in table order.
pub fn boots() -> Vec<(&'static str, &'static str, EventKind)> {
    TASKS.iter().map(|t| (t.boot)()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::taskrig::TaskRig;

    /// Every `pub const NAME` declared by a module under `watchers/`, read from the source so the
    /// list does not come from the table it checks.
    fn watcher_names() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/watchers");
        let mut out = vec![];
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let src = std::fs::read_to_string(e.path()).unwrap();
            for line in src.lines() {
                if let Some(rest) = line.strip_prefix("pub const NAME: &str = \"") {
                    out.push(rest.trim_end_matches("\";").to_string());
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn every_watcher_module_and_the_heartbeat_have_a_row() {
        let names = watcher_names();
        assert!(names.len() >= 14 && names.contains(&"asks".to_string()), "the source scan must find the watchers: {names:?}");
        let rows: Vec<&str> = TASKS.iter().map(|t| t.name).collect();
        let missing: Vec<&String> = names.iter().filter(|n| !rows.contains(&n.as_str())).collect();
        assert!(missing.is_empty(), "watchers with no task row: {missing:?}");
        assert!(rows.contains(&heartbeat::NAME));
        let mut sorted = rows.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), rows.len(), "a task name appears twice");
    }

    #[test]
    fn the_registrations_and_the_boot_list_are_the_table() {
        let s = crate::settings::fixture();
        let regs: Vec<String> = crate::registrations(&s).into_iter().map(|r| r.name).collect();
        assert_eq!(regs, TASKS.iter().map(|t| t.name.to_string()).collect::<Vec<_>>());
        assert_eq!(boots().iter().map(|b| b.1).collect::<Vec<_>>(), TASKS.iter().map(|t| t.stream()).collect::<Vec<_>>());
    }

    #[test]
    fn every_row_boots_arms_its_cadence_and_asks_only_for_declared_effects() {
        let s = crate::settings::fixture();
        for t in TASKS {
            let mut rig = TaskRig::new(1, (t.build)(&s)).unwrap();
            let (_, stream, kind) = (t.boot)();
            rig.tick(stream, kind).unwrap();
            let next = rig.engine().next_timer_at();
            assert_eq!(next, Some((t.cadence_ms)(&s)), "{}: the boot tick must arm the next one a cadence later", t.name);
            for q in rig.requested() {
                assert!(t.effects.contains(&q.name.as_str()), "{} asked for {} which its row does not declare", t.name, q.name);
            }
        }
    }
}
