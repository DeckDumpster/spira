//! Resolve the run's configuration: every key below is registered in `spira/conf.d/` and
//! read exactly once, through the one door — `spira_config::process::cfg`/`cfg_parse`,
//! resolved from the file `$SPIRA_TOML` names. A key that fails to resolve is a refusal
//! naming the key; this file carries no default of its own (per Ryan 2026-10-05, one
//! source of config).

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

/// The binary's config struct constructor — called once, at the top of `main`'s dispatch.
/// `SPIRA_WIKI` is the one key whose own declared default is the empty string: empty IS
/// the configured meaning "no wiki configured", not a fallback this crate invents, so it
/// becomes `None` rather than `Some(String::new())`.
pub fn resolve() -> Result<Env, String> {
    let wiki = spira_config::process::cfg("SPIRA_WIKI")?;
    Ok(Env {
        db: spira_config::process::cfg("SPIRA_DB")?,
        run: spira_config::process::cfg("SPIRA_RUN")?,
        chamber: spira_config::process::cfg("SPIRA_CHAMBER")?,
        token_projects: spira_config::process::cfg("SPIRA_TOKEN_PROJECTS")?,
        wiki: if wiki.trim().is_empty() { None } else { Some(wiki) },
        agent: spira_config::process::cfg("SPIRA_AGENT")?,
        tz: spira_config::process::cfg("SPIRA_TZ")?,
        every: spira_config::process::cfg_parse("SPIRA_ARCHIVIST_EVERY")?,
        idle: spira_config::process::cfg_parse("SPIRA_ARCHIVIST_IDLE")?,
        model: spira_config::process::cfg("SPIRA_ARCHIVIST_MODEL")?,
        timeout: spira_config::process::cfg_parse("SPIRA_ARCHIVIST_TIMEOUT")?,
        per_pass: spira_config::process::cfg_parse("SPIRA_ARCHIVIST_PER_PASS")?,
        timeout_retries: spira_config::process::cfg_parse("SPIRA_ARCHIVIST_TIMEOUT_RETRIES")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The only test in this binary that calls [`resolve`] — `spira_config::process::cfg`'s
    /// resolution is a process-global `OnceLock`, computed once and never reset, so no
    /// other test here may also drive it. `SPIRA_HOME`/`SPIRA_TOML` point at a throwaway
    /// fixture, never the real box's.
    #[test]
    fn resolve_reads_every_key_through_the_one_door() {
        let dir = testkit::TempDir::new("archivist-config");
        let toml =
            spira_config::process::fixture_toml(dir.path(), &[("SPIRA_ARCHIVIST_EVERY", "10"), ("SPIRA_WIKI", "/wiki")]);
        // SPIRA_HOME must be the checkout's own spira/ (where conf.d — the key registry —
        // actually lives), never the throwaway fixture dir: `cfg()` needs both a real
        // registry AND the fixture's declared values.
        let real_home = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let _env = testkit::env(&[("SPIRA_HOME", Some(real_home.to_str().unwrap())), ("SPIRA_TOML", Some(toml.to_str().unwrap()))]);

        let e = resolve().expect("a complete fixture toml must resolve every key this crate needs");
        assert_eq!(e.every, 10);
        assert_eq!(e.wiki, Some("/wiki".to_string()));
        assert_eq!(e.agent, "claude");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
