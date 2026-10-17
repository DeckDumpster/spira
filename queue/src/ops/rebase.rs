//! `rebase-waiting`: after a landing, rebase every bead still waiting for a round onto the
//! new landing ref, so a landing does not turn the queue into mechanical rework.

use std::collections::{BTreeMap, BTreeSet, HashSet};

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
    let queued: HashSet<&str> = rows.iter().filter(|r| waiting(r) && here.contains(&r.bead_id)).map(|r| r.bead_id.as_str()).collect();
    let on_waiting_base = |r: &LcBeadRow| r.stack.keys().any(|p| queued.contains(p.as_str()));
    let ids: Vec<String> = rows.iter().filter(|r| queued.contains(r.bead_id.as_str()) && !on_waiting_base(r)).map(|r| r.bead_id.clone()).collect();
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
    stack_waiting(w, label, &path, &c, &landed);
    OK
}

fn branch_ref(id: &str) -> String {
    format!("refs/heads/spira/{id}")
}

fn shared_changes(w: &World, path: &std::path::Path, landref: &str, tip: &str, shared: &[String]) -> BTreeSet<String> {
    let Some(base) = w.git.merge_base(path, landref, tip) else { return BTreeSet::new() };
    let Ok(stat) = w.git.numstat(path, &base, tip) else { return BTreeSet::new() };
    stat.into_iter().map(|(p, _, _)| p).filter(|p| shared.iter().any(|s| if s.ends_with('/') { p.starts_with(s.as_str()) } else { p == s })).collect()
}

/// Cut `id` onto `onto` in place and record the stack edge with one Restack: the branch moves only
/// once the machine has taken the event, and moves back if it refuses.
#[allow(clippy::too_many_arguments)]
fn restack(w: &World, c: &super::Ctx, path: &std::path::Path, id: &str, tip: &str, upstream: &str, base: (&str, &str), depth: u32) -> Result<String, String> {
    let scratch = c.s.run.join(format!("restack-{id}"));
    let new = w.git.rebase_onto(path, &scratch, tip, upstream, base.1, &c.s.git_name, &c.s.git_email).map_err(|files| format!("conflict files={}", files.join(",")))?;
    let (state, version) = w.lc.bead_state(id).ok_or("no lifecycle row")?;
    let reference = branch_ref(id);
    if !w.git.update_ref(path, &reference, &new, Some(tip)) {
        return Err("branch moved underneath".into());
    }
    let kind = serde_json::json!({"Restack": {"tip": new, "stack": {base.0: base.1}, "stack_depth": depth}});
    if let Err(e) = event(w, id, &state, &version, kind) {
        w.git.update_ref(path, &reference, tip, Some(&new));
        return Err(e);
    }
    Ok(new)
}

/// Beads waiting for a round that change the same shared files are stacked, the later on the
/// earlier, instead of left to turn each landing into a does-not-merge mend for the rest. A bead
/// already stacked on a waiting prerequisite is re-cut when that prerequisite's tip moved.
fn stack_waiting(w: &World, label: &str, path: &std::path::Path, c: &super::Ctx, landref: &str) {
    let shared: Vec<String> = match w.env.config("SPIRA_STACK_SHARED_FILES") {
        Ok(v) => v.split_whitespace().map(String::from).collect(),
        Err(e) => return w.err(format!("queue.sh {label}: auto-stack off: {e}")),
    };
    if shared.is_empty() {
        return;
    }
    let max_depth = match w.env.config("SPIRA_STACK_MAX_DEPTH").map(|v| v.trim().parse::<u32>()) {
        Ok(Ok(n)) => n,
        _ => return w.err(format!("queue.sh {label}: auto-stack off: SPIRA_STACK_MAX_DEPTH unreadable")),
    };
    let Ok(rows) = w.lc.bead_rows(None) else { return };
    let here: HashSet<String> = w.git.branches(path, "spira/").into_iter().map(|(b, _)| b.trim_start_matches("spira/").to_string()).collect();
    let mut queue: Vec<&LcBeadRow> = rows.iter().filter(|r| matches!(r.state.as_str(), "SUBMITTED" | "CERTIFIED") && here.contains(&r.bead_id)).collect();
    queue.sort_by(|a, b| (a.since, &a.bead_id).cmp(&(b.since, &b.bead_id)));
    let ids: HashSet<&str> = queue.iter().map(|r| r.bead_id.as_str()).collect();
    let mut placed: Vec<(String, BTreeSet<String>)> = Vec::new();
    let mut depth: BTreeMap<String, u32> = BTreeMap::new();
    let mut pending: Vec<&LcBeadRow> = queue.clone();
    let say = |id: &str, outcome: String| {
        landing_log(&c.s.run, &format!("QUEUE STACK-WAITING {} repo={} id={id} outcome={}", w.clock.now(), c.r.name, outcome.replace('\n', " ")));
        w.out(format!("queue.sh {label}: {id}: {outcome}"));
    };
    while !pending.is_empty() {
        let before = pending.len();
        let mut next = Vec::new();
        for row in pending {
            let waiting_bases: Vec<(&String, &String)> = row.stack.iter().filter(|(p, _)| ids.contains(p.as_str())).collect();
            if waiting_bases.iter().any(|(p, _)| !depth.contains_key(p.as_str())) {
                next.push(row);
                continue;
            }
            let id = row.bead_id.as_str();
            let Some(tip) = row.tip.as_deref().filter(|t| w.git.rev_parse(path, &branch_ref(id)).as_deref() == Some(*t)) else { continue };
            let own = shared_changes(w, path, landref, tip, &shared);
            if let Some((prereq, recorded)) = waiting_bases.first() {
                let d = depth[prereq.as_str()] + 1;
                depth.insert(id.to_string(), d);
                placed.push((id.to_string(), own));
                if let Some(now) = w.git.rev_parse(path, &branch_ref(prereq)).filter(|n| n != *recorded) {
                    let outcome = restack(w, c, path, id, tip, recorded, (prereq, &now), d).map_or_else(|e| format!("recut-failed: {e}"), |_| format!("recut onto {prereq}"));
                    say(id, outcome);
                }
                continue;
            }
            let base = placed.iter().rev().find(|(_, f)| !f.is_disjoint(&own)).map(|(p, _)| p.clone());
            depth.insert(id.to_string(), 0);
            if let Some(prereq) = base {
                let d = depth[&prereq] + 1;
                let outcome = match (d <= max_depth, w.git.rev_parse(path, &branch_ref(&prereq)), w.git.merge_base(path, landref, tip)) {
                    (false, ..) => format!("not-stacked: depth {d} over SPIRA_STACK_MAX_DEPTH {max_depth}"),
                    (_, Some(on), Some(mb)) => match restack(w, c, path, id, tip, &mb, (&prereq, &on), d) {
                        Ok(_) => {
                            depth.insert(id.to_string(), d);
                            format!("stacked on {prereq}")
                        }
                        Err(e) => format!("not-stacked: {e}"),
                    },
                    _ => "not-stacked: no base to cut onto".to_string(),
                };
                say(id, outcome);
            }
            placed.push((id.to_string(), own));
        }
        if next.len() == before {
            break;
        }
        pending = next;
    }
}
