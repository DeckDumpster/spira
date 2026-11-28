//! One Heimdall pass after the gathering: probe the store, classify, reconcile each key
//! against it, then report through the fallback file and the heartbeat. `main` reads the
//! machine; this takes what it read and `now`, so a pass is a function call.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::alerts::{self, ProbeOutcome};
use crate::classify::{self, Firing, Observation};
use crate::conditions::{Conditions, Failure};
use crate::fallback;
use crate::heartbeat::{self, DbStatus, Heartbeat};
use crate::reconcile::{self, Trigger};
use crate::state::State;
use crate::util;

pub struct PassInput {
    /// Everything gathered; `now`, `heimdall_first` and the `db_*` fields are set by `run`.
    pub obs: Observation,
    pub confirm: i64,
    pub clear_n: i64,
    pub refresh: i64,
    pub tz: String,
    /// Renders an epoch for an alert body; `util::iso_display` shells out to `date`.
    pub render_time: fn(i64, &str) -> String,
    /// Probe and classify only: no alert is written and no file touched.
    pub report_only: bool,
}

pub struct PassPaths {
    pub fallback: PathBuf,
    pub heartbeat: PathBuf,
    /// The Concierge inbox.
    pub inbox: PathBuf,
    /// How far the inbox has been read.
    pub inbox_cursor: PathBuf,
    /// The broker for `alarm/<key>` and for a page's `concierge/inbox`.
    pub inbox_bus: inbox_rules::Bus,
}

pub struct PassReport {
    pub firing: Vec<Firing>,
    pub probe: ProbeOutcome,
    pub db_reachable: bool,
    pub db_saturated: bool,
    pub acted: i64,
    pub fallback_written: bool,
    pub out: Vec<String>,
    pub err: Vec<String>,
}

