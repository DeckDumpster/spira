//! `round_keys` — the round in flight, read only from the lifecycle batch machine
//! (`spira-lc list --batches`) and the bead rows. `?` is CANNOT TELL, never an idle pane.

use super::{push, Kv};
use crate::quoting::sanitize;
use serde_json::Value;
use std::fs;
use std::path::Path;
use testenv::phase::RunLog;
use std::process::{Command, Stdio};

const MAX_ROWS: usize = 12;
const MAX_ROUNDS: usize = 4;

pub fn round_keys(cap_secs: i64, queue_dir: &str, run: &str) -> Kv {
    let batches = list_batches().and_then(|raw| parse_batches(&raw));
    let pool = spira_config::lc_state::list().ok().map(|rows| pool_waiting(&rows));
    let mut kv = round_keys_from(batches.as_deref(), pool, cap_secs, Path::new(queue_dir));
    if let Some(batches) = batches.as_deref() {
        progress_keys(&mut kv, batches, Path::new(run));
    }
    kv
}

/// The verdict's last look at the forge for `batch`, read from the file it leaves beside the
/// queue's `open` record; a record naming another batch is stale and ignored.
fn forge_state(queue_dir: &Path, repo: &str, batch: &str) -> Option<Vec<(String, String)>> {
    let text = std::fs::read_to_string(queue_dir.join(repo).join("forge-state")).ok()?;
    let kv: Vec<(String, String)> = text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let get = |k: &str| kv.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    (get("batch_id") == Some(batch)).then_some(kv)
}

pub struct Progress {
    pub done: usize,
    pub total: Option<usize>,
    pub reds: Vec<String>,
}

/// A certifying round's suites so far, read from the run's recorded events (`run.events`) and
/// the suite count the batcher wrote beside the corpus. A missing dir is 0 done, not unknown.
pub fn progress(run: &Path, repo: &str, round: &str) -> Option<Progress> {
    let root = run.join("batch-results").join(format!("{repo}-{round}"));
    let total = fs::read_to_string(root.join("total")).ok().and_then(|t| t.trim().parse().ok());
    let suites = RunLog::in_dir(&root.join("corpus")).load().ok()?.suites();
    let reds = suites.iter().filter(|(_, s)| matches!(s.as_str(), "red" | "timeout" | "fault")).map(|(n, _)| n.clone()).collect();
    Some(Progress { done: suites.len(), total, reds })
}

fn progress_keys(kv: &mut Kv, batches: &[Batch], run: &Path) {
    let live = children_after_parents(batches.iter().filter(|b| !terminal(&b.state) && b.state != "DISCARDED").collect());
    for (r, b) in live.iter().take(MAX_ROUNDS).enumerate() {
        if phase(&b.state) != "certifying" {
            continue;
        }
        let k = format!("SP_ROUNDS{r}");
        let Some(p) = progress(run, &b.repo, &b.id) else { continue };
        push(kv, &format!("{k}_SUITES_DONE"), p.done.to_string());
        push(kv, &format!("{k}_SUITES_TOTAL"), p.total.map_or("?".to_string(), |t| t.to_string()));
        push(kv, &format!("{k}_REDS_N"), p.reds.len().to_string());
        for (i, n) in p.reds.iter().take(MAX_ROWS).enumerate() {
            push(kv, &format!("{k}_RED{i}"), sanitize(n));
        }
    }
}

