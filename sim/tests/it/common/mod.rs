//! The settings the simulator boots the service's task table with, shared by the scenarios.

use spira_runtime::{tasks, Settings};

/// `key`'s value under `[spira]` in the complete fixture (`spira/testlib/complete.toml`).
pub fn complete_fixture(key: &str) -> Option<String> {
    let doc: toml::Value = toml::from_str(&spira_config::process::complete_fixture_text()).ok()?;
    let v = doc.get("spira")?.get(spira_config::registry::field_of(key))?;
    Some(v.as_str().map_or_else(|| v.to_string(), str::to_string))
}

/// Every task acts, so its effects reach the doubles; the container cap is the host run
/// ceiling; the store flag is off; a calendar key (`*_CALENDAR`) holds the complete fixture's `OnCalendar` spec;
/// `set` overrides; every other key is 300.
pub fn settings_with(set: &[(&str, &str)]) -> Settings {
    let modes: Vec<String> = tasks::all().map(|t| format!("{}=act", t.name)).collect();
    Settings::from_lookup(&|k| {
        if let Some((_, v)) = set.iter().find(|(s, _)| *s == k) {
            return Ok(v.to_string());
        }
        Ok(match k {
            "SPIRA_TASK_MODES" => modes.join(","),
            "SPIRA_POOL_CAP_CONTAINER" => "2".into(),
            "SPIRA_LC_LIFECYCLE_ON_STORE" => "0".into(),
            k if k.ends_with("_CALENDAR") => complete_fixture(k).ok_or_else(|| format!("{k} has no value in the complete fixture"))?,
            _ => "300".into(),
        })
    })
    .expect("every key answered")
}

#[allow(dead_code)]
pub fn settings() -> Settings {
    settings_with(&[])
}
