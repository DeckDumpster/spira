//! Every effect `testenv suites` has outside its own state directory, as a trait
//! (DESIGN-suites.md §5): git, the lib.sh seam, incident.sh, mail, host-check.sh, the
//! queue, the change bead (bead.sh and lib.sh's claim), the clock and the two output
//! streams. `real.rs` implements them against the host; the unit tests implement them as
//! fakes.

use std::path::{Path, PathBuf};

use crate::settings::Source;
use spira_config::process::{cfg, cfg_parse};

/// Resolved settings (DESIGN-suites.md §2.5): registered keys (`spira/conf.d`) through
/// `spira_config::process::cfg`/`cfg_parse` — the one source of config; a handful of
/// non-registered names (SPIRA_AEON) still read the environment directly via `Source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The harness root (suites.sh's `$HERE/..`).
    pub root: PathBuf,
    /// `<root>/spira` (suites.sh's `$HERE`): the suites, gate-suites, the helper scripts.
    pub suite_dir: PathBuf,
    pub state: PathBuf,
    pub stale: u64,
    pub priority: u8,
    pub gate_list: PathBuf,
    /// Repository-relative path of the lifecycle file.
    pub suite_state_file: String,
    pub flake_at: u64,
    pub flake_window: u64,
    pub max_age: u64,
    /// SPIRA_AEON is non-empty: transitions are refused.
    pub aeon: bool,
    pub git_name: String,
    pub git_email: String,
}

/// SPIRA_SUITES_PRIORITY's declared range (0..=4) — pure, so a unit test can drive it
/// without touching spira-config's per-process resolution cache.
fn validate_priority(priority: u8) -> Result<u8, String> {
    if priority > 4 {
        return Err(format!(
            "SPIRA_SUITES_PRIORITY={priority} is refused — priority must be 0..=4"
        ));
    }
    Ok(priority)
}

impl Settings {
    pub fn load(src: &Source, root: &Path) -> Result<Settings, String> {
        let suite_dir = root.join("spira");
        let gate_list = PathBuf::from(cfg("SPIRA_GATE_SUITES")?);
        let priority = validate_priority(cfg_parse("SPIRA_SUITES_PRIORITY")?)?;
        Ok(Settings {
            state: PathBuf::from(cfg("SPIRA_SUITES_STATE")?),
            stale: cfg_parse("SPIRA_SUITES_STALE")?,
            priority,
            gate_list,
            suite_state_file: cfg("SPIRA_SUITE_STATE_FILE")?,
            flake_at: cfg_parse("SPIRA_FLAKE_QUARANTINE_AT")?,
            flake_window: cfg_parse("SPIRA_FLAKE_WINDOW")?,
            max_age: cfg_parse("SPIRA_QUARANTINE_MAX_AGE")?,
            aeon: src.get("SPIRA_AEON").is_some(),
            git_name: cfg("SPIRA_GIT_NAME")?,
            git_email: cfg("SPIRA_GIT_EMAIL")?,
            root: root.to_path_buf(),
            suite_dir,
        })
    }

    pub fn suite_path(&self, suite: &str) -> PathBuf {
        self.suite_dir.join(suite)
    }
    pub fn state_file(&self, suite: &str, ext: &str) -> PathBuf {
        self.state.join(format!("{suite}.{ext}"))
    }
    /// The harness checkout's lifecycle file (what list, corpus and hygiene read).
    pub fn lifecycle_file(&self) -> PathBuf {
        self.root.join(&self.suite_state_file)
    }
}

/// What the lib.sh seam answers (DESIGN-suites.md §4, S1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conf {
    pub home_repo: String,
    /// `SPIRA_SCOPE_LABEL` when set (possibly empty); None when unset.
    pub scope_label: Option<String>,
    pub db: String,
    /// `repo_root <home_repo>`; None when the map does not resolve it.
    pub repo_path: Option<PathBuf>,
    /// `spira_landref <repo_path>`; None when it cannot be resolved.
    pub landref: Option<String>,
}

/// The finding observe-flake files through incident.sh (DESIGN-suites.md §3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlakeFiling {
    pub suite: String,
    pub title: String,
    pub priority: u8,
    pub labels: String,
    pub repo: String,
    pub reference: String,
    pub path: PathBuf,
    pub db: String,
    pub payload: String,
}

pub trait Clock {
    fn now(&self) -> u64;
}

pub trait LibSeam {
    fn conf(&self) -> Result<Conf, String>;
}

pub trait Intake {
    /// Run `incident.sh file <title> -`; Ok(its stdout) or Err(the diagnostic).
    fn file(&self, f: &FlakeFiling) -> Result<String, String>;
}

pub trait Mail {
    /// `mail send operator --from … --subject … [--bead …]`, body on stdin.
    fn send_operator(&self, from: &str, subject: &str, bead: Option<&str>, body: &str) -> bool;
}

