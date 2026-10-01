//! Resolve the run's configuration from a probed `SPIRA_*` environment (whatever
//! [`crate::seam::Seam::probe`] returned). Pure: given the map, this is deterministic and
//! table-tested without a subprocess. Defaults mirror `spira/conf.sh`'s own.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub db: String,
    pub run: String,
    pub chamber: String,
    pub token_projects: String,
    pub wiki: Option<String>,
    pub agent: String,
    pub tz: String,
    pub every: u64,
    pub idle: i64,
    pub model: String,
    pub timeout: u64,
    pub per_pass: u32,
    pub timeout_retries: u32,
}

fn get(m: &HashMap<String, String>, key: &str) -> Option<String> {
    m.get(key).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn num<T: std::str::FromStr>(m: &HashMap<String, String>, key: &str, default: T) -> T {
    get(m, key).and_then(|s| s.parse().ok()).unwrap_or(default)
}

pub fn resolve(m: &HashMap<String, String>) -> Env {
    Env {
        db: get(m, "SPIRA_DB").unwrap_or_else(|| ".".into()),
        run: get(m, "SPIRA_RUN").unwrap_or_else(|| "/tmp".into()),
        chamber: get(m, "SPIRA_CHAMBER").unwrap_or_default(),
        token_projects: get(m, "SPIRA_TOKEN_PROJECTS").unwrap_or_default(),
        wiki: get(m, "SPIRA_WIKI"),
        agent: get(m, "SPIRA_AGENT").unwrap_or_else(|| "claude".into()),
        tz: get(m, "SPIRA_TZ").unwrap_or_else(|| "UTC".into()),
        every: num(m, "SPIRA_ARCHIVIST_EVERY", 40),
        idle: num(m, "SPIRA_ARCHIVIST_IDLE", 1800),
        model: get(m, "SPIRA_ARCHIVIST_MODEL").unwrap_or_else(|| "claude-opus-5".into()),
        timeout: num(m, "SPIRA_ARCHIVIST_TIMEOUT", 900),
        per_pass: num(m, "SPIRA_ARCHIVIST_PER_PASS", 1),
        timeout_retries: num(m, "SPIRA_ARCHIVIST_TIMEOUT_RETRIES", 3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_conf_sh_when_the_probe_is_empty() {
        let e = resolve(&HashMap::new());
        assert_eq!(e.every, 40);
        assert_eq!(e.idle, 1800);
        assert_eq!(e.model, "claude-opus-5");
        assert_eq!(e.timeout, 900);
        assert_eq!(e.per_pass, 1);
        assert_eq!(e.timeout_retries, 3);
        assert_eq!(e.agent, "claude");
        assert_eq!(e.wiki, None);
    }

    #[test]
    fn probed_values_override_defaults() {
        let mut m = HashMap::new();
        m.insert("SPIRA_ARCHIVIST_EVERY".to_string(), "10".to_string());
        m.insert("SPIRA_WIKI".to_string(), "/wiki".to_string());
        let e = resolve(&m);
        assert_eq!(e.every, 10);
        assert_eq!(e.wiki, Some("/wiki".to_string()));
    }

    #[test]
    fn a_blank_value_falls_back_to_the_default_rather_than_an_empty_string() {
        let mut m = HashMap::new();
        m.insert("SPIRA_ARCHIVIST_MODEL".to_string(), "".to_string());
        let e = resolve(&m);
        assert_eq!(e.model, "claude-opus-5");
    }
}
