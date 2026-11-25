//! `--release-skew-check` — live `spira-*` units spread over more than one release for
//! longer than a dwarf's lifetime means something old is still spawning successors.

use crate::incident::{self, Finding};
use crate::log::log;
use std::collections::BTreeMap;
use runtime::units::{UnitDetail, UnitState, Units};
use std::path::Path;

pub struct Cfg {
    pub systemctl: Option<String>,
    pub max_secs: i64,
    pub dwarf_max_secs: i64,
}

impl Cfg {
    fn grace(&self, activated: Option<i64>, now: i64) -> Grace {
        Grace { activated, now, dwarf_max_secs: self.dwarf_max_secs }
    }
}

/// A dwarf (either name, `runtime::units::is_dwarf_unit`) that started before `activated` and is within `dwarf_max_secs` of its start finishes
/// on the release it was called from by design; it is not skew. Sessions renew their lease while
/// they work, so this bound is the longest a healthy session runs, and is wider than `Cfg::max_secs`.
pub struct Grace {
    pub activated: Option<i64>,
    pub now: i64,
    pub dwarf_max_secs: i64,
}

impl Grace {
    fn excuses(&self, id: &str, started: Option<i64>) -> bool {
        match (self.activated, started) {
            (Some(act), Some(st)) => runtime::units::is_dwarf_unit(id) && st < act && self.now - st <= self.dwarf_max_secs,
            _ => false,
        }
    }
}

fn release_of(u: &UnitDetail) -> Option<&str> {
    u.environment.iter().find_map(|e| e.trim_matches(['"', '\'']).strip_prefix("SPIRA_RELEASE="))
}

