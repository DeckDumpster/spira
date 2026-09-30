//! `groomer sweep [--dry-run]` — mechanical livelock and whole-graph STATE remedies run
//! before the model pass. Parsing each detector's line is pure ([`parse`]); applying the
//! remedy is the impure half below, against [`crate::bd::Bd`] and [`crate::seam::Seam`].
//!
//! Three passes, in the order `groomer.sh` ran them (DESIGN.md §3):
//! 1. the LIVELOCK worklist (`ask-no-overseer`, `ci-stuck`, `unmapped-repo`, `unclaimable`);
//! 2. the whole-graph STATE scan (`landed-but-open`, `closed-no-branch`,
//!    `closed-never-landed` conflict/batch-ready, then `blocked-by-unlanded` over whatever
//!    got reopened);
//! 3. the incident partition (`incident-is-code`).

use crate::bd::Bd;
use crate::litter;
use crate::seam::Seam;

/// One parsed `LIVELOCK <id> <category> — <reason>` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Livelock {
    pub id: String,
    pub category: String,
    pub reason: String,
}

/// One parsed `STATE <id> <kind> [<extra>] — <evidence>` line. `extra` carries
/// `closed-never-landed`'s `conflict`/`batch-ready` tag, or `blocked-by-unlanded`'s
/// blocker id; empty otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateLine {
    pub id: String,
    pub kind: String,
    pub extra: String,
    pub evidence: String,
}

/// `LIVELOCK <id> <category> — <reason>` → fields, or `None` for a blank or malformed
/// line (mirrors the bash `case` falling through silently).
pub fn parse_livelock(line: &str) -> Option<Livelock> {
    let rest = line.strip_prefix("LIVELOCK ")?;
    let (id, rest) = rest.split_once(' ')?;
    let (category, reason) = rest.split_once(" — ").unwrap_or((rest, ""));
    Some(Livelock { id: id.to_string(), category: category.trim().to_string(), reason: reason.to_string() })
}

/// `STATE <id> <kind> [<extra>] — <evidence>`. `kind` is the first token after the id;
/// for `closed-never-landed` and `blocked-by-unlanded` a second token (the conflict tag
/// or the blocker id) follows before the em dash.
pub fn parse_state(line: &str) -> Option<StateLine> {
    let rest = line.strip_prefix("STATE ")?;
    let (id, rest) = rest.split_once(' ')?;
    let (head, evidence) = rest.split_once(" — ").unwrap_or((rest, ""));
    let mut parts = head.splitn(2, ' ');
    let kind = parts.next().unwrap_or("").to_string();
    let extra = parts.next().unwrap_or("").trim().to_string();
    Some(StateLine { id: id.to_string(), kind, extra, evidence: evidence.to_string() })
}

pub struct Outcome {
    pub acted: u32,
    /// One line per action, in `groomer.sh`'s own log vocabulary (`OVERSEER`, `UNSTUCK`,
    /// `CLOSED`, `REPORT`, `DROPPED`, `REOPENED`, `NOTED`, `REROUTED`), for the caller to
    /// print and append to `$SPIRA_RUN/groom.log`.
    pub log: Vec<String>,
}

