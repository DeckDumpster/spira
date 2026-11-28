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

/// What a tree's flake offers as a dev shell. `DoesNotEnter` is only ever nix's own refusal;
/// a fault of the checker is an `Err` beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevShell {
    Entered(ShellEnv),
    Undeclared,
    DoesNotEnter(String),
}

/// Whether a `flake.nix` names `devShells`.
pub fn declares_dev_shell(flake_nix: &[u8]) -> bool {
    String::from_utf8_lossy(flake_nix).contains("devShells")
}

/// The environment cached for `key`, only while every PATH directory it names still exists:
/// the store paths of a realised shell can be garbage-collected.
pub fn cached_env(cache: &Path, key: &str) -> Option<ShellEnv> {
    let json = std::fs::read_to_string(cached_file(cache, key)).ok()?;
    let env = parse_dev_env(&json).ok().filter(|e| e.path_dirs().iter().all(|d| d.is_dir()))?;
    Some(ShellEnv { key: key.to_string(), ..env })
}

fn cached_file(cache: &Path, key: &str) -> PathBuf {
    cache.join(format!("{key}.json"))
}

/// The tree's flake dev shell; `Undeclared` (nothing spawned) when it has no `flake.nix`
/// or its flake names no `devShells`: `print-dev-env` would otherwise fall back to a package's
/// build environment, which is not a shell anyone declared.
/// Cached at `<cache>/<cache_key>.json`.
pub fn shell_env(tree: &Path, nix_bin: &Path, cache: &Path) -> Result<DevShell, String> {
    let Ok(flake_nix) = std::fs::read(tree.join("flake.nix")) else {
        return Ok(DevShell::Undeclared);
    };
    if !declares_dev_shell(&flake_nix) {
        return Ok(DevShell::Undeclared);
    }
    let flake_lock = std::fs::read(tree.join("flake.lock")).unwrap_or_default();
    let key = cache_key(&flake_nix, &flake_lock);
    if let Some(env) = cached_env(cache, &key) {
        return Ok(DevShell::Entered(env));
    }
    let cached = cached_file(cache, &key);
    let out = spira_config::bounded::bounded_for(nix_bin, "600").args(["print-dev-env", "--json"]).arg(tree).output().map_err(|e| format!("cannot run {}: {e}", nix_bin.display()))?;
    if !matches!(out.status.code(), Some(c) if !TIMEOUT_OWN_CODES.contains(&c)) {
        return Err(format!("cannot run {}: {}", nix_bin.display(), exit_words(&out)));
    }
    if !out.status.success() {
        return Ok(DevShell::DoesNotEnter(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let json = String::from_utf8_lossy(&out.stdout);
    let env = ShellEnv { key, ..parse_dev_env(&json)? };
    std::fs::create_dir_all(cache).map_err(|e| format!("cannot create {}: {e}", cache.display()))?;
    let tmp = cached.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, json.as_bytes()).and_then(|()| std::fs::rename(&tmp, &cached)).map_err(|e| format!("cannot write {}: {e}", cached.display()))?;
    Ok(DevShell::Entered(env))
}

/// The statuses `timeout` itself gives (deadline, bad deadline, not executable, not found): the
/// run never reached nix's verdict on the flake.
const TIMEOUT_OWN_CODES: [i32; 4] = [124, 125, 126, 127];

fn exit_words(out: &std::process::Output) -> String {
    match out.status.code() {
        Some(124) => "it hit its deadline".into(),
        Some(c) => format!("it exited {c}"),
        None => "it was killed by a signal".into(),
    }
}

/// The cache directory under the run dir.
pub fn cache_dir() -> Result<PathBuf, String> {
    Ok(spira_config::resolve::run_dir_for_process()?.join("repo-contract").join("shell"))
}

/// nix, from `$SPIRA_NIX_PROFILE_BIN`; a missing key is refused by name.
pub fn nix_bin() -> Result<PathBuf, String> {
    spira_config::process::nix_profile_program("nix")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/print-dev-env.json");

    #[test_id(tt1d2c8)]
    #[test]
    fn cache_key_changes_when_either_file_changes() {
        let base = cache_key(b"a", b"b");
        assert_ne!(base, cache_key(b"a2", b"b"));
        assert_ne!(base, cache_key(b"a", b"b2"));
        assert_ne!(cache_key(b"ab", b""), cache_key(b"a", b"b"));
        assert_eq!(base, cache_key(b"a", b"b"));
    }

    #[test_id(tujoigw)]
    #[test]
    fn parse_dev_env_keeps_only_exported_variables() {
        let env = parse_dev_env(FIXTURE).unwrap();
        assert!(env.vars["HOME"].ends_with("homeless-shelter"));
        assert!(!env.vars.contains_key("name") && !env.vars.contains_key("buildInputs") && !env.vars.contains_key("shellHook"));
        assert_eq!(env.path_dirs().len(), 3);
        assert!(env.row_vars().all(|(k, _)| k != "HOME" && k != "NIX_BUILD_TOP") && env.row_vars().any(|(k, _)| k == "out"));
        assert!(env.path_dirs()[0].ends_with("aaa-jq-1.7.1-bin/bin"));
    }

    #[test_id(ttx6u5d)]
    #[test]
    fn parse_dev_env_refuses_malformed_json_by_name() {
        assert!(parse_dev_env("{not json").unwrap_err().contains("not JSON"));
        assert!(parse_dev_env("{}").unwrap_err().contains("`variables`"));
        assert!(parse_dev_env(r#"{"variables":{}}"#).unwrap_err().contains("PATH"));
    }

    #[test_id(tc5bx3v)]
    #[test]
    fn a_tree_without_a_flake_has_no_shell_and_spawns_nothing() {
        let dir = testkit::TempDir::new("repo-contract-noflake");
        let r = shell_env(&dir, &dir.join("no-nix"), &dir.join("cache"));
        assert_eq!(r, Ok(DevShell::Undeclared));
    }

    #[test_id(tk2eud5)]
    #[test]
    fn a_cached_shell_whose_store_paths_are_gone_is_realised_again_not_trusted() {
        let dir = testkit::TempDir::new("repo-contract-gc");
        std::fs::write(dir.join("flake.nix"), "{ outputs = _: { devShells = {}; }; }").unwrap();
        let cache = dir.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let key = cache_key(&std::fs::read(dir.join("flake.nix")).unwrap(), b"");
        std::fs::write(cache.join(format!("{key}.json")), FIXTURE).unwrap();
        let r = shell_env(&dir, &dir.join("no-nix"), &cache);
        assert!(r.is_err());
    }

    #[test_id(t4avzjq)]
    #[test]
    fn a_flake_with_no_dev_shell_has_no_shell_and_spawns_nothing() {
        let dir = testkit::TempDir::new("repo-contract-nodevshell");
        std::fs::write(dir.join("flake.nix"), "{ outputs = _: { packages = {}; }; }").unwrap();
        let r = shell_env(&dir, &dir.join("no-nix"), &dir.join("cache"));
        assert_eq!(r, Ok(DevShell::Undeclared));
    }

    #[test_id(tj70yls)]
    #[test]
    fn a_cached_environment_is_read_without_running_nix() {
        let dir = testkit::TempDir::new("repo-contract-cached");
        let cache = dir.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(dir.join("flake.nix"), "devShells").unwrap();
        let live = format!(r#"{{"variables":{{"PATH":{{"type":"exported","value":"{}"}}}}}}"#, dir.display());
        std::fs::write(cache.join(format!("{}.json", cache_key(b"devShells", b""))), live).unwrap();
        let r = shell_env(&dir, &dir.join("no-nix"), &cache);
        assert!(matches!(r.unwrap(), DevShell::Entered(e) if e.vars.contains_key("PATH")));
    }

    #[test_id(thew85y)]
    #[test]
    fn cached_env_answers_only_a_primed_key_whose_path_dirs_exist() {
        let dir = testkit::TempDir::new("repo-contract-cached-env");
        let tools = dir.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        let live = format!(r#"{{"variables":{{"PATH":{{"type":"exported","value":"{}"}}}}}}"#, tools.display());
        std::fs::write(dir.join("k1.json"), live).unwrap();
        let env = cached_env(&dir, "k1").unwrap();
        assert_eq!((env.key.as_str(), env.path_dirs()), ("k1", vec![tools.clone()]));
        assert_eq!(cached_env(&dir, "k2"), None);
        std::fs::remove_dir(&tools).unwrap();
        assert_eq!(cached_env(&dir, "k1"), None);
    }

    fn fake_nix(dir: &Path, body: &str) -> PathBuf {
        let nix = dir.join("nix");
        testkit::write_exe(&nix, &format!("#!/bin/sh\n{body}\n"));
        nix
    }

    #[test_id(te4vsj7)]
    #[test]
    fn a_nix_that_refuses_the_flake_is_a_shell_that_does_not_enter() {
        let dir = testkit::TempDir::new("repo-contract-refused");
        std::fs::write(dir.join("flake.nix"), "devShells").unwrap();
        let nix = fake_nix(&dir, "echo boom >&2; exit 1");
        assert_eq!(shell_env(&dir, &nix, &dir.join("cache")), Ok(DevShell::DoesNotEnter("boom".into())));
    }

    #[test_id(tzfzk7q)]
    #[test]
    fn a_nix_that_cannot_be_run_is_a_fault_of_the_checker() {
        let dir = testkit::TempDir::new("repo-contract-nonix");
        std::fs::write(dir.join("flake.nix"), "devShells").unwrap();
        assert!(shell_env(&dir, &dir.join("no-nix"), &dir.join("cache")).unwrap_err().contains("cannot run"));
    }

    #[test_id(t7cvnvg)]
    #[test]
    fn a_nix_that_the_timeout_could_not_run_or_cut_is_a_fault_of_the_checker() {
        let dir = testkit::TempDir::new("repo-contract-timeout-codes");
        std::fs::write(dir.join("flake.nix"), "devShells").unwrap();
        for code in [124, 127] {
            let nix = fake_nix(&dir, &format!("exit {code}"));
            assert!(shell_env(&dir, &nix, &dir.join("cache")).unwrap_err().contains("cannot run"), "{code}");
        }
    }

    #[test_id(tikxrdq)]
    #[test]
    fn a_cache_that_cannot_be_written_is_a_fault_of_the_checker() {
        let dir = testkit::TempDir::new("repo-contract-cache-fault");
        std::fs::write(dir.join("flake.nix"), "devShells").unwrap();
        let json = dir.join("out.json");
        std::fs::write(&json, FIXTURE).unwrap();
        let nix = fake_nix(&dir, &format!("cat {}", json.display()));
        let cache = dir.join("cache-is-a-file");
        std::fs::write(&cache, "").unwrap();
        assert!(shell_env(&dir, &nix, &cache).unwrap_err().contains("cannot create"));
    }

    #[test_id(trp8n4h)]
    #[test]
    fn a_nix_that_prints_a_dev_env_enters_and_is_cached() {
        let dir = testkit::TempDir::new("repo-contract-enters");
        std::fs::write(dir.join("flake.nix"), "devShells").unwrap();
        let json = dir.join("out.json");
        std::fs::write(&json, FIXTURE).unwrap();
        let nix = fake_nix(&dir, &format!("cat {}", json.display()));
        let cache = dir.join("cache");
        assert!(matches!(shell_env(&dir, &nix, &cache), Ok(DevShell::Entered(e)) if e.path_dirs().len() == 3));
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
    }
}