/// Live-unit count per release, from the active units' `Environment` and start time.
pub fn count_by_release(units: &[UnitDetail], resolve: impl Fn(&str) -> Option<String>, grace: &Grace) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for u in units {
        if let Some(r) = release_of(u) {
            let started = u.since_ms.map(|ms| (ms / 1000) as i64);
            if grace.excuses(&u.name, started) {
                continue;
            }
            let r = r.trim_end_matches('/');
            let resolved = resolve(r);
            let r = resolved.as_deref().unwrap_or(r).trim_end_matches('/');
            let sha = r.rsplit('/').next().unwrap_or(r);
            *counts.entry(sha.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

/// When `current` was last repointed: the mtime of the symlink beside the release roots.
fn activation_time(units: &[UnitDetail]) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    let root = units.iter().find_map(release_of)?;
    let parent = Path::new(root.trim_end_matches('/')).parent()?;
    std::fs::symlink_metadata(parent.join(spira_config::release_skew::CURRENT)).ok().map(|m| m.mtime())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Clear,
    Since(i64),
    Anomaly,
}

/// `since` is when more than one release was first seen live; `None` if it was not.
pub fn decide(releases: usize, since: Option<i64>, now: i64, max_secs: i64) -> Verdict {
    if releases < 2 {
        return Verdict::Clear;
    }
    match since {
        None => Verdict::Since(now),
        Some(s) if now - s > max_secs => Verdict::Anomaly,
        Some(s) => Verdict::Since(s),
    }
}

pub fn run(now: i64, run_dir: &Path, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) {
    let _ = check(runtime::units::host(cfg.systemctl.as_deref()).as_ref(), now, run_dir, db, home_repo, incident_sh, cfg);
}

pub fn check(units: &dyn Units, now: i64, run_dir: &Path, db: &str, home_repo: &str, incident_sh: &str, cfg: &Cfg) -> Option<Verdict> {
    let active: Vec<UnitDetail> = match units.list("spira-*") {
        Ok(all) => all.into_iter().filter(|u| u.state == UnitState::Active).collect(),
        Err(e) => {
            log(&format!("watchtower: release-skew-check skipped — cannot list units: {e}"));
            return None;
        }
    };
    let grace = cfg.grace(activation_time(&active), now);
    let counts = count_by_release(
        &active,
        |p| std::fs::canonicalize(p).ok().map(|c| c.to_string_lossy().into_owned()),
        &grace,
    );
    let stamp = run_dir.join("release-skew.since");
    let since = std::fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse().ok());
    let verdict = decide(counts.len(), since, now, cfg.max_secs);
    match &verdict {
        Verdict::Clear => {
            let _ = std::fs::remove_file(&stamp);
        }
        &Verdict::Since(s) => {
            if since != Some(s) {
                let _ = std::fs::write(&stamp, s.to_string());
            }
            log(&format!("watchtower: release-skew-check: live units span {} releases {counts:?}", counts.len()));
        }
        Verdict::Anomaly => {
            let body = format!(
                "Live spira units have run from more than one release for over {}s: {counts:?}.\n\nA process from an old release keeps spawning successors (refills, landing passes) from itself, so landed fixes are not in force. Find the old-release units and stop them; `release status` names the current release.\n",
                cfg.max_secs
            );
            let f = Finding::new(db, home_repo, "RELEASE SKEW: live units span more than one release", &body)
                .priority(1)
                .reference("incident:release-skew")
                .cause("release-skew");
            if incident::is_usable(incident_sh) {
                incident::alarm(incident_sh, &f);
            }
            log("watchtower: release-skew-check filed escalation");
        }
    }
    Some(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_GRACE: Grace = Grace { activated: None, now: 0, dwarf_max_secs: 0 };

    fn detail(id: &str, env: &[&str], started_s: Option<u64>) -> UnitDetail {
        UnitDetail { name: id.into(), state: UnitState::Active, environment: env.iter().map(|e| e.to_string()).collect(), since_ms: started_s.map(|s| s * 1000), ..Default::default() }
    }

    fn shown() -> Vec<UnitDetail> {
        vec![
            detail("spira-dwarf-builder-1.service", &["SPIRA_RELEASE=/r/aaa", "PATH=/r/aaa/bin"], None),
            detail("spira-dwarf-ops-2.service", &["\"SPIRA_RELEASE=/r/bbb\"", "PATH=/x"], None),
            detail("spira-dwarf-ops-3.service", &["SPIRA_RELEASE=/r/bbb"], None),
            detail("spira-x.service", &[], None),
        ]
    }

    #[test_id(telfkiu)]
    #[test]
    fn counts_live_units_by_release_and_ignores_units_without_one() {
        let c = count_by_release(&shown(), |_| None, &NO_GRACE);
        assert_eq!((c.get("aaa"), c.get("bbb"), c.len()), (Some(&1), Some(&2), 2));
        assert!(count_by_release(&[detail("x", &["FOO=1"], None)], |_| None, &NO_GRACE).is_empty());
    }

    #[test_id(t2aytvr)]
    #[test]
    fn a_current_symlink_and_its_sha_path_are_one_release() {
        let show = [detail("a", &["SPIRA_RELEASE=r/current"], None), detail("b", &["SPIRA_RELEASE=r/abc123"], None)];
        let resolve = |p: &str| (p == "r/current").then(|| "r/abc123".to_string());
        let c = count_by_release(&show, resolve, &NO_GRACE);
        assert_eq!((c.get("abc123"), c.len()), (Some(&2), 1));
        assert_eq!(count_by_release(&show, |_| None, &NO_GRACE).len(), 2);
    }

    #[test_id(tambus3)]
    #[test]
    fn a_symlink_and_the_release_it_resolves_to_are_one_release() {
        let d = testkit::TempDir::new("wt-skew-link");
        std::fs::create_dir_all(d.join("abc123")).unwrap();
        std::os::unix::fs::symlink(d.join("abc123"), d.join("current")).unwrap();
        let show = [detail("a.service", &[&format!("SPIRA_RELEASE={}/current", d.display())], None), detail("b.service", &[&format!("SPIRA_RELEASE={}/abc123", d.display())], None)];
        let c = count_by_release(&show, |p| std::fs::canonicalize(p).ok().map(|c| c.to_string_lossy().into_owned()), &NO_GRACE);
        assert_eq!((c.get("abc123"), c.len()), (Some(&2), 1));
    }

    #[test_id(tbrdqui)]
    #[test]
    fn skew_is_an_anomaly_only_after_longer_than_one_lifetime() {
        assert_eq!(decide(1, Some(0), 9999, 3600), Verdict::Clear);
        assert_eq!(decide(2, None, 100, 3600), Verdict::Since(100));
        assert_eq!(decide(2, Some(100), 3700, 3600), Verdict::Since(100));
        assert_eq!(decide(2, Some(100), 3701, 3600), Verdict::Anomaly);
    }

    fn unit(id: &str, rel: &str, started: u64) -> Vec<UnitDetail> {
        vec![detail(id, &[&format!("SPIRA_RELEASE=/r/{rel}")], Some(started))]
    }

    fn releases_of(show: &[UnitDetail]) -> usize {
        let grace = Grace { activated: Some(1000), now: 1500, dwarf_max_secs: 3600 };
        count_by_release(show, |_| None, &grace).len()
    }

    #[test_id(tclca2n)]
    #[test]
    fn an_dwarf_started_before_the_activation_is_not_skew() {
        let show = [unit("spira-dwarf-a-1.service", "A", 900), unit("spira-watchd.service", "B", 1000)].concat();
        assert_eq!(releases_of(&show), 1);
    }

    #[test_id(t8qywkv)]
    #[test]
    fn sp_li4zys_a_scheduler_started_dwarf_before_the_activation_is_not_skew() {
        let show = [unit("spira-run-sentinel_2dcall-builder_2d1794009721297_2d1.service", "A", 900), unit("spira-watchd.service", "B", 1000)].concat();
        assert_eq!(releases_of(&show), 1, "a scheduler-named dwarf finishes on the release it was called from");
        let show = [unit("spira-run-sentinel_2dpass-landing.service", "A", 900), unit("spira-watchd.service", "B", 1000)].concat();
        assert_eq!(releases_of(&show), 2, "a landing pass is not a dwarf and has no grace");
    }

    #[test_id(t0qedkf)]
    #[test]
    fn an_dwarf_started_after_the_activation_is_skew() {
        let show = [unit("spira-dwarf-a-1.service", "A", 1100), unit("spira-watchd.service", "B", 1000)].concat();
        assert_eq!(releases_of(&show), 2);
    }

    #[test_id(tne33kq)]
    #[test]
    fn a_long_lived_unit_on_the_old_release_is_skew_whenever_it_started() {
        let show = [unit("spira-watchd.service", "A", 900), unit("spira-dwarf-a-1.service", "B", 1200)].concat();
        assert_eq!(releases_of(&show), 2);
    }

    #[test_id(tbwwln0)]
    #[test]
    fn an_dwarf_past_its_deadline_is_skew() {
        let show = [unit("spira-dwarf-a-1.service", "A", 900), unit("spira-watchd.service", "B", 1000)].concat();
        let grace = Grace { activated: Some(1000), now: 900 + 3601, dwarf_max_secs: 3600 };
        assert_eq!(count_by_release(&show, |_| None, &grace).len(), 2);
    }

    #[test_id(tffex5v)]
    #[test]
    fn a_dwarf_older_than_the_skew_window_but_within_the_dwarf_maximum_is_not_skew() {
        let show = [unit("spira-dwarf-builder-1.service", "A", 1000 - 100), unit("spira-watchd.service", "B", 1000)].concat();
        let grace = Grace { activated: Some(1000), now: 900 + 7200, dwarf_max_secs: 14400 };
        assert_eq!(count_by_release(&show, |_| None, &grace).len(), 1);
    }

    #[test_id(trf0mgi)]
    #[test]
    fn the_configured_dwarf_bound_not_the_skew_window_excuses_a_dwarf() {
        let show = [unit("spira-dwarf-builder-1.service", "A", 900), unit("spira-watchd.service", "B", 1000)].concat();
        let cfg = Cfg { systemctl: None, max_secs: 3600, dwarf_max_secs: 9000 };
        assert_eq!(count_by_release(&show, |_| None, &cfg.grace(Some(1000), 900 + 7200)).len(), 1);
        assert_eq!(count_by_release(&show, |_| None, &cfg.grace(Some(1000), 900 + 9001)).len(), 2);
    }

    #[test_id(tyiivyc)]
    #[test]
    fn a_service_on_an_older_release_is_skew_however_young() {
        let show = [unit("spira-watchd.service", "A", 990), unit("spira-sentinel.timer", "B", 1000)].concat();
        assert_eq!(releases_of(&show), 2);
    }

    fn scripted(units: Vec<UnitDetail>) -> runtime::units::ScriptedUnits {
        let u = runtime::units::ScriptedUnits::new(std::sync::Arc::new(runtime::ports::WallClock));
        units.into_iter().for_each(|d| u.install_detail(d));
        u
    }

    fn two_live_releases() -> runtime::units::ScriptedUnits {
        scripted(vec![detail("spira-watchd.service", &["SPIRA_RELEASE=/r/aaa"], Some(1)), detail("spira-loom.service", &["SPIRA_RELEASE=/r/bbb"], Some(2))])
    }

    fn cfg() -> Cfg {
        Cfg { systemctl: None, max_secs: 3600, dwarf_max_secs: 14400 }
    }

    #[test_id(tnstfze)]
    #[test]
    fn two_live_releases_through_the_unit_manager_raise_skew_once_they_outlast_the_window() {
        let d = testkit::TempDir::new("wt-skew-live");
        let units = two_live_releases();
        assert_eq!(check(&units, 100, d.path(), "", "", "", &cfg()), Some(Verdict::Since(100)));
        assert_eq!(check(&units, 100 + 3601, d.path(), "", "", "", &cfg()), Some(Verdict::Anomaly));
    }

    #[test_id(t6kkbk9)]
    #[test]
    fn an_inactive_unit_of_another_release_is_not_live_skew() {
        let d = testkit::TempDir::new("wt-skew-inactive");
        let mut stopped = detail("spira-old.service", &["SPIRA_RELEASE=/r/ccc"], Some(1));
        stopped.state = UnitState::Inactive;
        let units = scripted(vec![detail("spira-watchd.service", &["SPIRA_RELEASE=/r/aaa"], Some(1)), stopped]);
        assert_eq!(check(&units, 100, d.path(), "", "", "", &cfg()), Some(Verdict::Clear));
    }

    #[test_id(ts2gi1m)]
    #[test]
    fn an_unreadable_unit_manager_skips_the_check_and_leaves_the_stamp() {
        let d = testkit::TempDir::new("wt-skew-down");
        std::fs::write(d.join("release-skew.since"), "5").unwrap();
        let down = runtime::units::unreachable("bus gone");
        assert_eq!(check(down.as_ref(), 100, d.path(), "", "", "", &cfg()), None);
        assert_eq!(std::fs::read_to_string(d.join("release-skew.since")).unwrap(), "5");
    }

    #[test_id(tcx193o)]
    #[test]
    fn activation_is_the_mtime_of_current_beside_the_release_root() {
        let d = testkit::TempDir::new("wt-skew-act");
        std::fs::create_dir_all(d.join("A")).unwrap();
        std::os::unix::fs::symlink("A", d.join("current")).unwrap();
        let show = [detail("a", &[&format!("SPIRA_RELEASE={}/A", d.display())], None)];
        assert!(activation_time(&show).is_some_and(|t| t > 0));
        assert_eq!(activation_time(&[detail("a", &["FOO=1"], None)]), None);
    }
}
