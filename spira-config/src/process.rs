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

/// The spec `$SPIRA_TOML` names — one file, or `base:override` layers — for a caller that must
/// HAND it on (a rendered unit, a launched process) rather than read it: the only door onto the
/// spec's location (spira-config is the only door onto spira.toml). Unset is a refusal.
pub fn spec() -> Result<String, String> {
    std::env::var("SPIRA_TOML")
        .ok()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "SPIRA_TOML is not set — it names the one source of config".to_string())
}

/// One key's declared value. `Err` when the config cannot be resolved or the key is not a
/// registered config key — never a default.
pub fn cfg(key: &str) -> Result<String, String> {
    config()?
        .values
        .get(key)
        .cloned()
        .ok_or_else(|| format!("{key} is not a registered config key (spira/conf.d) — refusing"))
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
        std::os::unix::fs::symlink(tree.join(d), release.join("spira").join(d)).expect("the registry is linked");
    }
    release
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
