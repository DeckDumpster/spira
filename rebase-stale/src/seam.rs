//! The impure boundary to the rest of the harness: repo resolution, the bead store, landstate
//! and the certification gate. Production shells to lib.sh / the queue binary — the same tested
//! functions the bash called, so their side effects (release_claim, WITHDRAWN on reopen, the
//! requeue event, the TSD dual-write) are not re-derived here. Tests use a recording fake.

use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub name: String,
    pub path: PathBuf,
    /// The land ref (`spira_landref`), e.g. `local/main` or `origin/master`.
    pub landref: String,
}

/// The status witness's answer. `Unreachable` is distinct from any status: an unproven
/// database reads as "somebody may be home" (law-absence-needs-a-positive-control).
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
    fn land_mark(&self, id: &str, state: &str, tip: &str, reason: &str);
    fn note(&self, id: &str, text: &str);
    /// `queue submit <branch> <repo>` — (green?, combined output).
    fn submit(&self, branch: &str, repo_name: &str) -> (bool, String);
}

pub struct LibSeam {
    pub home: PathBuf,
    pub db: Option<PathBuf>,
    pub bd: String,
    pub goal: String,
    /// The queue binary: SPIRA_QUEUE_BIN, else the `queue` installed beside this binary.
    pub queue_bin: PathBuf,
    db_ok: OnceCell<bool>,
}

impl LibSeam {
    pub fn new(home: PathBuf, db: Option<PathBuf>, bd: String, goal: String) -> LibSeam {
        LibSeam {
            home,
            db,
            bd,
            goal,
            queue_bin: std::env::var_os("SPIRA_QUEUE_BIN")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|d| d.join("queue")))
                        .unwrap_or_else(|| PathBuf::from("queue"))
                }),
            db_ok: OnceCell::new(),
        }
    }

    /// `. lib.sh && <func> <args…> [<payload from stdin>]`.
    fn lib(&self, func: &str, args: &[&str], payload: Option<&str>) -> Result<String, String> {
        let script = if payload.is_some() {
            r#". "$0" >/dev/null 2>&1 || exit 97; __p="$(cat)"; "$@" "$__p""#
        } else {
            r#". "$0" >/dev/null 2>&1 || exit 97; "$@""#
        };
        let mut c = Command::new("bash");
        c.arg("-c")
            .arg(script)
            .arg(self.home.join("lib.sh"))
            .arg(func)
            .args(args);
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

    /// THE POSITIVE CONTROL: the store answered with at least one bead. It used to show the
    /// goal bead, "the one row this harness cannot run without" — but the configured goal
    /// (sp-spira) never existed, so the control never passed and every live-worktree check
    /// read "the bead database did not answer" (2026-09-29 walkthrough). Any row proves the
    /// store can say "in_progress"; no particular row is needed.
    fn bd_answers(&self) -> bool {
        let Some(db) = self.db.as_ref() else { return false };
        let Ok(o) = Command::new(&self.bd)
            .arg("-C")
            .arg(db)
            .args(["list", "--limit", "1", "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return false;
        };
        o.status.success()
            && matches!(serde_json::from_slice::<serde_json::Value>(&o.stdout),
                        Ok(serde_json::Value::Array(a)) if !a.is_empty())
    }

    fn bd_show_status(&self, id: &str) -> Option<String> {
        let db = self.db.as_ref()?;
        let o = Command::new(&self.bd)
            .arg("-C")
            .arg(db)
            .args(["show", id, "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).ok()?;
        let first = match v {
            serde_json::Value::Array(mut a) if !a.is_empty() => a.swap_remove(0),
            serde_json::Value::Object(_) => v,
            _ => return None,
        };
        Some(
            first
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        )
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
        let reachable = *self
            .db_ok
            .get_or_init(|| self.bd_answers());
        if !reachable {
            return BeadStatus::Unreachable;
        }
        // The database answered for the goal bead; a bead it cannot show is "unknown",
        // which is not in_progress — spira_bead_status's own reading.
        BeadStatus::Known(self.bd_show_status(id).unwrap_or_default())
    }

    fn reopen(&self, id: &str, cause: &str, note: &str) {
        let _ = self.lib("bead_reopen", &[id, cause], Some(note));
    }

    fn bump_requeue(&self, id: &str, reason: &str) {
        let _ = self.lib("bump_requeue", &[id, reason], None);
    }

    fn land_mark(&self, id: &str, state: &str, tip: &str, reason: &str) {
        let _ = self.lib("land_mark", &[id, state, tip, reason], None);
    }

    fn note(&self, id: &str, text: &str) {
        let _ = self.lib("bdq", &["note", id], Some(text));
    }

    fn submit(&self, branch: &str, repo_name: &str) -> (bool, String) {
        // stdout and stderr combined into one capture, as queue.sh's `2>&1` did.
        let o = Command::new("bash")
            .arg("-c")
            .arg(r#"exec "$0" submit "$1" "$2" 2>&1"#)
            .arg(&self.queue_bin)
            .arg(branch)
            .arg(repo_name)
            .stdin(Stdio::null())
            .output();
        match o {
            Ok(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).into_owned(),
            ),
            Err(e) => (false, format!("queue submit could not run: {e}")),
        }
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stub bd: `list` answers with `list_json`; `show sp-held` is in_progress; any other
    /// `show` (the missing goal among them) fails, as bd does for an unknown id.
    fn stub(name: &str, list_json: &str) -> (PathBuf, LibSeam) {
        let d = std::env::temp_dir().join(format!("rs-probe-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let bd = d.join("bd");
        std::fs::write(
            &bd,
            format!(
                "#!/bin/sh\ncase \"$3\" in\n list) printf '%s' '{list_json}' ;;\n show) [ \"$4\" = sp-held ] && printf '[{{\"id\":\"sp-held\",\"status\":\"in_progress\"}}]' && exit 0; exit 1 ;;\nesac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bd, std::fs::Permissions::from_mode(0o755)).unwrap();
        let seam = LibSeam::new(d.clone(), Some(d.clone()), bd.to_string_lossy().into(), "sp-spira".into());
        (d, seam)
    }

    #[test]
    fn a_missing_goal_bead_does_not_make_the_store_unreachable() {
        let (d, seam) = stub("goal", r#"[{"id":"sp-any"}]"#);
        assert_eq!(seam.bead_status("sp-held"), BeadStatus::Known("in_progress".into()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_store_that_lists_nothing_is_unreachable() {
        let (d, seam) = stub("empty", "[]");
        assert_eq!(seam.bead_status("sp-held"), BeadStatus::Unreachable);
        let _ = std::fs::remove_dir_all(&d);
    }
}
