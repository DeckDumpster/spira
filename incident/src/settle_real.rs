//! [`Settler`] against the host: `bd`, the lifecycle machine, `git` and the `Units` port.


use crate::decide::Scope;
use crate::ports::Bd;
use crate::real::RealBd;
use crate::settle::{Candidate, Detector, FixState, Landing, Settler};

pub struct RealSettler<'a> {
    pub bd: &'a RealBd,
    pub db: String,
    pub incident_label: String,
    pub home_repo: String,
    pub registry: spira_config::repos::Registry,
    pub release_sha: Option<String>,
    /// The unit port the detector reads; `Err` says why there is none.
    pub units: Result<Box<dyn runtime::units::Units>, String>,
    /// The fixes the last `candidates` read, by id.
    pub fixes: std::cell::RefCell<std::collections::HashMap<String, serde_json::Value>>,
}

/// What a unit's facts say of its detector: failed is firing, anything else quiet (what
/// `systemctl is-failed` exited 0 and 1 for); no facts is an error.
pub fn detector_of(unit: &str, facts: Result<runtime::units::UnitFacts, String>) -> Result<Detector, String> {
    match facts?.state {
        runtime::units::UnitState::Failed => Ok(Detector::Firing(format!("{unit} is failed"))),
        _ => Ok(Detector::Quiet),
    }
}

