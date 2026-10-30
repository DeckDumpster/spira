//! An idle runtime asks the store in proportion to the events it handles, not to how often it wakes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::event::*;
use runtime::journal::{Journal, Offset, Poison, ScheduleRow};
use runtime::ports::{Clock, VirtualClock};
use runtime::{Executor, MemJournal, Outcome, Result};
use spira_runtime::watchers::{asks::Asks, bead_sweep::BeadSweep, design_asks::DesignAsks, supersede_duty::SupersedeDuty, inbox_keeper::InboxKeeper, pr_notify::PrNotify, publish_backlog::PublishBacklog, round_duty::RoundDuty};
use runtime::marker::NullMarker;
use runtime::wide::NullSink;
use spira_runtime::{open_engine, Settings};

struct Counting {
    inner: Arc<MemJournal>,
    calls: Mutex<BTreeMap<&'static str, u64>>,
}

impl Counting {
    fn hit(&self, verb: &'static str) {
        *self.calls.lock().unwrap().entry(verb).or_default() += 1;
    }
    fn total(&self) -> u64 {
        self.calls.lock().unwrap().values().sum()
    }
}

impl Journal for Counting {
    fn append(&self, ev: &Event) -> Result<Offset> {
        self.hit("append");
        self.inner.append(ev)
    }
    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        self.hit("read_from");
        self.inner.read_from(from, limit)
    }
    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        self.hit("applied");
        self.inner.applied(handler, id)
    }
    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.hit("mark_applied");
        self.inner.mark_applied(handler, id)
    }
    fn cursor(&self) -> Result<Offset> {
        self.hit("cursor");
        self.inner.cursor()
    }
    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.hit("set_cursor");
        self.inner.set_cursor(next)
    }
    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.hit("schedule");
        self.inner.schedule(row)
    }
    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        self.hit("pending_schedules");
        self.inner.pending_schedules()
    }
    fn mark_fired(&self, row_id: &str) -> Result<()> {
        self.hit("mark_fired");
        self.inner.mark_fired(row_id)
    }
    fn poison(&self, p: &Poison) -> Result<()> {
        self.hit("poison");
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.hit("poisoned");
        self.inner.poisoned(stream)
    }
}

const PER_EVENT: u64 = 6;
const IDLE_CALLS_PER_MINUTE: u64 = 12;

/// The registration set the service builds from its shipped intervals, not a hand-copied one.
fn production() -> Settings {
    let get = |k: &str| -> std::result::Result<String, String> {
        Ok(match k {
            "SPIRA_RUNTIME_HEARTBEAT_URL" => "http://127.0.0.1:9/ping".into(),
            "SPIRA_METRICS_ENDPOINT" => String::new(),
            "SPIRA_RUNTIME_HEARTBEAT_SECS" | "SPIRA_RUNTIME_INBOX_KEEPER_SECS" => "60".into(),
            "SPIRA_RUNTIME_PR_NOTIFY_SECS" => "90".into(),
            "SPIRA_RUNTIME_ASKS_SECS" => "60".into(),
            "SPIRA_RUNTIME_SUPERSEDE_DUTY_SECS" => "60".into(),
            "SPIRA_RUNTIME_DESIGN_ASKS_SECS" => "600".into(),
            "SPIRA_DESIGN_ASKS_REPEAT" => "86400".into(),
            "SPIRA_RUNTIME_BRAIN_DIRTY_SECS" => "600".into(),
            "SPIRA_BRAIN_DIRTY_AGE" => "3600".into(),
            "SPIRA_PR_DEPLOY_GIVEUP" => "3600".into(),
            "SPIRA_GH_API" => "http://127.0.0.1:9".into(),
            "SPIRA_BROKER_GH_TOKEN" => String::new(),
            "SPIRA_MAIL" | "SPIRA_ASK_LABEL" | "SPIRA_WIKI" => "/unused".into(),
            "SPIRA_RUNTIME_ROUND_DUTY_SECS" => "15".into(),
            "SPIRA_RUNTIME_STALL_LIMIT" => "2".into(),
            "SPIRA_ROUND_MIN" | "SPIRA_ROUND_MAX_OPEN" => "1".into(),
            "SPIRA_ROUND_DUE_REMIND" | "SPIRA_CONCIERGE_INBOX_STALL" => "600".into(),
            "SPIRA_ROUND_IDLE_CUT" => "900".into(),
            "SPIRA_CONCIERGE_INBOX_BACKOFF" => "3600".into(),
            "SPIRA_RUN" | "SPIRA_QUEUE_DIR" | "SPIRA_REPO_MAP" | "SPIRA_CONCIERGE_INBOX" | "SPIRA_LC_SOCKET" => "/unused".into(),
            "SPIRA_HOME_REPO" => "spira".into(),
            "SPIRA_RUNTIME_BEAD_SWEEP_SECS" => "1800".into(),
            "SPIRA_BEAD_SWEEP_HOLD_MIN" | "SPIRA_BEAD_SWEEP_WAIT_MIN" => "120".into(),
            "SPIRA_BEAD_SWEEP_REWORK_MIN" => "20".into(),
            "SPIRA_BEAD_SWEEP_CERT_MIN" => "10".into(),
            "SPIRA_BEAD_SWEEP_BATCH_MIN" => "180".into(),
            "SPIRA_SCOPED_STOP_REVIEW_SECS" => "86400".into(),
            "SPIRA_RUNTIME_PUBLISH_BACKLOG_SECS" => "900".into(),
            "SPIRA_LOCAL_BACKLOG_COUNT" => "50".into(),
            "SPIRA_LOCAL_BACKLOG_AGE" => "10800".into(),
            _ => return Err(k.to_string()),
        })
    };
    Settings::from_lookup(&get).unwrap()
}

