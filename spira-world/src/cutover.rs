//! Cutover mode: the world held by scoped stops instead of the halt stamp, so a round driver
//! passes the halt check while every other actor stays stopped. `world cutover-mode` enters it
//! and `world stop` leaves it, putting back exactly the stops it laid over.

use serde::{Deserialize, Serialize};
use spira_config::scoped_stop::{self, RepoStop};
use std::path::{Path, PathBuf};

pub const RECORD: &str = "world.cutover-mode";
pub const OWNER: &str = "cutover-mode";
pub const UNTIL: &str = "world stop";
/// Every lane but `rounds`: the round drivers are the one thing cutover mode lets run.
pub const LANES: [&str; 7] = ["claims", "publish", "land", "dwarves", "czar", "heimdall", "remedies"];

/// One repo's stop as cutover mode found and changed it: the lanes it added, and the stop it
/// was laid over.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Overlay {
    pub repo: String,
    pub lanes: Vec<String>,
    pub prior: Option<RepoStop>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Record {
    pub rev: String,
    pub reason: String,
    pub owner: String,
    pub since: u64,
    pub stops: Vec<Overlay>,
}

impl Record {
    /// The line `world status` prints.
    pub fn describe(&self, now: u64) -> String {
        format!(
            "CUTOVER MODE for {} (store revision {}, owner {}): {} — {} repo(s) held on [{}], rounds running; leave with: world stop",
            scoped_stop::age(self.since, now),
            self.rev,
            self.owner,
            self.reason,
            self.stops.len(),
            LANES.join(",")
        )
    }
}

pub fn record_path(run: &Path) -> PathBuf {
    run.join(RECORD)
}