pub fn run(inp: &PassInput, ops: &dyn Conditions, state: &mut State, paths: &PassPaths, now: i64) -> PassReport {
    let mut rep = PassReport { firing: Vec::new(), probe: ProbeOutcome { id: None, write_ok: None, saturated: false }, db_reachable: false, db_saturated: false, acted: 0, fallback_written: false, out: Vec::new(), err: Vec::new() };
    if state.first_run == 0 {
        state.first_run = now;
    }

    // The list is also the read probe: a second "is the store readable" question could disagree.
    let (db_reachable, db_error, rows) = match ops.list() {
        Ok(rows) => (true, String::new(), rows),
        Err(f) => {
            rep.db_saturated = f == Failure::Saturated;
            (false, "spira-lc condition list did not answer".to_string(), Vec::new())
        }
    };
    rep.db_reachable = db_reachable;

    if db_reachable {
        rep.probe = alerts::maintain_probe(ops);
    }
    state.probe_id = rep.probe.id.clone().unwrap_or_default();
    rep.db_saturated |= rep.probe.saturated;

    let mut obs = inp.obs.clone();
    obs.now = now;
    obs.heimdall_first = state.first_run;
    obs.db_reachable = db_reachable;
    obs.db_error = db_error;
    rep.firing = classify::run(&obs);
    if obs.gate_runs_unread {
        let raised: Vec<String> = rows.iter().filter(|c| c.firing()).map(|c| c.key.clone()).collect();
        crate::streak::hold(&raised, &mut rep.firing);
    }
    if inp.report_only {
        return rep;
    }

    let firing_keys: BTreeSet<String> = rep.firing.iter().map(|f| f.key.clone()).collect();
    let titles: BTreeMap<&str, &str> = rep.firing.iter().map(|f| (f.key.as_str(), f.title.as_str())).collect();
    let evidence_of = |k: &str| rep.firing.iter().find(|f| f.key == k).map(|f| f.evidence.clone()).unwrap_or_default();
    let body_for = |key: &str, first: i64, flaps: i64| alerts::body_with(key, &(inp.render_time)(first, &inp.tz), flaps, &evidence_of(key));

    let steps = reconcile::step_keys(now, inp.confirm.max(1), inp.clear_n.max(1), inp.refresh.max(1), db_reachable, &firing_keys, &state.keys);
    let mut beads_ok = true;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut acted = 0;
    state.keys.clear();
    for step in &steps {
        let key = &step.key;
        let existing = alerts::find_by_key(&rows, key);
        let title = titles.get(key.as_str()).copied().unwrap_or(key.as_str());
        match step.trigger {
            Trigger::None => {
                state.keys.insert(key.clone(), step.counters.clone());
            }
            Trigger::ConfirmRaise => match alerts::alert_write(ops, key, title, &body_for(key, step.counters.first, step.counters.flaps)) {
                Ok(id) => {
                    out.push(util::log_line(now, &format!("HEIMDALL raised {key} as {id}")));
                    let mut k = step.on_success(now);
                    k.bead = id;
                    state.keys.insert(key.clone(), k);
                    acted += 1;
                    if key.starts_with(crate::streak::NOVERDICT_PREFIX) {
                        if let Err(e) = page_inbox(paths, now, title, &evidence_of(key)) {
                            err.push(util::log_line(now, &format!("HEIMDALL could not write the inbox line for {key}: {e}")));
                        }
                    }
                }
                Err(_) => {
                    err.push(util::log_line(now, &format!("HEIMDALL could not raise the alert condition for {key}")));
                    state.keys.insert(key.clone(), step.on_failure());
                    beads_ok = false;
                }
            },
            Trigger::ConfirmClear => {
                let reason = alerts::clear_reason_with(&(inp.render_time)(step.counters.first, &inp.tz), step.counters.flaps, inp.clear_n);
                match alerts::alert_clear_with(ops, existing, &reason) {
                    Ok(()) => {
                        if let Some(row) = existing {
                            out.push(util::log_line(now, &format!("HEIMDALL cleared {key} ({})", row.key)));
                        }
                        state.keys.insert(key.clone(), step.on_success(now));
                        acted += 1;
                    }
                    Err(_) => {
                        state.keys.insert(key.clone(), step.on_failure());
                        beads_ok = false;
                    }
                }
            }
            Trigger::Refresh => {
                if existing.is_some_and(|r| !r.firing()) {
                    state.keys.insert(key.clone(), step.on_refresh_skip_closed(now));
                } else {
                    match alerts::alert_write(ops, key, title, &body_for(key, step.counters.first, step.counters.flaps)) {
                        Ok(id) => {
                            out.push(util::log_line(now, &format!("HEIMDALL re-raised {key} on {id}")));
                            let mut k = step.on_success(now);
                            k.bead = id;
                            state.keys.insert(key.clone(), k);
                        }
                        Err(_) => {
                            state.keys.insert(key.clone(), step.counters.clone());
                            beads_ok = false;
                        }
                    }
                }
            }
        }
    }

    let fallback_needed = !db_reachable || !beads_ok || rep.probe.write_ok == Some(false);
    if fallback_needed {
        let doc = fallback::build(now, db_reachable, rep.probe.write_ok, &rep.firing);
        match fallback::write(&paths.fallback, &doc) {
            Ok(()) => out.push(util::log_line(now, &format!("HEIMDALL wrote the fallback channel — db_read={} db_write={:?}", if db_reachable { "ok" } else { "DOWN" }, rep.probe.write_ok))),
            Err(e) => err.push(util::log_line(now, &format!("HEIMDALL could not write the fallback channel: {e}"))),
        }
    } else {
        fallback::remove(&paths.fallback);
    }

    // After the fallback, so a slow broker cannot delay it.
    let previous = std::fs::read_to_string(&paths.heartbeat).unwrap_or_default();
    let (alarm_line, delivered) = crate::alarm::publish(&previous, &rep.firing, &paths.inbox_bus, now);
    if let Err(e) = delivered {
        err.push(util::log_line(now, &format!("HEIMDALL could not publish the alarm topics: {e}")));
    }

    let db_read = heartbeat::db_read_status(db_reachable, rep.db_saturated);
    let db_write = heartbeat::db_write_status(rep.probe.write_ok, rep.db_saturated);
    let keys: Vec<String> = firing_keys.into_iter().collect();
    let hb = Heartbeat { at: now, firing_keys: &keys, db_read, db_write, db_saturated: rep.db_saturated, fallback_written: fallback_needed, acted };
    let _ = heartbeat::write(&paths.heartbeat, &(hb.render() + &alarm_line));
    out.push(util::log_line(now, &format!("heimdall: {} firing [{}], {} change(s), db_read={} db_write={}", keys.len(), keys.join(","), acted, DbStatus::as_str(&db_read), DbStatus::as_str(&db_write))));

    rep.acted = acted;
    rep.fallback_written = fallback_needed;
    rep.out = out;
    rep.err = err;
    rep
}

