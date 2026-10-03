//! The environment `mail` reads. Every default here matches conf.sh's own `:
//! "${VAR:=default}"` so a caller that has not sourced conf.sh (a unit test, a suite
//! fixture) still gets the same behaviour a real install would after conf.sh ran — the
//! same contract every other crate in this workspace holds with conf.sh (aeon, landing-pass,
//! queue read `std::env::var` directly; conf.sh has already exported these into the process
//! tree by the time a bare-named binary runs).

use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Env {
    pub mail_root: PathBuf,
    pub kinds_dir: PathBuf,
    pub index_file: PathBuf,
    pub mute: bool,
    pub loom_budget_ms: u64,
    pub repeat_window_s: u64,
    pub tidy_fresh_s: u64,
    pub id_prefix: String,
    pub ask_label: String,
    pub db: String,
    pub bd_bin: String,
    pub bd_conn_retries: u32,
    pub operator_actor: String,
    pub run_dir: PathBuf,
    pub home: PathBuf,
    pub mail_from: Option<String>,
    pub lint_considered: Option<String>,
    pub repeat_considered: Option<String>,
    pub allow_blocking: bool,
    pub bead_id: Option<String>,
    pub lock_timeout_ms: u64,
}

fn var(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.is_empty())
}

fn var_u64(k: &str, default: u64) -> u64 {
    var(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Env {
    /// Declared config, the one source (per Ryan 2026-10-05): every REGISTERED key comes from
    /// the config file `$SPIRA_TOML` names, through spira_config — never the environment, never
    /// a built-in default. A key that does not resolve refuses, named. Only per-call facts
    /// (`--from`-style overrides, the bead in hand, `*_CONSIDERED` acknowledgements) and the two
    /// knobs not yet registered are read from the environment.
    pub fn load() -> Result<Env, String> {
        let fail = |e: String| format!("FATAL: {e}");
        let home = spira_config::resolve::locate_home_for_process().map_err(fail)?;
        let cfg = |k: &str| spira_config::process::cfg(k).map_err(fail);
        let cfg_u64 = |k: &str| spira_config::process::cfg_parse::<u64>(k).map_err(fail);
        let path = |k: &str| cfg(k).map(PathBuf::from);
        Ok(Env {
            mail_root: path("SPIRA_MAIL")?,
            kinds_dir: path("SPIRA_MAIL_KINDS")?,
            index_file: path("SPIRA_MAIL_INDEX")?,
            mute: matches!(cfg("SPIRA_MAIL_MUTE")?.as_str(), "1" | "true"),
            loom_budget_ms: cfg_u64("SPIRA_LOOM_BUDGET_MS")?,
            repeat_window_s: cfg_u64("SPIRA_MAIL_REPEAT_WINDOW")?,
            tidy_fresh_s: cfg_u64("SPIRA_MAIL_TIDY_FRESH")?,
            id_prefix: cfg("SPIRA_ID_PREFIX")?,
            ask_label: cfg("SPIRA_ASK_LABEL")?,
            db: cfg("SPIRA_DB")?,
            bd_bin: cfg("SPIRA_BD")?,
            // Not registered yet: read as before until the registration round declares them.
            bd_conn_retries: var_u64("SPIRA_BDQ_CONN_RETRIES", 2) as u32,
            operator_actor: cfg("SPIRA_OPERATOR_ACTOR")?,
            run_dir: path("SPIRA_RUN")?,
            home,
            mail_from: var("SPIRA_MAIL_FROM"),
            lint_considered: var("SPIRA_MAIL_LINT_CONSIDERED"),
            repeat_considered: var("SPIRA_MAIL_REPEAT_CONSIDERED"),
            allow_blocking: var("SPIRA_MAIL_ALLOW_BLOCKING").is_some(),
            bead_id: var("BEAD_ID"),
            lock_timeout_ms: var_u64("SPIRA_MAIL_LOCK_TIMEOUT_MS", 30_000),
        })
    }

    /// The bead-id pattern, `<prefix>-[a-z0-9]{4,}` — `_bead_id_re` in mail.sh, prefix
    /// defaulting to "sp" only as mail.sh's own shell-parameter fallback does (`${SPIRA_ID_PREFIX:-sp}`).
    pub fn id_prefix_for_regex(&self) -> &str {
        if self.id_prefix.is_empty() {
            "sp"
        } else {
            &self.id_prefix
        }
    }
}

/// `SPIRA_HOME`, else the first directory holding `lib.sh` among the release and cargo
/// layouts around this executable (same pattern as `sending::locate_home`, `queue`'s
/// `harness_home` — every crate re-derives this itself; conf.sh deliberately never exports
/// `SPIRA_HOME` to child processes, "and that is a fence", so a caller that sources conf.sh
/// without also exporting it — every `env -i` test fixture, and any real caller that never
/// bothered — leaves this unset on purpose. mail.sh never had this gap: it sourced conf.sh
/// itself, which derives `SPIRA_HOME` from *mail.sh's own* `BASH_SOURCE`, not from the
/// environment. A compiled binary has no `BASH_SOURCE`; deriving from `current_exe()`'s own
/// location is the equivalent). Sp-ooh1k's own regression: `SPIRA_MAIL_KINDS` (and the
/// chamber-persona check in `sendmail::reply_mailbox`) silently resolved relative to `.`
/// instead, so `--kind question`/`event`/etc. refused as "unknown kind" the moment a caller
/// did not also happen to export `SPIRA_HOME` — found by `test-pr-notify.sh`'s `env -i`
/// fixture, the one case that does not.
pub fn locate_home(env_home: Option<&str>, exe: &Path) -> Option<PathBuf> {
    if let Some(h) = env_home.filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h));
    }
    let dir = exe.parent()?;
    [dir.join("../spira"), dir.join("../../spira"), dir.join("../../../spira")]
        .into_iter()
        .find(|c| c.join("lib.sh").is_file())
        .map(|c| c.canonicalize().unwrap_or(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_home_prefers_the_explicit_env_var() {
        let t = testkit::TempDir::new("mail-home-env");
        assert_eq!(locate_home(Some("/h"), &t.path().join("bin/mail")), Some(PathBuf::from("/h")));
    }

    #[test]
    fn locate_home_finds_the_release_layouts_sibling_spira_dir() {
        let t = testkit::TempDir::new("mail-home-release");
        let rel = t.path().join("rel");
        std::fs::create_dir_all(rel.join("bin")).unwrap();
        std::fs::create_dir_all(rel.join("spira")).unwrap();
        std::fs::write(rel.join("spira/lib.sh"), "").unwrap();
        let exe = rel.join("bin/mail");
        assert_eq!(locate_home(None, &exe), Some(rel.join("spira").canonicalize().unwrap()));
    }

    #[test]
    fn locate_home_finds_nothing_with_no_lib_sh_around() {
        let t = testkit::TempDir::new("mail-home-none");
        assert_eq!(locate_home(None, &t.path().join("x/y")), None);
    }
}