/// The record, `None` when the world is not in cutover mode; an unreadable record is an error,
/// never "not in cutover mode".
pub fn read(run: &Path) -> Result<Option<Record>, String> {
    let p = record_path(run);
    match std::fs::read_to_string(&p) {
        Ok(t) => serde_json::from_str(&t).map(Some).map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

/// What entering cutover mode needs to know about the world.
pub struct Enter<'a> {
    pub run: &'a Path,
    pub halt_stamp: &'a Path,
    pub repos: &'a [String],
    pub live_dwarves: &'a [String],
    pub rev: &'a str,
    pub reason: &'a str,
    pub owner: &'a str,
    pub now: u64,
}

/// Stops [`LANES`] on every repo, records why, then removes the halt stamp. Refuses unless the
/// world is stopped and no dwarf lives; on any failed write the stops laid so far are lifted and
/// the stamp stays.
pub fn enter(e: &Enter) -> Result<Record, String> {
    if let Some(r) = read(e.run)? {
        return Err(format!("already in cutover mode (store revision {}, since {}): leave it with world stop first", r.rev, r.since));
    }
    for (what, v) in [("--rev", e.rev), ("--reason", e.reason), ("--owner", e.owner)] {
        if v.trim().is_empty() {
            return Err(format!("cutover mode needs {what}"));
        }
    }
    if !e.halt_stamp.is_file() {
        return Err(format!("the world is not stopped ({} is absent): run world stop first", e.halt_stamp.display()));
    }
    if !e.live_dwarves.is_empty() {
        return Err(format!("{} dwarf(s) still live ({}): cutover mode starts with every dwarf slain", e.live_dwarves.len(), e.live_dwarves.join(" ")));
    }
    if e.repos.is_empty() {
        return Err("no registered repos to hold".into());
    }
    let lanes: Vec<String> = LANES.iter().map(|l| l.to_string()).collect();
    let stops: Vec<Overlay> = e
        .repos
        .iter()
        .map(|repo| {
            let prior = scoped_stop::repo_stops(e.run).into_iter().find(|s| &s.repo == repo);
            let had = prior.as_ref().map(|p| p.lanes.clone()).unwrap_or_default();
            Overlay { repo: repo.clone(), lanes: lanes.iter().filter(|l| !had.contains(l)).cloned().collect(), prior }
        })
        .collect();
    let rec = Record { rev: e.rev.into(), reason: e.reason.into(), owner: e.owner.into(), since: e.now, stops };
    write(e.run, &rec)?;
    let why = format!("cutover mode to store revision {}: {}", e.rev, e.reason);
    for o in &rec.stops {
        if let Err(err) = scoped_stop::pause_repo(e.run, &o.repo, &lanes, OWNER, &why, UNTIL, e.now) {
            let _ = leave(e.run);
            return Err(format!("{}: {err} — every stop cutover mode laid is lifted", o.repo));
        }
    }
    std::fs::remove_file(e.halt_stamp).map_err(|err| {
        let _ = leave(e.run);
        format!("{}: {err} — every stop cutover mode laid is lifted", e.halt_stamp.display())
    })?;
    Ok(rec)
}

/// Lifts the lanes cutover mode added and puts back each stop it was laid over, then deletes the
/// record. `None` when the world was not in cutover mode.
pub fn leave(run: &Path) -> Result<Option<Record>, String> {
    let Some(rec) = read(run)? else { return Ok(None) };
    for o in &rec.stops {
        if o.lanes.is_empty() {
            continue;
        }
        let left = scoped_stop::unpause_repo(run, &o.repo, &o.lanes)?.unwrap_or_default();
        if let Some(prior) = o.prior.as_ref().filter(|_| !left.is_empty()) {
            scoped_stop::put_repo(run, &RepoStop { lanes: left, ..prior.clone() })?;
        }
    }
    let p = record_path(run);
    std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(Some(rec))
}

fn write(run: &Path, rec: &Record) -> Result<(), String> {
    std::fs::create_dir_all(run).map_err(|e| format!("{}: {e}", run.display()))?;
    let p = record_path(run);
    let tmp = run.join(format!(".{RECORD}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(rec).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct W {
        d: testkit::TempDir,
        repos: Vec<String>,
    }

    impl W {
        fn new(name: &str) -> W {
            let d = testkit::TempDir::new(name);
            std::fs::write(d.join("world.halted"), "stopped\n").unwrap();
            W { d, repos: vec!["spira".into(), "cubby".into()] }
        }
        fn run(&self) -> &Path {
            self.d.path()
        }
        fn enter(&self, live: &[String]) -> Result<Record, String> {
            let stamp = self.d.join("world.halted");
            enter(&Enter { run: self.run(), halt_stamp: &stamp, repos: &self.repos, live_dwarves: live, rev: "r42", reason: "controller cutover", owner: "concierge", now: 100 })
        }
    }

    #[test]
    fn cutover_mode_refuses_a_world_that_is_not_stopped() {
        let w = W::new("cutover-unstopped");
        std::fs::remove_file(w.d.join("world.halted")).unwrap();
        let e = w.enter(&[]).unwrap_err();
        assert!(e.contains("not stopped"), "{e}");
        assert!(scoped_stop::repo_stops(w.run()).is_empty() && read(w.run()).unwrap().is_none(), "a refusal writes nothing");
    }

    #[test]
    fn cutover_mode_refuses_while_a_dwarf_lives() {
        let w = W::new("cutover-live-dwarf");
        let e = w.enter(&["sp-live".into()]).unwrap_err();
        assert!(e.contains("sp-live"), "{e}");
        assert!(w.d.join("world.halted").is_file(), "the stamp stays");
        assert!(scoped_stop::repo_stops(w.run()).is_empty());
    }

    #[test]
    fn cutover_mode_stops_every_non_round_lane_on_every_repo_records_why_and_lifts_the_stamp() {
        let w = W::new("cutover-enter");
        let rec = w.enter(&[]).unwrap();
        assert!(!w.d.join("world.halted").exists(), "the halt stamp is removed for the window");
        let stops = scoped_stop::repo_stops(w.run());
        assert_eq!(stops.len(), 2, "one stop per repo");
        for s in &stops {
            assert_eq!(s.lanes, LANES.to_vec(), "{s:?}");
            assert_eq!((s.owner.as_str(), s.until.as_str()), (OWNER, UNTIL));
            assert!(s.reason.contains("r42"), "{s:?}");
            assert!(scoped_stop::stop_for(w.run(), &s.repo, "rounds").is_none(), "rounds run");
        }
        assert_eq!(read(w.run()).unwrap(), Some(rec.clone()));
        assert_eq!((rec.rev.as_str(), rec.reason.as_str(), rec.owner.as_str(), rec.since, rec.stops.len()), ("r42", "controller cutover", "concierge", 100, 2));
        assert!(w.enter(&[]).unwrap_err().contains("already in cutover mode"));
    }

    #[test]
    fn leaving_lifts_exactly_the_stops_cutover_mode_laid() {
        let w = W::new("cutover-leave");
        scoped_stop::pause_repo(w.run(), "cubby", &["rounds".to_string(), "claims".to_string()], "concierge", "drain", "fix lands", 7).unwrap();
        let before = scoped_stop::stop_for(w.run(), "cubby", "rounds").unwrap();
        w.enter(&[]).unwrap();
        assert_eq!(scoped_stop::stop_for(w.run(), "cubby", "rounds").unwrap().owner, OWNER);
        let rec = leave(w.run()).unwrap().unwrap();
        assert_eq!(rec.stops.iter().find(|o| o.repo == "cubby").unwrap().lanes, ["publish", "land", "dwarves", "czar", "heimdall", "remedies"], "claims was already stopped");
        assert!(scoped_stop::stop_for(w.run(), "spira", "claims").is_none(), "spira's stop is gone");
        assert_eq!(scoped_stop::repo_stops(w.run()), vec![before], "cubby's own stop is back as it was");
        assert!(read(w.run()).unwrap().is_none());
        assert_eq!(leave(w.run()).unwrap(), None, "leaving twice is a no-op");
    }
}
