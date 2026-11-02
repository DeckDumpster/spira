//! Resolve the run's configuration: every key below is registered in `spira/conf.d/` and
//! read exactly once, through the one door — `spira_config::process::cfg`/`cfg_parse`,
//! resolved from the file `$SPIRA_TOML` names. A key that fails to resolve is a refusal
//! naming the key; this file carries no default of its own (per Ryan 2026-10-05, one
//! source of config).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub db: String,
    pub run: String,
    pub runes: String,
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
    pub fail_retries: u32,
    pub fail_backoff: u64,
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
        runes: spira_config::process::cfg("SPIRA_RUNES_DIR")?,
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
        fail_retries: spira_config::process::cfg_parse("SPIRA_ARCHIVIST_FAIL_RETRIES")?,
        fail_backoff: spira_config::process::cfg_parse("SPIRA_ARCHIVIST_FAIL_BACKOFF")?,
    })
}