/// One page in the Concierge inbox: recorded and announced like any other writer's line.
pub fn page_inbox(paths: &PassPaths, now: i64, title: &str, evidence: &str) -> Result<(), String> {
    let beads = evidence.lines().find_map(|l| l.strip_prefix("BEADS")).map(str::trim).unwrap_or_default();
    let line = inbox_rules::line(&util::iso_utc(now), &format!("[heimdall] {title}; beads {beads}"));
    let publisher = || paths.inbox_bus.publisher("heimdall");
    inbox_rules::record(&paths.inbox, &line, &paths.inbox_cursor, publisher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{Mirror, RestartAlert, Thresholds};
    use crate::conditions::{ALERT, PROBE, PROBE_KEY};
    use crate::testing::{Fake, Mode};

    const STALLED: &str = "sentinel-stalled";
    const LOOPING: &str = "restart-loop:spira-runtime-prod.service";

    fn sentinel_log(now: i64, wedged: bool) -> String {
        let at = |t: i64| util::iso_utc(t);
        let state = |t: i64| format!("{} spira: state: open=20 plan_ready=5 in_progress=0 dwarves=1 runes=[builder ]\n", at(t));
        let done = |t: i64| format!("{} spira: pass complete — 0 action(s), 0 progress\n", at(t));
        if wedged {
            format!("{}{}{}{}{}", state(now - 3600), done(now - 3600), state(now - 240), state(now - 120), state(now))
        } else {
            format!("{}{}", state(now), done(now))
        }
    }

    struct Rig {
        dir: testkit::TempDir,
        ops: Fake,
        state: State,
        now: i64,
        wedged: bool,
        halted: bool,
        restart_delta: Option<i64>,
        refresh: i64,
        gate_rows: Vec<serde_json::Value>,
        gate_unread: bool,
    }

    impl Rig {
        fn new() -> Rig {
            Rig { dir: testkit::TempDir::new("heimdall-pass"), ops: Fake::default(), state: State::default(), now: 1_800_000_000, wedged: false, halted: false, restart_delta: None, refresh: 3600, gate_rows: Vec::new(), gate_unread: false }
        }
        fn paths(&self) -> PassPaths {
            PassPaths { fallback: self.dir.join("heimdall.alerts.json"), heartbeat: self.dir.join("heimdall.status"), inbox: self.dir.join("inbox.log"), inbox_cursor: self.dir.join("inbox.cursor"), inbox_bus: inbox_rules::Bus::Off }
        }
        fn pass(&mut self) -> PassReport {
            self.now += 120;
            let now = self.now;
            let restart_alerts = self.restart_delta.map(|delta| RestartAlert { unit: "spira-runtime-prod.service".into(), current: delta, baseline: 0, delta, window: 3600 }).into_iter().collect();
            let obs = Observation {
                sentinel_log: sentinel_log(now, self.wedged),
                sentinel_log_readable: true,
                sentinel_log_mtime: now,
                sentinel_timer: "active".into(),
                mirror: Mirror::default(),
                strands: serde_json::json!({}),
                restart_alerts,
                world_halted: self.halted,
                thresholds: Thresholds { pass_stale: 600, starve_passes: 5, mirror_stale: 90_000, ghost_stale: 1800, restart_threshold: 3, restart_window: 3600, red_streak: 4, noverdict_streak: 4 },
                gate_runs: if self.gate_unread { vec![] } else { vec![("spira".into(), self.gate_rows.clone())] },
                gate_runs_unread: self.gate_unread,
                ..Observation::default()
            };
            let inp = PassInput { obs, confirm: 2, clear_n: 2, refresh: self.refresh, tz: "UTC".into(), render_time: |e, _| format!("@{e}"), report_only: false };
            let paths = self.paths();
            run(&inp, &self.ops, &mut self.state, &paths, now)
        }
        fn passes(&mut self, n: usize) {
            for _ in 0..n {
                self.pass();
            }
        }
        fn raise_stalled(&mut self) {
            self.wedged = true;
            self.passes(2);
        }
        fn clear_stalled(&mut self) {
            self.wedged = false;
            self.passes(2);
        }
        fn status_and_flaps(&self, key: &str) -> Option<(String, u64)> {
            self.ops.row(key).map(|c| (c.status, c.flaps))
        }
        fn heartbeat(&self) -> String {
            std::fs::read_to_string(self.dir.join("heimdall.status")).unwrap_or_default()
        }
        fn inbox(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("inbox.log")).unwrap_or_default().lines().map(String::from).collect()
        }
        fn fallback_exists(&self) -> bool {
            self.dir.join("heimdall.alerts.json").exists()
        }
        fn fallback(&self) -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(self.dir.join("heimdall.alerts.json")).unwrap()).unwrap()
        }
    }

    fn gate_row(bead: &str, status: &str, reason: &str, unit: &str, line: &str) -> serde_json::Value {
        serde_json::json!({ "bead": bead, "status": status, "reason": reason, "red_unit": unit, "red_line": line })
    }

    fn reds(beads: &[&str], line: &str) -> Vec<serde_json::Value> {
        beads.iter().map(|b| gate_row(b, "FAIL", "branch-red", "unit-tests", line)).collect()
    }

    const STREAK: &str = "gate-red-streak/spira";
    const CARGO: &str = "bwrap: execvp cargo: No such file or directory";

    #[test_id(t52y8wm)]
    #[test]
    fn a_shared_red_on_four_beads_raises_one_streak_condition_and_a_pass_clears_it() {
        let mut r = Rig::new();
        r.gate_rows = reds(&["a", "b", "c", "d"], CARGO);
        r.passes(2);
        assert_eq!(r.status_and_flaps(STREAK), Some(("True".into(), 1)));
        let message = r.ops.message(STREAK).unwrap();
        assert!(message.contains("stopped by a red shared across branches"), "{message}");
        for needle in ["a, b, c, d", "unit-tests", CARGO] {
            assert!(message.contains(needle), "{needle} missing from {message}");
        }
        r.gate_rows.insert(0, gate_row("e", "FAIL", "branch-red", "unit-tests", CARGO));
        r.passes(2);
        assert_eq!(r.status_and_flaps(STREAK), Some(("True".into(), 1)), "a fifth identical red raises nothing new");
        assert_eq!(r.ops.count_of_kind(ALERT), 1);
        r.gate_rows.insert(0, gate_row("f", "PASS", "", "", ""));
        r.passes(2);
        assert_eq!(r.status_and_flaps(STREAK), Some(("False".into(), 1)));
    }

    const NOVERDICT: &str = "gate-noverdict-streak/spira";

    fn no_verdicts(beads: &[&str], reason: &str) -> Vec<serde_json::Value> {
        beads.iter().map(|b| gate_row(b, "NO_VERDICT", reason, "", "")).collect()
    }

    #[test_id(twtmgy5)]
    #[test]
    fn four_no_verdicts_with_one_reason_on_four_beads_page_once_and_a_pass_clears_it() {
        let mut r = Rig::new();
        r.gate_rows = no_verdicts(&["a", "b", "c", "d"], "fence-silent");
        r.passes(2);
        assert_eq!(r.status_and_flaps(NOVERDICT), Some(("True".into(), 1)));
        let inbox = r.inbox();
        assert_eq!(inbox.len(), 1, "{inbox:?}");
        for needle in ["reason=fence-silent", "a, b, c, d", "[heimdall]"] {
            assert!(inbox[0].contains(needle), "{needle} missing from {}", inbox[0]);
        }
        let alerts = r.ops.count_of_kind(ALERT);
        r.gate_rows.insert(0, gate_row("e", "NO_VERDICT", "fence-silent", "", ""));
        r.passes(2);
        assert_eq!(r.inbox().len(), 1, "a fifth does not page again");
        assert_eq!(r.ops.count_of_kind(ALERT), alerts);
        r.gate_rows.insert(0, gate_row("p", "PASS", "", "", ""));
        r.passes(2);
        assert_eq!(r.status_and_flaps(NOVERDICT), Some(("False".into(), 1)));
        assert_eq!(r.inbox().len(), 1, "a clear writes no line");
        r.gate_rows = no_verdicts(&["f", "g", "h", "i"], "fence-silent");
        r.passes(2);
        assert_eq!(r.inbox().len(), 2);
    }

    #[test_id(t7j846y)]
    #[test]
    fn a_no_verdict_streak_holds_through_an_unreadable_section_and_a_failed_append_leaves_the_raise() {
        let mut r = Rig::new();
        std::fs::create_dir(r.dir.join("inbox.log")).unwrap();
        r.gate_rows = no_verdicts(&["a", "b", "c", "d"], "x");
        r.passes(1);
        let rep = r.pass();
        assert!(rep.err.iter().any(|l| l.contains("could not write the inbox line")), "{:?}", rep.err);
        assert_eq!(r.status_and_flaps(NOVERDICT), Some(("True".into(), 1)));
        r.gate_unread = true;
        r.passes(3);
        assert_eq!(r.status_and_flaps(NOVERDICT), Some(("True".into(), 1)));
    }

    #[test_id(tqcmz0g)]
    #[test]
    fn no_verdicts_that_do_not_share_a_reason_or_four_beads_page_nothing() {
        let mut r = Rig::new();
        r.gate_rows = no_verdicts(&["a", "b", "c"], "x");
        r.passes(3);
        let mut rows = no_verdicts(&["a", "b", "c", "d"], "x");
        rows[1] = gate_row("c", "NO_VERDICT", "y", "", "");
        r.gate_rows = rows;
        r.passes(3);
        r.gate_rows = no_verdicts(&["a", "a", "a", "a"], "x");
        r.passes(3);
        assert_eq!(r.ops.row(NOVERDICT), None);
        assert!(r.inbox().is_empty());
        let mut rows = no_verdicts(&["a", "b", "c", "d"], "x");
        rows.insert(2, gate_row("z", "FAIL", "branch-red", "u", "l"));
        r.gate_rows = rows;
        r.passes(2);
        assert_eq!(r.status_and_flaps(NOVERDICT), Some(("True".into(), 1)), "a FAIL between does not break it");
    }

    #[test_id(tj50xij)]
    #[test]
    fn a_red_that_is_not_one_shared_red_on_four_beads_raises_nothing() {
        let mut r = Rig::new();
        r.gate_rows = reds(&["a", "b", "c", "a"], CARGO);
        r.passes(3);
        assert_eq!(r.ops.row(STREAK), None, "three beads is not four");
        let mut rows = reds(&["a", "b", "c", "d"], CARGO);
        rows[2] = gate_row("c", "FAIL", "branch-red", "unit-tests", "another line");
        r.gate_rows = rows;
        r.passes(3);
        assert_eq!(r.ops.row(STREAK), None, "two lines are two reds");
        let mut rows = reds(&["a", "b", "c", "d"], CARGO);
        rows[1] = gate_row("b", "NO_VERDICT", "died", "", "");
        r.gate_rows = rows;
        r.passes(3);
        assert_eq!(r.ops.row(STREAK), None, "a no-verdict is not a red");
    }

    #[test_id(tnkchae)]
    #[test]
    fn a_streak_holds_through_a_no_verdict_and_an_unreadable_section_and_clears_only_on_a_pass() {
        let mut r = Rig::new();
        r.gate_rows = reds(&["a", "b", "c", "d"], CARGO);
        r.passes(2);
        r.gate_rows.insert(0, gate_row("e", "NO_VERDICT", "died", "", ""));
        r.passes(3);
        assert_eq!(r.status_and_flaps(STREAK), Some(("True".into(), 1)), "a no-verdict is not a PASS");
        r.gate_unread = true;
        r.passes(3);
        assert_eq!(r.status_and_flaps(STREAK), Some(("True".into(), 1)), "an unreadable section is not a PASS");
    }

    // reconcile.rs holds the confirm rule (one_sighting_does_not_fire_a_condition_must_be_confirmed);
    // this asserts what it does to the store.
    #[test_id(tbjyzz2)]
    #[test]
    // covers: UC-ops-detection-remediation-19
    fn one_sighting_does_not_fire_a_condition_must_be_confirmed() {
        let mut r = Rig::new();
        r.wedged = true;
        r.pass();
        assert_eq!(r.ops.row(STALLED), None);
        assert!(r.heartbeat().contains("SP_HEIMDALL_AT="), "the heartbeat is written even when nothing is raised");
    }

    #[test_id(tlqn5pw)]
    #[test]
    // covers: UC-ops-detection-remediation-19
    fn a_confirmed_condition_raises_one_alert_whose_message_leads_with_the_title() {
        let mut r = Rig::new();
        r.raise_stalled();
        assert_eq!(r.status_and_flaps(STALLED), Some(("True".into(), 1)));
        assert_eq!(r.ops.row(STALLED).unwrap().kind, ALERT);
        let message = r.ops.message(STALLED).unwrap();
        assert!(message.starts_with("The Spira sentinel has not completed a pass in "), "{message}");
        r.pass();
        assert_eq!(r.status_and_flaps(STALLED), Some(("True".into(), 1)), "a condition that keeps holding is still one firing");
        assert_eq!(r.ops.count_of_kind(ALERT), 1);
    }

    #[test_id(t9g2z6l)]
    #[test]
    // covers: UC-ops-detection-remediation-19
    fn one_clear_sighting_does_not_retract_and_the_second_clears_it() {
        let mut r = Rig::new();
        r.raise_stalled();
        r.wedged = false;
        r.pass();
        assert_eq!(r.status_and_flaps(STALLED), Some(("True".into(), 1)));
        r.pass();
        assert_eq!(r.status_and_flaps(STALLED), Some(("False".into(), 1)));
        assert!(r.ops.calls().contains(&format!("clear {STALLED}")));
    }

    // The store forgets `acked` and keeps `silent_until` on this same `set`
    // (spira-lc: a_refresh_keeps_the_transition_and_a_reraise_counts_a_flap_from_the_first_sighting).
    #[test_id(tzyjnhy)]
    #[test]
    // covers: UC-ops-detection-remediation-19
    fn a_returning_condition_reopens_the_same_alert_as_one_more_flap() {
        let mut r = Rig::new();
        r.raise_stalled();
        r.clear_stalled();
        r.raise_stalled();
        assert_eq!(r.status_and_flaps(STALLED), Some(("True".into(), 2)));
        assert_eq!(r.ops.count_of_kind(ALERT), 1, "a flap does not accumulate alerts");
        let sets = r.ops.calls().iter().filter(|c| *c == &format!("set {STALLED} {ALERT} true")).count();
        assert_eq!(sets, 2, "the reopen is the same write as the raise, so the store's acked/silence rules apply");
    }

    #[test_id(t5y8ha6)]
    #[test]
    // covers: UC-ops-detection-remediation-19
    fn an_alert_closed_by_hand_is_not_reopened_while_it_still_fires() {
        let mut r = Rig::new();
        r.refresh = 60;
        r.raise_stalled();
        r.ops.clear_by_hand(STALLED);
        let before = r.ops.calls().len();
        r.passes(2);
        assert_eq!(r.status_and_flaps(STALLED), Some(("False".into(), 1)));
        let writes = r.ops.calls()[before..].iter().filter(|c| c.starts_with(&format!("set {STALLED}"))).count();
        assert_eq!(writes, 0);
    }

    #[test_id(tufyoib)]
    #[test]
    fn an_unreachable_store_falls_back_to_the_file_channel_and_the_file_goes_when_it_answers() {
        let mut r = Rig::new();
        r.wedged = true;
        r.ops.set_mode(Mode::Down(Failure::Other));
        r.pass();
        assert!(r.fallback_exists());
        let fb = r.fallback();
        assert_eq!(fb["db_reachable"], false);
        let keys: Vec<&str> = fb["alerts"].as_array().unwrap().iter().map(|a| a["key"].as_str().unwrap()).collect();
        assert!(keys.contains(&"db-unreachable"), "{keys:?}");
        assert!(r.heartbeat().contains("SP_HEIMDALL_DB_READ=down"), "the heartbeat is written with the store down");
        r.ops.set_mode(Mode::Up);
        r.pass();
        assert!(!r.fallback_exists(), "the fallback survived the store coming back");
    }

    #[test_id(th40ve8)]
    #[test]
    fn a_healthy_store_shows_write_ok_with_a_probe_that_never_fires_and_no_fallback() {
        let mut r = Rig::new();
        r.pass();
        assert!(r.heartbeat().contains("SP_HEIMDALL_DB_WRITE=ok"));
        assert!(r.heartbeat().contains("SP_HEIMDALL_DB_READ=ok"));
        assert_eq!(r.status_and_flaps(PROBE_KEY), Some(("False".into(), 0)));
        assert_eq!(r.ops.row(PROBE_KEY).unwrap().kind, PROBE);
        assert_eq!(r.ops.count_of_kind(ALERT), 0, "the probe is not listed among the alerts");
        assert!(!r.fallback_exists());
    }

    #[test_id(tgzxfga)]
    #[test]
    fn a_write_only_failure_reads_ok_reports_write_down_and_recovers() {
        let mut r = Rig::new();
        r.pass();
        r.ops.set_mode(Mode::WriteFails(Failure::Other));
        r.pass();
        let hb = r.heartbeat();
        assert!(hb.contains("SP_HEIMDALL_DB_READ=ok"), "{hb}");
        assert!(hb.contains("SP_HEIMDALL_DB_WRITE=down"), "{hb}");
        let fb = r.fallback();
        assert_eq!((fb["db_reachable"].clone(), fb["db_write_ok"].clone()), (true.into(), false.into()));
        r.ops.set_mode(Mode::Up);
        r.pass();
        assert!(r.heartbeat().contains("SP_HEIMDALL_DB_WRITE=ok"));
        assert!(!r.fallback_exists());
    }

    #[test_id(t8a4vpp)]
    #[test]
    fn a_raise_the_store_refuses_is_retried_next_pass_and_written_to_the_fallback() {
        let mut r = Rig::new();
        r.wedged = true;
        r.pass();
        r.ops.set_mode(Mode::WriteFails(Failure::Other));
        let rep = r.pass();
        assert_eq!(rep.acted, 0);
        assert_eq!(r.ops.row(STALLED), None);
        assert_eq!(r.fallback()["alerts"][0]["key"], STALLED);
        assert_eq!(r.state.keys[STALLED].state, "clear", "the failed raise is not recorded as done");
        r.ops.set_mode(Mode::Up);
        r.pass();
        assert_eq!(r.status_and_flaps(STALLED), Some(("True".into(), 1)));
        assert!(!r.fallback_exists());
    }

    #[test_id(tt3gv8e)]
    #[test]
    fn a_timed_out_store_is_saturated_not_down_and_writes_the_fallback() {
        let mut r = Rig::new();
        r.ops.set_mode(Mode::Down(Failure::Saturated));
        r.pass();
        let hb = r.heartbeat();
        assert!(hb.contains("SP_HEIMDALL_DB_READ=saturated"), "{hb}");
        assert!(hb.contains("SP_HEIMDALL_DB_SATURATED=1"), "{hb}");
        assert!(r.fallback_exists());
    }

    // restart.rs holds the threshold (restart_count_below_threshold_produces_a_small_delta); an
    // alert is only handed in above it.
    #[test_id(tomj8ir)]
    #[test]
    fn a_restart_loop_is_confirmed_stays_one_firing_clears_and_reopens_the_same_alert() {
        let mut r = Rig::new();
        r.restart_delta = Some(3);
        r.passes(2);
        assert_eq!(r.ops.row(LOOPING), None, "a delta at the threshold fires nothing");
        r.restart_delta = Some(10);
        r.pass();
        assert_eq!(r.ops.row(LOOPING), None, "one sighting does not fire");
        r.pass();
        assert_eq!(r.status_and_flaps(LOOPING), Some(("True".into(), 1)));
        r.pass();
        assert_eq!(r.status_and_flaps(LOOPING), Some(("True".into(), 1)), "a second pass on a standing loop is no new firing");
        r.restart_delta = None;
        r.passes(2);
        assert_eq!(r.status_and_flaps(LOOPING), Some(("False".into(), 1)));
        r.restart_delta = Some(8);
        r.passes(2);
        assert_eq!(r.status_and_flaps(LOOPING), Some(("True".into(), 2)));
    }

    #[test_id(t3ncfvq)]
    #[test]
    fn a_halted_loop_raises_nothing_and_lifting_the_halt_restores_alerting() {
        let mut r = Rig::new();
        r.wedged = true;
        r.halted = true;
        r.passes(2);
        assert_eq!(r.ops.row(STALLED), None);
        r.halted = false;
        r.passes(2);
        assert_eq!(r.status_and_flaps(STALLED).map(|s| s.0), Some("True".into()));
    }
}
