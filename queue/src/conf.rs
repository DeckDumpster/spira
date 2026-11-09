//! In-process resolution of the `SPIRA_*` values queue reads outside `Lib::context`: an
//! environment value wins outright (the `:=` rule), otherwise `spira_config` resolves the key
//! from the config store and the `conf.d` registry. A caller never supplies a literal default.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The declared harness home (`SPIRA_HOME`, else `$SPIRA_RELEASE/spira`) when it holds `lib.sh`.
/// Never `spira.prod` or a search beside this binary.
pub fn harness_home() -> Option<PathBuf> {
    spira_config::resolve::locate_home_for_process()
        .ok()
        .filter(|p| p.join("lib.sh").is_file())
}

pub fn value(key: &str) -> Result<String, String> {
    if let Some(v) = std::env::var(key).ok().filter(|s| !s.is_empty()) {
        return Ok(v);
    }
    let home = harness_home().ok_or_else(|| {
        format!("{key}: cannot find the harness (lib.sh): set SPIRA_HOME or SPIRA_RELEASE")
    })?;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let repo = spira_config::resolve::derive_home_repo(&home, &env);
    let resolved = spira_config::resolve::resolve_for_process(&home, &repo, &env)
        .map_err(|e| format!("{key}: {e}"))?;
    Ok(resolved.get(key).to_string())
}

pub fn nonempty(key: &str) -> Result<String, String> {
    value(key).and_then(|v| {
        if v.is_empty() {
            Err(format!("{key} resolves to an empty value"))
        } else {
            Ok(v)
        }
    })
}
