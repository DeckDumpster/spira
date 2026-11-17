//! The checkout of a repository the registry holds.

use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
