//! THE ONE DOOR to config for every binary (per Ryan 2026-10-05: one source of config).
//!
//! A process reads its configuration here and nowhere else: the file `$SPIRA_TOML` names,
//! resolved once per process. No `env::var("SPIRA_X")`, no default, no fallback — a key the
//! file does not declare is an error, and so is a resolution that fails. A caller that
//! cannot do its job without a value refuses, naming the key; it never invents one.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::OnceLock;

use crate::resolve::Resolved;

static CONFIG: OnceLock<Result<Resolved, String>> = OnceLock::new();

/// This process's resolved config: `$SPIRA_TOML`, read once.
pub fn config() -> Result<&'static Resolved, String> {
    CONFIG
        .get_or_init(|| {
            let env: BTreeMap<String, String> = std::env::vars().collect();
            let home = crate::resolve::locate_home_for_process()?;
            let repo = crate::resolve::derive_home_repo(&home, &env);
            crate::resolve::resolve_for_process(&home, &repo, &env)
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// The environment variable naming the one config spec: for a caller that must strip or hand
/// it on by name (a production-locator guard) rather than read it.
pub const TOML_ENV: &str = "SPIRA_TOML";

/// The spec `$SPIRA_TOML` names — one file, or `base:override` layers — for a caller that must
/// HAND it on (a rendered unit, a launched process) rather than read it: the only door onto the
/// spec's location (spira-config is the only door onto spira.toml). Unset is a refusal.
pub fn spec() -> Result<String, String> {
    std::env::var("SPIRA_TOML")
        .ok()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "SPIRA_TOML is not set — it names the one source of config".to_string())
}

/// Keys whose resolved value is not simply what the file declares.
const COMPUTED_KEYS: &[&str] = &["SPIRA_RELEASE_REPO", "SPIRA_AGENT", "SPIRA_RUNES_DIR"];

/// `key` as the merged `[spira]` table declares it, when that is the whole answer: a declared,
/// plainly-valued key of a `prod` instance. Anything else is `None` and takes the full resolve,
/// which owns every refusal; so this never answers where resolution would have refused.
pub(crate) fn declared_value(merged: &toml::map::Map<String, toml::Value>, key: &str) -> Option<String> {
    if !(key.starts_with("SPIRA_") || key.starts_with("COCKPIT_")) || COMPUTED_KEYS.contains(&key) {
        return None;
    }
    let spira = merged.get("spira")?.as_table()?;
    let instance = crate::shell_value(spira.get("instance")?)?;
    if instance != "prod" {
        return None;
    }
    crate::shell_value(spira.get(&crate::registry::field_of(key))?)
}

fn declared_fast(key: &str) -> Option<String> {
    static MERGED: OnceLock<Option<toml::map::Map<String, toml::Value>>> = OnceLock::new();
    let merged = MERGED.get_or_init(|| {
        let retired = std::env::var(crate::LIFECYCLE_ENFORCE_ENV).ok();
        crate::check_lifecycle_switch_env(retired.as_deref()).ok()?;
        crate::merge_layers(&spec().ok()?).ok()
    });
    declared_value(merged.as_ref()?, key)
}

/// One key's declared value. `Err` when the config cannot be resolved or the key is not a
/// registered config key — never a default.
pub fn cfg(key: &str) -> Result<String, String> {
    if let Some(v) = declared_fast(key) {
        return Ok(v);
    }
    config()?
        .values
        .get(key)
        .cloned()
        .ok_or_else(|| {
            let registry = crate::resolve::locate_home_for_process().map(|h| h.join("conf.d").display().to_string()).unwrap_or_else(|e| e);
            format!("{key} is not a registered config key (spira/conf.d) — refusing; registry read: {registry}")
        })
}

/// The startup self-check of a long-running binary: this process's registry loads (refusing with
/// `no config registry at <path>`), its config resolves, and every key in `keys` is registered
/// and resolves. `Ok` is a summary line for `check-config` to print after `ok`.
pub fn check_config(keys: &[&str]) -> Result<String, String> {
    let reg = crate::registry::for_process()?;
    let resolved = config()?;
    let bad: Vec<&str> = keys.iter().copied().filter(|k| reg.get(k).is_none() || !resolved.values.contains_key(*k)).collect();
    if !bad.is_empty() {
        return Err(format!("unregistered or unresolved config keys: {} — refusing", bad.join(", ")));
    }
    Ok(format!("{} registered keys, {} checked", reg.keys.len(), keys.len()))
}

/// The `check-config` verb of a long-running binary: prints `ok <summary>` and returns 0, or
/// names the refusal on stderr under `who` and returns 1.
pub fn check_config_verb(who: &str, keys: &[&str]) -> i32 {
    match check_config(keys) {
        Ok(summary) => {
            println!("ok {summary}");
            0
        }
        Err(e) => {
            eprintln!("{who}: {e}");
            1
        }
    }
}

/// The metrics endpoint components push to; `None` when config does not resolve or declares none.
pub fn metrics_endpoint() -> Option<String> {
    cfg("SPIRA_METRICS_ENDPOINT").ok()
}

/// [`cfg`], parsed. A declared value that does not parse is an error naming the key.
pub fn cfg_parse<T: FromStr>(key: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    let v = cfg(key)?;
    v.trim().parse::<T>().map_err(|e| format!("{key} = {v:?} in spira.toml does not parse: {e}"))
}

/// The metrics sink for `service`: `SPIRA_METRICS_ENDPOINT` as declared, disabled when the config
/// does not resolve. Short deadline: a push never holds its producer.
pub fn metrics_sink(service: &str) -> spira_otel::metrics::MetricsSink {
    let endpoint = cfg("SPIRA_METRICS_ENDPOINT").ok();
    spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), service, std::time::Duration::from_millis(500))
}

