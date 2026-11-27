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
//! same authority (doctor's own checks, `testenv/doctor-check.sh`).
//! `version_min` is read for `release verify`; `version_probe` is read only by the
//! shipped-manifest test below.

use serde::Deserialize;
use std::path::Path;

/// The two fallbacks `conf.sh`'s own functions gave an undeclared name, or a declared one
/// missing the field — preserved byte for byte; the shipped-manifest test uses
/// them as its positive control.
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
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    version_min: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawCompat {
    name: String,
    alias: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawManifest {
    #[serde(default)]
    dep: Vec<RawDep>,
    #[serde(default)]
    compat: Vec<RawCompat>,
}

/// One `[[compat]]` row: a real binary and the old bare-name alias that must also resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compat {
    pub name: String,
    pub alias: String,
}

/// The whole manifest, typed: `[[dep]]` entries with fallbacks applied, and `[[compat]]`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub deps: Vec<Dep>,
    pub compat: Vec<Compat>,
}

/// One declared program, with its two fallbacks already applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    pub name: String,
    pub tier: String,
    pub purpose: String,
    pub absent: String,
    pub provider: String,
    pub version_min: String,
}

/// Every `[[dep]]` in `deps.toml`, in file order — `toml`'s array-of-tables parse keeps the
/// document's own order, the same order bash's python loader walked `data.get("dep", [])`
/// in, so `spira_deps_list`'s callers see entries in the same order they always did. A
/// missing or unparseable file answers an empty manifest, exactly like bash's own
/// `[ -f "$_SPIRA_DEPS" ]` guard did — never a hard refusal, because every lookup below still
/// has a fallback to answer with.
pub fn load(path: &Path) -> Vec<Dep> {
    load_manifest(path).deps
}

/// Strict parse of `deps.toml` text: `Err` carries the parser's message. Callers that must
/// refuse on an unreadable manifest use this; the lenient readers wrap it.
pub fn parse(text: &str) -> Result<Manifest, String> {
    let raw = toml::from_str::<RawManifest>(text).map_err(|e| e.to_string())?;
    Ok(Manifest {
        deps: raw
            .dep
            .into_iter()
            .map(|d| Dep {
                name: d.name,
                tier: d.tier.filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_TIER.to_string()),
                purpose: d.purpose.filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_PURPOSE.to_string()),
                absent: d.absent.unwrap_or_default(),
                provider: d.provider.unwrap_or_default(),
                version_min: d.version_min.map(|v| v.trim().to_string()).unwrap_or_default(),
            })
            .collect(),
        compat: raw.compat.into_iter().map(|c| Compat { name: c.name, alias: c.alias }).collect(),
    })
}

/// [`parse`] over a file; a missing or unparseable file answers an empty manifest.
pub fn load_manifest(path: &Path) -> Manifest {
    std::fs::read_to_string(path).ok().and_then(|t| parse(&t).ok()).unwrap_or_default()
}

