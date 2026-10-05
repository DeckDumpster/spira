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

/// One key's declared value. `Err` when the config cannot be resolved or the key is not a
/// registered config key — never a default.
pub fn cfg(key: &str) -> Result<String, String> {
    config()?
        .values
        .get(key)
        .cloned()
        .ok_or_else(|| format!("{key} is not a registered config key (spira/conf.d) — refusing"))
}

/// [`cfg`], parsed. A declared value that does not parse is an error naming the key.
pub fn cfg_parse<T: FromStr>(key: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    let v = cfg(key)?;
    v.trim().parse::<T>().map_err(|e| format!("{key} = {v:?} in spira.toml does not parse: {e}"))
}

/// A complete `spira.toml` for TESTS in any crate: every key declared (spira-config's
/// `tests/fixtures/complete.toml`), with `declare`'s `SPIRA_<KEY>` pairs written as
/// `spira.<key>`. Written to `<dir>/spira.toml`; point `SPIRA_TOML` at the returned path.
pub fn fixture_toml(dir: &std::path::Path, declare: &[(&str, &str)]) -> std::path::PathBuf {
    let mut d = crate::validate(include_str!("../tests/fixtures/complete.toml")).expect("the complete fixture validates");
    for (k, v) in declare {
        let rest = k.strip_prefix("SPIRA_").unwrap_or(k);
        d = crate::set_path(&d, &format!("spira.{}", rest.to_ascii_lowercase()), v)
            .unwrap_or_else(|e| panic!("fixture_toml: cannot declare {k}: {e}"));
    }
    let p = dir.join(crate::FILE_NAME);
    crate::serialize_and_write(&p, &d).expect("the fixture writes");
    p
}