/// The complete fixture's text, read from the harness tree this crate was built in.
pub fn complete_fixture_text() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/testlib/complete.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A release directory for a test: `<dir>/release/spira` linking this harness tree's
/// `conf.toml.d`, and its `conf.d` when `with_conf_d`. Point `SPIRA_RELEASE` at the returned path.
pub fn fixture_release(dir: &std::path::Path, with_conf_d: bool) -> std::path::PathBuf {
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let release = dir.join("release");
    std::fs::create_dir_all(release.join("spira")).expect("the fixture release is created");
    let dirs: &[&str] = if with_conf_d { &["conf.d", "conf.toml.d"] } else { &["conf.toml.d"] };
    for d in dirs {
        let link = release.join("spira").join(d);
        if !link.exists() {
            std::os::unix::fs::symlink(tree.join(d), &link).expect("the registry is linked");
        }
    }
    release
}

/// A harness home for a test that writes into it: `<dir>/home` holding copies of this tree's
/// `conf.d` and `conf.toml.d`. Point `SPIRA_HOME` at the returned path.
pub fn fixture_registry_home(dir: &std::path::Path) -> std::path::PathBuf {
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let home = dir.join("home");
    for d in ["conf.d", "conf.toml.d"] {
        std::fs::create_dir_all(home.join(d)).expect("the fixture home is created");
        for e in std::fs::read_dir(tree.join(d)).expect("the registry is readable").flatten() {
            std::fs::copy(e.path(), home.join(d).join(e.file_name())).expect("a registry file is copied");
        }
    }
    home
}

/// A complete `spira.toml` for TESTS in any crate: every key declared (`spira/testlib/complete.toml`), with `declare`'s `SPIRA_<KEY>` pairs written as
/// `spira.<key>`. Written to `<dir>/spira.toml`; point `SPIRA_TOML` at the returned path.
pub fn fixture_toml(dir: &std::path::Path, declare: &[(&str, &str)]) -> std::path::PathBuf {
    let mut d = crate::validate(&complete_fixture_text()).expect("the complete fixture validates");
    for (k, v) in declare {
        let rest = k.strip_prefix("SPIRA_").unwrap_or(k);
        d = crate::set_path(&d, &format!("spira.{}", rest.to_ascii_lowercase()), v)
            .unwrap_or_else(|e| panic!("fixture_toml: cannot declare {k}: {e}"));
    }
    let p = dir.join(crate::FILE_NAME);
    crate::serialize_and_write(&p, &d).expect("the fixture writes");
    p
}