fn quiet_world(effect: &str, _poll: u64) -> String {
    match effect {
        spira_runtime::watchers::round_duty::OBSERVE => r#"{"certified":0,"by_repo":"","stopped":"","state":"none","open":0,"lines":{},"fresh":[]}"#.into(),
        spira_runtime::watchers::inbox_keeper::OBSERVE => r#"{"triage_alive":true,"inbox_mtime":0}"#.into(),
        spira_runtime::watchers::pr_notify::OBSERVE => r#"{"repos":[]}"#.into(),
        spira_runtime::watchers::asks::OBSERVE => r#"{"open":[],"held":[]}"#.into(),
        spira_runtime::watchers::supersede_duty::OBSERVE => r#"{"held":[]}"#.into(),
        spira_runtime::watchers::bead_sweep::OBSERVE => r#"{"rows":[],"content":[],"terminal":[],"stops":[],"reserved":[],"carried":[],"solo_skipped":[],"pr_repos":[]}"#.into(),
        spira_runtime::watchers::publish_backlog::OBSERVE => r#"{"repos":[]}"#.into(),
        _ => String::new(),
    }
}

/// A backlog that changes on every observation: every poll has something to journal.
fn busy_world(effect: &str, poll: u64) -> String {
    match effect {
        spira_runtime::watchers::round_duty::OBSERVE => format!(r#"{{"certified":{},"by_repo":"","stopped":"","state":"none","open":0,"lines":{{}},"fresh":[]}}"#, poll + 1),
        other => quiet_world(other, poll),
    }
}

struct Run {
    total: u64,
    events: u64,
    wakes: u64,
    calls: BTreeMap<&'static str, u64>,
}

fn ten_minutes(world: fn(&str, u64) -> String) -> Run {
    let mem = MemJournal::new();
    let counting = Arc::new(Counting { inner: mem.clone(), calls: Mutex::new(BTreeMap::new()) });
    let clock = VirtualClock::new(1_800_000_000_000);
    let start = clock.now_ms();
    let mut e = open_engine(&production(), counting.clone(), Arc::new(clock.clone()), Arc::new(NullMarker), Arc::new(NullSink), vec![]).unwrap();
    for (_, stream, kind) in [runtime::heartbeat::Heartbeat::boot(), RoundDuty::boot(), InboxKeeper::boot(), PrNotify::boot(), Asks::boot(), DesignAsks::boot(), SupersedeDuty::boot(), BeadSweep::boot(), PublishBacklog::boot()] {
        e.start(stream, kind);
    }
    let (mut wakes, mut polls) = (0, 0);
    while clock.now_ms() < start + 600_000 {
        e.run_until_idle().unwrap();
        for p in e.pending_effects() {
            polls += 1;
            e.complete(&p.key, Outcome::Succeeded(world(&p.spec.name, polls))).unwrap();
        }
        e.run_until_idle().unwrap();
        let Some(next) = e.next_timer_at() else { break };
        clock.advance_to(next.max(clock.now_ms() + 1));
        wakes += 1;
    }
    let events = mem.read_from(0, 100_000).unwrap().len() as u64;
    let calls = counting.calls.lock().unwrap().clone();
    Run { total: counting.total(), events, wakes, calls }
}

#[test]
fn ten_idle_minutes_at_production_intervals_cost_a_dozen_store_calls_a_minute_at_most() {
    let r = ten_minutes(quiet_world);
    assert!(r.wakes >= 40, "the simulation did not run: {} wakes", r.wakes);
    assert!(r.total <= IDLE_CALLS_PER_MINUTE * 10, "{} store calls in ten idle minutes ({}/min), {} journal events: {:?}", r.total, r.total / 10, r.events, r.calls);
}

#[test]
fn a_world_that_changes_on_every_poll_costs_a_few_store_calls_per_journal_event() {
    let r = ten_minutes(busy_world);
    assert!(r.events > 40 && r.wakes >= 40, "the simulation did not run: {} events, {} wakes", r.events, r.wakes);
    assert!(r.total <= PER_EVENT * r.events, "{} store calls for {} events over {} wakes ({}/event): {:?}", r.total, r.events, r.wakes, r.total / r.events, r.calls);
}

#[test]
fn every_canned_observe_reply_parses_into_its_watchers_snapshot() {
    use spira_runtime::watchers::{asks, inbox_keeper, pr_notify, round_duty};
    let quiet = |effect| quiet_world(effect, 0);
    serde_json::from_str::<round_duty::Snapshot>(&quiet(round_duty::OBSERVE)).expect("round-duty reply");
    serde_json::from_str::<spira_runtime::watchers::supersede_duty::Snapshot>(&quiet(spira_runtime::watchers::supersede_duty::OBSERVE)).expect("supersede-duty reply");
    serde_json::from_str::<inbox_keeper::Snapshot>(&quiet(inbox_keeper::OBSERVE)).expect("inbox-keeper reply");
    serde_json::from_str::<pr_notify::Snapshot>(&quiet(pr_notify::OBSERVE)).expect("pr-notify reply");
    serde_json::from_str::<asks::Snapshot>(&quiet(asks::OBSERVE)).expect("asks reply");
    serde_json::from_str::<round_duty::Snapshot>(&busy_world(round_duty::OBSERVE, 3)).expect("busy round-duty reply");
}
