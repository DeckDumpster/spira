//! The environment `mail` reads. Every default here matches conf.sh's own `:
//! "${VAR:=default}"` so a caller that has not sourced conf.sh (a unit test, a suite
//! fixture) still gets the same behaviour a real install would after conf.sh ran — the
//! same contract every other crate in this workspace holds with conf.sh (aeon, landing-pass,
//! queue read `std::env::var` directly; conf.sh has already exported these into the process
//! tree by the time a bare-named binary runs).

use std::env;
use std::path::PathBuf;

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
        let home = var("SPIRA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
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
