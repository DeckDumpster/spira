//! Scoped stops and bead reservations: typed records under `<run>/scoped-stops/`, written by
//! `world.sh pause` and read by the rounds scheduler, the claim ranker, the publisher and the
//! watchers. A stop is deliberate state with an owner, a reason and a release condition;
//! nothing that reads one may report it as a fault.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const LANES: [&str; 3] = ["claims", "rounds", "publish"];

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct RepoStop {
    pub repo: String,
    pub lanes: Vec<String>,
    pub owner: String,
    pub reason: String,
    pub until: String,
    pub since: u64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Reservation {
    pub bead: String,
    pub owner: String,
    pub reason: String,
    pub until: String,
    pub since: u64,
}

pub fn dir(run: &Path) -> PathBuf {
    run.join("scoped-stops")
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !s.starts_with('.')
}

/// The bead and state of a `bead:ID=STATE` release condition; `None` for free text, which only
/// a person lifts. A `bead:` condition that does not parse is an error, never free text.
pub fn bead_condition(until: &str) -> Option<Result<(String, String), String>> {
    let rest = until.trim().strip_prefix("bead:")?;
    let parsed = rest.split_once('=').filter(|(b, st)| valid_name(b) && !st.is_empty() && st.chars().all(|c| c.is_ascii_uppercase() || c == '_'));
    Some(parsed.map(|(b, st)| (b.to_string(), st.to_string())).ok_or_else(|| format!("release condition {until:?} is not bead:<id>=<STATE> (STATE in capitals)")))
}

/// Whether lifecycle `history` rows show the bead entering `state` at or after `since`.
pub fn entered_state(history: &[serde_json::Value], state: &str, since: u64) -> bool {
    let num = |v: Option<&serde_json::Value>| v.and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok())));
    history.iter().any(|r| {
        r.get("to_state").and_then(|v| v.as_str()) == Some(state) && num(r.get("applied")).unwrap_or(0) == 1 && num(r.get("at")).is_some_and(|at| at >= since)
    })
}

/// Lifts every stop whose `bead:ID=STATE` condition `reached` reports met; a stop whose
/// state cannot be read stays. Returns the repos lifted and the conditions that could not be read.
pub fn sweep(run: &Path, reached: &mut dyn FnMut(&str, &str, u64) -> Result<bool, String>) -> (Vec<String>, Vec<String>) {
    let (mut lifted, mut unread) = (Vec::new(), Vec::new());
    for s in repo_stops(run) {
        let Some(Ok((bead, state))) = bead_condition(&s.until) else { continue };
        match reached(&bead, &state, s.since) {
            Ok(true) if unpause_repo(run, &s.repo, &[]).is_ok() => lifted.push(s.repo),
            Ok(_) => {}
            Err(e) => unread.push(format!("{}: {e}", s.repo)),
        }
    }
    (lifted, unread)
}

pub fn parse_lanes(csv: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for l in csv.split(',').map(str::trim).filter(|l| !l.is_empty()) {
        if !LANES.contains(&l) {
            return Err(format!("unknown lane {l:?} — lanes are {}", LANES.join(",")));
        }
        if !out.iter().any(|o| o == l) {
            out.push(l.to_string());
        }
    }
    if out.is_empty() {
        return Err("no lanes named".into());
    }
    Ok(out)
}

fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    let d = path.parent().ok_or("no parent directory")?;
    fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    let tmp = d.join(format!(".{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("stop")));
    fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn repo_path(run: &Path, repo: &str) -> PathBuf {
    dir(run).join(format!("repo-{repo}.json"))
}

fn reserve_path(run: &Path, bead: &str) -> PathBuf {
    dir(run).join(format!("reserve-{bead}.json"))
}

/// Records a stop of `lanes` for `repo`; lanes already stopped keep their record's lane set
/// merged in, with the newest owner, reason and release condition.
pub fn pause_repo(run: &Path, repo: &str, lanes: &[String], owner: &str, reason: &str, until: &str, now: u64) -> Result<RepoStop, String> {
    if !valid_name(repo) {
        return Err(format!("bad repo name {repo:?}"));
    }
    for (what, v) in [("owner", owner), ("reason", reason), ("release condition (--until)", until)] {
        if v.trim().is_empty() {
            return Err(format!("a stop needs a {what}"));
        }
    }
    if let Some(Err(e)) = bead_condition(until) {
        return Err(e);
    }
    let prev = read_repo(&repo_path(run, repo));
    let mut all: Vec<String> = prev.as_ref().map(|p| p.lanes.clone()).unwrap_or_default();
    for l in lanes {
        if !all.contains(l) {
            all.push(l.clone());
        }
    }
    all.sort_by_key(|l| LANES.iter().position(|x| x == l));
    let stop = RepoStop {
        repo: repo.into(),
        lanes: all,
        owner: owner.into(),
        reason: reason.into(),
        until: until.into(),
        since: prev.map(|p| p.since).unwrap_or(now),
    };
    write_atomic(&repo_path(run, repo), &serde_json::to_string_pretty(&stop).map_err(|e| e.to_string())?)?;
    Ok(stop)
}

/// Lifts `lanes` (all when empty) of `repo`'s stop; returns the lanes still stopped, `None`
/// when no stop was recorded.
pub fn unpause_repo(run: &Path, repo: &str, lanes: &[String]) -> Result<Option<Vec<String>>, String> {
    if !valid_name(repo) {
        return Err(format!("bad repo name {repo:?}"));
    }
    let path = repo_path(run, repo);
    let Some(mut stop) = read_repo(&path) else { return Ok(None) };
    if !lanes.is_empty() {
        stop.lanes.retain(|l| !lanes.contains(l));
    } else {
        stop.lanes.clear();
    }
    if stop.lanes.is_empty() {
        fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok(Some(Vec::new()));
    }
    write_atomic(&path, &serde_json::to_string_pretty(&stop).map_err(|e| e.to_string())?)?;
    Ok(Some(stop.lanes))
}

pub fn reserve(run: &Path, bead: &str, owner: &str, reason: &str, until: &str, now: u64) -> Result<(), String> {
    if !valid_name(bead) {
        return Err(format!("bad bead id {bead:?}"));
    }
    for (what, v) in [("owner", owner), ("reason", reason)] {
        if v.trim().is_empty() {
            return Err(format!("a reservation needs a {what}"));
        }
    }
    let r = Reservation { bead: bead.into(), owner: owner.into(), reason: reason.into(), until: until.into(), since: now };
    write_atomic(&reserve_path(run, bead), &serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?)
}

/// Returns whether a reservation existed.
pub fn unreserve(run: &Path, bead: &str) -> Result<bool, String> {
    if !valid_name(bead) {
        return Err(format!("bad bead id {bead:?}"));
    }
    match fs::remove_file(reserve_path(run, bead)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", reserve_path(run, bead).display())),
    }
}

fn read_repo(p: &Path) -> Option<RepoStop> {
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

fn entries(run: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir(run))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(".json")))
        .collect();
    v.sort();
    v
}

pub fn repo_stops(run: &Path) -> Vec<RepoStop> {
    entries(run, "repo-").iter().filter_map(|p| read_repo(p)).collect()
}

pub fn reservations(run: &Path) -> Vec<Reservation> {
    entries(run, "reserve-").iter().filter_map(|p| serde_json::from_str(&fs::read_to_string(p).ok()?).ok()).collect()
}

/// The stop on `repo`'s `lane`, if one is recorded.
pub fn stop_for(run: &Path, repo: &str, lane: &str) -> Option<RepoStop> {
    if !valid_name(repo) {
        return None;
    }
    read_repo(&repo_path(run, repo)).filter(|s| s.lanes.iter().any(|l| l == lane))
}

pub fn reserved_ids(run: &Path) -> BTreeSet<String> {
    reservations(run).into_iter().map(|r| r.bead).collect()
}

pub fn overdue(since: u64, now: u64, review_secs: u64) -> bool {
    now.saturating_sub(since) >= review_secs
}

