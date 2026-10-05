//! `unpoison` against an in-memory world (DESIGN.md §8.4). No store, no clock, no sleep.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

const T0: i64 = 1_790_596_800; // 2026-09-28T12:00:00Z

/// What the audit worker does while the watch sleeps.
type Sentinel = Box<dyn FnMut(&mut Fake)>;

#[derive(Default)]
struct Fake {
    beads: BTreeMap<String, BeadRecord>,
    events: Vec<EventRow>,
    lc: BTreeMap<String, LcRow>,
    asked: BTreeSet<String>,
    asks: Vec<AskRow>,
    closed: Vec<(String, String)>,
    notes: Vec<(String, String)>,
    removed_labels: Vec<(String, String)>,
    audit: Vec<u8>,
    now: i64,
    /// Every mutating call, in order: "event:<type>", "unhold", "note", "close", "rm-asked", "label".
    trail: Vec<String>,
    fail_events_read: bool,
    fail_lc_read: bool,
    fail_event_write: bool,
    refuse_unhold_times: u32,
    /// Called on each sleep with the fake: the "sentinel" running meanwhile.
    on_sleep: Option<Sentinel>,
    sleeps: u32,
    /// lifecycle_enforce off: any spira-lc call is a test failure.
    lc_forbidden: bool,
}

impl Fake {
    fn new() -> Self {
        Fake { now: T0, ..Default::default() }
    }
    fn bead(mut self, id: &str, status: &str, assignee: Option<&str>, labels: &[&str]) -> Self {
        self.beads.insert(
            id.into(),
            BeadRecord {
                id: id.into(),
                status: status.into(),
                assignee: assignee.map(str::to_string),
                labels: labels.iter().map(|s| s.to_string()).collect(),
            },
        );
        self
    }
    fn claims(mut self, id: &str, n: usize) -> Self {
        for i in 0..n {
            self.events.push(EventRow::new(id, "claimed", "", &format!("2026-09-28T09:{i:02}:00Z")));
        }
        self
    }
    fn lc(mut self, id: &str, state: BeadState, holds: &[HoldKind], holder: Option<&str>) -> Self {
        self.lc.insert(
            id.into(),
            LcRow { state, version: 7, holds: holds.iter().copied().collect(), holder: holder.map(str::to_string) },
        );
        self
    }
    fn ask(mut self, id: &str, title: &str) -> Self {
        self.asks.push(AskRow { id: id.into(), title: title.into() });
        self
    }
    fn log(&mut self, at: i64, msg: &str) {
        self.audit.extend_from_slice(format!("{} spira: {msg}\n", fmt_utc(at)).as_bytes());
    }
    fn writes(&self) -> usize {
        self.trail.len()
    }
}

impl World for Fake {
    fn bead(&mut self, id: &str) -> Result<Option<BeadRecord>, String> {
        Ok(self.beads.get(id).cloned())
    }
    fn events(&mut self, id: &str) -> Result<Vec<EventRow>, String> {
        if self.fail_events_read {
            return Err("bd sql: exit 1: connection refused".into());
        }
        Ok(self.events.iter().filter(|e| e.issue_id == id).cloned().collect())
    }
    fn lc_row(&mut self, id: &str) -> Result<Option<LcRow>, String> {
        assert!(!self.lc_forbidden, "spira-lc show called with lifecycle_enforce off ({id})");
        if self.fail_lc_read {
            return Err("exit 2: cannot tell".into());
        }
        Ok(self.lc.get(id).cloned())
    }
    fn lc_unhold_poison(&mut self, id: &str, row: &LcRow, _actor: &str) -> LcApply {
        assert!(!self.lc_forbidden, "spira-lc event called with lifecycle_enforce off ({id})");
        if self.refuse_unhold_times > 0 {
            self.refuse_unhold_times -= 1;
            if let Some(r) = self.lc.get_mut(id) {
                r.version += 1; // someone else wrote: the caller's version is stale
            }
            return LcApply::Refused("refused: lost the race to another writer".into());
        }
        let cur = self.lc.get_mut(id).expect("unhold on a missing row");
        assert_eq!(cur.version, row.version, "unhold must use the version it just read");
        cur.holds.remove(&HoldKind::Poison);
        cur.version += 1;
        self.trail.push("unhold".into());
        LcApply::Applied
    }
    fn write_event(&mut self, id: &str, event_type: &str, value: &str) -> Result<(), String> {
        if self.fail_event_write {
            return Err("bd sql: exit 1: read-only".into());
        }
        self.events.push(EventRow::new(id, event_type, value, &fmt_utc(self.now)));
        self.trail.push(format!("event:{event_type}"));
        Ok(())
    }
    fn clear_ask_history(&mut self, id: &str) -> Result<(), String> {
        self.asked.remove(id);
        self.trail.push("rm-asked".into());
        Ok(())
    }
    fn ask_history_exists(&mut self, id: &str) -> bool {
        self.asked.contains(id)
    }
    fn remove_label(&mut self, id: &str, label: &str) -> Result<(), String> {
        if let Some(b) = self.beads.get_mut(id) {
            b.labels.retain(|l| l != label);
        }
        self.removed_labels.push((id.into(), label.into()));
        self.trail.push("label".into());
        Ok(())
    }
    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        self.notes.push((id.into(), text.into()));
        self.trail.push("note".into());
        Ok(())
    }
    fn open_asks(&mut self) -> Result<Vec<AskRow>, String> {
        Ok(self.asks.clone())
    }
    fn close(&mut self, id: &str, reason: &str) -> Result<(), String> {
        self.closed.push((id.into(), reason.into()));
        self.asks.retain(|a| a.id != id);
        self.trail.push("close".into());
        Ok(())
    }
    fn audit_len(&mut self) -> u64 {
        self.audit.len() as u64
    }
    fn audit_read_from(&mut self, offset: u64) -> Result<Vec<u8>, String> {
        Ok(self.audit[offset as usize..].to_vec())
    }
    fn now(&mut self) -> i64 {
        self.now
    }
    fn sleep(&mut self, secs: u64) {
        self.now += secs as i64;
        self.sleeps += 1;
        if let Some(mut f) = self.on_sleep.take() {
            f(self);
            self.on_sleep = Some(f);
        }
    }
    fn mark_poison_lifted(&mut self, _id: &str, _attempts: u32) -> Result<(), String> {
        unreachable!("unpoison never marks a lift — its own floor is poison.cleared")
    }
}