/// `spira_deps_list [tier]` — every declared name, optionally filtered to one tier.
pub fn list(deps: &[Dep], tier: Option<&str>) -> Vec<String> {
    deps.iter()
        .filter(|d| tier.is_none_or(|t| d.tier == t))
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

/// Every program `deps.toml` marks `provider = "<provider>"`, in file order.
pub fn provided_by(deps: &[Dep], provider: &str) -> Vec<String> {
    deps.iter().filter(|d| d.provider == provider).map(|d| d.name.clone()).collect()
}

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// `command -v <name>` over `path` (a colon-separated PATH string). An empty segment means the current directory, exactly as the shell resolves one.
pub fn which(path: &str, name: &str) -> Option<std::path::PathBuf> {
    path.split(':')
        .map(|dir| Path::new(if dir.is_empty() { "." } else { dir }).join(name))
        .find(|p| is_executable_file(p))
}

/// `spira_require <bin> [<bin>...]` -> `Ok(())`, or `Err` holding the exact diagnostic text
/// `conf.sh`'s own version printed to stderr: one line per missing program naming its
/// purpose, then the PATH that was searched, then where to fix it.
pub fn require(deps: &[Dep], bins: &[&str], path: &str, conf_file: &str) -> Result<(), String> {
    let missing: Vec<&str> = bins.iter().copied().filter(|b| which(path, b).is_none()).collect();
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

    #[test_id(tyh9kkw)]
    #[test]
    fn provided_by_lists_only_the_marked_programs() {
        let m = parse("[[dep]]\nname = \"bd\"\nprovider = \"spira-env\"\n[[dep]]\nname = \"git\"\n[[dep]]\nname = \"dolt\"\nprovider = \"spira-env\"\n").unwrap();
        assert_eq!(provided_by(&m.deps, "spira-env"), vec!["bd", "dolt"]);
        assert!(provided_by(&m.deps, "other").is_empty());
    }

    #[test_id(t88lew3)]
    #[test]
    fn loads_in_file_order() {
        let dir = testkit::TempDir::new("spira-config-deps-order");
        let deps = load(&write_sample(dir.path()));
        assert_eq!(deps.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["bd", "duckdb", "bd-embedded"]);
    }

    #[test_id(t14xm0a)]
    #[test]
    fn missing_file_is_an_empty_manifest_not_a_refusal() {
        let deps = load(Path::new("nonexistent-sp-wqj3o/deps.toml"));
        assert!(deps.is_empty());
    }

    #[test_id(th7ygwa)]
    #[test]
    fn tier_and_purpose_fall_back_for_an_undeclared_name() {
        let dir = testkit::TempDir::new("spira-config-deps-fallback");
        let deps = load(&write_sample(dir.path()));
        assert_eq!(tier_of(&deps, "spira-no-such-program"), DEFAULT_TIER);
        assert_eq!(purpose_of(&deps, "spira-no-such-program"), DEFAULT_PURPOSE);
        assert_eq!(absent_of(&deps, "spira-no-such-program"), "");
    }

    #[test_id(tkp1m0k)]
    #[test]
    fn declared_fields_read_back_exactly() {
        let dir = testkit::TempDir::new("spira-config-deps-fields");
        let deps = load(&write_sample(dir.path()));
        assert_eq!(tier_of(&deps, "bd"), "runtime");
        assert_eq!(purpose_of(&deps, "bd"), "the beads issue tracker");
        assert_eq!(absent_of(&deps, "bd"), "");
    }

    #[test_id(tcxpwjy)]
    #[test]
    fn a_silent_downgrade_declares_what_it_downgrades_to() {
        let dir = testkit::TempDir::new("spira-config-deps-absent");
        let deps = load(&write_sample(dir.path()));
        assert!(absent_of(&deps, "bd-embedded").contains("shared Dolt server"));
    }

    #[test_id(tggh4rz)]
    #[test]
    fn list_filters_by_tier() {
        let dir = testkit::TempDir::new("spira-config-deps-list");
        let deps = load(&write_sample(dir.path()));
        assert_eq!(list(&deps, None), vec!["bd", "duckdb", "bd-embedded"]);
        assert_eq!(list(&deps, Some("optional")), vec!["duckdb"]);
    }

    #[test_id(tsxl6fy)]
    #[test]
    fn require_names_each_missing_program_and_its_purpose() {
        let dir = testkit::TempDir::new("spira-config-deps-require-missing");
        let deps = load(&write_sample(dir.path()));
        let path = dir.path().to_string_lossy().into_owned();
        let err = require(&deps, &["bd", "duckdb"], &path, "etc/spira/spira.conf").unwrap_err();
        assert!(err.contains("bd — the beads issue tracker"), "{err}");
        assert!(err.contains("duckdb — tsd queries"), "{err}");
        assert!(err.contains(&format!("PATH is {path}")), "{err}");
        assert!(err.contains("set SPIRA_PATH in etc/spira/spira.conf"), "{err}");
    }

    #[test_id(tva6qbf)]
    #[test]
    fn require_names_an_undeclared_missing_program_with_the_fallback_purpose() {
        let dir = testkit::TempDir::new("spira-config-deps-require-undeclared");
        let deps = load(&write_sample(dir.path()));
        let path = dir.path().to_string_lossy().into_owned();
        let err = require(&deps, &["spira-no-such-program"], &path, "spira.conf").unwrap_err();
        assert!(err.contains(&format!("spira-no-such-program — {DEFAULT_PURPOSE}")), "{err}");
    }

    // covers: UC-config-store-preflight-08
    #[test_id(tjzz1hq)]
    #[test]
    fn every_program_the_shipped_manifest_declares_has_a_tier_a_purpose_and_a_version_probe() {
        let text = std::fs::read_to_string(crate::process::fixture_home().join("deps.toml")).unwrap();
        let manifest = parse(&text).unwrap();
        assert!(manifest.deps.len() >= 8, "the manifest loaded: {} programs", manifest.deps.len());
        let raw: toml::Value = text.parse().unwrap();
        let probes: std::collections::BTreeMap<&str, &str> = raw["dep"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (d["name"].as_str().unwrap(), d.get("version_probe").and_then(|v| v.as_str()).unwrap_or("")))
            .collect();
        let bad = |f: &dyn Fn(&Dep) -> bool| manifest.deps.iter().filter(|d| f(d)).map(|d| d.name.as_str()).collect::<Vec<_>>();
        let tiers = ["runtime", "optional", "dev", "operator", "release"];
        assert_eq!(bad(&|d| !tiers.contains(&d.tier.as_str())), Vec::<&str>::new(), "a program with no valid tier");
        assert_eq!(bad(&|d| d.purpose == DEFAULT_PURPOSE), Vec::<&str>::new(), "a program stating no purpose");
        assert_eq!(bad(&|d| probes[d.name.as_str()].is_empty()), Vec::<&str>::new(), "a program with no version probe");
        assert!(absent_of(&manifest.deps, "bd-embedded").contains("shared Dolt server"), "a silent downgrade names what it falls back to");
        assert_eq!(tier_of(&manifest.deps, "spira-no-such-program"), DEFAULT_TIER, "positive control: an undeclared program is untiered");
    }
}
