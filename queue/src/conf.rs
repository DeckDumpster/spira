//! In-process resolution of the `SPIRA_*` values queue reads outside `Lib::context`: an
//! environment value wins outright (the `:=` rule), otherwise `spira_config` resolves the key
//! from `spira.toml` and the `conf.d` registry. A caller never supplies a literal default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn harness_home() -> Option<PathBuf> {
    let has_lib = |p: &Path| p.join("lib.sh").is_file();
    if let Some(h) = std::env::var_os("SPIRA_HOME").map(PathBuf::from).filter(|p| has_lib(p)) {
        return Some(h);
    }
    if let Some(doc) = spira_config::discover(None).and_then(|p| spira_config::load(&p).ok()) {
        if let Some(p) = spira_config::get_path(&doc, "spira.prod").map(PathBuf::from).filter(|p| has_lib(p)) {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    exe.ancestors().skip(1).take(4).map(|a| a.join("spira")).find(|p| has_lib(p))
}

pub fn value(key: &str) -> Result<String, String> {
    if let Some(v) = std::env::var(key).ok().filter(|s| !s.is_empty()) {
        return Ok(v);
    }
    let home = harness_home().ok_or_else(|| format!("{key}: cannot find the harness (lib.sh): set SPIRA_HOME or spira.prod"))?;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let repo = spira_config::resolve::derive_home_repo(&home, &env);
    let resolved = spira_config::resolve::resolve_for_process(&home, &repo, &env).map_err(|e| format!("{key}: {e}"))?;
    Ok(resolved.get(key).to_string())
}

pub fn nonempty(key: &str) -> Result<String, String> {
    value(key).and_then(|v| if v.is_empty() { Err(format!("{key} resolves to an empty value")) } else { Ok(v) })
}
