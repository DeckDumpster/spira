//! `--czar-outcome-check` — verifies czar-trigger beads are handled and that the condition
//! they fired for actually cleared after closure (law-measure-the-outcome: the czar closing
//! a bead is evidence the czar ran, not evidence the condition cleared).
//!
//! `classify` is `watchtower-czar-outcome.py` inlined (DESIGN.md §4) — the classifier had
//! exactly one caller, so the extra process fork per sentinel pass bought nothing once the
//! caller itself moved to Rust. The logic is unchanged line-for-line from the python.

use crate::incident::{self, Finding};
use spira_store::lc_call::LcClient;
use crate::log::{log, parse_iso_utc};
use serde::Deserialize;
use spira_store::lc_state;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct TriggerBead {
    pub id: String,
    /// The bead's lifecycle state (`spira-lc list`), joined in after the bd read: a czar
    /// trigger is work the czar persona claims, so its state is the machine's, never bd's
    /// `status` (design §3.4, sp-mve9i). Empty when the machine has no row for it.
    #[serde(skip)]
    pub state: String,
    #[serde(default)]
    pub external_ref: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub closed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Unclaimed { id: String, external_ref: String },
    NotCleared { id: String, external_ref: String },
}

/// The pure classifier: `now_s`/`outcome_mins`/`unclaimed_mins` and the full czar-trigger
/// bead list in, zero or more findings out, one per `incident:queue-*`-refed group.
pub fn classify(
    now_s: i64,
    outcome_mins: i64,
    unclaimed_mins: i64,
    beads: &[TriggerBead],
) -> Vec<Outcome> {
    let out_secs = outcome_mins * 60;
    let unc_secs = unclaimed_mins * 60;

    type Timestamped<'a> = (&'a TriggerBead, Option<i64>, Option<i64>);
    let mut by_ref: std::collections::BTreeMap<String, Vec<Timestamped>> = Default::default();
    for b in beads {
        if !b.external_ref.starts_with("incident:queue-") {
            continue;
        }
        let ct = parse_iso_utc(&b.created_at);
        let cla = parse_iso_utc(&b.closed_at);
        by_ref
            .entry(b.external_ref.clone())
            .or_default()
            .push((b, ct, cla));
    }

    let mut findings = Vec::new();
    for (reference, mut group) in by_ref {
        group.sort_by_key(|(_, ct, _)| ct.unwrap_or(0));
        let (newest, newest_ct, _) = group.last().cloned().unwrap();
        // Not yet handed on by the czar (READY/REWORK/WORKING): the old open/in_progress.
        if !newest.state.is_empty() && !lc_state::past_builder(&newest.state) {
            if let Some(ct) = newest_ct {
                if now_s - ct >= unc_secs {
                    findings.push(Outcome::Unclaimed {
                        id: newest.id.clone(),
                        external_ref: reference.clone(),
                    });
                }
            }
            continue;
        }

        for i in 0..group.len() {
            let (bead, _, cla) = &group[i];
            // The czar handed it on: the old `closed`. No row is no state, never "closed".
            if !lc_state::past_builder(&bead.state) {
                continue;
            }
            let cla = match cla {
                Some(c) => *c,
                None => continue,
            };
            if now_s - cla < out_secs {
                continue;
            }
            let recurred = group[i + 1..]
                .iter()
                .any(|(_, ct, _)| ct.map(|c| c > cla).unwrap_or(false));
            if recurred {
                findings.push(Outcome::NotCleared {
                    id: bead.id.clone(),
                    external_ref: reference.clone(),
                });
                break;
            }
        }
    }
    findings
}

pub struct Cfg {
    pub outcome_mins: i64,
    pub unclaimed_mins: i64,
    pub label: String,
    pub lookback_hours: i64,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            outcome_mins: 30,
            unclaimed_mins: 10,
            label: "czar-trigger".to_string(),
            lookback_hours: 24,
        }
    }
}

/// The label's open beads and those closed within `lookback_hours`, joined to their lifecycle
/// states. A read that fails is an `Err` carrying the store's text, never an empty list.
pub fn query_beads(lc: &LcClient, label: &str, now: i64, lookback_hours: i64) -> Result<Vec<TriggerBead>, String> {
    let beads = query_bd(lc, label, now, lookback_hours)?;
    let ids: Vec<String> = beads.iter().map(|b| b.id.clone()).collect();
    let rows = lc_state::list_ids_on(lc, &ids).map_err(|e| format!("cannot read lifecycle state ({e})"))?;
    Ok(join_states(beads, &lc_state::index(rows)))
}

