//! `reachable_keys` — `SP_REACHABLE`/`SP_STRANDED`: a BFS over `blocks` edges from ready/
//! in_progress seeds, the same reachability query `spira/cockpit.sh` ran in an embedded
//! `python3 -c` (DESIGN.md "Design"). The BFS itself ([`reachable_bfs`]) is pure and unit
//! tested directly; only the data gathering (bd, the ctrl file, the chamber) is impure.

use super::{push, Kv};
use crate::io;
use serde_json::Value;
use spira_config::lc_state;
use std::collections::{HashMap, HashSet, VecDeque};

/// A label set that, if fully contained in a bead's labels, marks it a "stopper" — never
/// reachable, never a seed. Suspended-fayth sets and the "no live partition matches" test
/// are both expressed this way.
pub type LabelSet = HashSet<String>;

pub struct Bead {
    pub id: String,
    /// The lifecycle row's state (READY/REWORK/WORKING for every bead in the universe):
    /// bd holds content, spira-lc holds state (design §3.4, sp-mve9i).
    pub state: String,
    /// The lifecycle row's holds; a poison or ask hold stops a bead like the old labels did.
    pub holds: Vec<String>,
    pub labels: HashSet<String>,
    /// ids this bead is blocked BY (open `blocks` dependencies pointing at it).
    pub blocked_by: HashSet<String>,
}

fn is_stopper(b: &Bead, ask: &str, suspended: &[LabelSet], live: &[LabelSet]) -> bool {
    let labels = &b.labels;
    if labels.contains(ask) || labels.contains("spira-poison") || b.holds.iter().any(|h| h == "poison" || h == "ask") {
        return true;
    }
    if suspended.iter().any(|s| s.iter().all(|l| labels.contains(l))) {
        return true;
    }
    if !live.is_empty() && !live.iter().any(|s| s.iter().all(|l| labels.contains(l))) {
        return true;
    }
    false
}

/// The BFS itself: seeds are WORKING beads, or claimable (READY/REWORK) beads with no open blocker, both
/// excluding stoppers. A downstream bead becomes reachable once every one of its open
/// blockers already is. Returns `(reachable_count, total_work_count)`.
pub fn reachable_bfs(beads: &[Bead], ask: &str, scope: &str, suspended: &[LabelSet], live: &[LabelSet]) -> (usize, usize) {
    let by_id: HashMap<String, &Bead> = beads.iter().map(|b| (b.id.clone(), b)).collect();
    let all_ids: HashSet<String> = beads
        .iter()
        .filter(|b| !b.labels.contains("insight") && !b.labels.contains(ask))
        .filter(|b| scope.is_empty() || b.labels.contains(scope))
        .map(|b| b.id.clone())
        .collect();

    let mut blocker_of: HashMap<String, HashSet<String>> = all_ids.iter().map(|id| (id.clone(), HashSet::new())).collect();
    let mut blocks: HashMap<String, HashSet<String>> = all_ids.iter().map(|id| (id.clone(), HashSet::new())).collect();
    for b in beads {
        if !all_ids.contains(&b.id) {
            continue;
        }
        for up in &b.blocked_by {
            if by_id.contains_key(up) {
                blocker_of.get_mut(&b.id).unwrap().insert(up.clone());
                if all_ids.contains(up) {
                    blocks.entry(up.clone()).or_default().insert(b.id.clone());
                }
            }
        }
    }

    let mut seeds: HashSet<String> = HashSet::new();
    for b in beads {
        if !all_ids.contains(&b.id) {
            continue;
        }
        if is_stopper(b, ask, suspended, live) {
            continue;
        }
        let blockers_empty = blocker_of.get(&b.id).map(|s| s.is_empty()).unwrap_or(true);
        if lc_state::is_working(&b.state) || (lc_state::is_claimable(&b.state) && blockers_empty) {
            seeds.insert(b.id.clone());
        }
    }

    let mut reachable: HashSet<String> = seeds.clone();
    let mut q: VecDeque<String> = seeds.into_iter().collect();
    while let Some(cur) = q.pop_front() {
        for dn in blocks.get(&cur).cloned().unwrap_or_default() {
            if reachable.contains(&dn) {
                continue;
            }
            let dn_bead = by_id[&dn];
            if is_stopper(dn_bead, ask, suspended, live) {
                continue;
            }
            let all_blockers_reachable = blocker_of.get(&dn).map(|s| s.iter().all(|b| reachable.contains(b))).unwrap_or(true);
            if all_blockers_reachable {
                reachable.insert(dn.clone());
                q.push_back(dn);
            }
        }
    }
    (reachable.len(), all_ids.len())
}

