//! Repository configuration for the migration classifier: which land mode each repository
//! uses (queue/pr/push), read from whichever of the two formats the deployed release
//! actually reads. Per this bead's own note: production still reads `spira.conf` +
//! `repo-map` today and has not taken a release since acceptance went red, but it will move
//! to releases mid-cutover to `spira.toml` — so this detects which is in force rather than
//! assuming either.

use std::collections::BTreeMap;
use std::path::Path;

use spira_config::convert::{self, ConvertWarnings};
use spira_config::{LandMode, RepoSection};

pub struct RepoConfig {
    /// `"spira.toml"` or `"spira.conf+repo-map"` — named on the contradiction report so a
    /// reader can tell which record a repository's mode came from.
    pub source: &'static str,
    pub repos: BTreeMap<String, RepoSection>,
}

/// Detects and reads the repository configuration under `home`: `home/spira.toml` if it
/// exists, else `home/spira.conf` plus `home/repo-map` (falling back to `repo-map.example`
/// beside it, exactly `conf.sh`'s own fallback for a clean clone with no repo-map written
/// yet).
pub fn detect(home: &Path) -> Result<RepoConfig, String> {
    if let Some(toml_path) = spira_config::find_under(home) {
        let doc = spira_config::load(&toml_path)?;
        return Ok(RepoConfig { source: "spira.toml", repos: doc.repo });
    }

    let conf_text = std::fs::read_to_string(home.join("spira.conf")).unwrap_or_default();
    let home_env = std::env::var("HOME").unwrap_or_default();
    let mut warnings = ConvertWarnings::default();
    let raw = convert::read_conf(&conf_text, &home_env);
    let _spira = convert::spira_section(&raw, &mut warnings);

    let repo_map_path = {
        let primary = home.join("repo-map");
        if primary.is_file() {
            primary
        } else {
            home.join("repo-map.example")
        }
    };
    let repo_map_text = std::fs::read_to_string(&repo_map_path).unwrap_or_default();
    let repos = convert::repo_sections(&repo_map_text, &mut warnings).map_err(|errs| errs.join("; "))?;
    Ok(RepoConfig { source: "spira.conf+repo-map", repos })
}

pub fn mode_str(m: LandMode) -> &'static str {
    match m {
        LandMode::Push => "push",
        LandMode::Pr => "pr",
        LandMode::Hold => "hold",
        // queue.forge is today's queue under its new name; queue.local rides the queue delivery
        // machine until its own lands (sp-iwhgw).
        LandMode::Queue | LandMode::QueueForge | LandMode::QueueLocal => "queue",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("spira-lc-repo-config-test-{tag}"))
    }

    #[test]
    fn queue_aliases_map_to_the_queue_delivery_machine() {
        assert_eq!(mode_str(LandMode::QueueForge), "queue");
        assert_eq!(mode_str(LandMode::QueueLocal), "queue");
    }

    #[test]
    fn detects_spira_toml_when_present() {
        let dir = scratch("toml");
        fs::write(
            dir.join("spira.toml"),
            "[repo.service]\npath = \"/srv/service\"\nmode = \"pr\"\n\n[repo.other]\npath = \"/srv/other\"\nmode = \"push\"\n",
        )
        .unwrap();
        let cfg = detect(&dir).expect("valid spira.toml");
        assert_eq!(cfg.source, "spira.toml");
        assert_eq!(mode_str(cfg.repos["service"].mode), "pr");
        assert_eq!(mode_str(cfg.repos["other"].mode), "push");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_to_conf_and_repo_map_when_no_toml_exists() {
        let dir = scratch("conf");
        fs::write(dir.join("spira.conf"), "SPIRA_HOME_REPO=service\n").unwrap();
        fs::write(dir.join("repo-map"), "service|/srv/service|pr|main|rustfmt|./gate.sh|plan\nother|/srv/other|push|main|\n").unwrap();
        let cfg = detect(&dir).expect("valid legacy config");
        assert_eq!(cfg.source, "spira.conf+repo-map");
        assert_eq!(mode_str(cfg.repos["service"].mode), "pr");
        assert_eq!(mode_str(cfg.repos["other"].mode), "push");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn spira_toml_takes_precedence_over_a_legacy_conf_beside_it() {
        let dir = scratch("both");
        fs::write(dir.join("spira.conf"), "SPIRA_HOME_REPO=stale\n").unwrap();
        fs::write(dir.join("repo-map"), "stale|/srv/stale|push|main|\n").unwrap();
        fs::write(dir.join("spira.toml"), "[repo.fresh]\npath = \"/srv/fresh\"\nmode = \"queue\"\n").unwrap();
        let cfg = detect(&dir).expect("valid");
        assert_eq!(cfg.source, "spira.toml");
        assert!(cfg.repos.contains_key("fresh"));
        assert!(!cfg.repos.contains_key("stale"));
        fs::remove_dir_all(&dir).ok();
    }
}