/// Each bead's lifecycle state, by id.
pub fn join_states(mut beads: Vec<TriggerBead>, lc: &HashMap<String, lc_state::Row>) -> Vec<TriggerBead> {
    for b in &mut beads {
        b.state = lc.get(&b.id).map(|r| r.state.clone()).unwrap_or_default();
    }
    beads
}

/// The label's open beads, and its beads closed since `now − lookback`, merged by id. The store
/// refuses a label as a bound on the whole history, so the closed read carries a time window.
fn query_bd(lc: &LcClient, label: &str, now: i64, lookback_hours: i64) -> Result<Vec<TriggerBead>, String> {
    let since = crate::log::fmt_iso(crate::lc::held_bound(now - lookback_hours * 3600));
    let reads: [&[&str]; 2] = [
        &["--label", label, "--limit", "0", "--json", "--brief"],
        &["--label", label, "--all", "--limit", "0", "--closed-after", &since, "--json", "--brief"],
    ];
    let mut merged: Vec<TriggerBead> = Vec::new();
    for args in reads {
        let text = crate::lc::content_list_read(lc, args)?;
        let rows: Vec<TriggerBead> = serde_json::from_str(&text).map_err(|e| format!("unreadable rows ({e})"))?;
        for r in rows {
            if !merged.iter().any(|m| m.id == r.id) {
                merged.push(r);
            }
        }
    }
    Ok(merged)
}

pub fn run(now: i64, lc: &LcClient, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    if !incident::is_usable(incident_sh) {
        log(&format!(
            "watchtower: czar-outcome-check skipped — {} not readable",
            incident_sh
        ));
        return;
    }
    check(now, lc, db, home_repo, cfg, &mut |f| {
        incident::alarm(incident_sh, f);
    });
}