/// The environment that points a spawned binary's config door at a fixture declaring
/// `SPIRA_METRICS_ENDPOINT = endpoint` (and `declare`'s other keys), for tests of what it pushes.
pub fn fixture_metrics_env(dir: &std::path::Path, endpoint: &str, declare: &[(&str, &str)]) -> Vec<(&'static str, String)> {
    let mut keys = vec![("SPIRA_METRICS_ENDPOINT", endpoint)];
    keys.extend_from_slice(declare);
    let toml = fixture_toml(dir, &keys);
    let home = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira");
    vec![("SPIRA_TOML", toml.display().to_string()), ("SPIRA_HOME", home.display().to_string())]
}

/// The read-only database user's secret file, kept beside the service user's: `<file>-ro`.
/// Empty when the service credential is unnamed.
pub fn ro_credential(service_file: &str) -> String {
    if service_file.trim().is_empty() { String::new() } else { format!("{service_file}-ro") }
}

#[cfg(test)]
mod ro_credential_tests {
    use super::ro_credential;

    #[test]
    fn the_read_only_secret_is_the_service_secrets_ro_sibling_and_never_a_guess() {
        assert_eq!(ro_credential("/c/spira-lc.credential"), "/c/spira-lc.credential-ro");
        assert_eq!(ro_credential(" "), "", "no service credential names no read-only one");
    }
}

#[cfg(test)]
mod declared_value_tests {
    use super::declared_value;
    use std::collections::BTreeMap;

    #[test]
    fn a_declared_key_answers_the_same_as_the_full_resolve_and_the_full_resolve_owns_the_rest() {
        let dir = testkit::TempDir::new("spira-config-declared-value");
        let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let declare: BTreeMap<String, String> = [("SPIRA_INSTANCE", "prod"), ("SPIRA_ASK_LABEL", "pinned-ask"), ("SPIRA_AGENT", "pinned-agent")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let toml = crate::fixture_toml_file(dir.path(), &declare);
        let env: BTreeMap<String, String> = [("HOME", "/h"), ("SPIRA_TOML", toml.to_str().unwrap())].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let full = crate::resolve::resolve_for_process(&tree, &tree, &env).expect("the fixture resolves");
        let merged = crate::merge_layers(toml.to_str().unwrap()).unwrap();

        let answered: Vec<_> = full.values.iter().filter_map(|(k, v)| declared_value(&merged, k).map(|d| (k, v, d))).collect();
        assert!(answered.len() > 100, "positive control: most keys answer from the declaration, got {}", answered.len());
        for (k, full_v, fast_v) in &answered {
            assert_eq!(*full_v, fast_v, "{k}");
        }
        assert_eq!(declared_value(&merged, "SPIRA_ASK_LABEL").as_deref(), Some("pinned-ask"));
        assert_eq!(declared_value(&merged, "SPIRA_AGENT"), None, "a computed key takes the full resolve");
        assert_eq!(declared_value(&merged, "NOT_A_SPIRA_KEY"), None);
        assert_eq!(declared_value(&merged, "SPIRA_NO_SUCH_KEY_ANYWHERE"), None, "an undeclared key is the full resolve's refusal");
    }

    #[test]
    fn a_confined_instance_takes_the_full_resolve_so_containment_still_runs() {
        let dir = testkit::TempDir::new("spira-config-declared-value-confined");
        let declare: BTreeMap<String, String> = [("SPIRA_INSTANCE", "scratch"), ("SPIRA_ASK_LABEL", "x")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let toml = crate::fixture_toml_file(dir.path(), &declare);
        let merged = crate::merge_layers(toml.to_str().unwrap()).unwrap();
        assert_eq!(declared_value(&merged, "SPIRA_ASK_LABEL"), None);
    }
}
