//! `_seed_prod_instance` (`systemd/install.sh`, in Rust): on a split dev/prod checkout,
//! write `SPIRA_INSTANCE=<instance>` into the separate `SPIRA_PROD` checkout's own config,
//! so every path its own `conf.sh` derives (`SPIRA_DB`, `SPIRA_RUN`, ...) inherits the
//! instance qualifier and its containment fence fires as intended. Always `spira.toml`,
//! through the real `spira-config` binary (`get`/`set`/`convert`) — never a hand-rolled
//! writer of either format, matching conf.sh's own `spira_config_set_at`, which this
//! shells to the same binary rather than re-deriving.

use std::path::Path;
use std::process::Command;

/// `spira_toml_write_target_for` (conf.sh): the toml path a writer at `conf`/`toml` should
/// target — `toml` itself if it exists; else a full `spira-config convert` from `conf` (plus
/// `repo_map` and every `fayth`) if `conf` exists; else `toml` itself (to be created fresh
/// by `spira-config set`). `None` only when a required convert fails.
fn write_target_for(conf: &str, toml: &str, home: &str, repo_map: Option<&str>, fayth: &[String]) -> Option<String> {
    if Path::new(toml).is_file() {
        return Some(toml.to_string());
    }
    if !Path::new(conf).is_file() {
        return Some(toml.to_string());
    }
    let mut cmd = Command::new("spira-config");
    cmd.arg("convert").arg("--conf").arg(conf).arg("--home").arg(home).arg("--out").arg(toml);
    if let Some(rm) = repo_map {
        cmd.arg("--repo-map").arg(rm);
    }
    for f in fayth {
        cmd.arg("--fayth").arg(f);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => Some(toml.to_string()),
        Ok(o) => {
            eprintln!("install: spira.conf auto-convert to spira.toml failed: {}", String::from_utf8_lossy(&o.stderr).trim());
            None
        }
        Err(e) => {
            eprintln!("install: cannot run spira-config convert: {e}");
            None
        }
    }
}

/// `_spira_repo_map_candidate` (conf.sh), simplified for a target root that is not this
/// process's own `SPIRA_HOME`: `<dirname conf>/repo-map`, else `<home>/repo-map.example`.
fn repo_map_candidate(conf: &str, home: &str) -> Option<String> {
    let d = Path::new(conf).parent()?;
    for c in [d.join("repo-map"), Path::new(home).join("repo-map.example")] {
        if c.is_file() {
            return Some(c.to_string_lossy().to_string());
        }
    }
    None
}

/// `_spira_fayth_paths` (conf.sh): every `*.fayth` under `<home>/chamber` (or
/// `$SPIRA_CHAMBER`), sorted for a deterministic convert argv.
fn fayth_paths(home: &str) -> Vec<String> {
    let dir = std::env::var("SPIRA_CHAMBER").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| format!("{home}/chamber"));
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("fayth") && p.is_file() {
                out.push(p.to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    out
}

/// `_seed_prod_instance <conf> <toml> <instance>`. Returns the message to print on stdout
/// when it wrote something (`install: seeded <target> with instance = <instance>`), `None`
/// when it was already correct or the write failed (the failure itself already reported on
/// stderr).
pub fn seed_prod_instance(conf: &str, toml: &str, instance: &str, home: &str) -> Option<String> {
    let rm = repo_map_candidate(conf, home);
    let fy = fayth_paths(home);
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
        Some(format!("install: seeded {target} with instance = {instance}"))
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
    let conf = prod_root.join("spira.conf");
    let toml = prod_root.join("spira.toml");
    seed_prod_instance(&conf.to_string_lossy(), &toml.to_string_lossy(), instance, home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn td() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("seed-instance-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn skips_when_instance_is_prod_or_not_split() {
        assert!(seed_prod_instance_if_split("prod", "/p/spira", "/h/spira").is_none());
        assert!(seed_prod_instance_if_split("test", "", "/h/spira").is_none());
        // Same parent (no real split) is skipped too.
        assert!(seed_prod_instance_if_split("test", "/h/prod-spira", "/h/spira").is_none());
    }

    #[test]
    fn write_target_prefers_an_existing_toml() {
        let d = td();
        let toml = d.join("spira.toml");
        fs::write(&toml, "[spira]\n").unwrap();
        let conf = d.join("spira.conf");
        fs::write(&conf, "SPIRA_INSTANCE=x\n").unwrap();
        let t = write_target_for(&conf.to_string_lossy(), &toml.to_string_lossy(), &d.to_string_lossy(), None, &[]).unwrap();
        assert_eq!(t, toml.to_string_lossy());
    }

    #[test]
    fn write_target_is_the_bare_toml_path_when_neither_file_exists() {
        let d = td();
        let toml = d.join("spira.toml");
        let conf = d.join("spira.conf");
        let t = write_target_for(&conf.to_string_lossy(), &toml.to_string_lossy(), &d.to_string_lossy(), None, &[]).unwrap();
        assert_eq!(t, toml.to_string_lossy());
        assert!(!toml.exists(), "write_target_for only names the target; it does not create it itself");
    }
}
