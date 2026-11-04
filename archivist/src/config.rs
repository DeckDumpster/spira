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
    from_lookup(&|k| spira_config::process::cfg(k))
}

/// [`resolve`] over any lookup: the scheduler's task hands in the config it pinned at boot.
pub fn from_lookup(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Env, String> {
    let num = |k: &str| -> Result<u64, String> { get(k)?.trim().parse().map_err(|e| format!("{k}: {e}")) };
    let wiki = get("SPIRA_WIKI")?;
    Ok(Env {
        db: get("SPIRA_DB")?,
        run: get("SPIRA_RUN")?,
        runes: get("SPIRA_RUNES_DIR")?,
        token_projects: get("SPIRA_TOKEN_PROJECTS")?,
        wiki: if wiki.trim().is_empty() { None } else { Some(wiki) },
        agent: get("SPIRA_AGENT")?,
        tz: get("SPIRA_TZ")?,
        every: num("SPIRA_ARCHIVIST_EVERY")?,
        idle: num("SPIRA_ARCHIVIST_IDLE")? as i64,
        model: get("SPIRA_ARCHIVIST_MODEL")?,
        timeout: num("SPIRA_ARCHIVIST_TIMEOUT")?,
        per_pass: num("SPIRA_ARCHIVIST_PER_PASS")? as u32,
        timeout_retries: num("SPIRA_ARCHIVIST_TIMEOUT_RETRIES")? as u32,
        fail_retries: num("SPIRA_ARCHIVIST_FAIL_RETRIES")? as u32,
        fail_backoff: num("SPIRA_ARCHIVIST_FAIL_BACKOFF")?,
    })
}
