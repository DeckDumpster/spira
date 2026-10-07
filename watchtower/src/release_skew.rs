//! `--release-skew-check` — live `spira-*` units spread over more than one release for
//! longer than an aeon's lifetime means something old is still spawning successors.

use crate::incident::{self, Finding};
use crate::log::log;
use std::collections::BTreeMap;
use std::path::Path;

pub struct Cfg {
    pub systemctl: String,
    pub max_secs: i64,
}

/// Live-unit count per release, from `systemctl show -p Id -p Environment` blocks.
pub fn count_by_release(show: &str, resolve: impl Fn(&str) -> Option<String>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for block in show.split("\n\n") {
        let release = block
            .lines()
            .filter(|l| l.starts_with("Environment="))
            .flat_map(|l| l.split_whitespace())
            .find_map(|w| w.trim_start_matches("Environment=").trim_matches(['"', '\'']).strip_prefix("SPIRA_RELEASE="));
        if let Some(r) = release {
            let r = r.trim_end_matches('/');
            let resolved = resolve(r);
            let r = resolved.as_deref().unwrap_or(r).trim_end_matches('/');
            let sha = r.rsplit('/').next().unwrap_or(r);
            *counts.entry(sha.to_string()).or_insert(0) += 1;
        }
    }
    counts
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
        .args(["--user", "show", "spira-*", "--state=active", "-p", "Id", "-p", "Environment"])
        .output();
    let Some(out) = out.ok().filter(|o| o.status.success()) else {
        log("watchtower: release-skew-check skipped — systemctl show failed");
        return;
    };
    let counts = count_by_release(&String::from_utf8_lossy(&out.stdout), |p| {
        std::fs::canonicalize(p).ok().map(|c| c.to_string_lossy().into_owned())
    });
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
                incident::file(incident_sh, &f);
            }
            log("watchtower: release-skew-check filed escalation");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOW: &str = "Id=spira-aeon-builder-1.service\nEnvironment=SPIRA_RELEASE=/r/aaa PATH=/r/aaa/bin\n\nId=spira-aeon-ops-2.service\nEnvironment=\"SPIRA_RELEASE=/r/bbb\" PATH=/x\n\nId=spira-aeon-ops-3.service\nEnvironment=SPIRA_RELEASE=/r/bbb\n\nId=spira-x.service\nEnvironment=\n";

    #[test]
    fn counts_live_units_by_release_and_ignores_units_without_one() {
        let c = count_by_release(SHOW, |_| None);
        assert_eq!((c.get("aaa"), c.get("bbb"), c.len()), (Some(&1), Some(&2), 2));
        assert!(count_by_release("Id=x\nEnvironment=FOO=1\n", |_| None).is_empty());
    }

    #[test]
    fn a_current_symlink_and_its_sha_path_are_one_release() {
        let show = "Id=a\nEnvironment=SPIRA_RELEASE=/r/current\n\nId=b\nEnvironment=SPIRA_RELEASE=/r/abc123\n";
        let resolve = |p: &str| (p == "/r/current").then(|| "/r/abc123".to_string());
        let c = count_by_release(show, resolve);
        assert_eq!((c.get("abc123"), c.len()), (Some(&2), 1));
        assert_eq!(count_by_release(show, |_| None).len(), 2);
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
        let c = count_by_release(&show, |p| std::fs::canonicalize(p).ok().map(|c| c.to_string_lossy().into_owned()));
        assert_eq!((c.get("abc123"), c.len()), (Some(&2), 1));
    }

    #[test]
    fn skew_is_an_anomaly_only_after_longer_than_one_lifetime() {
        assert_eq!(decide(1, Some(0), 9999, 3600), Verdict::Clear);
        assert_eq!(decide(2, None, 100, 3600), Verdict::Since(100));
        assert_eq!(decide(2, Some(100), 3700, 3600), Verdict::Since(100));
        assert_eq!(decide(2, Some(100), 3701, 3600), Verdict::Anomaly);
    }
}