pub fn reachable_keys() -> Kv {
    let mut out = Kv::new();
    let scope = std::env::var("SPIRA_SCOPE_LABEL").unwrap_or_default();
    // The universe is every work bead the machine has as claimable or WORKING (what bd's
    // `open,in_progress` meant); bd supplies only its labels and edges.
    let mut args = vec!["list", "--all", "--limit", "0"];
    if !scope.is_empty() {
        args.push("--label");
        args.push(&scope);
    }
    let raw = io::bdjson(&args);
    let Some((rows, lc)) = io::bd_rows(raw).zip(super::lc::state_index()) else {
        push(&mut out, "SP_REACHABLE", "?");
        push(&mut out, "SP_STRANDED", "?");
        return out;
    };

    let home = io::home_dir();
    let ask = spira_config::resolve::key_for_process("SPIRA_ASK_LABEL").unwrap_or_default(); // the configured ask label; never a literal fallback (literal-lint ask_fallback)

    let mut live: Vec<LabelSet> = Vec::new();
    if let Some(fayths) = io::lib_call(&home, "spira_fayths", &[]) {
        for f in fayths.split_whitespace() {
            if let Some(labels) = io::lib_call(&home, "fayth_get", &[f, "FAYTH_LABELS"]) {
                let set: LabelSet = labels.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                if !set.is_empty() {
                    live.push(set);
                }
            }
        }
    }

    let mut suspended: Vec<LabelSet> = Vec::new();
    let ctrl_path = std::env::var("SPIRA_CTRL").unwrap_or_default();
    if !ctrl_path.is_empty() {
        if let Ok(content) = std::fs::read_to_string(&ctrl_path) {
            if let Ok(Value::Object(ctrl)) = serde_json::from_str::<Value>(&content) {
                for (subj, ops) in &ctrl {
                    let has_suspend = ops.as_array().map(|a| a.iter().any(|v| v.as_str() == Some("suspend"))).unwrap_or(false)
                        || ops.as_object().map(|o| o.contains_key("suspend")).unwrap_or(false);
                    if !has_suspend {
                        continue;
                    }
                    let fayth_file = home.join("chamber").join(format!("{subj}.fayth"));
                    if let Ok(content) = std::fs::read_to_string(&fayth_file) {
                        if let Some(line) = content.lines().find(|l| l.starts_with("FAYTH_LABELS=")) {
                            let v = line["FAYTH_LABELS=".len()..].trim();
                            let set: LabelSet = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                            if !set.is_empty() {
                                suspended.push(set);
                            }
                        }
                    }
                }
            }
        }
    }

    let beads = beads_of(&rows, &lc);
    let (reach, total) = reachable_bfs(&beads, &ask, &scope, &suspended, &live);
    push(&mut out, "SP_REACHABLE", reach.to_string());
    push(&mut out, "SP_STRANDED", (total - reach).to_string());
    out
}

