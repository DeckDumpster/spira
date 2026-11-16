//! The environment of a repository's flake default dev shell.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The variables a dev shell exports, PATH among them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellEnv {
    pub vars: BTreeMap<String, String>,
    pub key: String,
}

impl ShellEnv {
    /// The shell's PATH directories, empty when it exports none.
    pub fn path_dirs(&self) -> Vec<PathBuf> {
        self.vars.get("PATH").map(|p| p.split(':').filter(|d| !d.is_empty()).map(PathBuf::from).collect()).unwrap_or_default()
    }
}

/// Variables of nix's build sandbox, not of the repository's tools: HOME names a directory that
/// does not exist, and the temp dirs are the sandbox's own.
const SANDBOX_VARS: &[&str] = &["HOME", "TMP", "TMPDIR", "TEMP", "TEMPDIR", "NIX_BUILD_TOP"];

impl ShellEnv {
    /// What a row's process receives from the shell: every exported variable but the sandbox's.
    pub fn row_vars(&self) -> impl Iterator<Item = (&String, &String)> {
        self.vars.iter().filter(|(k, _)| !SANDBOX_VARS.contains(&k.as_str()))
    }
}

/// A key for the cached environment: it changes when either flake file changes.
pub fn cache_key(flake_nix: &[u8], flake_lock: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for b in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
            h = (h ^ u64::from(*b)).wrapping_mul(0x100000001b3);
        }
    };
    feed(flake_nix);
    feed(flake_lock);
    format!("{h:016x}")
}

/// The exported variables of `nix print-dev-env --json` output.
pub fn parse_dev_env(json: &str) -> Result<ShellEnv, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("print-dev-env output is not JSON: {e}"))?;
    let vars = v.get("variables").and_then(|v| v.as_object()).ok_or("print-dev-env output has no `variables` object")?;
    let vars: BTreeMap<String, String> = vars
        .iter()
        .filter(|(_, d)| d.get("type").and_then(|t| t.as_str()) == Some("exported"))
        .filter_map(|(k, d)| Some((k.clone(), d.get("value")?.as_str()?.to_string())))
        .collect();
    if !vars.contains_key("PATH") {
        return Err("print-dev-env output exports no PATH".into());
    }
    Ok(ShellEnv { vars, key: String::new() })
}

/// The tree's flake dev shell environment; `None` (nothing spawned) when it has no `flake.nix`
/// or its flake names no `devShells`: `print-dev-env` would otherwise fall back to a package's
/// build environment, which is not a shell anyone declared.
/// Cached at `<cache>/<cache_key>.json`.
pub fn shell_env(tree: &Path, nix_bin: &Path, cache: &Path) -> Result<Option<ShellEnv>, String> {
    let Ok(flake_nix) = std::fs::read(tree.join("flake.nix")) else {
        return Ok(None);
    };
    if !String::from_utf8_lossy(&flake_nix).contains("devShells") {
        return Ok(None);
    }
    let flake_lock = std::fs::read(tree.join("flake.lock")).unwrap_or_default();
    let key = cache_key(&flake_nix, &flake_lock);
    let cached = cache.join(format!("{key}.json"));
    if let Some(env) = std::fs::read_to_string(&cached).ok().and_then(|j| parse_dev_env(&j).ok()) {
        return Ok(Some(ShellEnv { key, ..env }));
    }
    let out = spira_config::bounded::bounded_for(nix_bin, "600").args(["print-dev-env", "--json"]).arg(tree).output().map_err(|e| format!("cannot run {}: {e}", nix_bin.display()))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let json = String::from_utf8_lossy(&out.stdout);
    let env = ShellEnv { key, ..parse_dev_env(&json)? };
    std::fs::create_dir_all(cache).map_err(|e| format!("cannot create {}: {e}", cache.display()))?;
    let tmp = cached.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, json.as_bytes()).and_then(|()| std::fs::rename(&tmp, &cached)).map_err(|e| format!("cannot write {}: {e}", cached.display()))?;
    Ok(Some(env))
}

/// The cache directory under the run dir.
pub fn cache_dir() -> Result<PathBuf, String> {
    Ok(spira_config::resolve::run_dir_for_process()?.join("repo-contract").join("shell"))
}

/// nix, from `$SPIRA_NIX_PROFILE_BIN`; a missing key is refused by name.
pub fn nix_bin() -> Result<PathBuf, String> {
    let dir = spira_config::process::cfg("SPIRA_NIX_PROFILE_BIN")?;
    if dir.is_empty() {
        return Err("SPIRA_NIX_PROFILE_BIN is not set; `work checks` cannot find nix".into());
    }
    Ok(Path::new(&dir).join("nix"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/print-dev-env.json");

    #[test]
    fn cache_key_changes_when_either_file_changes() {
        let base = cache_key(b"a", b"b");
        assert_ne!(base, cache_key(b"a2", b"b"));
        assert_ne!(base, cache_key(b"a", b"b2"));
        assert_ne!(cache_key(b"ab", b""), cache_key(b"a", b"b"));
        assert_eq!(base, cache_key(b"a", b"b"));
    }

    #[test]
    fn parse_dev_env_keeps_only_exported_variables() {
        let env = parse_dev_env(FIXTURE).unwrap();
        assert_eq!(env.vars["HOME"], "/homeless-shelter");
        assert!(!env.vars.contains_key("name") && !env.vars.contains_key("buildInputs") && !env.vars.contains_key("shellHook"));
        assert_eq!(env.path_dirs().len(), 3);
        assert!(env.row_vars().all(|(k, _)| k != "HOME" && k != "NIX_BUILD_TOP") && env.row_vars().any(|(k, _)| k == "out"));
        assert_eq!(env.path_dirs()[0], PathBuf::from("/nix/store/aaa-jq-1.7.1-bin/bin"));
    }

    #[test]
    fn parse_dev_env_refuses_malformed_json_by_name() {
        assert!(parse_dev_env("{not json").unwrap_err().contains("not JSON"));
        assert!(parse_dev_env("{}").unwrap_err().contains("`variables`"));
        assert!(parse_dev_env(r#"{"variables":{}}"#).unwrap_err().contains("PATH"));
    }

    #[test]
    fn a_tree_without_a_flake_has_no_shell_and_spawns_nothing() {
        let dir = testkit::TempDir::new("repo-contract-noflake");
        let r = shell_env(&dir, Path::new("/nonexistent/nix"), &dir.join("cache"));
        assert_eq!(r, Ok(None));
    }

    #[test]
    fn a_flake_with_no_dev_shell_has_no_shell_and_spawns_nothing() {
        let dir = testkit::TempDir::new("repo-contract-nodevshell");
        std::fs::write(dir.join("flake.nix"), "{ outputs = _: { packages = {}; }; }").unwrap();
        let r = shell_env(&dir, Path::new("/nonexistent/nix"), &dir.join("cache"));
        assert_eq!(r, Ok(None));
    }

    #[test]
    fn a_cached_environment_is_read_without_running_nix() {
        let dir = testkit::TempDir::new("repo-contract-cached");
        let cache = dir.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(dir.join("flake.nix"), "devShells").unwrap();
        std::fs::write(cache.join(format!("{}.json", cache_key(b"devShells", b""))), FIXTURE).unwrap();
        let r = shell_env(&dir, Path::new("/nonexistent/nix"), &cache);
        assert!(r.unwrap().unwrap().vars.contains_key("PATH"));
    }
}
