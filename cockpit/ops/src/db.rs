//! db — resolve the cockpit's one beads database.
//!
//! Replaces the `cockpit_db()` / `BD` parts of `cockpit/db.sh` for this crate's binaries.
//! `cockpit/db.sh` itself is **not** deleted: `cockpit/moot-sweep.sh` and
//! `cockpit/verify-asks.sh` still source it and are out of this bead's scope (bash-only
//! callers keep the bash function until they, too, move). See ../DESIGN.md.
//!
//! There is exactly one beads database the cockpit's agent-facing tools write to. `db.sh`'s
//! own header states why this is a function and not five copies of a directory check: two
//! live databases holding the same beads was "a bug factory in its own right" (dedupe drift,
//! replies landing in the wrong copy). A crate that gave `resolve` and `reply` each their own
//! inline copy of this check would recreate exactly the duplication `db.sh` exists to
//! prevent.

use std::path::{Path, PathBuf};

/// Pure resolution logic, independent of the real environment and filesystem so it can be
/// unit-tested without races on process-global env vars.
///
/// `cockpit_db` and `spira_db` are the raw `$COCKPIT_DB` / `$SPIRA_DB` values (empty string
/// and "unset" are treated the same, matching bash's `${VAR:-default}`). `has_beads_dir`
/// answers whether `<candidate>/.beads` is a directory.
pub fn resolve_db(
    cockpit_db: Option<&str>,
    spira_db: Option<&str>,
    has_beads_dir: impl Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    let candidate = cockpit_db
        .filter(|s| !s.is_empty())
        .or(spira_db.filter(|s| !s.is_empty()))
        .unwrap_or("");
    let path = PathBuf::from(candidate);
    if has_beads_dir(&path.join(".beads")) {
        Ok(path)
    } else {
        Err(format!(
            "cockpit: {candidate} has no .beads — refusing to guess a database"
        ))
    }
}

/// `cockpit_db()` against the real process's resolved config (`spira_config::process::cfg`,
/// the one door). `COCKPIT_DB`'s own toml default already chains to `SPIRA_DB`
/// (`spira/conf.d`), so `resolve_db`'s own OR is now belt-and-suspenders, not load-bearing —
/// kept anyway so this still answers correctly if that default ever changes.
pub fn cockpit_db() -> Result<PathBuf, String> {
    let cockpit_db = spira_config::process::cfg("COCKPIT_DB").ok();
    let spira_db = spira_config::process::cfg("SPIRA_DB").ok();
    resolve_db(cockpit_db.as_deref(), spira_db.as_deref(), |p| p.is_dir())
}

/// `$SPIRA_LC_BIN` if set and non-empty, else `"spira-lc"` — the one door to bead content.
pub fn lc_bin() -> String {
    std::env::var("SPIRA_LC_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "spira-lc".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cockpit_db_env_used_when_present_and_valid() {
        let r = resolve_db(Some("/db1"), Some("/db2"), |p| p == Path::new("/db1/.beads"));
        assert_eq!(r.unwrap(), PathBuf::from("/db1"));
    }

    #[test]
    fn falls_back_to_spira_db_when_cockpit_db_unset() {
        let r = resolve_db(None, Some("/db2"), |p| p == Path::new("/db2/.beads"));
        assert_eq!(r.unwrap(), PathBuf::from("/db2"));
    }

    #[test]
    fn falls_back_to_spira_db_when_cockpit_db_empty() {
        let r = resolve_db(Some(""), Some("/db2"), |p| p == Path::new("/db2/.beads"));
        assert_eq!(r.unwrap(), PathBuf::from("/db2"));
    }

    #[test]
    fn refuses_with_named_path_when_no_beads_dir() {
        let r = resolve_db(Some("/nope"), None, |_| false);
        assert_eq!(
            r.unwrap_err(),
            "cockpit: /nope has no .beads — refusing to guess a database"
        );
    }

    #[test]
    fn refuses_with_empty_path_when_neither_set() {
        let r = resolve_db(None, None, |_| false);
        assert_eq!(
            r.unwrap_err(),
            "cockpit:  has no .beads — refusing to guess a database"
        );
    }

    #[test]
    fn cockpit_db_takes_precedence_over_spira_db_even_when_spira_db_would_validate() {
        // COCKPIT_DB set (even if it won't validate) must not silently fall through to
        // SPIRA_DB — matching bash's ${COCKPIT_DB:-${SPIRA_DB:-}}, which only falls back on
        // unset/empty, never on "set but invalid".
        let r = resolve_db(Some("/db1"), Some("/db2"), |p| p == Path::new("/db2/.beads"));
        assert_eq!(
            r.unwrap_err(),
            "cockpit: /db1 has no .beads — refusing to guess a database"
        );
    }
}
