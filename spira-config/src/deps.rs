//! The dependency manifest (wave4-decomposition.md row C7 "deps / units"; bead sp-wqj3o,
//! "wave 4.10: small conf.sh families") — `conf.sh`'s own `spira_require`,
//! `spira_deps_list`, `spira_bin_tier`, `spira_bin_purpose` and `spira_bin_absent`, ported
//! off a `python3 -c 'import tomllib'` subshell run unconditionally at every source of
//! `conf.sh` onto one `toml` parse, read lazily — in-process — only by whichever function a
//! caller actually invokes. A box without `python3` used to silently lose this whole family
//! (`conf.sh`'s own `command -v python3` guard); there is no such fallback left to need,
//! because nothing here depends on `python3` at all now.
//!
//! `deps.toml` itself (`spira/deps.toml`) is untouched by this bead: same fields, same file,
//! same authority (doctor's own checks, `testenv/doctor-check.sh`, `test-bin-manifest.sh`).
//! Only `version_min`/`version_probe` are left unread here — nothing in the five functions
//! this module replaces ever read them either; `test-bin-manifest.sh` parses those two
//! fields itself, directly, as its own expected-value oracle.

use serde::Deserialize;
use std::path::Path;

/// The two fallbacks `conf.sh`'s own functions gave an undeclared name, or a declared one
/// missing the field — preserved byte for byte because `test-bin-manifest.sh` asserts on
/// both as its positive control.
pub const DEFAULT_TIER: &str = "optional";
pub const DEFAULT_PURPOSE: &str = "required by the harness";

#[derive(Debug, Clone, Deserialize)]
struct RawDep {
    name: String,
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    absent: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawManifest {
    #[serde(default)]
    dep: Vec<RawDep>,
}

/// One declared program, with its two fallbacks already applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    pub name: String,
    pub tier: String,
    pub purpose: String,
    pub absent: String,
}

/// Every `[[dep]]` in `deps.toml`, in file order — `toml`'s array-of-tables parse keeps the
/// document's own order, the same order bash's python loader walked `data.get("dep", [])`
/// in, so `spira_deps_list`'s callers see entries in the same order they always did. A
/// missing or unparseable file answers an empty manifest, exactly like bash's own
/// `[ -f "$_SPIRA_DEPS" ]` guard did — never a hard refusal, because every lookup below still
/// has a fallback to answer with.
pub fn load(path: &Path) -> Vec<Dep> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(raw) = toml::from_str::<RawManifest>(&text) else { return Vec::new() };
    raw.dep
        .into_iter()
        .map(|d| Dep {
            name: d.name,
            tier: d.tier.filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_TIER.to_string()),
            purpose: d.purpose.filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_PURPOSE.to_string()),
            absent: d.absent.unwrap_or_default(),
        })
        .collect()
}

/// `spira_deps_list [tier]` — every declared name, optionally filtered to one tier.
pub fn list(deps: &[Dep], tier: Option<&str>) -> Vec<String> {
    deps.iter()
        .filter(|d| tier.map_or(true, |t| d.tier == t))
        .map(|d| d.name.clone())
        .collect()
}

/// `spira_bin_tier <bin>` -> its declared tier, or [`DEFAULT_TIER`] for a name `deps.toml`
/// never named.
pub fn tier_of(deps: &[Dep], name: &str) -> String {
    deps.iter().find(|d| d.name == name).map(|d| d.tier.clone()).unwrap_or_else(|| DEFAULT_TIER.to_string())
}

/// `spira_bin_purpose <bin>` -> its declared purpose, or [`DEFAULT_PURPOSE`].
pub fn purpose_of(deps: &[Dep], name: &str) -> String {
    deps.iter().find(|d| d.name == name).map(|d| d.purpose.clone()).unwrap_or_else(|| DEFAULT_PURPOSE.to_string())
}

/// `spira_bin_absent <bin>` -> what actually happens on a box without it, or `""` when
/// `deps.toml` never said (most programs: absence just means the feature is off).
pub fn absent_of(deps: &[Dep], name: &str) -> String {
    deps.iter().find(|d| d.name == name).map(|d| d.absent.clone()).unwrap_or_default()
}

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// `command -v <name>` over `path` (a colon-separated PATH string) — the same search
/// [`crate::env_bootstrap::resolve_bd`] makes for `bd`, generalised to any name. An empty
/// segment means the current directory, exactly as the shell resolves one.
fn which(path: &str, name: &str) -> bool {
    path.split(':').any(|dir| {
        let dir = if dir.is_empty() { "." } else { dir };
        is_executable_file(&Path::new(dir).join(name))
    })
}