fn opts(beads: &[&str]) -> Opts {
    Opts {
        beads: beads.iter().map(|s| s.to_string()).collect(),
        cause: "every charged session ended waiting for a background batch — yield-headless".into(),
        watch: false,
        watch_timeout_s: WATCH_TIMEOUT_S,
        dry_run: false,
        credit: None,
        actor: "unpoison".into(),
        poison_at: 3,
        enforce: true,
    }
}

/// lifecycle_enforce off: the same options, and the machine must never be asked.
fn legacy(beads: &[&str]) -> Opts {
    Opts { enforce: false, ..opts(beads) }
}

const ASK: &str = "Spira bead sp-a — 3 in_progress transition(s) without landing (3 attempts) — change the approach or drop it?";

fn poisoned_a() -> Fake {
    let mut f = Fake::new()
        .bead("sp-a", "open", None, &["spira", "plan", "spira-poison"])
        .claims("sp-a", 3)
        .lc("sp-a", BeadState::Ready, &[HoldKind::Poison], None)
        .ask("sp-ask1", ASK);
    f.asked.insert("sp-a".into());
    f
}

// ---------------------------------------------------------------------------------------

#[test]
fn clear_then_verify_ok_with_zero_events() {
    // sp-r66qd: right after the floor there are NO events to count. The count must be 0,
    // not "<nil>", and the clear must verify.
    let mut f = poisoned_a();
    let (code, out) = run(&opts(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert!(out.contains("OK   sp-a: cleared — attempts 3 -> 0, check4 decides \"none\""), "{out}");
    assert!(!f.lc["sp-a"].poisoned());
    assert_eq!(f.lc["sp-a"].state, BeadState::Ready, "releasing a hold moves no state");
    assert!(!f.asked.contains("sp-a"));
    assert!(!f.beads["sp-a"].labels.contains(&"spira-poison".to_string()));
    assert_eq!(f.notes.len(), 1);
    assert!(f.notes[0].1.contains("attempts were 3"), "{:?}", f.notes);
    assert!(f.notes[0].1.contains("yield-headless"));
    // The only events after the floor: none.
    let l = events::fold("sp-a", &f.events);
    assert_eq!(l.attempts, 0);
}

#[test]
fn clear_a_bead_that_never_had_events() {
    // Poisoned by hand with no events at all: still 0, still OK.
    let mut f = Fake::new().bead("sp-z", "open", None, &[]).lc("sp-z", BeadState::Ready, &[HoldKind::Poison], None);
    let (code, out) = run(&opts(&["sp-z"]), &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert!(out.contains("attempts 0 -> 0"), "{out}");
}

#[test]
fn floor_is_written_before_the_hold_is_released() {
    let mut f = poisoned_a();
    run(&opts(&["sp-a"]), &mut f);
    let floor = f.trail.iter().position(|t| t == "event:poison.cleared").unwrap();
    let unhold = f.trail.iter().position(|t| t == "unhold").unwrap();
    assert!(floor < unhold, "{:?}", f.trail);
    assert_eq!(f.trail.first().map(String::as_str), Some("event:poison.cleared"));
}

#[test]
fn refuse_when_held_by_bd_assignee() {
    let mut f = Fake::new()
        .bead("pz3", "in_progress", Some("aeon-test"), &["spira-poison"])
        .claims("pz3", 3)
        .lc("pz3", BeadState::Ready, &[HoldKind::Poison], None);
    let (code, out) = run(&opts(&["pz3"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("FAIL pz3: held by aeon-test (in_progress)"), "{out}");
    assert!(out.contains("let that aeon finish"), "{out}");
    assert!(out.contains("slay.sh --bead pz3"), "the refusal names the exit: {out}");
    assert_eq!(f.writes(), 0, "{:?}", f.trail);
    assert!(f.lc["pz3"].poisoned());
}

#[test]
fn refuse_when_lifecycle_holder() {
    let mut f = Fake::new()
        .bead("sp-w", "open", None, &[])
        .claims("sp-w", 3)
        .lc("sp-w", BeadState::Working, &[HoldKind::Poison], Some("aeon-kimahri"));
    let (code, out) = run(&opts(&["sp-w"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("held by aeon-kimahri (lifecycle WORKING)"), "{out}");
    assert_eq!(f.writes(), 0);
}

#[test]
fn ask_closed_by_title_match_only() {
    let mut f = poisoned_a()
        .ask("sp-rq", "Spira bead sp-a — completed and requeued 5 times, never landed (gate-red x5) — the harness cannot land it")
        .ask("sp-other", "Spira bead sp-ab — 3 in_progress transition(s) without landing (3 attempts) — change the approach or drop it?")
        .ask("sp-sub", "Spira bead sp-a.1 — 3 in_progress transition(s) without landing (3 attempts) — change the approach or drop it?");
    let (code, out) = run(&opts(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    let closed: Vec<&str> = f.closed.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(closed, vec!["sp-ask1"]);
    assert!(out.contains("     resolved ask sp-ask1"), "{out}");
    assert!(f.closed[0].1.contains("sp-a's poison was cleared — every charged session"), "{:?}", f.closed);
    assert!(is_poison_ask(ASK, "sp-a"));
    assert!(!is_poison_ask(ASK, "sp-"));
}

#[test]
fn skip_when_not_poisoned_and_below_threshold() {
    let mut f = Fake::new().bead("pz4", "open", None, &["spira"]).claims("pz4", 2);
    let (code, out) = run(&opts(&["pz4"]), &mut f);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("SKIP pz4: not poisoned and attempts 2 < 3 — nothing to clear"), "{out}");
    assert_eq!(f.writes(), 0);
}

#[test]
fn at_threshold_without_hold_is_cleared() {
    // No lifecycle row, but attempts at the threshold: the next pass would poison — clear it.
    let mut f = Fake::new().bead("sp-t", "open", None, &[]).claims("sp-t", 4);
    let (code, out) = run(&opts(&["sp-t"]), &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert!(out.contains("attempts 4 -> 0"), "{out}");
    assert!(!f.trail.contains(&"unhold".to_string()));
}

#[test]
fn cannot_tell_writes_nothing() {
    let mut f = poisoned_a();
    f.fail_events_read = true;
    let (code, out) = run(&opts(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("FAIL sp-a: cannot tell (events:"), "{out}");
    assert_eq!(f.writes(), 0);

    let mut f = poisoned_a();
    f.fail_lc_read = true;
    let (code, out) = run(&opts(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("FAIL sp-a: cannot tell (spira-lc show:"), "{out}");
    assert_eq!(f.writes(), 0);

    let mut f = Fake::new();
    let (code, out) = run(&opts(&["sp-nope"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("FAIL sp-nope: no such bead"), "{out}");
}

#[test]
fn floor_write_failure_stops_before_unhold() {
    let mut f = poisoned_a();
    f.fail_event_write = true;
    let (code, out) = run(&opts(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("could not write the poison.cleared floor"), "{out}");
    assert_eq!(f.writes(), 0);
    assert!(f.lc["sp-a"].poisoned(), "the hold stays when the floor could not be written");
}

#[test]
fn unhold_refused_retries_once() {
    let mut f = poisoned_a();
    f.refuse_unhold_times = 1;
    let (code, out) = run(&opts(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert!(!f.lc["sp-a"].poisoned());
}

#[test]
fn verify_fails_when_hold_remains() {
    let mut f = poisoned_a();
    f.refuse_unhold_times = 2; // lost the race twice
    let (code, out) = run(&opts(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("FAIL sp-a: did not verify: lifecycle-hold-still-present"), "{out}");
}

#[test]
fn verify_fails_when_count_does_not_drop() {
    // A claim stamped after the floor (a racing aeon) keeps the count; 3 of them re-poison.
    let mut f = poisoned_a();
    for i in 0..3 {
        f.events.push(EventRow::new("sp-a", "claimed", "", &format!("2026-09-28T13:0{i}:00Z")));
    }
    let (code, out) = run(&opts(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("attempts-still-3"), "{out}");
}

#[test]
fn dry_run_writes_nothing() {
    let mut f = poisoned_a();
    let mut o = opts(&["sp-a"]);
    o.dry_run = true;
    o.watch = true;
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_OK);
    assert!(out.starts_with("WOULD sp-a: attempts 3, poisoned=1 — write poison.cleared"), "{out}");
    assert_eq!(f.writes(), 0);
    assert_eq!(f.sleeps, 0, "no watch on a dry run");
}

#[test]
fn credit_written_before_floor_and_not_counted() {
    let mut f = poisoned_a();
    let mut o = opts(&["sp-a"]);
    o.credit = Some("yield-headless".into());
    o.actor = "groomer".into();
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert_eq!(&f.trail[..2], &["event:requeued".to_string(), "event:poison.cleared".to_string()]);
    assert!(f.events.iter().any(|e| e.event_type == "requeued" && e.new_value.as_deref() == Some("unjudged-yield-headless")));
    // Same second as the floor, so it is floored away and credits nothing later.
    let l = events::fold("sp-a", &f.events);
    assert_eq!((l.attempts, l.legacy_credits), (0, 0));
    assert!(f.notes[0].1.contains("(groomer;"), "{:?}", f.notes);

    // A refused bead gets no credit written.
    let mut f = Fake::new().bead("pz3", "in_progress", Some("aeon-test"), &[]).claims("pz3", 3);
    let (_, _) = run(&o_with_credit(&["pz3"]), &mut f);
    assert_eq!(f.writes(), 0);
}

fn o_with_credit(b: &[&str]) -> Opts {
    let mut o = opts(b);
    o.credit = Some("x".into());
    o
}

#[test]
fn several_beads_one_failure_fails_the_run() {
    let mut f = poisoned_a().bead("pz3", "in_progress", Some("aeon-test"), &[]).claims("pz3", 3);
    let (code, out) = run(&opts(&["sp-a", "pz3"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("OK   sp-a:") && out.contains("FAIL pz3:"), "{out}");
}

#[test]
fn bounded_cause_strips_quotes_and_bounds() {
    let c = bounded_cause(&format!("it's \"quoted\" \\ and\nnew {}", "x".repeat(400)));
    assert!(!c.contains('\'') && !c.contains('"') && !c.contains('\\') && !c.contains('\n'));
    assert_eq!(c.chars().count(), CAUSE_EVENT_MAX);
    assert!(c.starts_with("its quoted  andnew x"));
}

// ---------------------------------------------------------------------------------------
// watch

fn watching() -> (Fake, Opts) {
    let mut f = poisoned_a();
    // History from before the clear: a whole pass, plus bash noise.
    f.log(T0 - 600, "CHECK4 examining 205 dispatchable bead(s), poison=3 requeue=5 reclaim=5");
    f.audit.extend_from_slice(b"/srv/harness/spira/sentinel.sh: line 705: _phase: command not found\n");
    f.log(T0 - 500, "audit pass complete — 2 action(s), 2 progress");
    let mut o = opts(&["sp-a"]);
    o.watch = true;
    (f, o)
}

#[test]
fn watch_reads_audit_log_line() {
    let (mut f, o) = watching();
    f.on_sleep = Some(Box::new(|f: &mut Fake| match f.sleeps {
        2 => {
            let t = f.now;
            f.log(t, "CHECK4 examining 205 dispatchable bead(s), poison=3 requeue=5 reclaim=5");
            f.audit.extend_from_slice(b"/srv/harness/spira/sentinel.sh: line 894: _phase: command not found\n");
        }
        4 => {
            let t = f.now;
            f.log(t, "CHECK5: repo:ephemeral-ci is not in repo-map — skipping its closed beads");
            // half a line first: must not be parsed until complete
            f.audit.extend_from_slice(format!("{} spira: audit pass comp", fmt_utc(t)).as_bytes());
        }
        5 => f.audit.extend_from_slice(b"lete \xe2\x80\x94 1 action(s), 1 progress\n"),
        _ => {}
    }));
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert!(out.contains("watch: waiting for an audit pass that starts after 2026-09-28T12:00:00Z (up to 2400s)..."), "{out}");
    assert!(out.contains("OK   watch sp-a: still clear after a full audit pass"), "{out}");
    assert_eq!(f.sleeps, 5, "completed on the poll after the line was whole");
}

#[test]
fn watch_ignores_pass_started_before_clear() {
    let (mut f, o) = watching();
    // A pass already running when the clear happened (its CHECK 4 read predates the clear).
    f.log(T0 - 30, "CHECK4 examining 205 dispatchable bead(s), poison=3 requeue=5 reclaim=5");
    f.on_sleep = Some(Box::new(|f: &mut Fake| {
        let t = f.now;
        match f.sleeps {
            1 => f.log(t, "audit pass complete — 0 action(s), 0 progress"), // the old pass ends
            3 => f.log(t, "CHECK4 examining 205 dispatchable bead(s), poison=3 requeue=5 reclaim=5"),
            6 => f.log(t, "audit pass complete — 0 action(s), 0 progress"),
            _ => {}
        }
    }));
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert_eq!(f.sleeps, 6, "the pass that started before the clear proved nothing: {out}");
}

#[test]
fn watch_disqualifies_failed_counts_pass() {
    let (mut f, o) = watching();
    f.on_sleep = Some(Box::new(|f: &mut Fake| {
        let t = f.now;
        match f.sleeps {
            1 => f.log(t, "CHECK4 examining 205 dispatchable bead(s), poison=3 requeue=5 reclaim=5"),
            2 => f.log(t, "CHECK4 bulk attempts query failed (rc=2) — making no poison/requeue/reclaim decision this pass"),
            3 => f.log(t, "audit pass complete — 0 action(s), 0 progress"),
            4 => f.log(t, "CHECK4 examining 205 dispatchable bead(s), poison=4 requeue=5 reclaim=5"),
            5 => f.log(t, "audit pass complete — 0 action(s), 0 progress"),
            _ => {}
        }
    }));
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert_eq!(f.sleeps, 5, "{out}");
    assert!(out.contains("watch: note — the audit pass ran with poison=4, this clear verified against 3"), "{out}");
}

#[test]
fn watch_fails_when_repoisoned() {
    let (mut f, o) = watching();
    f.on_sleep = Some(Box::new(|f: &mut Fake| {
        let t = f.now;
        if f.sleeps == 1 {
            f.log(t, "CHECK4 examining 205 dispatchable bead(s), poison=3 requeue=5 reclaim=5");
            f.lc.get_mut("sp-a").unwrap().holds.insert(HoldKind::Poison);
        }
    }));
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("FAIL watch sp-a: re-poisoned by the pass"), "{out}");
    assert_eq!(f.sleeps, 1, "fails at once, without waiting for the pass to end");
}

#[test]
fn watch_times_out() {
    let (mut f, mut o) = watching();
    o.watch_timeout_s = 60;
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("FAIL watch: no audit pass completed in 60s"), "{out}");
    assert_eq!(f.sleeps, 6);
}

#[test]
fn watch_survives_rotation() {
    let (mut f, o) = watching();
    f.on_sleep = Some(Box::new(|f: &mut Fake| {
        if f.sleeps == 1 {
            f.audit.clear(); // logrotate truncated it
            let t = f.now;
            f.log(t, "CHECK4 examining 1 dispatchable bead(s), poison=3 requeue=5 reclaim=5");
            f.log(t + 1, "audit pass complete — 0 action(s), 0 progress");
        }
    }));
    // the rotated file is shorter than the recorded offset
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
}

#[test]
fn watch_not_started_when_a_bead_failed() {
    let (mut f, mut o) = watching();
    f = f.bead("pz3", "in_progress", Some("aeon-test"), &[]).claims("pz3", 3);
    o.beads.push("pz3".into());
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(!out.contains("watch:"), "{out}");
}

#[test]
fn audit_line_parsing() {
    assert_eq!(
        parse_audit_line("2026-09-29T03:56:00Z spira: CHECK4 examining 205 dispatchable bead(s), poison=3 requeue=5 reclaim=5"),
        AuditLine::Examining { at: events::epoch_s("2026-09-29T03:56:00").unwrap(), poison_at: Some(3) }
    );
    assert!(matches!(parse_audit_line("2026-09-29T03:52:00Z spira: audit pass complete — 2 action(s), 2 progress"), AuditLine::Complete { .. }));
    assert!(matches!(
        parse_audit_line("2026-09-29T03:52:00Z spira: CHECK4 bulk attempts query failed (rc=2) — making no poison/requeue/reclaim decision this pass"),
        AuditLine::CountsFailed { .. }
    ));
    assert_eq!(parse_audit_line("/srv/harness/spira/sentinel.sh: line 705: _phase: command not found"), AuditLine::Other);
    assert_eq!(parse_audit_line("2026-09-29T03:52:00Z spira: CHECK5: resolved 1 incident(s)"), AuditLine::Other);
    assert_eq!(parse_audit_line(""), AuditLine::Other);
    assert_eq!(fmt_utc(T0), "2026-09-28T12:00:00Z");
    assert_eq!(fmt_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(events::epoch_s("2026-09-28T12:00:00"), Some(T0));
}

// ---------------------------------------------------------------------------------------
// parsing the stores

#[test]
fn parse_store_outputs() {
    let b = parse_bead("warning: something\n[{\"id\":\"sp-a\",\"status\":\"in_progress\",\"assignee\":\"aeon-x\",\"labels\":[\"spira\",\"spira-poison\"]}]").unwrap().unwrap();
    assert_eq!((b.status.as_str(), b.assignee.as_deref(), b.labels.len()), ("in_progress", Some("aeon-x"), 2));
    assert_eq!(parse_bead("{\"error\": \"no issues found matching the provided IDs\", \"schema_version\": 1}").unwrap(), None);
    assert_eq!(parse_bead("[]").unwrap(), None);
    assert!(parse_bead("garbage").is_err());

    // dolt string-encodes every column, holds is JSON text
    let r = parse_lc_show(r#"{"bead":{"bead_id":"sp-a","state":"WORKING","holder":"aeon-x","holds":"[\"poison\"]","version":"12"},"delivery":null}"#).unwrap();
    assert_eq!((r.state, r.version, r.poisoned(), r.holder.as_deref()), (BeadState::Working, 12, true, Some("aeon-x")));
    let r = parse_lc_show(r#"{"bead":{"state":"READY","holds":[],"version":3,"holder":null}}"#).unwrap();
    assert!(!r.poisoned() && r.holder.is_none());
    let r = parse_lc_show(r#"{"bead":{"state":"READY","holds":"","version":"0","holder":""}}"#).unwrap();
    assert!(!r.poisoned() && r.holder.is_none());
    assert!(parse_lc_show(r#"{"bead":{"state":"READY","holds":"[]"}}"#).is_err(), "no version: cannot tell");

    let a = parse_asks("[{\"id\":\"sp-1\",\"title\":\"t\"},{\"id\":\"sp-2\"}]").unwrap();
    assert_eq!(a.len(), 2);
    assert!(parse_asks("").is_err());
}

#[test]
fn ids_and_slugs() {
    assert!(valid_id("sp-a.1_x"));
    assert!(!valid_id("sp a") && !valid_id("") && !valid_id("o'hara"));
    assert!(valid_slug("pre-session-death") && !valid_slug("Yield") && !valid_slug(""));
}

// ---------------------------------------------------------------------------------------
// the live world's seams: argv and stdin of the bd / spira-lc calls

fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    testkit::write_exe(&p, &format!("#!/bin/sh\n{body}\n"));
    p.to_string_lossy().into_owned()
}

fn live(tag: &str, bd_body: &str, lc_body: &str) -> (Live, testkit::TempDir) {
    let dir = testkit::TempDir::new(&format!("spira-claim-unpoison-{tag}"));
    std::fs::create_dir_all(dir.join("run/poison-asked")).unwrap();
    let bd = script(&dir, "bd", &bd_body.replace("@LOG@", &dir.join("bd.log").to_string_lossy()));
    let lc = script(&dir, "spira-lc", &lc_body.replace("@LOG@", &dir.join("lc.log").to_string_lossy()));
    let l = Live {
        store: Store { bd, db: Some("/fake/db".into()), lc, timeout: Duration::from_secs(10) },
        run_dir: dir.join("run"),
        asked_dir: dir.join("run/poison-asked"),
        ask_label: "needs-ryan".into(), // literal-ok: fixture/fallback
        beads_actor: "harness".into(),
    };
    (l, dir)
}

const RECORD: &str = r#"{ printf 'ARGV'; for a in "$@"; do printf ' [%s]' "$a"; done; printf '\nSTDIN '; /bin/cat; printf '\n'; } >> @LOG@"#;

#[test]
fn live_bd_writes_pass_text_on_stdin() {
    let (mut w, dir) = live("stdin", RECORD, "exit 0");
    let cause = "it's \"the harness\" — not the work\nsecond line";
    w.note("sp-a", &format!("Poison cleared: {cause}")).unwrap();
    w.close("sp-ask1", &format!("Resolved: {cause}")).unwrap();
    let log = std::fs::read_to_string(dir.join("bd.log")).unwrap();
    assert!(log.contains("ARGV [-C] [/fake/db] [note] [sp-a] [--stdin]\nSTDIN Poison cleared: it's \"the harness\""), "{log}");
    assert!(log.contains("ARGV [-C] [/fake/db] [close] [sp-ask1] [--reason-file] [-]\nSTDIN Resolved: it's"), "{log}");
    for line in log.lines().filter(|l| l.starts_with("ARGV")) {
        assert!(!line.contains("harness"), "no free text in argv: {line}");
    }
}

#[test]
fn live_event_insert_is_bounded() {
    let (mut w, dir) = live("event", RECORD, "exit 0");
    w.write_event("sp-a", "poison.cleared", &format!("it's {}", "y".repeat(500))).unwrap();
    let log = std::fs::read_to_string(dir.join("bd.log")).unwrap();
    let argv = log.lines().next().unwrap();
    assert!(argv.starts_with("ARGV [-C] [/fake/db] [sql] [INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('"), "{argv}");
    assert!(argv.contains("'sp-a', 'poison.cleared', 'harness', 'its yyy"), "{argv}");
    assert!(argv.len() < 600, "the one argv payload is bounded: {}", argv.len());
    let (mut bad, _bad_dir) = live("event-fail", "echo 'Error: read-only' >&2; exit 1", "exit 0");
    assert!(bad.write_event("sp-a", "poison.cleared", "x").unwrap_err().contains("read-only"));
}

#[test]
fn live_bead_and_asks() {
    let (mut w, _dir) = live(
        "show",
        r#"case "$3" in
  show) if [ "$4" = sp-a ]; then echo '[{"id":"sp-a","status":"open","labels":["spira-poison"]}]';
        elif [ "$4" = sp-down ]; then echo 'dolt: connection refused' >&2; exit 1;
        else echo '{"error":"no issues found matching the provided IDs"}'; echo "Error fetching $4: no issue found matching \"$4\"" >&2; exit 1; fi ;;
  # literal-ok: fixture
  list) [ "$7" = needs-ryan ] || exit 9; echo '[{"id":"sp-ask1","title":"t"}]' ;;
esac"#,
        "exit 0",
    );
    assert_eq!(w.bead("sp-a").unwrap().unwrap().labels, vec!["spira-poison"]);
    assert_eq!(w.bead("sp-gone").unwrap(), None);
    assert!(w.bead("sp-down").unwrap_err().contains("connection refused"));
    assert_eq!(w.open_asks().unwrap(), vec![AskRow { id: "sp-ask1".into(), title: "t".into() }]);
}

#[test]
fn live_lifecycle_show_and_unhold() {
    let (mut w, dir) = live(
        "lc",
        "exit 0",
        r#"echo "$*" >> @LOG@
case "$1:$2" in
  show:sp-a) echo '{"bead":{"bead_id":"sp-a","state":"READY","holds":"[\"poison\"]","version":"7","holder":null}}' ;;
  show:sp-none) echo '{}'; exit 1 ;;
  show:*) echo 'cannot tell: db down' >&2; exit 2 ;;
  event:bead) case "$3" in sp-a) exit 0 ;; sp-race) echo 'refused: lost the race to another writer'; exit 3 ;; *) exit 2 ;; esac ;;
esac"#,
    );
    let row = w.lc_row("sp-a").unwrap().unwrap();
    assert!(row.poisoned() && row.version == 7 && row.state == BeadState::Ready);
    assert_eq!(w.lc_row("sp-none").unwrap(), None);
    let (mut f, _dir) = live("lc-false", "exit 0", "exit 1");
    assert!(f.lc_row("sp-a").is_err(), "a bare exit 1 is not spira-lc's no-row answer");
    assert!(w.lc_row("sp-x").is_err());
    assert_eq!(w.lc_unhold_poison("sp-a", &row, "unpoison"), LcApply::Applied);
    assert!(matches!(w.lc_unhold_poison("sp-race", &row, "unpoison"), LcApply::Refused(_)));
    assert!(matches!(w.lc_unhold_poison("sp-x", &row, "unpoison"), LcApply::CannotTell(_)));
    let log = std::fs::read_to_string(dir.join("lc.log")).unwrap();
    assert!(log.contains(r#"event bead sp-a --expect READY --version 7 --actor unpoison --kind {"Unhold":{"kind":"Poison"}}"#), "{log}");
}

#[test]
fn live_ask_history_and_audit_log() {
    let (mut w, dir) = live("files", "exit 0", "exit 0");
    std::fs::write(dir.join("run/poison-asked/sp-a"), "3\n").unwrap();
    assert!(w.ask_history_exists("sp-a"));
    w.clear_ask_history("sp-a").unwrap();
    assert!(!w.ask_history_exists("sp-a"));
    w.clear_ask_history("sp-a").unwrap(); // absent is fine
    assert_eq!(w.audit_len(), 0, "absent log is length 0");
    std::fs::write(dir.join("run/audit.log"), "abc\ndef\n").unwrap();
    assert_eq!(w.audit_len(), 8);
    assert_eq!(w.audit_read_from(4).unwrap(), b"def\n");
    assert!(dir.join("run/sentinel.log").metadata().is_err(), "watch never needs sentinel.log");
}

// ---------------------------------------------------------------------------------------
// lifecycle_enforce OFF (production today): the label is the poison, spira-lc is never called

fn legacy_poisoned_a() -> Fake {
    let mut f = Fake::new()
        .bead("sp-a", "open", None, &["spira", "plan", "spira-poison"])
        .claims("sp-a", 3)
        .ask("sp-ask1", ASK);
    f.asked.insert("sp-a".into());
    f.lc_forbidden = true;
    // Even an unreachable machine must not matter: it is never asked.
    f.fail_lc_read = true;
    f
}

#[test]
fn off_clear_removes_the_label_and_never_calls_spira_lc() {
    let mut f = legacy_poisoned_a();
    let (code, out) = run(&legacy(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert!(out.contains("OK   sp-a: cleared — attempts 3 -> 0, check4 decides \"none\""), "{out}");
    assert!(!f.beads["sp-a"].labels.contains(&"spira-poison".to_string()));
    assert_eq!(f.trail.first().map(String::as_str), Some("event:poison.cleared"), "floor first: {:?}", f.trail);
    let floor = f.trail.iter().position(|t| t == "event:poison.cleared").unwrap();
    let label = f.trail.iter().position(|t| t == "label").unwrap();
    assert!(floor < label, "{:?}", f.trail);
    assert!(!f.trail.contains(&"unhold".to_string()));
    assert_eq!(f.closed.len(), 1);
    assert!(!f.asked.contains("sp-a"));
}

#[test]
fn off_live_holder_is_bd_in_progress_with_assignee() {
    let mut f = Fake::new().bead("pz3", "in_progress", Some("aeon-test"), &["spira-poison"]).claims("pz3", 3);
    f.lc_forbidden = true;
    let (code, out) = run(&legacy(&["pz3"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("FAIL pz3: held by aeon-test (in_progress)"), "{out}");
    assert_eq!(f.writes(), 0);
    // A lifecycle WORKING row is not consulted when off: an open, unassigned bead is clearable.
    let mut f = legacy_poisoned_a().lc("sp-a", BeadState::Working, &[HoldKind::Poison], Some("aeon-x"));
    let (code, out) = run(&legacy(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
}

#[test]
fn off_skip_and_dry_run() {
    let mut f = Fake::new().bead("pz4", "open", None, &["spira"]).claims("pz4", 2);
    f.lc_forbidden = true;
    let (code, out) = run(&legacy(&["pz4"]), &mut f);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("SKIP pz4"), "{out}");
    let mut f = legacy_poisoned_a();
    let mut o = legacy(&["sp-a"]);
    o.dry_run = true;
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("WOULD sp-a: attempts 3, poisoned=1 — write poison.cleared, reset ask history, remove the spira-poison label (lifecycle_enforce off), note, resolve ask"), "{out}");
    assert_eq!(f.writes(), 0);
    // Off, at the threshold with no label: the floor still matters (the next pass would poison).
    let mut f = Fake::new().bead("sp-n", "open", None, &["incident"]).claims("sp-n", 26);
    f.lc_forbidden = true;
    let (code, out) = run(&o_dry(&["sp-n"]), &mut f);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("WOULD sp-n: attempts 26, poisoned=0 — write poison.cleared, reset ask history, no spira-poison label to remove (lifecycle_enforce off)"), "{out}");
}

fn o_dry(b: &[&str]) -> Opts {
    Opts { dry_run: true, ..legacy(b) }
}

#[test]
fn off_verify_fails_when_label_stays() {
    // Off, the label IS the poison: a failed removal is reported, and verify re-reads it.
    let mut f2 = legacy_poisoned_a();
    struct NoRemove<'a>(&'a mut Fake);
    impl World for NoRemove<'_> {
        fn bead(&mut self, id: &str) -> Result<Option<BeadRecord>, String> { self.0.bead(id) }
        fn events(&mut self, id: &str) -> Result<Vec<EventRow>, String> { self.0.events(id) }
        fn lc_row(&mut self, id: &str) -> Result<Option<LcRow>, String> { self.0.lc_row(id) }
        fn lc_unhold_poison(&mut self, id: &str, r: &LcRow, a: &str) -> LcApply { self.0.lc_unhold_poison(id, r, a) }
        fn write_event(&mut self, id: &str, t: &str, v: &str) -> Result<(), String> { self.0.write_event(id, t, v) }
        fn clear_ask_history(&mut self, id: &str) -> Result<(), String> { self.0.clear_ask_history(id) }
        fn ask_history_exists(&mut self, id: &str) -> bool { self.0.ask_history_exists(id) }
        fn remove_label(&mut self, _: &str, _: &str) -> Result<(), String> { Err("bd label: exit 1: timeout".into()) }
        fn note(&mut self, id: &str, t: &str) -> Result<(), String> { self.0.note(id, t) }
        fn open_asks(&mut self) -> Result<Vec<AskRow>, String> { self.0.open_asks() }
        fn close(&mut self, id: &str, r: &str) -> Result<(), String> { self.0.close(id, r) }
        fn audit_len(&mut self) -> u64 { self.0.audit_len() }
        fn audit_read_from(&mut self, o: u64) -> Result<Vec<u8>, String> { self.0.audit_read_from(o) }
        fn now(&mut self) -> i64 { self.0.now() }
        fn sleep(&mut self, s: u64) { self.0.sleep(s) }
        fn mark_poison_lifted(&mut self, id: &str, a: u32) -> Result<(), String> { self.0.mark_poison_lifted(id, a) }
    }
    let (code, out) = run(&legacy(&["sp-a"]), &mut NoRemove(&mut f2));
    assert_eq!(code, EXIT_FAILED, "{out}");
    assert!(out.contains("     warn sp-a: label remove: bd label: exit 1: timeout"), "{out}");
    assert!(out.contains("FAIL sp-a: did not verify: label-still-present"), "{out}");
    assert!(out.contains("check4=clear"), "the legacy decision sees the label as the poison: {out}");
}

#[test]
fn off_watch_reads_the_label_and_the_audit_log() {
    let mut f = legacy_poisoned_a();
    let mut o = legacy(&["sp-a"]);
    o.watch = true;
    f.on_sleep = Some(Box::new(|f: &mut Fake| {
        let t = f.now;
        match f.sleeps {
            1 => f.log(t, "CHECK4 examining 205 dispatchable bead(s), poison=3 requeue=5 reclaim=5"),
            2 => f.log(t, "audit pass complete — 0 action(s), 0 progress"),
            _ => {}
        }
    }));
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_OK, "{out}");
    assert!(out.contains("OK   watch sp-a: still clear after a full audit pass"), "{out}");

    let mut f = legacy_poisoned_a();
    f.on_sleep = Some(Box::new(|f: &mut Fake| {
        if f.sleeps == 1 {
            f.beads.get_mut("sp-a").unwrap().labels.push("spira-poison".into());
        }
    }));
    let (code, out) = run(&o, &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("FAIL watch sp-a: re-poisoned by the pass"), "{out}");
}

#[test]
fn on_unreachable_machine_is_a_loud_fail() {
    let mut f = poisoned_a();
    f.fail_lc_read = true;
    let (code, out) = run(&opts(&["sp-a"]), &mut f);
    assert_eq!(code, EXIT_FAILED);
    assert!(out.contains("lifecycle_enforce is on, so the machine must answer"), "{out}");
    assert_eq!(f.writes(), 0);
}

#[test]
fn reopen_sets_status_open_so_an_in_progress_bead_is_ready_again() {
    let a = crate::unpoison::reopen_status_args("sp-a");
    assert_eq!(a, ["update", "sp-a", "--status", "open", "--assignee", ""]);
}
