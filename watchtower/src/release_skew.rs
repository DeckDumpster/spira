//! `--release-skew-check` — live `spira-*` units spread over more than one release for
//! longer than a dwarf's lifetime means something old is still spawning successors.

use crate::incident::{self, Finding};
use crate::log::log;
use std::collections::BTreeMap;
use std::path::Path;

pub struct Cfg {
    pub systemctl: String,
    pub max_secs: i64,
    pub dwarf_max_secs: i64,
}

impl Cfg {
    fn grace(&self, activated: Option<i64>, now: i64) -> Grace {
        Grace { activated, now, dwarf_max_secs: self.dwarf_max_secs }
    }
}

const DWARF_UNIT_PREFIX: &str = "spira-dwarf-";

/// A dwarf that started before `activated` and is within `dwarf_max_secs` of its start finishes
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
            (Some(act), Some(st)) => id.starts_with(DWARF_UNIT_PREFIX) && st < act && self.now - st <= self.dwarf_max_secs,
            _ => false,
        }
    }
}

/// Live-unit count per release, from `systemctl show --timestamp=unix -p Id -p Environment
/// -p ActiveEnterTimestamp` blocks.
pub fn count_by_release(show: &str, resolve: impl Fn(&str) -> Option<String>, grace: &Grace) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for block in show.split("\n\n") {
        let field = |k: &str| block.lines().find_map(|l| l.strip_prefix(k)).unwrap_or("");
        let started = field("ActiveEnterTimestamp=").trim().trim_start_matches('@').parse().ok();
        let release = block
            .lines()
            .filter(|l| l.starts_with("Environment="))
            .flat_map(|l| l.split_whitespace())
            .find_map(|w| w.trim_start_matches("Environment=").trim_matches(['"', '\'']).strip_prefix("SPIRA_RELEASE="));
        if let Some(r) = release {
            if grace.excuses(field("Id="), started) {
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
fn activation_time(show: &str) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    let root = show
        .split_whitespace()
        .find_map(|w| w.trim_start_matches("Environment=").trim_matches(['"', '\'']).strip_prefix("SPIRA_RELEASE="))?;
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
    let out = spira_config::bounded::bounded(&cfg.systemctl)
        .args(["--user", "show", "spira-*", "--state=active", "--timestamp=unix", "-p", "Id", "-p", "Environment", "-p", "ActiveEnterTimestamp"])
        .output();
    let Some(out) = out.ok().filter(|o| o.status.success()) else {
        log("watchtower: release-skew-check skipped — systemctl show failed");
        return;
    };
    let show = String::from_utf8_lossy(&out.stdout);
    let grace = cfg.grace(activation_time(&show), now);
    let counts = count_by_release(
        &show,
        |p| std::fs::canonicalize(p).ok().map(|c| c.to_string_lossy().into_owned()),
        &grace,
    );
    let stamp = run_dir.join("release-skew.since");
    let since = std::fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse().ok());
    match decide(counts.len(), since, now, cfg.max_secs) {
        Verdict::Clear => {
            let _ = std::fs::remove_file(&stamp);
        }
        Verdict::Since(s) => {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_GRACE: Grace = Grace { activated: None, now: 0, dwarf_max_secs: 0 };

    const SHOW: &str = "Id=spira-dwarf-builder-1.service\nEnvironment=SPIRA_RELEASE=/r/aaa PATH=/r/aaa/bin\n\nId=spira-dwarf-ops-2.service\nEnvironment=\"SPIRA_RELEASE=/r/bbb\" PATH=/x\n\nId=spira-dwarf-ops-3.service\nEnvironment=SPIRA_RELEASE=/r/bbb\n\nId=spira-x.service\nEnvironment=\n";

    #[test]
    fn counts_live_units_by_release_and_ignores_units_without_one() {
        let c = count_by_release(SHOW, |_| None, &NO_GRACE);
        assert_eq!((c.get("aaa"), c.get("bbb"), c.len()), (Some(&1), Some(&2), 2));
        assert!(count_by_release("Id=x\nEnvironment=FOO=1\n", |_| None, &NO_GRACE).is_empty());
    }

    #[test]
    fn a_current_symlink_and_its_sha_path_are_one_release() {
        let show = "Id=a\nEnvironment=SPIRA_RELEASE=/r/current\n\nId=b\nEnvironment=SPIRA_RELEASE=/r/abc123\n";
        let resolve = |p: &str| (p == "/r/current").then(|| "/r/abc123".to_string());
        let c = count_by_release(show, resolve, &NO_GRACE);
        assert_eq!((c.get("abc123"), c.len()), (Some(&2), 1));
        assert_eq!(count_by_release(show, |_| None, &NO_GRACE).len(), 2);
    }

    #[test]
    fn a_symlink_and_the_release_it_resolves_to_are_one_release() {
        let d = testkit::TempDir::new("wt-skew-link");
        std::fs::create_dir_all(d.join("abc123")).unwrap();
        std::os::unix::fs::symlink(d.join("abc123"), d.join("current")).unwrap();
        let show = format!(
            "Id=a.service\nEnvironment=SPIRA_RELEASE={0}/current\n\nId=b.service\nEnvironment=SPIRA_RELEASE={0}/abc123\n",
            d.display()
        );
        let c = count_by_release(&show, |p| std::fs::canonicalize(p).ok().map(|c| c.to_string_lossy().into_owned()), &NO_GRACE);
        assert_eq!((c.get("abc123"), c.len()), (Some(&2), 1));
    }

    #[test]
    fn skew_is_an_anomaly_only_after_longer_than_one_lifetime() {
        assert_eq!(decide(1, Some(0), 9999, 3600), Verdict::Clear);
        assert_eq!(decide(2, None, 100, 3600), Verdict::Since(100));
        assert_eq!(decide(2, Some(100), 3700, 3600), Verdict::Since(100));
        assert_eq!(decide(2, Some(100), 3701, 3600), Verdict::Anomaly);
    }

    fn unit(id: &str, rel: &str, started: i64) -> String {
        format!("Id={id}\nEnvironment=SPIRA_RELEASE=/r/{rel}\nActiveEnterTimestamp=@{started}\n\n")
    }

    fn releases_of(show: &str) -> usize {
        let grace = Grace { activated: Some(1000), now: 1500, dwarf_max_secs: 3600 };
        count_by_release(show, |_| None, &grace).len()
    }

    #[test]
    fn an_dwarf_started_before_the_activation_is_not_skew() {
        let show = unit("spira-dwarf-a-1.service", "A", 900) + &unit("spira-watchd.service", "B", 1000);
        assert_eq!(releases_of(&show), 1);
    }

    #[test]
    fn an_dwarf_started_after_the_activation_is_skew() {
        let show = unit("spira-dwarf-a-1.service", "A", 1100) + &unit("spira-watchd.service", "B", 1000);
        assert_eq!(releases_of(&show), 2);
    }

    #[test]
    fn a_long_lived_unit_on_the_old_release_is_skew_whenever_it_started() {
        let show = unit("spira-watchd.service", "A", 900) + &unit("spira-dwarf-a-1.service", "B", 1200);
        assert_eq!(releases_of(&show), 2);
    }

    #[test]
    fn an_dwarf_past_its_deadline_is_skew() {
        let show = unit("spira-dwarf-a-1.service", "A", 900) + &unit("spira-watchd.service", "B", 1000);
        let grace = Grace { activated: Some(1000), now: 900 + 3601, dwarf_max_secs: 3600 };
        assert_eq!(count_by_release(&show, |_| None, &grace).len(), 2);
    }

    #[test]
    fn a_dwarf_older_than_the_skew_window_but_within_the_dwarf_maximum_is_not_skew() {
        let show = unit("spira-dwarf-builder-1.service", "A", 1000 - 100) + &unit("spira-watchd.service", "B", 1000);
        let grace = Grace { activated: Some(1000), now: 900 + 7200, dwarf_max_secs: 14400 };
        assert_eq!(count_by_release(&show, |_| None, &grace).len(), 1);
    }

    #[test]
    fn the_configured_dwarf_bound_not_the_skew_window_excuses_a_dwarf() {
        let show = unit("spira-dwarf-builder-1.service", "A", 900) + &unit("spira-watchd.service", "B", 1000);
        let cfg = Cfg { systemctl: String::new(), max_secs: 3600, dwarf_max_secs: 9000 };
        assert_eq!(count_by_release(&show, |_| None, &cfg.grace(Some(1000), 900 + 7200)).len(), 1);
        assert_eq!(count_by_release(&show, |_| None, &cfg.grace(Some(1000), 900 + 9001)).len(), 2);
    }

    #[test]
    fn a_service_on_an_older_release_is_skew_however_young() {
        let show = unit("spira-watchd.service", "A", 990) + &unit("spira-sentinel.timer", "B", 1000);
        assert_eq!(releases_of(&show), 2);
    }

    #[test]
    fn activation_is_the_mtime_of_current_beside_the_release_root() {
        let d = testkit::TempDir::new("wt-skew-act");
        std::fs::create_dir_all(d.join("A")).unwrap();
        std::os::unix::fs::symlink("A", d.join("current")).unwrap();
        let show = format!("Environment=SPIRA_RELEASE={}/A\n", d.display());
        assert!(activation_time(&show).is_some_and(|t| t > 0));
        assert_eq!(activation_time("Environment=FOO=1\n"), None);
    }
}