pub fn sweep(bd: &dyn Bd, seam: &dyn Seam, dry_run: bool) -> Result<Outcome, String> {
    let mut log = Vec::new();
    let mut acted = 0u32;

    // ---- pass 1: the LIVELOCK worklist -------------------------------------------------
    let ll = seam.detect_livelocked()?;
    if ll.trim().is_empty() {
        log.push("no livelocked beads".to_string());
    }
    for line in ll.lines() {
        let Some(row) = parse_livelock(line) else { continue };
        match row.category.as_str() {
            "ask-no-overseer" => {
                log.push(format!("OVERSEER {} — adding overseer label; decisions pane cannot see this bead", row.id));
                if !dry_run {
                    let _ = bd.label_add(&row.id, "overseer");
                }
                acted += 1;
            }
            "ci-stuck" => {
                let ci_label = seam.conf("SPIRA_CI_LABEL")?;
                log.push(format!(
                    "UNSTUCK {} — stripping {ci_label}; repo not pr-mode, no run will ever clear this label",
                    row.id
                ));
                if !dry_run {
                    if !ci_label.is_empty() {
                        let _ = bd.label_remove(&row.id, &ci_label);
                    }
                    let _ = std::process::Command::new("spira-lc").args(["unhold", &row.id, "wait", "groomer"]).status();
                }
                acted += 1;
            }
            "unmapped-repo" => {
                let bj = bd.show_json(&row.id);
                match bj {
                    Err(_) => log.push(format!("REPORT {} unmapped-repo — bd show failed; leaving for model. {}", row.id, row.reason)),
                    Ok(v) => {
                        let verdict = litter::judge(&v);
                        if !verdict.has_content {
                            log.push(format!("CLOSED {} — litter: no description, {}. Detector: {}", row.id, verdict.meta, row.reason));
                            if !dry_run {
                                let _ = bd.close(&row.id, &format!("litter: no description, {}. Detector: {}", verdict.meta, row.reason));
                            }
                            acted += 1;
                        } else {
                            log.push(format!("REPORT {} unmapped-repo — has description; leaving for model. {}", row.id, row.reason));
                        }
                    }
                }
            }
            "unclaimable" => {
                log.push(format!("REPORT {} unclaimable — {}", row.id, row.reason));
            }
            _ => {}
        }
    }

    // ---- pass 2: whole-graph STATE scan -------------------------------------------------
    let mut reopened: Vec<String> = Vec::new();
    let lbo = seam.detect_landed_but_open()?;
    for line in lbo.lines() {
        let Some(row) = parse_state(line) else { continue };
        if row.kind == "landed-but-open" {
            log.push(format!("CLOSED {} — landed-but-open: {}", row.id, row.evidence));
            if !dry_run {
                let _ = bd.close(&row.id, &format!("landed-but-open: {}", row.evidence));
            }
            acted += 1;
        }
    }

    let cul = seam.detect_closed_unlanded_states()?;
    for line in cul.lines() {
        let Some(row) = parse_state(line) else { continue };
        match row.kind.as_str() {
            "closed-no-branch" => {
                log.push(format!("DROPPED {} — closed-no-branch: {}", row.id, row.evidence));
                if !dry_run {
                    let _ = bd.label_add(&row.id, "spira-dropped");
                    let _ = bd.note(&row.id, &format!("Labeled spira-dropped by groomer sweep: closed-no-branch — {}", row.evidence));
                }
                acted += 1;
            }
            "closed-never-landed" if row.extra == "conflict" => {
                log.push(format!("REOPENED {} — closed-never-landed, needs a rebase: {}", row.id, row.evidence));
                if !dry_run {
                    let _ = seam.bead_reopen(
                        &row.id,
                        "closed-never-landed-conflict",
                        &format!("Reopened by groomer sweep: closed but its branch does not merge cleanly into the base — {}. Needs a rebase before it can land.", row.evidence),
                    );
                }
                reopened.push(row.id.clone());
                acted += 1;
            }
            "closed-never-landed" if row.extra == "batch-ready" => {
                log.push(format!("REOPENED {} — closed-never-landed, batch-ready: {}", row.id, row.evidence));
                if !dry_run {
                    let _ = seam.bead_reopen(
                        &row.id,
                        "closed-never-landed-batch-ready",
                        &format!("Reopened by groomer sweep: closed but never landed — {}. The branch merges cleanly; it is batch-ready to requeue as-is.", row.evidence),
                    );
                }
                reopened.push(row.id.clone());
                acted += 1;
            }
            _ => {}
        }
    }

    if !reopened.is_empty() {
        let ids = reopened.join(" ");
        let fb = seam.detect_false_blockers(&ids)?;
        for line in fb.lines() {
            let Some(row) = parse_state(line) else { continue };
            if row.kind == "blocked-by-unlanded" {
                log.push(format!("NOTED {} — false blocker: {} {}", row.id, row.extra, row.evidence));
                if !dry_run {
                    let _ = bd.note(
                        &row.id,
                        &format!(
                            "groomer sweep: this bead was blocked-by-unlanded — {} {}. The blocker was reopened this pass; this bead should become ready once it lands.",
                            row.extra, row.evidence
                        ),
                    );
                }
                acted += 1;
            }
        }
    }

    // ---- pass 3: incident partition ----------------------------------------------------
    let inc = seam.detect_incident_needs_builder()?;
    for line in inc.lines() {
        let Some(row) = parse_state(line) else { continue };
        if row.kind == "incident-is-code" {
            log.push(format!("REROUTED {} — incident-is-code: {}", row.id, row.evidence));
            if !dry_run {
                let incident_label = seam.conf("SPIRA_INCIDENT_LABEL")?;
                let plan_label = seam.conf("SPIRA_PLAN_LABEL")?;
                if !incident_label.is_empty() {
                    let _ = bd.label_remove(&row.id, &incident_label);
                }
                if !plan_label.is_empty() {
                    let _ = bd.label_add(&row.id, &plan_label);
                }
                let _ = bd.note(&row.id, &format!("Moved by groomer sweep: {incident_label} -> {plan_label}. {}", row.evidence));
            }
            acted += 1;
        }
    }

    log.push(format!("sweep complete; acted on {acted} bead(s)"));
    Ok(Outcome { acted, log })
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use crate::bd::fake::FakeBd;
    use crate::seam::fake::FakeSeam;

    #[test]
    fn parses_a_livelock_line() {
        let row = parse_livelock("LIVELOCK sp-1 unmapped-repo — repo: label not in the repo-map").unwrap();
        assert_eq!(row.id, "sp-1");
        assert_eq!(row.category, "unmapped-repo");
        assert_eq!(row.reason, "repo: label not in the repo-map");
    }

    #[test]
    fn parse_livelock_ignores_a_blank_or_malformed_line() {
        assert!(parse_livelock("").is_none());
        assert!(parse_livelock("not a livelock line").is_none());
    }

    #[test]
    fn parses_a_simple_state_line() {
        let row = parse_state("STATE sp-1 landed-but-open — commit abc123 on main names it").unwrap();
        assert_eq!(row.id, "sp-1");
        assert_eq!(row.kind, "landed-but-open");
        assert_eq!(row.extra, "");
        assert_eq!(row.evidence, "commit abc123 on main names it");
    }

    #[test]
    fn parses_a_state_line_with_extra_tag() {
        let row = parse_state("STATE sp-2 closed-never-landed conflict — branch does not merge").unwrap();
        assert_eq!(row.kind, "closed-never-landed");
        assert_eq!(row.extra, "conflict");
        assert_eq!(row.evidence, "branch does not merge");
    }

    #[test]
    fn parses_a_blocked_by_unlanded_line_with_the_blocker_id_as_extra() {
        let row = parse_state("STATE sp-3 blocked-by-unlanded sp-2 — sp-2 was reopened this pass").unwrap();
        assert_eq!(row.kind, "blocked-by-unlanded");
        assert_eq!(row.extra, "sp-2");
        assert_eq!(row.evidence, "sp-2 was reopened this pass");
    }

    #[test]
    fn sweep_adds_overseer_label_for_ask_no_overseer() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        *seam.livelocked.borrow_mut() = "LIVELOCK sp-1 ask-no-overseer — no overseer label\n".into();
        let out = sweep(&bd, &seam, false).unwrap();
        assert!(bd.log().contains(&"label add sp-1 overseer".to_string()));
        assert_eq!(out.acted, 1);
    }

    #[test]
    fn sweep_dry_run_reports_without_acting() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        *seam.livelocked.borrow_mut() = "LIVELOCK sp-1 ask-no-overseer — no overseer label\n".into();
        let out = sweep(&bd, &seam, true).unwrap();
        assert!(bd.log().is_empty(), "dry-run must not call bd: {:?}", bd.log());
        assert_eq!(out.acted, 1, "dry-run still counts what it WOULD do");
    }

    #[test]
    fn sweep_closes_described_unmapped_repo_bead_as_litter_but_not_a_described_one() {
        let bd = FakeBd::new();
        bd.set_show("sp-lit", serde_json::json!([{"description": ""}]));
        bd.set_show("sp-desc", serde_json::json!([{"description": "has real content"}]));
        let seam = FakeSeam::new();
        *seam.livelocked.borrow_mut() = "LIVELOCK sp-lit unmapped-repo — repo: label unmapped\nLIVELOCK sp-desc unmapped-repo — repo: label unmapped\n".into();
        let out = sweep(&bd, &seam, false).unwrap();
        assert!(out.log.iter().any(|l| l.starts_with("CLOSED sp-lit")));
        assert!(out.log.iter().any(|l| l.starts_with("REPORT sp-desc")));
        assert!(bd.log().iter().any(|c| c.starts_with("close sp-lit")));
        assert!(!bd.log().iter().any(|c| c.starts_with("close sp-desc")));
    }

    #[test]
    fn sweep_reports_unclaimable_without_acting() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        *seam.livelocked.borrow_mut() = "LIVELOCK sp-unc unclaimable — no partition label\n".into();
        let out = sweep(&bd, &seam, false).unwrap();
        assert!(bd.log().is_empty());
        assert!(out.log.iter().any(|l| l.starts_with("REPORT sp-unc unclaimable")));
    }

    #[test]
    fn sweep_closes_landed_but_open() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        *seam.landed_but_open.borrow_mut() = "STATE sp-4 landed-but-open — commit names it\n".into();
        let out = sweep(&bd, &seam, false).unwrap();
        assert!(bd.log().iter().any(|c| c.starts_with("close sp-4 landed-but-open")));
        assert_eq!(out.acted, 1);
    }

    #[test]
    fn sweep_drops_closed_no_branch() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        *seam.closed_unlanded.borrow_mut() = "STATE sp-5 closed-no-branch — no branch ever recorded\n".into();
        sweep(&bd, &seam, false).unwrap();
        assert!(bd.log().contains(&"label add sp-5 spira-dropped".to_string()));
    }

    #[test]
    fn sweep_reopens_conflict_and_batch_ready_and_then_checks_false_blockers() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        *seam.closed_unlanded.borrow_mut() = "STATE sp-6 closed-never-landed conflict — does not merge\nSTATE sp-7 closed-never-landed batch-ready — merges clean\n".into();
        *seam.false_blockers.borrow_mut() = "STATE sp-8 blocked-by-unlanded sp-6 — was blocked\n".into();
        let out = sweep(&bd, &seam, false).unwrap();
        assert!(seam.log().iter().any(|c| c == "bead_reopen sp-6 closed-never-landed-conflict Reopened by groomer sweep: closed but its branch does not merge cleanly into the base — does not merge. Needs a rebase before it can land."));
        assert!(seam.log().iter().any(|c| c.starts_with("detect_false_blockers sp-6 sp-7")));
        assert!(bd.log().iter().any(|c| c.starts_with("note sp-8")));
        assert_eq!(out.acted, 3);
    }

    #[test]
    fn sweep_does_not_check_false_blockers_when_nothing_was_reopened() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        sweep(&bd, &seam, false).unwrap();
        assert!(!seam.log().iter().any(|c| c.starts_with("detect_false_blockers")));
    }

    #[test]
    fn sweep_reroutes_incident_is_code() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        seam.confs.borrow_mut().insert("SPIRA_INCIDENT_LABEL".into(), "ops-incident".into());
        seam.confs.borrow_mut().insert("SPIRA_PLAN_LABEL".into(), "plan".into());
        *seam.incident_needs_builder.borrow_mut() = "STATE sp-9 incident-is-code — branch already carries a commit\n".into();
        sweep(&bd, &seam, false).unwrap();
        assert!(bd.log().contains(&"label remove sp-9 ops-incident".to_string()));
        assert!(bd.log().contains(&"label add sp-9 plan".to_string()));
    }

    #[test]
    fn sweep_with_no_livelocks_reports_none() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        let out = sweep(&bd, &seam, false).unwrap();
        assert!(out.log.iter().any(|l| l == "no livelocked beads"));
        assert_eq!(out.acted, 0);
    }
}
