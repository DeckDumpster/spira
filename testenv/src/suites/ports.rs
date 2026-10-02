//! Every effect `testenv suites` has outside its own state directory, as a trait
//! (DESIGN-suites.md §5): git, the lib.sh seam, incident.sh, mail, host-check.sh, the
//! queue, the clock and the two output streams. `real.rs` implements them against the
//! host; the unit tests implement them as fakes.

use std::path::{Path, PathBuf};

use crate::settings::{derive_run, Source};

/// Resolved settings (DESIGN-suites.md §2.5): environment, then spira.toml through the
/// spira-config library, then the default.
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

impl Settings {
    pub fn load(src: &Source, root: &Path) -> Settings {
        let num = |env: &str, key: Option<&str>, d: u64| -> u64 {
            src.get(env, key)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(d)
        };
        let instance = src
            .get("SPIRA_INSTANCE", Some("spira.instance"))
            .unwrap_or_else(|| "prod".into());
        let run = src
            .get("SPIRA_RUN", Some("spira.run"))
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let repo = src
                    .get("SPIRA_REPO", None)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| root.to_path_buf());
                derive_run(&repo, &instance, src.env)
            });
        let suite_dir = root.join("spira");
        let gate_list = src
            .get("SPIRA_GATE_SUITES", Some("spira.gate_suites"))
            .map(PathBuf::from)
            .or_else(|| {
                src.get("SPIRA_HOME", None)
                    .map(|h| PathBuf::from(h).join("gate-suites"))
            })
            .unwrap_or_else(|| suite_dir.join("gate-suites"));
        let priority = src
            .get("SPIRA_SUITES_PRIORITY", Some("spira.suites_priority"))
            .and_then(|v| v.trim().parse::<u8>().ok())
            .filter(|p| *p <= 4)
            .unwrap_or(3);
        Settings {
            state: src
                .get("SPIRA_SUITES_STATE", Some("spira.suites_state"))
                .map(PathBuf::from)
                .unwrap_or_else(|| run.join("suites")),
            stale: num("SPIRA_SUITES_STALE", Some("spira.suites_stale"), 21_600),
            priority,
            gate_list,
            suite_state_file: src
                .get("SPIRA_SUITE_STATE_FILE", Some("spira.suite_state_file"))
                .unwrap_or_else(|| "spira/suite-state".into()),
            flake_at: num("SPIRA_FLAKE_QUARANTINE_AT", Some("spira.flake_quarantine_at"), 2),
            flake_window: num("SPIRA_FLAKE_WINDOW", Some("spira.flake_window"), 604_800),
            max_age: num("SPIRA_QUARANTINE_MAX_AGE", Some("spira.quarantine_max_age"), 604_800),
            aeon: src.get("SPIRA_AEON", None).is_some(),
            git_name: src.get("SPIRA_GIT_NAME", None).unwrap_or_else(|| "spira".into()),
            git_email: src
                .get("SPIRA_GIT_EMAIL", None)
                .unwrap_or_else(|| "spira@spira.invalid".into()),
            root: root.to_path_buf(),
            suite_dir,
        }
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

    #[test]
    fn settings_env_then_config_then_default() {
        let env: BTreeMap<&str, &str> = [
            ("SPIRA_RUN", "/run/x"),
            ("SPIRA_SUITES_STALE", "60"),
            ("SPIRA_FLAKE_QUARANTINE_AT", "junk"),
            ("SPIRA_AEON", "sp-1"),
        ]
        .into();
        let f = |k: &str| env.get(k).map(|v| v.to_string());
        let doc = spira_config::validate("[spira]\nsuites_state = \"/cfg/suites\"\nsuites_priority = \"1\"\n").unwrap();
        let s = Settings::load(&Source { env: &f, config: Some(&doc) }, Path::new("/h"));
        assert_eq!(s.state, PathBuf::from("/cfg/suites"));
        assert_eq!(s.stale, 60);
        assert_eq!(s.priority, 1);
        assert_eq!(s.flake_at, 2, "a malformed value falls back to the default");
        assert_eq!(s.gate_list, PathBuf::from("/h/spira/gate-suites"));
        assert_eq!(s.lifecycle_file(), PathBuf::from("/h/spira/suite-state"));
        assert!(s.aeon);
        let none = |_: &str| None;
        let d = Settings::load(&Source { env: &none, config: None }, Path::new("/h"));
        assert_eq!(d.priority, 3, "conf.sh's default, not suites.sh's shadowed 2");
        assert_eq!((d.flake_window, d.max_age), (604_800, 604_800));
        assert!(!d.aeon);
        let home = |k: &str| (k == "SPIRA_HOME").then(|| "/prod/spira".to_string());
        let h = Settings::load(&Source { env: &home, config: None }, Path::new("/h"));
        assert_eq!(h.gate_list, PathBuf::from("/prod/spira/gate-suites"));
    }
}
