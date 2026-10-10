//! `rebase-waiting`: after a landing, rebase every bead still waiting for a round onto the
//! new landing ref, so a landing does not turn the queue into mechanical rework.

use std::collections::HashSet;

use super::{landing_log, repo_path, require_lc, resolve, World, FAIL, OK};
use crate::model::LcBeadRow;

const ACTOR: &str = "rebase-stale";
const CLAIM_LEASE_SECS: u64 = 600;

fn waiting(row: &LcBeadRow) -> bool {
    match row.state.as_str() {
        "SUBMITTED" | "CERTIFIED" => true,
        "REWORK" => row.reason.as_deref().is_some_and(|r| r.contains("no-rebase")),
        _ => false,
    }
}

fn event(w: &World, id: &str, state: &str, version: &str, kind: serde_json::Value) -> Result<String, String> {
    let next = version.trim().parse::<u64>().map_err(|_| format!("unreadable version {version:?}"))? + 1;
    w.lc.bead_event(id, state, version, ACTOR, &kind.to_string()).map_err(|(rc, e)| format!("{kind} refused (rc={rc}): {e}"))?;
    Ok(next.to_string())
}

/// Move the bead's recorded tip to `tip` in place; it keeps its queue position.
fn move_tip(w: &World, id: &str, tip: &str) -> Result<(), String> {
    let (mut state, mut version) = w.lc.bead_state(id).ok_or("no lifecycle row")?;
    if state == "REWORK" {
        let claim = serde_json::json!({"Claim": {"holder": ACTOR, "lease_until": w.clock.now() + CLAIM_LEASE_SECS}});
        version = event(w, id, &state, &version, claim)?;
        state = "WORKING".into();
    }
    if !matches!(state.as_str(), "WORKING" | "SUBMITTED" | "CERTIFIED") {
        return Err(format!("in state {state}, not waiting"));
    }
    event(w, id, &state, &version, serde_json::json!({"Submit": {"tip": tip}})).map(|_| ())
}

fn return_conflict(w: &World, id: &str, landref: &str, files: &str) -> Result<(), String> {
    if let Some(row) = w.lc.bead_row(id) {
        if matches!(row.state.as_str(), "SUBMITTED" | "CERTIFIED") {
            if let (Some(tip), Some((state, version))) = (row.tip, w.lc.bead_state(id)) {
                event(w, id, &state, &version, serde_json::json!({"GateRed": {"tip": tip, "reason": "no-rebase"}}))?;
            }
        }
    }
    w.lib.comment(id, &format!("rebase-conflict: does not rebase onto {landref}\npaths: {files}"));
    Ok(())
}

pub fn rebase_waiting(w: &World, repo: Option<&str>) -> i32 {
    let label = "rebase-waiting";
    let Ok(c) = resolve(w, label, repo) else { return FAIL };
    let Ok(path) = repo_path(w, label, &c) else { return FAIL };
    if require_lc(w, label).is_err() {
        return FAIL;
    }
    let rows = match w.lc.bead_rows(None) {
        Ok(r) => r,
        Err(e) => {
            w.err(format!("queue.sh {label}: cannot list the waiting beads: {e}"));
            return FAIL;
        }
    };
    let here: HashSet<String> = w.git.branches(&path, "spira/").into_iter().map(|(b, _)| b.trim_start_matches("spira/").to_string()).collect();
    let ids: Vec<String> = rows.iter().filter(|r| waiting(r) && here.contains(&r.bead_id)).map(|r| r.bead_id.clone()).collect();
    let landref = c.r.landref.clone().unwrap_or_default();
    let landed = w.git.rev_parse(&path, &landref).unwrap_or_else(|| landref.clone());
    for (id, out) in w.scripts.rebase_stale(&ids, &c.r.name) {
        let outcome = match out.rc {
            0 => match w.git.rev_parse(&path, &format!("refs/heads/spira/{id}")) {
                Some(tip) if w.lc.bead_row(&id).is_some_and(|r| r.tip.as_deref() == Some(tip.as_str()) && waiting(&r)) => "current".to_string(),
                Some(tip) => move_tip(w, &id, &tip).map_or_else(|e| format!("tip-not-moved: {e}"), |()| "rebased".into()),
                None => "tip-not-moved: the branch is gone".into(),
            },
            1 => {
                let files = out.err.rsplit_once("files: ").map_or("unknown", |(_, f)| f.trim());
                return_conflict(w, &id, &landed, files).map_or_else(|e| format!("conflict-unrecorded: {e}"), |()| format!("conflict files={files}"))
            }
            rc => format!("not-moved rc={rc}"),
        };
        landing_log(&c.s.run, &format!("QUEUE REBASE-WAITING {} repo={} id={id} outcome={}", w.clock.now(), c.r.name, outcome.replace('\n', " ")));
        w.out(format!("queue.sh {label}: {id}: {outcome}"));
    }
    OK
}