/// The check with its raise handed in: each finding the classifier returns, as the alarm it files.
fn check(now: i64, lc: &LcClient, db: &str, home_repo: &str, cfg: &Cfg, raise: &mut dyn FnMut(&Finding)) {
    let beads = match query_beads(lc, &cfg.label, now, cfg.lookback_hours) {
        Ok(b) => b,
        Err(e) => {
            log(&format!("watchtower: czar-outcome-check cannot read the trigger beads ({e}) — skipped"));
            return;
        }
    };
    for finding in classify(now, cfg.outcome_mins, cfg.unclaimed_mins, &beads) {
        match finding {
            Outcome::Unclaimed { id, external_ref } => {
                let body = format!(
                    "Czar-trigger bead {id} (class: {external_ref}) has been open for more than {} minutes without being claimed or closed.\n\nThe czar's calling budget is 5 minutes. If the czar is not running, check: SPIRA_RUNES, the czar rune's RUNE_REACTIVE, and the spira-dwarf-czar unit.\n\nBead: {id}\nClass: {external_ref}\n",
                    cfg.unclaimed_mins
                );
                let f = Finding::new(
                    db,
                    home_repo,
                    &format!("CZAR: trigger bead {id} unclaimed ({external_ref})"),
                    &body,
                )
                .priority(1)
                .reference(format!("incident:czar-unclaimed-{id}"))
                .cause("czar-unclaimed");
                raise(&f);
                log(&format!(
                    "watchtower: czar-outcome-check filed unclaimed escalation for {id}"
                ));
            }
            Outcome::NotCleared { id, external_ref } => {
                let body = format!(
                    "Czar closed trigger bead {id} (class: {external_ref}) but the condition returned: a newer bead with the same class was filed after the closure, and the outcome window ({}m) has elapsed.\n\nThe czar's action did not hold. Investigate: was the batch actually fixed, or did the same fault recur?\n\nBead: {id}\nClass: {external_ref}\n",
                    cfg.outcome_mins
                );
                let f = Finding::new(
                    db,
                    home_repo,
                    &format!("CZAR: outcome not cleared — condition returned after {id} closed ({external_ref})"),
                    &body,
                )
                .priority(1)
                .reference(format!("incident:czar-not-cleared-{id}"))
                .cause("czar-not-cleared");
                raise(&f);
                log(&format!(
                    "watchtower: czar-outcome-check filed not-cleared escalation for {id}"
                ));
            }
        }
    }
    log("watchtower: czar-outcome-check complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: UC-ops-detection-remediation-25
    #[test_id(txrh30f)]
    #[test]
    fn trigger_beads_are_read_as_an_open_read_and_a_bounded_closed_read_merged_by_id() {
        let (t, lc) = crate::lc::scripted::scripted(0, r#"[{"id":"sp-a","external_ref":"czar:deadlock"}]"#);
        let beads = query_bd(&lc, "czar-trigger", NOW, 24).unwrap();
        assert_eq!(beads.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), vec!["sp-a"], "one bead read twice is one bead");
        let seen = t.seen.lock().unwrap();
        assert_eq!(seen[0], vec!["content", "list", "--label", "czar-trigger", "--limit", "0", "--json", "--brief"]);
        let since = crate::log::fmt_iso(crate::lc::held_bound(NOW - 24 * 3600));
        assert_eq!(seen[1], vec!["content", "list", "--label", "czar-trigger", "--all", "--limit", "0", "--closed-after", &since, "--json", "--brief"]);
        assert!(query_bd(&crate::lc::scripted::scripted(2, "cannot tell").1, "x", NOW, 24).is_err());
    }

    /// The fixtures name the old bd words; each stands for the lifecycle state it meant.
    fn bead(id: &str, status: &str, reference: &str, created: &str, closed: &str) -> TriggerBead {
        let state = match status {
            "open" => "READY",
            "in_progress" => "WORKING",
            "closed" => "DONE",
            other => other,
        };
        TriggerBead {
            id: id.to_string(),
            state: state.to_string(),
            external_ref: reference.to_string(),
            created_at: created.to_string(),
            closed_at: closed.to_string(),
        }
    }

    const NOW: i64 = 1_700_100_000;

    /// sp-mve9i: a trigger's state is its lifecycle row's. bd may say closed while the czar
    /// still holds it (WORKING): that is unclaimed-or-pending work, not a closure; and a
    /// bead the machine has no row for is neither.
    #[test_id(t1z0uej)]
    #[test]
    fn the_trigger_state_is_the_lifecycle_row_not_bd_status() {
        let raw: Vec<TriggerBead> = serde_json::from_str(&format!(
            r#"[{{"id":"sp-a","status":"closed","external_ref":"incident:queue-x","created_at":"{}","closed_at":"{}"}},
                {{"id":"sp-b","status":"open","external_ref":"incident:queue-y","created_at":"{}"}}]"#,
            minsago(60),
            minsago(50),
            minsago(60)
        ))
        .unwrap();
        let lc: HashMap<String, lc_state::Row> =
            [("sp-a".to_string(), lc_state::Row { bead_id: "sp-a".into(), state: "WORKING".into(), ..Default::default() })].into();
        let beads = join_states(raw, &lc);
        assert_eq!(beads[0].state, "WORKING");
        assert_eq!(beads[1].state, "");
        assert_eq!(
            classify(NOW, 30, 10, &beads),
            vec![Outcome::Unclaimed { id: "sp-a".into(), external_ref: "incident:queue-x".into() }]
        );
    }
    fn minsago(m: i64) -> String {
        crate::log::fmt_iso(NOW - m * 60)
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(tya4uh1)]
    #[test]
    fn positive_control_unclaimed_open_bead_past_threshold_fires() {
        let beads = vec![bead(
            "sp-czoc1",
            "open",
            "incident:queue-deadlock-batch-open",
            &minsago(15),
            "",
        )];
        let found = classify(NOW, 30, 10, &beads);
        assert_eq!(
            found,
            vec![Outcome::Unclaimed {
                id: "sp-czoc1".into(),
                external_ref: "incident:queue-deadlock-batch-open".into(),
            }]
        );
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(t8j4ek1)]
    #[test]
    fn positive_control_not_cleared_condition_returned_after_czar_closed_its_bead() {
        let beads = vec![
            bead(
                "sp-czoc2a",
                "closed",
                "incident:queue-attribution-failed-requeue",
                &minsago(50),
                &minsago(45),
            ),
            bead(
                "sp-czoc2b",
                "closed",
                "incident:queue-attribution-failed-requeue",
                &minsago(35),
                &minsago(15),
            ),
        ];
        let found = classify(NOW, 30, 10, &beads);
        assert_eq!(
            found,
            vec![Outcome::NotCleared {
                id: "sp-czoc2a".into(),
                external_ref: "incident:queue-attribution-failed-requeue".into(),
            }]
        );
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(tg1qtri)]
    #[test]
    fn open_bead_younger_than_unclaimed_threshold_is_silent() {
        let beads = vec![bead(
            "sp-czoc3",
            "open",
            "incident:queue-sort-failed-ranking",
            &minsago(5),
            "",
        )];
        assert!(classify(NOW, 30, 10, &beads).is_empty());
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(tk2gvm1)]
    #[test]
    fn closed_with_no_recurrence_is_silent() {
        let beads = vec![bead(
            "sp-czoc4",
            "closed",
            "incident:queue-loop-stalled",
            &minsago(50),
            &minsago(35),
        )];
        assert!(classify(NOW, 30, 10, &beads).is_empty());
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(tjbi8ji)]
    #[test]
    fn outcome_window_not_yet_elapsed_is_silent() {
        let beads = vec![
            bead(
                "sp-czoc5a",
                "closed",
                "incident:queue-deadlock-batch-open",
                &minsago(50),
                &minsago(15),
            ),
            bead(
                "sp-czoc5b",
                "open",
                "incident:queue-deadlock-batch-open",
                &minsago(5),
                "",
            ),
        ];
        assert!(classify(NOW, 30, 10, &beads).is_empty());
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(tkkeg1w)]
    #[test]
    fn a_non_queue_ref_is_ignored_but_queue_prefixed_still_fires() {
        let beads = vec![bead(
            "sp-czoc8",
            "open",
            "incident:queue-deadlock-batch-open-other",
            &minsago(15),
            "",
        )];
        assert_eq!(
            classify(NOW, 30, 10, &beads),
            vec![Outcome::Unclaimed {
                id: "sp-czoc8".into(),
                external_ref: "incident:queue-deadlock-batch-open-other".into(),
            }]
        );

        let beads2 = vec![bead("sp-czoc9", "open", "some-other-ref", &minsago(15), "")];
        assert!(classify(NOW, 30, 10, &beads2).is_empty());
    }

    fn world(trigger_rows: &str, states: &str) -> (std::sync::Arc<crate::lc::scripted::Scripted>, LcClient) {
        crate::lc::scripted::scripted_by(&[("content list", 0, trigger_rows), ("list --ids", 0, states)])
    }

    fn checked(lc: &LcClient, unclaimed_mins: i64) -> Vec<Finding> {
        let cfg = Cfg { unclaimed_mins, ..Cfg::default() };
        let mut raised = Vec::new();
        check(NOW, lc, "db", "spira", &cfg, &mut |f| raised.push(f.clone()));
        raised
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(tc1soq0)]
    #[test]
    fn an_unclaimed_trigger_is_raised_under_a_per_bead_ref_that_is_the_same_every_pass() {
        let rows = format!(r#"[{{"id":"sp-czoc1","external_ref":"incident:queue-deadlock-batch-open","created_at":"{}"}}]"#, minsago(15));
        let (t, lc) = world(&rows, r#"[{"bead_id":"sp-czoc1","state":"READY"}]"#);
        let first = checked(&lc, 10);
        let second = checked(&lc, 10);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].reference.as_deref(), Some("incident:czar-unclaimed-sp-czoc1"));
        assert_eq!(first[0].reference, second[0].reference, "a recurrence must dedup, not file a second");
        assert_eq!(first[0].cause.as_deref(), Some("czar-unclaimed"));
        assert!(first[0].title.contains("sp-czoc1"));
        assert_eq!(t.seen.lock().unwrap()[0][..4], ["content", "list", "--label", "czar-trigger"], "only czar-trigger beads are asked for");
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(t5tmcvx)]
    #[test]
    fn a_trigger_younger_than_the_configured_threshold_is_not_raised() {
        let rows = format!(r#"[{{"id":"sp-czoc1","external_ref":"incident:queue-deadlock-batch-open","created_at":"{}"}}]"#, minsago(15));
        let (_, lc) = world(&rows, r#"[{"bead_id":"sp-czoc1","state":"READY"}]"#);
        assert!(checked(&lc, 20).is_empty(), "15 minutes old is inside a 20 minute threshold");
        assert_eq!(checked(&lc, 14).len(), 1, "and outside a 14 minute one: the threshold is the configured one");
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(tre3trm)]
    #[test]
    fn a_recurrence_after_the_outcome_window_is_raised_as_not_cleared_under_its_own_ref() {
        let rows = format!(
            r#"[{{"id":"sp-czoc2a","external_ref":"incident:queue-sort-failed-x","created_at":"{}","closed_at":"{}"}},
                {{"id":"sp-czoc2b","external_ref":"incident:queue-sort-failed-x","created_at":"{}","closed_at":"{}"}}]"#,
            minsago(50),
            minsago(45),
            minsago(35),
            minsago(15)
        );
        let (_, lc) = world(&rows, r#"[{"bead_id":"sp-czoc2a","state":"DONE"},{"bead_id":"sp-czoc2b","state":"DONE"}]"#);
        let raised = checked(&lc, 10);
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].reference.as_deref(), Some("incident:czar-not-cleared-sp-czoc2a"));
        assert_eq!(raised[0].cause.as_deref(), Some("czar-not-cleared"));
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(ts80tzf)]
    #[test]
    fn an_unreadable_lifecycle_state_raises_nothing() {
        let rows = format!(r#"[{{"id":"sp-czoc1","external_ref":"incident:queue-x","created_at":"{}"}}]"#, minsago(15));
        let (_, lc) = crate::lc::scripted::scripted_by(&[("content list", 0, &rows), ("list --ids", 2, "cannot tell")]);
        assert!(checked(&lc, 10).is_empty());
    }

    /// The store as it answers: `content list` from the fake (which refuses the whole history),
    /// `list --ids` from the lifecycle rows given.
    struct Store {
        content: spira_store::lc_call::fake::Content,
        states: String,
    }

    impl spira_store::lc_call::Transport for Store {
        fn exchange(&self, argv: &[String], stdin: Option<&str>, d: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            if argv.get(..2) == Some(&["list".to_string(), "--ids".to_string()][..]) { // BDQ_READ_CONSIDERED="the lifecycle door, not a bd read"
                return Ok(spira_store::lc_call::Reply { code: 0, out: self.states.clone(), err: String::new(), read_ended: false });
            }
            self.content.exchange(argv, stdin, d)
        }
    }

    fn store(rows: Vec<serde_json::Value>, states: &str) -> (std::sync::Arc<Store>, LcClient) {
        let s = std::sync::Arc::new(Store { content: spira_store::lc_call::fake::Content::new(rows), states: states.to_string() });
        (s.clone(), LcClient::over(s, true))
    }

    fn trigger(id: &str, status: &str, reference: &str, created: &str, closed: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "status": status, "labels": ["czar-trigger"], "external_ref": reference, "created_at": created, "closed_at": closed})
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(t7qk2dm)]
    #[test]
    fn against_a_store_that_refuses_the_whole_history_both_alarms_are_raised() {
        let rows = vec![
            trigger("sp-open", "open", "incident:queue-a", &minsago(15), ""),
            trigger("sp-old", "closed", "incident:queue-b", &minsago(50), &minsago(45)),
            trigger("sp-new", "closed", "incident:queue-b", &minsago(35), &minsago(15)),
            serde_json::json!({"id": "sp-other", "status": "open", "labels": ["other"], "external_ref": "incident:queue-c", "created_at": minsago(90)}),
            trigger("sp-ancient", "closed", "incident:queue-d", "2020-01-01T00:00:00Z", "2020-01-02T00:00:00Z"),
        ];
        let states = r#"[{"bead_id":"sp-open","state":"READY"},{"bead_id":"sp-old","state":"DONE"},{"bead_id":"sp-new","state":"DONE"}]"#;
        let (s, lc) = store(rows, states);
        let raised = checked(&lc, 10);
        let refs: Vec<_> = raised.iter().map(|f| f.reference.clone().unwrap()).collect();
        assert_eq!(refs, vec!["incident:czar-unclaimed-sp-open", "incident:czar-not-cleared-sp-old"]);
        assert!(s.content.requests().iter().all(|r| !spira_store::lc_call::reads_whole_history(r)), "{:?}", s.content.requests());
    }

    // covers: UC-ops-detection-remediation-25
    #[test_id(tpw3n8c)]
    #[test]
    fn a_refused_read_is_logged_as_unread_and_raises_nothing() {
        let (_, lc) = crate::lc::scripted::scripted_by(&[("content list", 3, "refused: whole bead history")]);
        assert!(checked(&lc, 10).is_empty());
        let err = query_beads(&lc, "czar-trigger", NOW, 24).unwrap_err();
        assert!(err.contains("refused"), "the store's text is kept: {err}");
    }
}
