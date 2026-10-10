//! The impure boundary to the rest of the harness: repo resolution, the bead store
//! and the certification gate. Production shells to lib.sh / the queue binary — the same tested
//! functions the bash called, so their side effects (release_claim, the
//! requeue event, the TSD dual-write) are not re-derived here. Tests use a recording fake.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub name: String,
    pub path: PathBuf,
    /// The land ref (`spira_landref`), e.g. `local/main` or `origin/master`.
    pub landref: String,
}

/// The lifecycle witness's answer: the bead's row state ("" for a rowless bead). `Unreachable`
/// is distinct from any state: a machine that cannot answer reads as "somebody may be home"
/// (law-a-control-that-cannot-check-must-refuse).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BeadStatus {
    Known(String),
    Unreachable,
}

pub trait Seam {
    fn home_repo(&self) -> Result<String, String>;
    fn repo(&self, name: &str) -> Result<RepoInfo, String>;
    fn bead_status(&self, id: &str) -> BeadStatus;
    fn reopen(&self, id: &str, cause: &str, note: &str);
    fn bump_requeue(&self, id: &str, reason: &str);
    fn note(&self, id: &str, text: &str);
    /// `queue submit <branch> <repo>` — the gate's verdict and its combined output.
    fn submit(&self, branch: &str, repo_name: &str) -> (Gate, String);
}

/// What the gate said. `NoVerdict` (queue's exit 75) judged nothing — a budget cut, a
/// timeout — and is never a red.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Green,
    Red,
    NoVerdict,
    BaseRed,
}

const GATE_NO_VERDICT: i32 = 75;
const GATE_BASE_RED: i32 = 76;

pub struct LibSeam {
    pub home: PathBuf,
    /// The lifecycle machine's binary: the one place a bead's state is asked.
    pub lc_bin: String,
    /// The queue binary: `queue`, by name on the launcher's PATH (sp-gypjk).
    pub queue_bin: PathBuf,
    /// `$SPIRA_RUN`, passed explicitly rather than left to this process's own ambient environment.
    pub run: PathBuf,
}

impl LibSeam {
    pub fn new(home: PathBuf, lc_bin: String, run: PathBuf) -> LibSeam {
        LibSeam {
            home,
            lc_bin,
            queue_bin: PathBuf::from("queue"),
            run,
        }
    }

    /// `. lib.sh && <func> <args…> [<payload from stdin>]`.
    fn lib(&self, func: &str, args: &[&str], payload: Option<&str>) -> Result<String, String> {
        let script = if payload.is_some() {
            r#". "$0" >/dev/null 2>&1 || exit 97; __p="$(cat)"; "$@" "$__p""#
        } else {
            r#". "$0" >/dev/null 2>&1 || exit 97; "$@""#
        };
        let mut c = spira_config::bounded::bounded("bash");
        c.arg("-c")
            .arg(script)
            .arg(self.home.join("lib.sh"))
            .arg(func)
            .args(args);
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
        // release's bin/+spira/ on the CHILD's PATH, never only inherited.
        c.envs(spira_config::release_env::child_path_env_for_process());
        c.stdin(if payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| format!("lib.sh {func}: {e}"))?;
        if let (Some(p), Some(mut si)) = (payload, child.stdin.take()) {
            let _ = si.write_all(p.as_bytes());
        }
        let o = child
            .wait_with_output()
            .map_err(|e| format!("lib.sh {func}: {e}"))?;
        if !o.status.success() {
            return Err(format!(
                "lib.sh {func} exited {}",
                o.status.code().unwrap_or(-1)
            ));
        }
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    }
}

impl Seam for LibSeam {
    fn home_repo(&self) -> Result<String, String> {
        self.lib("spira_home_repo", &[], None).and_then(|s| {
            if s.is_empty() {
                Err("spira_home_repo is empty".into())
            } else {
                Ok(s)
            }
        })
    }

    fn repo(&self, name: &str) -> Result<RepoInfo, String> {
        let path = self
            .lib("repo_root", &[name], None)
            .map_err(|_| format!("cannot resolve repo {name}"))?;
        if path.is_empty() {
            return Err(format!("cannot resolve repo {name}"));
        }
        let landref = self
            .lib("spira_landref", &[name], None)
            .map_err(|_| format!("cannot resolve the land ref for {name}"))?;
        if landref.is_empty() {
            return Err(format!("cannot resolve the land ref for {name}"));
        }
        Ok(RepoInfo {
            name: name.into(),
            path: PathBuf::from(path),
            landref,
        })
    }

    fn bead_status(&self, id: &str) -> BeadStatus {
        match spira_config::lc_state::row_with(&self.lc_bin, id) {
            Err(_) => BeadStatus::Unreachable,
            Ok(row) => BeadStatus::Known(row.map(|r| r.state).unwrap_or_default()),
        }
    }

    fn reopen(&self, id: &str, cause: &str, note: &str) {
        let _ = self.lib("bead_reopen", &[id, cause], Some(note));
    }

    fn bump_requeue(&self, id: &str, reason: &str) {
        let _ = self.lib("bump_requeue", &[id, reason], None);
    }

    fn note(&self, id: &str, text: &str) {
        let _ = self.lib("bdq", &["note", id], Some(text));
    }

    fn submit(&self, branch: &str, repo_name: &str) -> (Gate, String) {
        // stdout and stderr combined into one capture, as queue.sh's `2>&1` did.
        let o = spira_config::bounded::bounded("bash")
            .arg("-c")
            .arg(r#"exec "$0" submit "$1" "$2" 2>&1"#)
            .arg(&self.queue_bin)
            .arg(branch)
            .arg(repo_name)
            .stdin(Stdio::null())
            .output();
        match o {
            Ok(o) => {
                let gate = match o.status.code() {
                    Some(0) => Gate::Green,
                    Some(GATE_NO_VERDICT) => Gate::NoVerdict,
                    Some(GATE_BASE_RED) => Gate::BaseRed,
                    _ => Gate::Red,
                };
                (gate, String::from_utf8_lossy(&o.stdout).into_owned())
            }
            Err(e) => (Gate::Red, format!("queue submit could not run: {e}")),
        }
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    fn stub(name: &str, body: &str) -> (testkit::TempDir, LibSeam) {
        let d = testkit::TempDir::new(&format!("rs-probe-{name}"));
        let lc = d.join("spira-lc");
        testkit::write_exe(&lc, &format!("#!/bin/sh\n{body}\n"));
        let seam = LibSeam::new(d.to_path_buf(), lc.to_string_lossy().into(), d.to_path_buf());
        (d, seam)
    }

    #[test]
    fn a_working_row_is_known_and_a_rowless_bead_is_known_empty() {
        let (d, seam) = stub(
            "row",
            r#"[ "$2" = sp-held ] && { echo '{"bead":{"bead_id":"sp-held","state":"WORKING"}}'; exit 0; }; exit 1"#,
        );
        assert_eq!(seam.bead_status("sp-held"), BeadStatus::Known("WORKING".into()));
        assert_eq!(seam.bead_status("sp-none"), BeadStatus::Known(String::new()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_machine_that_cannot_answer_is_unreachable() {
        let (d, seam) = stub("down", "echo broken >&2; exit 2");
        assert_eq!(seam.bead_status("sp-held"), BeadStatus::Unreachable);
        let _ = std::fs::remove_dir_all(&d);
    }
}