/// The host's unit port: the user manager over D-Bus where there is one.
pub fn host_units() -> Result<Box<dyn runtime::units::Units>, String> {
    #[cfg(target_os = "linux")]
    {
        let clock: std::sync::Arc<dyn runtime::ports::Clock> = std::sync::Arc::new(runtime::ports::WallClock);
        runtime::units::SystemdUnits::connect(clock).map(|u| Box::new(u) as Box<dyn runtime::units::Units>).map_err(|e| format!("the user manager: {e}"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("no unit manager on this platform".into())
    }
}

pub fn blocks_of(bead: &serde_json::Value) -> Vec<String> {
    let id = bead.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let mut out: Vec<String> = bead
        .get("dependencies")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter(|d| d.get("dep_type").or_else(|| d.get("type")).and_then(|t| t.as_str()) == Some("blocks"))
                .filter(|d| d.get("issue_id").and_then(|i| i.as_str()).is_none_or(|i| i == id))
                .filter_map(|d| d.get("depends_on_id").and_then(|i| i.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// A commit-message pattern matching `id` as a whole bead id, not a prefix of another.
pub fn id_pattern(id: &str) -> String {
    format!("(^|[^A-Za-z0-9-]){}([^A-Za-z0-9.-]|$)", id.replace('.', "\\."))
}

fn git(root: &str, args: &[&str]) -> Result<(i32, String), String> {
    let out = spira_config::bounded::bounded("git").args(["-C", root]).args(args).output().map_err(|e| format!("git: {e}"))?;
    Ok((out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

impl Settler for RealSettler<'_> {
    fn candidates(&self) -> Result<Vec<Candidate>, String> {
        let open: Vec<_> = self
            .bd
            .list_beads(Scope::Unfinished, Some(&self.incident_label), None)?
            .into_iter()
            .filter(|(r, _)| r.status == crate::decide::BeadStatus::Open)
            .map(|(r, bead)| Candidate { fixes: blocks_of(&bead), id: r.id, external_ref: r.external_ref, labels: r.labels })
            .collect();
        let mut all: Vec<String> = open.iter().flat_map(|c| c.fixes.clone()).collect();
        all.sort();
        all.dedup();
        let read = self.bd.list_ids_json(&all, None)?;
        *self.fixes.borrow_mut() = read.into_iter().filter_map(|b| Some((b.get("id")?.as_str()?.to_string(), b))).collect();
        Ok(open)
    }

    fn fix_state(&self, fix: &str) -> Result<FixState, String> {
        match spira_config::lc_state::row(fix)? {
            Some(r) if matches!(r.state.as_str(), "LANDED" | "DONE") => Ok(FixState::Landed),
            Some(r) => Ok(FixState::Unlanded(r.state)),
            None => Err("no lifecycle row".into()),
        }
    }

    fn landing_in_force(&self, fix: &str) -> Result<Option<Landing>, String> {
        let bead = self.fixes.borrow().get(fix).cloned().ok_or_else(|| format!("{fix} not found"))?;
        let repo = bead
            .get("labels")
            .and_then(|l| l.as_array())
            .and_then(|a| a.iter().filter_map(|l| l.as_str()).find_map(|l| l.strip_prefix("repo:")))
            .unwrap_or(&self.home_repo)
            .to_string();
        let root = self.registry.root(&repo).ok_or_else(|| format!("repo {repo} has no checkout"))?;
        let base = self.registry.base(&repo).filter(|b| !b.is_empty()).ok_or_else(|| format!("repo {repo} has no landing ref"))?;
        let (rc, sha) = git(&root, &["log", &base, "--format=%H", "-E", "--grep", &id_pattern(fix), "-n", "1"])?;
        if rc != 0 || sha.is_empty() {
            return Err(format!("no commit on {base} names {fix}"));
        }
        if repo != self.home_repo {
            return Ok(Some(Landing { sha }));
        }
        let release = self.release_sha.as_deref().ok_or("the active release is not a commit sha")?;
        match git(&root, &["merge-base", "--is-ancestor", &sha, release])?.0 {
            0 => Ok(Some(Landing { sha })),
            1 => Ok(None),
            rc => Err(format!("git merge-base exited {rc}")),
        }
    }

    fn detect(&self, c: &Candidate) -> Result<Detector, String> {
        let Some(unit) = c.external_ref.as_deref().and_then(|r| r.strip_prefix("incident:")) else { return Ok(Detector::NoProbe) };
        if ![".service", ".timer", ".socket", ".scope"].iter().any(|s| unit.ends_with(s)) {
            return Ok(Detector::NoProbe);
        }
        let units = self.units.as_ref().map_err(|e| format!("units: {e}"))?;
        detector_of(unit, units.facts(&runtime::units::UnitHandle::named(unit)).map_err(|e| format!("units: {e}")))
    }

    fn close(&self, id: &str, evidence: &str) -> Result<(), String> {
        spira_config::lifecycle_row::close(id, evidence, "incident", None)
    }

    fn persist(&self, id: &str, label: &str, note: &str) -> Result<(), String> {
        if !self.bd.note(&self.db, id, note) {
            return Err("bd note failed".into());
        }
        if !self.bd.label_add(&self.db, id, label) {
            return Err("bd label add failed".into());
        }
        Ok(())
    }

    fn now_iso(&self) -> String {
        crate::run::iso_now_public(spira_config::vtime::now_epoch() as i64)
    }
}

/// The 40-hex directory name of `$SPIRA_RELEASE`, when it is one.
pub fn release_sha_of(release: &str) -> Option<String> {
    let given = std::path::Path::new(release.trim_end_matches('/'));
    let resolved = std::fs::canonicalize(given).unwrap_or_else(|_| given.to_path_buf());
    let name = resolved.file_name()?.to_str()?;
    (name.len() == 40 && name.bytes().all(|b| b.is_ascii_hexdigit())).then(|| name.to_string())
}


#[cfg(test)]
mod tests {
    use super::*;

    /// sp-ipwlic: the detector reads the unit's state through the port: failed fires, every
    /// other state (absent included) is quiet, and no facts is an error, never quiet.
    #[test]
    fn the_detector_reads_failed_from_the_unit_port() {
        use runtime::units::{UnitFacts, UnitState};
        let at = |state| Ok(UnitFacts { state, ..UnitFacts::absent() });
        assert!(matches!(detector_of("u.service", at(UnitState::Failed)), Ok(Detector::Firing(m)) if m == "u.service is failed"));
        for quiet in [UnitState::Active, UnitState::Inactive, UnitState::Absent, UnitState::Activating] {
            assert!(matches!(detector_of("u.service", at(quiet)), Ok(Detector::Quiet)));
        }
        assert!(detector_of("u.service", Err("bus down".into())).is_err());
    }

    fn settler<'a>(bd: &'a RealBd) -> RealSettler<'a> {
        RealSettler {
            bd,
            db: String::new(),
            incident_label: "incident".into(),
            home_repo: "home".into(),
            registry: spira_config::repos::Registry::from_env(Default::default(), std::path::Path::new("/nonexistent")),
            release_sha: None,
            units: Err("none".into()),
            fixes: Default::default(),
        }
    }

    /// sp-7crdc8: a settle tick reads the lifecycle, the open incidents by id, then every
    /// fix by id, all through the door and none by a spawned `bdq`.
    #[test]
    fn a_settle_tick_reads_incidents_and_their_fixes_in_three_door_requests() {
        use serde_json::json;
        let live = |id: &str, st: &str| json!({"bead_id": id, "state": st});
        let inc = |id: &str, fix: &str| {
            json!({"id": id, "labels": ["incident"], "dependencies": [{"issue_id": id, "depends_on_id": fix, "type": "blocks"}]})
        };
        let rows = vec![inc("sp-i1", "sp-f1"), inc("sp-i2", "sp-f1"), json!({"id": "sp-f1", "labels": ["repo:other"]})];
        let fake = std::sync::Arc::new(
            spira_config::lc_call::fake::Content::new(rows).with_live(vec![live("sp-i1", "READY"), live("sp-i2", "REWORK"), live("sp-f1", "LANDED")]),
        );
        let bd = RealBd { bdq: "/nonexistent".into(), timeout_secs: 1, retries: 1, lc: Some(spira_config::lc_call::LcClient::over(fake.clone(), true)) };
        let s = settler(&bd);
        let cands = s.candidates().unwrap();
        assert_eq!(cands.iter().map(|c| c.fixes.clone()).collect::<Vec<_>>(), vec![vec!["sp-f1".to_string()]; 2]);
        if let Err(e) = s.landing_in_force("sp-f1") {
            assert!(!e.contains("not found"), "a fix the tick read is answered from the cache: {e}");
        }
        assert!(s.landing_in_force("sp-unread").unwrap_err().contains("not found"));
        let seen = fake.requests();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(seen[0], ["list", "--live"]);
        for r in &seen[1..] {
            assert!(r.iter().any(|a| a == "--id"), "{r:?}");
        }
    }

    #[test]
    fn blocks_of_keeps_only_this_beads_blocks_edges() {
        let b: serde_json::Value = serde_json::from_str(
            r#"{"id":"i","dependencies":[{"issue_id":"i","depends_on_id":"f","dep_type":"blocks"},
            {"issue_id":"i","depends_on_id":"r","dep_type":"relates-to"},{"issue_id":"x","depends_on_id":"o","dep_type":"blocks"}]}"#,
        )
        .unwrap();
        assert_eq!(blocks_of(&b), vec!["f"]);
    }

    #[test]
    fn the_id_pattern_does_not_match_a_longer_or_child_id() {
        let re = id_pattern("sp-ab.1");
        assert!(re.contains("sp-ab\\.1"));
        assert!(!re.contains("sp-ab.1"));
    }

    #[test]
    fn release_sha_is_the_forty_hex_directory_name() {
        let sha = "9f6f7e976db26649b7e4b753de0a992b1fc2f75b";
        assert_eq!(release_sha_of(&format!("/r/spira-releases/{sha}/")), Some(sha.to_string()));
        assert_eq!(release_sha_of("/r/checkout"), None);
    }

    #[test]
    fn release_sha_follows_a_current_symlink() {
        let sha = "9f6f7e976db26649b7e4b753de0a992b1fc2f75b";
        let tmp = testkit::TempDir::new("settle-sym");
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join(sha)).unwrap();
        std::os::unix::fs::symlink(dir.join(sha), dir.join("current")).unwrap();
        let got = release_sha_of(dir.join("current").to_str().unwrap());
        assert_eq!(got, Some(sha.to_string()));
    }
}