/// `spira_require <bin> [<bin>...]` -> `Ok(())`, or `Err` holding the exact diagnostic text
/// `conf.sh`'s own version printed to stderr: one line per missing program naming its
/// purpose, then the PATH that was searched, then where to fix it.
pub fn require(deps: &[Dep], bins: &[&str], path: &str, conf_file: &str) -> Result<(), String> {
    let missing: Vec<&str> = bins.iter().copied().filter(|b| !which(path, b)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut msg = String::new();
    for b in &missing {
        msg.push_str(&format!(
            "spira: required program not found on PATH: {b} — {}\n",
            purpose_of(deps, b)
        ));
    }
    msg.push_str(&format!("spira: PATH is {path}\n"));
    msg.push_str(&format!("spira: if it is installed elsewhere, set SPIRA_PATH in {conf_file}\n"));
    Err(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[dep]]
name = "bd"
tier = "runtime"
purpose = "the beads issue tracker"
version_min = ""
version_probe = "bd --version"

[[dep]]
name = "duckdb"
tier = "optional"
purpose = "tsd queries"
version_probe = "duckdb --version"

[[dep]]
name = "bd-embedded"
tier = "operator"
purpose = "the embedded dolt engine"
absent = "falls back to a shared Dolt server"
version_probe = ""
"#;

    fn write_sample(dir: &std::path::Path) -> std::path::PathBuf {
        let p = dir.join("deps.toml");
        std::fs::write(&p, SAMPLE).unwrap();
        p
    }

    #[test]
    fn loads_in_file_order() {
        let dir = testkit::TempDir::new("spira-config-deps-order");
        let deps = load(&write_sample(dir.path()));
        assert_eq!(deps.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["bd", "duckdb", "bd-embedded"]);
    }

    #[test]
    fn missing_file_is_an_empty_manifest_not_a_refusal() {
        let deps = load(Path::new("/nonexistent-sp-wqj3o/deps.toml"));
        assert!(deps.is_empty());
    }

    #[test]
    fn tier_and_purpose_fall_back_for_an_undeclared_name() {
        let dir = testkit::TempDir::new("spira-config-deps-fallback");
        let deps = load(&write_sample(dir.path()));
        assert_eq!(tier_of(&deps, "spira-no-such-program"), DEFAULT_TIER);
        assert_eq!(purpose_of(&deps, "spira-no-such-program"), DEFAULT_PURPOSE);
        assert_eq!(absent_of(&deps, "spira-no-such-program"), "");
    }

    #[test]
    fn declared_fields_read_back_exactly() {
        let dir = testkit::TempDir::new("spira-config-deps-fields");
        let deps = load(&write_sample(dir.path()));
        assert_eq!(tier_of(&deps, "bd"), "runtime");
        assert_eq!(purpose_of(&deps, "bd"), "the beads issue tracker");
        assert_eq!(absent_of(&deps, "bd"), "");
    }

    #[test]
    fn a_silent_downgrade_declares_what_it_downgrades_to() {
        let dir = testkit::TempDir::new("spira-config-deps-absent");
        let deps = load(&write_sample(dir.path()));
        assert!(absent_of(&deps, "bd-embedded").contains("shared Dolt server"));
    }

    #[test]
    fn list_filters_by_tier() {
        let dir = testkit::TempDir::new("spira-config-deps-list");
        let deps = load(&write_sample(dir.path()));
        assert_eq!(list(&deps, None), vec!["bd", "duckdb", "bd-embedded"]);
        assert_eq!(list(&deps, Some("optional")), vec!["duckdb"]);
    }

    #[test]
    fn require_passes_when_every_program_is_on_path() {
        let dir = testkit::TempDir::new("spira-config-deps-require-ok");
        let bin = dir.path().join("fake-bd");
        testkit::write_exe(&bin, "#!/bin/sh\n");
        let deps = load(&write_sample(dir.path()));
        let path = dir.path().to_string_lossy().into_owned();
        assert!(require(&deps, &["fake-bd"], &path, "spira.conf").is_ok());
    }

    #[test]
    fn require_names_each_missing_program_and_its_purpose() {
        let dir = testkit::TempDir::new("spira-config-deps-require-missing");
        let deps = load(&write_sample(dir.path()));
        let path = dir.path().to_string_lossy().into_owned();
        let err = require(&deps, &["bd", "duckdb"], &path, "/etc/spira/spira.conf").unwrap_err();
        assert!(err.contains("bd — the beads issue tracker"), "{err}");
        assert!(err.contains("duckdb — tsd queries"), "{err}");
        assert!(err.contains(&format!("PATH is {path}")), "{err}");
        assert!(err.contains("set SPIRA_PATH in /etc/spira/spira.conf"), "{err}");
    }

    #[test]
    fn require_names_an_undeclared_missing_program_with_the_fallback_purpose() {
        let dir = testkit::TempDir::new("spira-config-deps-require-undeclared");
        let deps = load(&write_sample(dir.path()));
        let path = dir.path().to_string_lossy().into_owned();
        let err = require(&deps, &["spira-no-such-program"], &path, "spira.conf").unwrap_err();
        assert!(err.contains(&format!("spira-no-such-program — {DEFAULT_PURPOSE}")), "{err}");
    }
}