/// bd rows joined to their lifecycle rows: a bead enters the universe only while the machine
/// has it claimable or WORKING. A bead with no lifecycle row is not in it — it can never be
/// claimed, so it is neither reachable nor stranded work (CHECK-ROWLESS reports it).
pub fn beads_of(rows: &[Value], lc: &HashMap<String, lc_state::Row>) -> Vec<Bead> {
    rows
        .iter()
        .filter_map(|b| {
            let id = b.get("id").and_then(Value::as_str)?.to_string();
            let row = lc.get(&id).filter(|r| r.claimable() || r.working())?;
            let (state, holds) = (row.state.clone(), row.holds.clone());
            let labels: HashSet<String> = b.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
            let blocked_by: HashSet<String> = b
                .get("dependencies")
                .and_then(Value::as_array)
                .map(|deps| {
                    deps.iter()
                        .filter(|d| {
                            let t = d.get("dependency_type").or_else(|| d.get("type")).and_then(Value::as_str);
                            t == Some("blocks")
                        })
                        .filter_map(|d| d.get("depends_on_id").and_then(Value::as_str).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(Bead { id, state, holds, labels, blocked_by })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bead(id: &str, state: &str, labels: &[&str], blocked_by: &[&str]) -> Bead {
        let state = match state {
            "open" => "READY",
            "in_progress" => "WORKING",
            other => other,
        };
        Bead {
            id: id.to_string(),
            state: state.to_string(),
            holds: Vec::new(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            blocked_by: blocked_by.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn ready_open_bead_with_no_blockers_is_reachable() {
        let beads = vec![bead("sp-1", "open", &["plan"], &[])];
        let (reach, total) = reachable_bfs(&beads, "needs-ryan", "plan", &[], &[]); // literal-ok: fixture/fallback
        assert_eq!((reach, total), (1, 1));
    }

    #[test]
    fn bead_behind_an_unresolved_blocker_is_stranded_until_it_resolves() {
        let beads = vec![
            bead("sp-1", "open", &["plan"], &[]),
            bead("sp-2", "open", &["plan"], &["sp-1"]),
        ];
        let (reach, total) = reachable_bfs(&beads, "needs-ryan", "plan", &[], &[]); // literal-ok: fixture/fallback
        // sp-1 is a seed and reachable; sp-2's only blocker (sp-1) is reachable, so sp-2
        // becomes reachable too via the BFS propagation.
        assert_eq!((reach, total), (2, 2));
    }

    #[test]
    fn ask_labelled_bead_is_excluded_from_the_universe_entirely() {
        let beads = vec![bead("sp-1", "open", &["plan", "needs-ryan"], &[])]; // literal-ok: fixture/fallback
        let (reach, total) = reachable_bfs(&beads, "needs-ryan", "plan", &[], &[]); // literal-ok: fixture/fallback
        assert_eq!((reach, total), (0, 0));
    }

    #[test]
    fn poisoned_bead_counts_as_stuck_work_not_reachable() {
        let beads = vec![bead("sp-1", "open", &["plan", "spira-poison"], &[])];
        let (reach, total) = reachable_bfs(&beads, "needs-ryan", "plan", &[], &[]); // literal-ok: fixture/fallback
        assert_eq!((reach, total), (0, 1));
    }

    #[test]
    fn bead_matching_no_live_partition_is_stranded() {
        let live = vec![["fayth:builder".to_string()].into_iter().collect::<LabelSet>()];
        let beads = vec![bead("sp-1", "open", &["plan"], &[])];
        let (reach, total) = reachable_bfs(&beads, "needs-ryan", "plan", &[], &live); // literal-ok: fixture/fallback
        assert_eq!((reach, total), (0, 1));
    }

    #[test]
    fn scope_label_is_required_to_enter_the_universe() {
        let beads = vec![bead("sp-1", "open", &["other-scope"], &[])];
        let (reach, total) = reachable_bfs(&beads, "needs-ryan", "plan", &[], &[]); // literal-ok: fixture/fallback
        assert_eq!((reach, total), (0, 0));
    }

    #[test]
    fn in_progress_bead_is_always_a_seed_even_with_open_blockers() {
        let beads = vec![
            bead("sp-1", "open", &["plan", "needs-ryan"], &[]), // ask blocker, excluded from universe // literal-ok: fixture/fallback
            bead("sp-2", "in_progress", &["plan"], &["sp-1"]),
        ];
        let (reach, _total) = reachable_bfs(&beads, "needs-ryan", "plan", &[], &[]); // literal-ok: fixture/fallback
        assert!(reach >= 1);
    }

    /// sp-mve9i: the universe and the seeds are the lifecycle row's, not bd's status: a bd
    /// "closed" bead the machine sent back to REWORK is reachable work, a bd "open" bead the
    /// machine has LANDED is not work at all, and a poison hold stops a bead.
    #[test]
    fn the_universe_and_seeds_come_from_the_lifecycle_rows() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"a","status":"closed","labels":["plan"]},
                {"id":"b","status":"open","labels":["plan"]},
                {"id":"c","status":"open","labels":["plan"]},
                {"id":"d","status":"open","labels":["plan"]}]"#,
        )
        .unwrap();
        let lc: HashMap<String, lc_state::Row> = [("a", "REWORK", ""), ("b", "LANDED", ""), ("c", "READY", "poison")]
            .iter()
            .map(|(id, st, h)| {
                let holds = if h.is_empty() { vec![] } else { vec![h.to_string()] };
                (id.to_string(), lc_state::Row { bead_id: id.to_string(), state: st.to_string(), holds, ..Default::default() })
            })
            .collect();
        let beads = beads_of(&rows, &lc);
        let ids: Vec<&str> = beads.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"]);
        assert_eq!(reachable_bfs(&beads, "needs-ryan", "plan", &[], &[]), (1, 2)); // literal-ok: fixture/fallback
    }

    #[test]
    fn suspended_partition_stops_a_bead_even_when_seeded() {
        let suspended = vec![["fayth:ops".to_string()].into_iter().collect::<LabelSet>()];
        let beads = vec![bead("sp-1", "open", &["plan", "fayth:ops"], &[])];
        let (reach, total) = reachable_bfs(&beads, "needs-ryan", "plan", &suspended, &[]); // literal-ok: fixture/fallback
        assert_eq!((reach, total), (0, 1));
    }
}