pub trait HostCheck {
    /// `host-check.sh <flag>`'s stdout (trailing newlines stripped), None when it cannot be
    /// read: not executable, failed, silent, or past its wall.
    fn count(&self, flag: &str) -> Option<String>;
}

pub trait Queue {
    /// `queue submit <branch>`, its output on our stderr. True on success.
    fn submit(&self, branch: &str) -> bool;
}

/// The bead a suite-state edit lands as (sp-lck63): the edit is a change like any other, so
/// it rides spira/<bead> through the lifecycle machine — READY, claimed by the writer,
/// SUBMITTED and CERTIFIED by `queue submit`, LANDED by a round.
pub trait Change {
    /// `bead.sh file <title> --for ops --repo <repo> --submitted --priority 1 --json`
    /// (`SPIRA_DB=<db>` when set); Ok(the filed id, parsed from the JSON) or Err(why).
    fn file(&self, title: &str, repo: &str, db: &str) -> Result<String, String>;
    /// lib.sh's `lc_claim_bead <id> <holder> <lease-until>` — the harness's one claim path,
    /// which creates the READY row first when the bead has none. Err(why) on any refusal.
    fn claim(&self, id: &str, holder: &str, lease_until: u64) -> Result<(), String>;
}

pub trait Git {
    /// `rev-parse --verify -q <rev>^{commit}`.
    fn commit_of(&self, repo: &Path, rev: &str) -> Option<String>;
    /// `cat-file -e <commit>:<path>`.
    fn tree_has(&self, repo: &Path, commit: &str, path: &str) -> bool;
    /// `show <commit>:<path>`; None when absent.
    fn show(&self, repo: &Path, commit: &str, path: &str) -> Option<String>;
    /// A commit on `parent` whose tree is `parent`'s with `path` = `content`. Touches no
    /// working tree, index or HEAD. Ok(commit sha).
    fn commit_file(
        &self,
        repo: &Path,
        parent: &str,
        path: &str,
        content: &str,
        message: &str,
        who: (&str, &str),
    ) -> Result<String, String>;
    /// `update-ref refs/heads/<branch> <commit> ""` — create only.
    fn create_branch(&self, repo: &Path, branch: &str, commit: &str) -> bool;
}

pub trait Emit {
    fn out(&self, line: &str);
    fn err(&self, line: &str);
}

/// Everything a command may touch.
pub struct World<'a> {
    pub s: &'a Settings,
    pub clock: &'a dyn Clock,
    pub lib: &'a dyn LibSeam,
    pub intake: &'a dyn Intake,
    pub mail: &'a dyn Mail,
    pub host: &'a dyn HostCheck,
    pub queue: &'a dyn Queue,
    pub change: &'a dyn Change,
    pub git: &'a dyn Git,
    pub io: &'a dyn Emit,
    /// Reads a `--reason-file` (a path, or `-` for stdin).
    pub read_input: &'a dyn Fn(&str) -> Result<String, String>,
}

impl World<'_> {
    pub fn out(&self, s: impl AsRef<str>) {
        self.io.out(s.as_ref());
    }
    pub fn err(&self, s: impl AsRef<str>) {
        self.io.err(s.as_ref());
    }
    /// lib.sh `log`'s shape, on stderr (DESIGN-suites.md §6 D1).
    pub fn log(&self, msg: &str) {
        self.io.err(&format!(
            "{} spira: {msg}",
            crate::util::iso_utc(self.clock.now())
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every registered key here (SPIRA_RUN, SPIRA_SUITES_STALE, SPIRA_FLAKE_QUARANTINE_AT,
    /// SPIRA_GATE_SUITES, SPIRA_SUITES_PRIORITY, SPIRA_SUITES_STATE, SPIRA_FLAKE_WINDOW,
    /// SPIRA_GIT_NAME, SPIRA_GIT_EMAIL, SPIRA_SUITE_STATE_FILE, SPIRA_QUARANTINE_MAX_AGE) now
    /// goes through `cfg`/`cfg_parse`, which resolve once per process from spira-config's own
    /// cache — not something a unit test can drive per-case. That default/parse behavior is
    /// spira-config's to test; what stays testable here is the pure validation below and the
    /// one remaining non-registered field, `aeon`.
    #[test]
    fn priority_above_four_is_refused_by_name() {
        assert_eq!(validate_priority(0), Ok(0));
        assert_eq!(validate_priority(4), Ok(4));
        assert!(validate_priority(5).is_err());
    }

    #[test]
    fn aeon_is_whether_spira_aeon_is_set_at_all() {
        let env: BTreeMap<&str, &str> = [("SPIRA_AEON", "sp-1")].into();
        let f = |k: &str| env.get(k).map(|v| v.to_string());
        assert!(Source { env: &f }.get("SPIRA_AEON").is_some());

        let none = |_: &str| None;
        assert!(Source { env: &none }.get("SPIRA_AEON").is_none());
    }
}
