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

/// The checkout of a registered repository, through the registry every other reader of a bead's
/// repository uses. A name it does not hold is refused, never defaulted to the home checkout.
pub fn registered_root(name: &str) -> Result<std::path::PathBuf, String> {
    if name.is_empty() {
        return Err("no repository is named: an empty name would resolve to the home checkout".to_string());
    }
    let home = spira_config::resolve::locate_home_for_process()?;
    let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), Path::new(&home));
    if let Some(root) = reg.root(name) {
        return Ok(root.into());
    }
    let key = "SPIRA_REPO_MAP".to_string();
    let declared = match spira_config::process::fresh(std::slice::from_ref(&key)).ok().and_then(|mut v| v.remove(&key)) {
        Some(map) if !map.is_empty() => format!("the repo map {map} (spira.repo_map)"),
        _ => format!("[repo.{name}] in spira.toml"),
    };
    Err(format!("no repository {name:?} is registered: repositories are declared in {declared}"))
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

    fn registry_world(tag: &str) -> (testkit::TempDir, std::path::PathBuf, testkit::EnvGuard) {
        let dir = scratch(tag);
        let map = spira_config::process::fixture_repo_map(&dir, &["spira", "dumpstar"]);
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_REPO_MAP", &map.display().to_string()), ("SPIRA_HOME_REPO", "spira")]);
        let guard = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
        (dir, map, guard)
    }

    #[test]
    fn registered_root_resolves_a_product_repository_the_map_holds() {
        let (dir, _map, _env) = registry_world("root-product");
        assert_eq!(registered_root("dumpstar").unwrap(), dir.join("dumpstar"));
    }

    #[test]
    fn registered_root_still_resolves_the_home_repository() {
        let (dir, _map, _env) = registry_world("root-home");
        assert_eq!(registered_root("spira").unwrap(), dir.join("spira"));
    }

    #[test]
    fn registered_root_refusal_names_the_repo_map() {
        let (_dir, map, _env) = registry_world("root-refuse");
        let e = registered_root("nosuch").unwrap_err();
        assert!(e.contains("no repository \"nosuch\" is registered"), "{e}");
        assert!(e.contains(&map.display().to_string()), "{e}");
    }

    #[test]
    fn registered_root_refuses_an_empty_name_rather_than_the_home_checkout() {
        let (_dir, _map, _env) = registry_world("root-empty");
        assert!(registered_root("").unwrap_err().contains("empty name"));
    }

    #[test]
    fn registered_root_refusal_names_the_toml_table_when_no_map_is_declared() {
        let dir = scratch("root-nomap");
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_REPO_MAP", ""), ("SPIRA_HOME_REPO", "spira")]);
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
        let e = registered_root("nosuch").unwrap_err();
        assert!(e.contains("[repo.nosuch] in spira.toml"), "{e}");
    }

    #[test]
    fn queue_aliases_map_to_the_queue_delivery_machine() {
        assert_eq!(mode_str(LandMode::QueueForge), "queue");
        assert_eq!(mode_str(LandMode::QueueLocal), "queue");
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
}