fn list_batches() -> Option<String> {
    let bin = spira_config::lifecycle_row::lc_bin();
    let o = Command::new("timeout").arg("5").arg(bin).args(["list", "--batches"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

pub struct Batch {
    pub id: String,
    pub repo: String,
    pub state: String,
    pub reason: String,
    pub parent: String,
    pub opened_at: i64,
    pub last_at: Option<i64>,
    pub members: Vec<(String, String)>,
    pub ejected: Vec<(String, String)>,
}

fn int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn pairs(v: &Value, k: &str, a: &str, b: &str) -> Vec<(String, String)> {
    v.get(k).and_then(Value::as_array).map(|rows| rows.iter().map(|r| (text(r, a), text(r, b))).collect()).unwrap_or_default()
}

pub fn parse_batches(raw: &str) -> Option<Vec<Batch>> {
    let Value::Array(rows) = serde_json::from_str::<Value>(raw).ok()? else { return None };
    Some(
        rows.iter()
            .map(|r| Batch {
                id: text(r, "batch_id"),
                repo: text(r, "repo"),
                state: text(r, "state"),
                reason: text(r, "reason"),
                parent: text(r, "parent"),
                opened_at: int(r.get("opened_at")).unwrap_or(0),
                last_at: int(r.get("last_at")),
                members: pairs(r, "members", "bead_id", "outcome"),
                ejected: pairs(r, "ejected", "bead_id", "reason"),
            })
            .collect(),
    )
}

pub fn pool_waiting(rows: &[spira_config::lc_state::Row]) -> usize {
    rows.iter().filter(|r| matches!(r.state.as_str(), "SUBMITTED" | "CERTIFIED") && r.holds.is_empty()).count()
}

fn terminal(state: &str) -> bool {
    matches!(state, "LANDED" | "SETTLED" | "ABANDONED")
}

pub fn phase(state: &str) -> &'static str {
    match state {
        "STAGED" => "staged",
        "OPEN" => "merging",
        "CI_RUNNING" => "certifying",
        "GREEN" => "landing",
        "ATTRIBUTING" => "attributing",
        "REBUILDING" => "rebuilding",
        _ => "?",
    }
}

fn children_after_parents(live: Vec<&Batch>) -> Vec<&Batch> {
    let is_child = |b: &Batch| b.state == "STAGED" && live.iter().any(|p| p.id == b.parent);
    let mut out = Vec::new();
    for b in live.iter().filter(|b| !is_child(b)) {
        out.push(*b);
        out.extend(live.iter().filter(|c| is_child(c) && c.parent == b.id));
    }
    out
}

fn member_state(outcome: &str) -> &str {
    if outcome.is_empty() { "in" } else { outcome }
}

pub fn round_keys_from(batches: Option<&[Batch]>, pool: Option<usize>, cap_secs: i64, queue_dir: &Path) -> Kv {
    let mut out = Kv::new();
    push(&mut out, "SP_ROUND_POOL", pool.map_or("?".to_string(), |n| n.to_string()));
    push(&mut out, "SP_ROUND_CAP", cap_secs.to_string());
    let Some(batches) = batches else {
        push(&mut out, "SP_ROUND_STATE", "?");
        return out;
    };
    let live = children_after_parents(batches.iter().filter(|b| !terminal(&b.state) && b.state != "DISCARDED").collect());
    if !live.is_empty() {
        push(&mut out, "SP_ROUND_STATE", "open");
        push(&mut out, "SP_ROUNDS_N", live.len().to_string());
        for (r, b) in live.iter().take(MAX_ROUNDS).enumerate() {
            let k = format!("SP_ROUNDS{r}");
            push(&mut out, &format!("{k}_NAME"), sanitize(&b.id));
            push(&mut out, &format!("{k}_PHASE"), phase(&b.state));
            if b.state == "STAGED" && live.iter().any(|p| p.id == b.parent) {
                push(&mut out, &format!("{k}_PARENT"), sanitize(&b.parent));
            }
            push(&mut out, &format!("{k}_OPENED"), b.opened_at.to_string());
            let ejected: Vec<&str> = b.ejected.iter().map(|(id, _)| id.as_str()).collect();
            let members: Vec<_> = b.members.iter().filter(|(id, _)| !ejected.contains(&id.as_str())).collect();
            push(&mut out, &format!("{k}_N"), members.len().to_string());
            for (i, (id, outcome)) in members.iter().take(MAX_ROWS).enumerate() {
                push(&mut out, &format!("{k}_MEMBER{i}"), format!("{}|{}", sanitize(id), sanitize(member_state(outcome))));
            }
            let mut progress = b.last_at.unwrap_or(b.opened_at);
            let mut stall = if members.is_empty() { "no members" } else { "" };
            if let Some(f) = forge_state(queue_dir, &b.repo, &b.id) {
                let get = |key: &str| f.iter().rev().find(|(n, _)| n == key).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty());
                for (key, name) in [("pr", "PR"), ("pr_state", "PRSTATE"), ("ci", "CI"), ("step", "CI_STEP"), ("run_started", "CI_START"), ("wall", "WALL")] {
                    if let Some(v) = get(key) {
                        push(&mut out, &format!("{k}_{name}"), sanitize(v));
                    }
                }
                let seen: Option<i64> = [get("last_activity"), get("run_started")].into_iter().flatten().filter_map(|v| v.parse().ok()).max();
                progress = progress.max(seen.unwrap_or(0));
                if matches!(get("pr_state"), Some("closed" | "merged")) && stall.is_empty() {
                    stall = if get("pr_state") == Some("merged") { "PR merged but batch not landed" } else { "PR closed but batch live" };
                }
            }
            push(&mut out, &format!("{k}_PROGRESS"), progress.to_string());
            if !stall.is_empty() {
                push(&mut out, &format!("{k}_STALL"), stall);
            }
            push(&mut out, &format!("{k}_EJECT_N"), b.ejected.len().to_string());
            for (i, (id, why)) in b.ejected.iter().take(MAX_ROWS).enumerate() {
                push(&mut out, &format!("{k}_EJECT{i}"), format!("{}|{}", sanitize(id), sanitize(why)));
            }
        }
        return out;
    }
    push(&mut out, "SP_ROUND_STATE", "idle");
    if let Some(b) = batches.first() {
        let green = b.state != "ABANDONED";
        push(&mut out, "SP_ROUND_LAST_NAME", sanitize(&b.id));
        push(&mut out, "SP_ROUND_LAST_VERDICT", if green { "GREEN" } else { "RED" });
        push(&mut out, "SP_ROUND_LAST_AT", b.last_at.unwrap_or(b.opened_at).to_string());
        let why = if green { String::new() } else { b.reason.clone() };
        push(&mut out, "SP_ROUND_LAST_WHY", sanitize(&why));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forge_dir(tag: &str, state: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new(tag);
        std::fs::create_dir_all(d.path().join("deckdumpster")).unwrap();
        std::fs::write(d.path().join("deckdumpster/forge-state"), state).unwrap();
        d
    }

    fn forge_keys(tag: &str, members: &str, state: &str) -> Kv {
        let raw = format!(r#"[{{"batch_id":"dd-1","repo":"deckdumpster","state":"CI_RUNNING","opened_at":1000,"last_at":1010,"members":[{members}],"ejected":[]}}]"#);
        round_keys_from(Some(&parse_batches(&raw).unwrap()), Some(0), 900, forge_dir(tag, state).path())
    }

    const M: &str = r#"{"bead_id":"dd-a","outcome":null}"#;

    #[test]
    fn a_running_forge_batch_carries_its_pr_run_step_and_progress() {
        let kv = forge_keys("run", M, "batch_id=dd-1\npr=422\npr_state=open\nci=pending\nrun=7\nrun_started=1100\nlast_activity=1300\nstep=unit / pytest\nwall=3600\n");
        for (k, v) in [("PR", "422"), ("PRSTATE", "open"), ("CI", "pending"), ("CI_STEP", "unit / pytest"), ("CI_START", "1100"), ("PROGRESS", "1300"), ("WALL", "3600")] {
            assert_eq!(get(&kv, &format!("SP_ROUNDS0_{k}")), Some(v), "{k}");
        }
        assert_eq!(get(&kv, "SP_ROUNDS0_STALL"), None);
    }

    #[test]
    fn green_awaiting_merge_is_not_stalled() {
        let kv = forge_keys("green", M, "batch_id=dd-1\npr=422\npr_state=open\nci=green\n");
        assert_eq!(get(&kv, "SP_ROUNDS0_CI"), Some("green"));
        assert_eq!(get(&kv, "SP_ROUNDS0_STALL"), None);
    }

    #[test]
    fn a_merged_or_closed_pr_under_a_live_batch_is_stalled() {
        let merged = forge_keys("merged", M, "batch_id=dd-1\npr=207\npr_state=merged\nci=green\n");
        assert_eq!(get(&merged, "SP_ROUNDS0_STALL"), Some("PR merged but batch not landed"));
        let closed = forge_keys("closed", M, "batch_id=dd-1\npr=422\npr_state=closed\nci=pending\n");
        assert_eq!(get(&closed, "SP_ROUNDS0_STALL"), Some("PR closed but batch live"));
    }

    #[test]
    fn an_empty_open_batch_is_stalled_and_a_stale_forge_record_is_ignored() {
        let kv = forge_keys("empty", "", "batch_id=dd-1\npr=422\npr_state=open\nci=pending\n");
        assert_eq!(get(&kv, "SP_ROUNDS0_STALL"), Some("no members"));
        let other = forge_keys("stale", M, "batch_id=dd-0\npr=1\npr_state=closed\nci=red\n");
        assert_eq!(get(&other, "SP_ROUNDS0_PR"), None);
        assert_eq!(get(&other, "SP_ROUNDS0_STALL"), None);
    }

    fn get<'a>(kv: &'a Kv, k: &str) -> Option<&'a str> {
        kv.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }

    const RAW: &str = r#"[
      {"batch_id":"r-9","repo":"spira","state":"CI_RUNNING","reason":null,"opened_at":"1000","last_at":1100,
       "members":[{"batch_id":"r-9","bead_id":"sp-a","outcome":null},{"batch_id":"r-9","bead_id":"sp-b","outcome":null},{"batch_id":"r-9","bead_id":"sp-c","outcome":null}],
       "ejected":[{"batch_id":"r-9","bead_id":"sp-c","reason":"red: test-x.sh"}]},
      {"batch_id":"r-8","repo":"spira","state":"LANDED","reason":null,"opened_at":500,"last_at":700,"members":[],"ejected":[]}]"#;

    #[test]
    fn an_open_batch_reports_phase_opened_members_and_ejections() {
        let b = parse_batches(RAW).unwrap();
        let kv = round_keys_from(Some(&b), Some(4), 900, Path::new("/nonexistent"));
        assert_eq!(get(&kv, "SP_ROUND_STATE"), Some("open"));
        assert_eq!(get(&kv, "SP_ROUNDS_N"), Some("1"));
        assert_eq!(get(&kv, "SP_ROUNDS0_NAME"), Some("r-9"));
        assert_eq!(get(&kv, "SP_ROUNDS0_PHASE"), Some("certifying"));
        assert_eq!(get(&kv, "SP_ROUNDS0_OPENED"), Some("1000"));
        assert_eq!(get(&kv, "SP_ROUND_CAP"), Some("900"));
        assert_eq!(get(&kv, "SP_ROUNDS0_N"), Some("2"));
        assert_eq!(get(&kv, "SP_ROUNDS0_MEMBER1"), Some("sp-b|in"));
        assert_eq!(get(&kv, "SP_ROUNDS0_MEMBER2"), None);
        assert_eq!(get(&kv, "SP_ROUNDS0_EJECT0"), Some("sp-c|red: test-x.sh"));
        assert_eq!(get(&kv, "SP_ROUND_POOL"), Some("4"));
    }

    #[test]
    fn two_live_rounds_are_both_reported_and_a_landed_one_is_not() {
        let raw = r#"[
          {"batch_id":"r-11","state":"STAGED","opened_at":2000,"members":[{"bead_id":"sp-z","outcome":"skipped: unlanded dependency"}],"ejected":[]},
          {"batch_id":"r-10","state":"CI_RUNNING","opened_at":1000,"members":[{"bead_id":"sp-y","outcome":null}],"ejected":[]},
          {"batch_id":"r-9","state":"LANDED","opened_at":500,"members":[{"bead_id":"sp-x","outcome":null}],"ejected":[]}]"#;
        let kv = round_keys_from(Some(&parse_batches(raw).unwrap()), Some(0), 900, Path::new("/nonexistent"));
        assert_eq!(get(&kv, "SP_ROUNDS_N"), Some("2"));
        assert_eq!(get(&kv, "SP_ROUNDS0_PHASE"), Some("staged"));
        assert_eq!(get(&kv, "SP_ROUNDS0_MEMBER0"), Some("sp-z|skipped: unlanded dependency"));
        assert_eq!(get(&kv, "SP_ROUNDS1_NAME"), Some("r-10"));
        assert!(!kv.iter().any(|(_, v)| v == "r-9"));
    }

    #[test]
    fn a_staged_round_follows_the_round_it_waits_behind_and_a_discarded_one_is_dropped() {
        let raw = r#"[
          {"batch_id":"r-12","state":"STAGED","parent":"r-10","opened_at":2000,"members":[{"bead_id":"sp-z","outcome":null}],"ejected":[]},
          {"batch_id":"r-11","state":"OPEN","parent":null,"opened_at":1500,"members":[],"ejected":[]},
          {"batch_id":"r-10","state":"CI_RUNNING","parent":null,"opened_at":1000,"members":[{"bead_id":"sp-y","outcome":null}],"ejected":[]},
          {"batch_id":"r-9","state":"DISCARDED","parent":"r-10","opened_at":900,"members":[],"ejected":[]}]"#;
        let kv = round_keys_from(Some(&parse_batches(raw).unwrap()), Some(0), 900, Path::new("/nonexistent"));
        assert_eq!(get(&kv, "SP_ROUNDS_N"), Some("3"));
        let names: Vec<_> = (0..3).map(|i| get(&kv, &format!("SP_ROUNDS{i}_NAME")).unwrap()).collect();
        assert_eq!(names, ["r-11", "r-10", "r-12"]);
        assert_eq!(get(&kv, "SP_ROUNDS2_PARENT"), Some("r-10"));
        assert_eq!(get(&kv, "SP_ROUNDS1_PARENT"), None);
    }

    #[test]
    fn each_open_state_has_a_phase() {
        for (s, p) in [("OPEN", "merging"), ("CI_RUNNING", "certifying"), ("GREEN", "landing"), ("ATTRIBUTING", "attributing"), ("REBUILDING", "rebuilding")] {
            assert_eq!(phase(s), p);
        }
    }

    #[test]
    fn no_open_batch_reports_the_last_verdict() {
        let b = parse_batches(RAW).unwrap();
        let kv = round_keys_from(Some(&b[1..]), Some(0), 900, Path::new("/nonexistent"));
        assert_eq!(get(&kv, "SP_ROUND_STATE"), Some("idle"));
        assert_eq!(get(&kv, "SP_ROUND_LAST_VERDICT"), Some("GREEN"));
        assert_eq!(get(&kv, "SP_ROUND_LAST_AT"), Some("700"));
        let red = parse_batches(r#"[{"batch_id":"r-7","state":"ABANDONED","reason":"red: test-a.sh test-b.sh","opened_at":1,"last_at":2,"members":[],"ejected":[]}]"#).unwrap();
        let kv = round_keys_from(Some(&red), None, 900, Path::new("/nonexistent"));
        assert_eq!(get(&kv, "SP_ROUND_LAST_VERDICT"), Some("RED"));
        assert_eq!(get(&kv, "SP_ROUND_LAST_WHY"), Some("red: test-a.sh test-b.sh"));
        assert_eq!(get(&kv, "SP_ROUND_POOL"), Some("?"));
    }

    #[test]
    fn an_unreadable_store_is_cannot_tell_not_idle() {
        let kv = round_keys_from(None, None, 900, Path::new("/nonexistent"));
        assert_eq!(get(&kv, "SP_ROUND_STATE"), Some("?"));
        assert!(parse_batches("cannot tell: x").is_none());
    }

    #[test]
    fn the_pool_counts_submitted_and_certified_without_a_hold() {
        use spira_config::lc_state::Row;
        let row = |state: &str, holds: &[&str]| Row { bead_id: "x".into(), state: state.into(), holds: holds.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        let rows = [row("SUBMITTED", &[]), row("CERTIFIED", &[]), row("CERTIFIED", &["wait"]), row("WORKING", &[]), row("LANDED", &[])];
        assert_eq!(pool_waiting(&rows), 2);
    }

    #[test]
    fn a_certifying_round_reports_suites_done_total_and_reds_and_an_empty_dir_is_zero() {
        let run = testkit::TempDir::new("round-progress");
        let root = run.path().join("batch-results/spira-r-9/corpus");
        fs::create_dir_all(&root).unwrap();
        let log = RunLog::in_dir(&root);
        for (n, s) in [("a.sh", "ok"), ("b.sh", "red"), ("c.sh", "ok"), ("d.sh", "timeout")] {
            log.record(testenv::phase::RunEvent::Suite { name: n.into(), status: s.into() }, 1).unwrap();
        }
        fs::write(run.path().join("batch-results/spira-r-9/total"), "10\n").unwrap();
        let b = parse_batches(RAW).unwrap();
        let mut kv = Kv::new();
        progress_keys(&mut kv, &b, run.path());
        assert_eq!(get(&kv, "SP_ROUNDS0_SUITES_DONE"), Some("4"));
        assert_eq!(get(&kv, "SP_ROUNDS0_SUITES_TOTAL"), Some("10"));
        assert_eq!(get(&kv, "SP_ROUNDS0_REDS_N"), Some("2"));
        assert_eq!(get(&kv, "SP_ROUNDS0_RED1"), Some("d.sh"));
        let empty = testkit::TempDir::new("round-progress");
        let mut kv = Kv::new();
        progress_keys(&mut kv, &b, empty.path());
        assert_eq!(get(&kv, "SP_ROUNDS0_SUITES_DONE"), Some("0"));
        assert_eq!(get(&kv, "SP_ROUNDS0_SUITES_TOTAL"), Some("?"));
    }
}
