//! `_seed_prod_instance` (`systemd/install.sh`, in Rust): on a split dev/prod checkout,
//! write the instance qualifier into the separate `SPIRA_PROD` checkout's own typed config,
//! so every path its own `conf.sh` derives (`SPIRA_DB`, `SPIRA_RUN`, ...) inherits it and its
//! containment fence fires as intended. Always through the real `spira-config` binary
//! (`get`/`set`/`convert`) — never a hand-rolled writer — matching conf.sh's own
//! `spira_config_set_at`, which this shells to the same binary rather than re-deriving.
//!
//! Names neither config filename itself: every path this file builds goes through
//! `spira_config::toml_path_at`/`repo_map_candidate`/`convert_command` (config-fence: only
//! spira-config may name the typed config file or the repo map).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The write target a writer at `conf`/`toml` should target (conf.sh's own cross-root
/// helper) — `toml` itself if it exists; else a full `spira-config convert` from `conf`
/// (plus `repo_map` and every `fayth`) if `conf` exists; else `toml` itself (to be created
/// fresh by `spira-config set`). `None` only when a required convert fails (already
/// reported on stderr).
fn write_target_for(conf: &Path, toml: &Path, home: &Path, repo_map: Option<&Path>, fayth: &[PathBuf]) -> Option<PathBuf> {
    if toml.is_file() {
        return Some(toml.to_path_buf());
    }
    if !conf.is_file() {
        return Some(toml.to_path_buf());
    }
    match spira_config::convert_command(conf, home, toml, repo_map, fayth).output() {
        Ok(o) if o.status.success() => Some(toml.to_path_buf()),
        Ok(o) => {
            eprintln!("install: auto-convert to the typed config failed: {}", String::from_utf8_lossy(&o.stderr).trim());
            None
        }
        Err(e) => {
            eprintln!("install: cannot run spira-config convert: {e}");
            None
        }
    }
}

/// `_spira_fayth_paths` (conf.sh): every `*.fayth` under `<home>/chamber` (or
/// `$SPIRA_CHAMBER`), sorted for a deterministic convert argv.
fn fayth_paths(home: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = PathBuf::from(spira_config::resolve::resolve_key(&std::env::vars().collect(), home, "SPIRA_CHAMBER")?);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("fayth") && p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `_seed_prod_instance <conf> <toml> <instance>`. Returns the message to print on stdout
/// when it wrote something (`install: seeded <target> with instance = <instance>`), `None`
/// when it was already correct or the write failed (the failure itself already reported on
/// stderr).
pub fn seed_prod_instance(conf: &Path, toml: &Path, instance: &str, home: &Path) -> Option<String> {
    let rm = spira_config::repo_map_candidate(conf.parent(), home);
    let fy = match fayth_paths(home) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("install: {e}");
            return None;
        }
    };
    let target = write_target_for(conf, toml, home, rm.as_deref(), &fy)?;
    let current = Command::new("spira-config")
        .args(["get", "spira.instance"])
        .arg(&target)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if current.as_deref() == Some(instance) {
        return None;
    }
    let ok = Command::new("spira-config").args(["set", "spira.instance", instance]).arg(&target).status().map(|s| s.success()).unwrap_or(false);
    if ok {
        Some(format!("install: seeded {} with instance = {instance}", target.display()))
    } else {
        None
    }
}

/// The root call site (`systemd/install.sh`'s own comment, preserved): only on a split
/// checkout, when the instance is not `prod` and `SPIRA_PROD`'s parent differs from
/// `SPIRA_HOME`'s parent — a test fixture or a no-split install shares the same parent and
/// is skipped.
pub fn seed_prod_instance_if_split(instance: &str, prod: &str, home: &str) -> Option<String> {
    if instance == "prod" || prod.is_empty() {
        return None;
    }
    let prod_root = Path::new(prod).parent()?;
    let home_root = Path::new(home).parent()?;
    if prod_root == home_root {
        return None;
    }
    let conf = prod_root.join("spira.conf"); // outside config-fence's scope (it watches the typed file and the repo map, not the legacy one)
    let toml = spira_config::toml_path_at(prod_root);
    seed_prod_instance(&conf, &toml, instance, Path::new(home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn skips_when_instance_is_prod_or_not_split() {
        assert!(seed_prod_instance_if_split("prod", "/p/spira", "/h/spira").is_none());
        assert!(seed_prod_instance_if_split("test", "", "/h/spira").is_none());
        // Same parent (no real split) is skipped too.
        assert!(seed_prod_instance_if_split("test", "/h/prod-spira", "/h/spira").is_none());
    }

    #[test]
    fn write_target_prefers_an_existing_toml() {
        let d = testkit::TempDir::new("seed-instance-test");
        let toml = spira_config::toml_path_at(&d);
        fs::write(&toml, "[spira]\n").unwrap();
        let conf = d.join("spira.conf");
        fs::write(&conf, "SPIRA_INSTANCE=x\n").unwrap();
        let t = write_target_for(&conf, &toml, &d, None, &[]).unwrap();
        assert_eq!(t, toml);
    }

    #[test]
    fn write_target_is_the_bare_toml_path_when_neither_file_exists() {
        let d = testkit::TempDir::new("seed-instance-test");
        let toml = spira_config::toml_path_at(&d);
        let conf = d.join("spira.conf");
        let t = write_target_for(&conf, &toml, &d, None, &[]).unwrap();
        assert_eq!(t, toml);
        assert!(!toml.exists(), "write_target_for only names the target; it does not create it itself");
    }
}
