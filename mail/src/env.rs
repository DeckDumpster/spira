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
    pub operator_actor: String,
    pub run_dir: PathBuf,
    pub home: PathBuf,
    pub mail_from: Option<String>,
    pub lint_considered: Option<String>,
    pub repeat_considered: Option<String>,
    pub allow_blocking: bool,
    pub session_epoch: Option<String>,
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
    pub fn load() -> Env {
        let run_dir = var("SPIRA_RUN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp/spira"));
        let exe = env::current_exe().unwrap_or_default();
        let home = locate_home(var("SPIRA_HOME").as_deref(), &exe).unwrap_or_else(|| PathBuf::from("."));
        let mail_root = var("SPIRA_MAIL").map(PathBuf::from).unwrap_or_else(|| run_dir.join("mail"));
        let kinds_dir = var("SPIRA_MAIL_KINDS").map(PathBuf::from).unwrap_or_else(|| home.join("mail/kinds"));
        let index_file = var("SPIRA_MAIL_INDEX").map(PathBuf::from).unwrap_or_else(|| mail_root.join("index"));
        let mute = matches!(var("SPIRA_MAIL_MUTE").as_deref(), Some("1") | Some("true"));
        Env {
            mail_root,
            kinds_dir,
            index_file,
            mute,
            loom_budget_ms: var_u64("SPIRA_LOOM_BUDGET_MS", 1500),
            repeat_window_s: var_u64("SPIRA_MAIL_REPEAT_WINDOW", 14400),
            tidy_fresh_s: var_u64("SPIRA_MAIL_TIDY_FRESH", 86400),
            id_prefix: var("SPIRA_ID_PREFIX").unwrap_or_default(),
            // literal-ok: rust fallback, matching mail.sh's own; SPIRA_ASK_LABEL set by conf.sh
            ask_label: var("SPIRA_ASK_LABEL").unwrap_or_else(|| "needs-operator".to_string()),
            db: var("SPIRA_DB").unwrap_or_default(),
            bd_bin: var("SPIRA_BD").unwrap_or_else(|| "bd".to_string()),
            operator_actor: var("SPIRA_OPERATOR_ACTOR").unwrap_or_else(|| "operator".to_string()),
            run_dir,
            home,
            mail_from: var("SPIRA_MAIL_FROM"),
            lint_considered: var("SPIRA_MAIL_LINT_CONSIDERED"),
            repeat_considered: var("SPIRA_MAIL_REPEAT_CONSIDERED"),
            allow_blocking: var("SPIRA_MAIL_ALLOW_BLOCKING").is_some(),
            session_epoch: var("SESSION_EPOCH"),
            bead_id: var("BEAD_ID"),
            lock_timeout_ms: var_u64("SPIRA_MAIL_LOCK_TIMEOUT_MS", 30_000),
        }
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
