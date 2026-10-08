//! `--czar-outcome-check` — verifies czar-trigger beads are handled and that the condition
//! they fired for actually cleared after closure (law-measure-the-outcome: the czar closing
//! a bead is evidence the czar ran, not evidence the condition cleared).
//!
//! `classify` is `watchtower-czar-outcome.py` inlined (DESIGN.md §4) — the classifier had
//! exactly one caller, so the extra process fork per sentinel pass bought nothing once the
//! caller itself moved to Rust. The logic is unchanged line-for-line from the python.

use crate::incident::{self, Finding};
use crate::log::{log, parse_iso_utc};
use serde::Deserialize;
use spira_config::lc_state;
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
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            outcome_mins: 30,
            unclaimed_mins: 10,
            label: "czar-trigger".to_string(),
        }
    }
}

/// `bd -C $db list --label <label> --all --json --limit 0 --brief` — a read failure (bad
/// db, `bd` missing) yields an empty list, matching the bash's `|| _co_raw=""` -> `'[]'`.
pub fn query_beads(bd_bin: &str, db: &str, label: &str) -> Vec<TriggerBead> {
    let beads = query_bd(bd_bin, db, label);
    match lc_state::list() {
        Ok(rows) => join_states(beads, &lc_state::index(rows)),
        Err(e) => {
            // A state that cannot be read is not read: no finding this pass.
            log(&format!("watchtower: czar-outcome-check cannot read lifecycle state ({e}) — skipped"));
            Vec::new()
        }
    }
}

/// Each bead's lifecycle state, by id.
pub fn join_states(mut beads: Vec<TriggerBead>, lc: &HashMap<String, lc_state::Row>) -> Vec<TriggerBead> {
    for b in &mut beads {
        b.state = lc.get(&b.id).map(|r| r.state.clone()).unwrap_or_default();
    }
    beads
}

fn query_bd(bd_bin: &str, db: &str, label: &str) -> Vec<TriggerBead> {
    let out = spira_config::bounded::bounded(bd_bin)
        .args(["-C", db, "list", "--label", label, "--all", "--json", "--limit", "0", "--brief"])
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out);
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn run(now: i64, bd_bin: &str, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    if !incident::is_usable(incident_sh) {
        log(&format!(
            "watchtower: czar-outcome-check skipped — {} not readable",
            incident_sh
        ));
        return;
    }
    let beads = query_beads(bd_bin, db, &cfg.label);
    for finding in classify(now, cfg.outcome_mins, cfg.unclaimed_mins, &beads) {
        match finding {
            Outcome::Unclaimed { id, external_ref } => {
                let body = format!(
                    "Czar-trigger bead {id} (class: {external_ref}) has been open for more than {} minutes without being claimed or closed.\n\nThe czar's summoning budget is 5 minutes. If the czar lane is not running, check: SPIRA_FAYTHS, SPIRA_LANES, and the spira-aeon-czar unit.\n\nBead: {id}\nClass: {external_ref}\n",
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
                incident::alarm(incident_sh, &f);
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
                incident::alarm(incident_sh, &f);
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

    #[test]
    fn query_beads_returns_empty_on_a_failing_bd() {
        let out = query_beads("/does/not/exist/bd", "/tmp/nope", "czar-trigger");
        assert!(out.is_empty());
    }
}
