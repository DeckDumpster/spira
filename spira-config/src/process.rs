//! THE ONE DOOR to config for every binary (per Ryan 2026-10-05: one source of config).
//!
//! A process reads its configuration here and nowhere else: the file `$SPIRA_TOML` names,
//! resolved once per process: a success is kept for the life of the process, a refusal never is. No `env::var("SPIRA_X")`, no default, no fallback — a key the
//! file does not declare is an error, and so is a resolution that fails. A caller that
//! cannot do its job without a value refuses, naming the key; it never invents one.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::OnceLock;

use crate::resolve::Resolved;

static CONFIG: OnceLock<Resolved> = OnceLock::new();

thread_local! {
    static FRESH: std::cell::RefCell<Option<Resolved>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` with every [`cfg`] on this thread answered from a resolution read from the files
/// now, not from what the process pinned. The pin is untouched: only `f` sees the fresh read.
pub fn with_fresh<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    let resolved = resolve_now(&std::env::vars().collect())?;
    FRESH.with(|c| *c.borrow_mut() = Some(resolved));
    let out = f();
    FRESH.with(|c| *c.borrow_mut() = None);
    Ok(out)
}

/// This process's resolved config: `$SPIRA_TOML`, read until one resolution succeeds and then
/// pinned. A refusal is returned, never kept: the next call reads again.
pub fn config() -> Result<&'static Resolved, String> {
    config_with(&std::env::vars().collect())
}

/// [`config`], resolving from `env` as handed in when nothing is pinned yet: a caller that names
/// its config by value never edits the process environment. A pinned config wins over `env`.
pub fn config_with(env: &BTreeMap<String, String>) -> Result<&'static Resolved, String> {
    if let Some(kept) = CONFIG.get() {
        return Ok(kept);
    }
    let resolved = resolve_now(env)?;
    Ok(CONFIG.get_or_init(|| resolved))
}

/// `spira.run` from this process's pinned config, judged against the instance's confinement as
/// [`crate::resolve::resolve_run_dir`] judges it; a refusal is returned and, as in [`config`], never kept.
pub fn run_dir() -> Result<std::path::PathBuf, String> {
    crate::resolve::run_dir_of(config().map_err(|e| format!("cannot resolve spira.run: {e}"))?)
}

/// `spira.instance` from this process's pinned config; empty is a refusal.
pub fn instance() -> Result<String, String> {
    crate::resolve::instance_of(config().map_err(|e| format!("cannot resolve spira.instance: {e}"))?)
}

fn resolve_now(env: &BTreeMap<String, String>) -> Result<Resolved, String> {
    let home = crate::resolve::locate_home(env, &std::env::current_exe().unwrap_or_default())?;
    let repo = crate::resolve::derive_home_repo(&home, env);
    crate::resolve::resolve_for_process(&home, &repo, env)
}

/// What `keys` resolve to now, read from the files again: the one way past the value this process pinned.
/// A caller that re-pins takes exactly the keys it names; everything else stays as it was read.
pub fn fresh(keys: &[String]) -> Result<BTreeMap<String, String>, String> {
    let resolved = resolve_now(&std::env::vars().collect())?;
    keys.iter().map(|k| resolved.values.get(k).map(|v| (k.clone(), v.clone())).ok_or_else(|| format!("{k} is not a registered config key — refusing"))).collect()
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
const COMPUTED_KEYS: &[&str] = &["SPIRA_RELEASE_REPO", "SPIRA_AGENT", "SPIRA_RUNES_DIR", "SPIRA_BROKER_SOCKET"];

/// `key` as the merged `[spira]` table declares it, when that is the whole answer: a declared,
/// plainly-valued key of a `prod` instance. It does not check the file's schema (the cost this path
/// exists to skip; `check-config` and [`config`] still do); [`cfg`] checks the key is registered. Anything
/// else is `None` and takes the full resolve, which owns every other refusal.
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
/// registered config key — never a default. A registered key a `prod` instance declares plainly
/// is answered from the file without the full resolve (see [`declared_value`]).
pub fn cfg(key: &str) -> Result<String, String> {
    if let Some(r) = FRESH.with(|c| c.borrow().as_ref().map(|r| r.values.get(key).cloned())) {
        return r.ok_or_else(|| format!("{key} is not a registered config key — refusing"));
    }
    if let Some(v) = declared_fast(key) {
        if crate::registry::declares_key(&crate::resolve::locate_home_for_process()?, key) {
            return Ok(v);
        }
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
    parse_declared(key, &cfg(key)?)
}

/// `value` as `key`'s declared type; the parse [`cfg_parse`] applies, for a caller that reads
/// the value from another source.
pub fn parse_declared<T: FromStr>(key: &str, value: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    value.trim().parse::<T>().map_err(|e| format!("{key} = {value:?} in spira.toml does not parse: {e}"))
}

/// The metrics sink for `service`: `SPIRA_METRICS_ENDPOINT` as declared, disabled when the config
/// does not resolve. Short deadline: a push never holds its producer.
pub fn metrics_sink(service: &str) -> spira_otel::metrics::MetricsSink {
    let endpoint = cfg("SPIRA_METRICS_ENDPOINT").ok();
    spira_otel::metrics::MetricsSink::new(endpoint.as_deref(), service, std::time::Duration::from_millis(500))
}

/// The complete fixture's text, read from the harness tree this crate was built in.
pub fn complete_fixture_text() -> String {
    let _env = testkit::env_read();
    complete_fixture_text_locked()
}

fn complete_fixture_text_locked() -> String {
    let path = crate::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree").join("testlib/complete.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A release directory for a test: `<dir>/release/spira` linking this harness tree's
/// `conf.toml.d`, and its `conf.d` when `with_conf_d`. Point `SPIRA_RELEASE` at the returned path.
pub fn fixture_release(dir: &std::path::Path, with_conf_d: bool) -> std::path::PathBuf {
    let tree = crate::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
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
    let _env = testkit::env_read();
    let tree = crate::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
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
    let mut d = {
        let _env = testkit::env_read();
        crate::validate(&complete_fixture_text_locked()).expect("the complete fixture validates")
    };
    for (k, v) in declare {
        let rest = k.strip_prefix("SPIRA_").unwrap_or(k);
        d = crate::set_path(&d, &format!("spira.{}", rest.to_ascii_lowercase()), v)
            .unwrap_or_else(|e| panic!("fixture_toml: cannot declare {k}: {e}"));
    }
    let p = dir.join(crate::FILE_NAME);
    crate::serialize_and_write(&p, &d).expect("the fixture writes");
    p
}

/// The repo map of a registry holding exactly `repos`, written under `dir`.
pub fn fixture_repo_map(dir: &std::path::Path, repos: &[&str]) -> std::path::PathBuf {
    let map: String = repos.iter().map(|r| format!("{r}|{}|local|local/main\n", dir.join(r).display())).collect();
    let path = dir.join("fixture.repos");
    std::fs::write(&path, map).expect("the fixture map is written");
    path
}

/// The environment of a world whose registry holds exactly `repos` (the first is the home repo)
/// and whose run directory is `run`, for a test that reads stops through the process's own
/// registry: `Some` sets, `None` unsets.
pub fn fixture_world_env(dir: &std::path::Path, run: &std::path::Path, repos: &[&str]) -> Vec<(&'static str, Option<String>)> {
    let map_path = fixture_repo_map(dir, repos);
    let (run_s, map_s) = (run.display().to_string(), map_path.display().to_string());
    let workspaces = dir.display().to_string();
    let toml = fixture_toml(dir, &[("SPIRA_RUN", &run_s), ("SPIRA_REPO_MAP", &map_s), ("SPIRA_HOME_REPO", repos[0]), ("SPIRA_WORKSPACES", &workspaces)]);
    let home = crate::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
    vec![
        ("SPIRA_TOML", Some(toml.display().to_string())),
        ("SPIRA_HOME", Some(home.display().to_string())),
        ("SPIRA_RELEASE", None),
        ("SPIRA_REPO_MAP", None),
        ("SPIRA_HOME_REPO", None),
    ]
}

/// Where the harness is installed, as this process's environment says; empty when it names none.
pub fn spira_home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("SPIRA_HOME").unwrap_or_default())
}

/// The environment that points a spawned binary's config door at a fixture declaring
/// `SPIRA_METRICS_ENDPOINT = endpoint` (and `declare`'s other keys), for tests of what it pushes.
pub fn fixture_metrics_env(dir: &std::path::Path, endpoint: &str, declare: &[(&str, &str)]) -> Vec<(&'static str, String)> {
    let mut keys = vec![("SPIRA_METRICS_ENDPOINT", endpoint)];
    keys.extend_from_slice(declare);
    let toml = fixture_toml(dir, &keys);
    let home = crate::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
    vec![("SPIRA_TOML", toml.display().to_string()), ("SPIRA_HOME", home.display().to_string())]
}

/// The read-only database user's secret file, kept beside the service user's: `<file>-ro`.
/// Empty when the service credential is unnamed.
pub fn ro_credential(service_file: &str) -> String {
    if service_file.trim().is_empty() { String::new() } else { format!("{service_file}-ro") }
}

#[cfg(test)]
mod fixture_env_lock_tests;

#[cfg(test)]
mod ro_credential_tests;

#[cfg(test)]
mod declared_value_tests;