pub fn age(since: u64, now: u64) -> String {
    let s = now.saturating_sub(since);
    match s {
        0..=119 => format!("{s}s"),
        120..=7199 => format!("{}m", s / 60),
        7200..=172799 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// The line `world.sh status` prints for a stop; the same text any watcher quotes.
pub fn describe_stop(s: &RepoStop, now: u64, review_secs: u64) -> String {
    format!(
        "scoped stop {} [{}] owner={} age={} until: {} — {}{}",
        s.repo,
        s.lanes.join(","),
        s.owner,
        age(s.since, now),
        s.until,
        s.reason,
        if overdue(s.since, now, review_secs) { " — OVERDUE: release condition unmet past review age" } else { "" }
    )
}

pub fn describe_reservation(r: &Reservation, now: u64, review_secs: u64) -> String {
    format!(
        "reserved {} owner={} age={} until: {} — {}{}",
        r.bead,
        r.owner,
        age(r.since, now),
        if r.until.is_empty() { "the bead lands" } else { &r.until },
        r.reason,
        if overdue(r.since, now, review_secs) { " — OVERDUE" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lanes(s: &str) -> Vec<String> {
        parse_lanes(s).unwrap()
    }

    #[test]
    fn a_stop_is_per_repo_and_per_lane() {
        let d = testkit::TempDir::new("scoped-stop-lanes");
        pause_repo(d.path(), "spira", &lanes("rounds"), "concierge", "why", "fix lands", 100).unwrap();
        assert!(stop_for(d.path(), "spira", "rounds").is_some());
        assert!(stop_for(d.path(), "spira", "claims").is_none());
        assert!(stop_for(d.path(), "cubby", "rounds").is_none(), "another repo is untouched");
    }

    #[test]
    fn unpause_lifts_named_lanes_then_the_record() {
        let d = testkit::TempDir::new("scoped-stop-lift");
        pause_repo(d.path(), "spira", &lanes("claims,rounds,publish"), "o", "r", "u", 1).unwrap();
        assert_eq!(unpause_repo(d.path(), "spira", &lanes("rounds")).unwrap(), Some(vec!["claims".into(), "publish".into()]));
        assert_eq!(unpause_repo(d.path(), "spira", &[]).unwrap(), Some(vec![]));
        assert!(repo_stops(d.path()).is_empty());
        assert_eq!(unpause_repo(d.path(), "spira", &[]).unwrap(), None);
    }

    #[test]
    fn a_second_pause_merges_lanes_and_keeps_the_age() {
        let d = testkit::TempDir::new("scoped-stop-merge");
        pause_repo(d.path(), "spira", &lanes("rounds"), "o", "r", "u", 10).unwrap();
        let s = pause_repo(d.path(), "spira", &lanes("claims"), "o", "r2", "u2", 99).unwrap();
        assert_eq!(s.lanes, vec!["claims", "rounds"]);
        assert_eq!(s.since, 10);
    }

    #[test]
    fn reservations_round_trip_and_lift() {
        let d = testkit::TempDir::new("scoped-stop-reserve");
        reserve(d.path(), "sp-abc", "concierge", "hand fix", "", 5).unwrap();
        assert!(reserved_ids(d.path()).contains("sp-abc"));
        assert!(unreserve(d.path(), "sp-abc").unwrap());
        assert!(!unreserve(d.path(), "sp-abc").unwrap());
    }

    #[test]
    fn a_stop_without_its_release_condition_is_refused() {
        let d = testkit::TempDir::new("scoped-stop-refuse");
        assert!(pause_repo(d.path(), "spira", &lanes("rounds"), "o", "r", " ", 1).is_err());
        assert!(pause_repo(d.path(), "../x", &lanes("rounds"), "o", "r", "u", 1).is_err());
        assert!(parse_lanes("rounds,bogus").is_err());
    }

    #[test]
    fn a_bead_condition_is_typed_and_free_text_is_not() {
        assert_eq!(bead_condition("bead:sp-x=REWORK"), Some(Ok(("sp-x".into(), "REWORK".into()))));
        assert!(bead_condition("fix lands").is_none());
        assert!(bead_condition("bead:sp-x").unwrap().is_err());
        assert!(bead_condition("bead:sp-x=rework").unwrap().is_err());
        let d = testkit::TempDir::new("scoped-stop-bad-cond");
        assert!(pause_repo(d.path(), "spira", &lanes("rounds"), "o", "r", "bead:sp-x", 1).is_err());
    }

    #[test]
    fn a_bead_that_passed_through_the_state_since_the_pause_counts() {
        let h = |to: &str, at: u64| serde_json::json!({"to_state": to, "applied": 1, "at": at.to_string()});
        let rows = vec![h("REWORK", 100), h("CERTIFIED", 200)];
        assert!(entered_state(&rows, "REWORK", 100));
        assert!(!entered_state(&rows, "REWORK", 101), "an entry before the pause is not a release");
        assert!(!entered_state(&[serde_json::json!({"to_state": "REWORK", "applied": 0, "at": 150})], "REWORK", 100), "a refused event did not happen");
    }

    #[test]
    fn sweep_lifts_only_stops_whose_condition_is_met() {
        let d = testkit::TempDir::new("scoped-stop-sweep");
        pause_repo(d.path(), "cubby", &lanes("rounds"), "o", "r", "bead:sp-x=REWORK", 10).unwrap();
        pause_repo(d.path(), "spira", &lanes("rounds"), "o", "r", "fix lands", 10).unwrap();
        let (lifted, unread) = sweep(d.path(), &mut |b, st, since| Ok(b == "sp-x" && st == "REWORK" && since == 10));
        assert_eq!((lifted, unread), (vec!["cubby".to_string()], vec![]));
        assert!(stop_for(d.path(), "cubby", "rounds").is_none());
        assert!(stop_for(d.path(), "spira", "rounds").is_some());
        pause_repo(d.path(), "cubby", &lanes("rounds"), "o", "r", "bead:sp-y=DONE", 10).unwrap();
        let (lifted, unread) = sweep(d.path(), &mut |_, _, _| Err("store down".into()));
        assert!(lifted.is_empty() && unread.len() == 1);
        assert!(stop_for(d.path(), "cubby", "rounds").is_some(), "an unreadable bead keeps the stop");
    }

    #[test]
    fn overdue_is_age_past_review() {
        assert!(!overdue(100, 150, 60));
        assert!(overdue(100, 160, 60));
    }
}
