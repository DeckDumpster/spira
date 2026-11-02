//! [`Installed`] on the running release: the round image is the one built from the release's own
//! recipe, which `round-vm recipe-sha` names and a template or AMI built from any other recipe is
//! refused against; the harness is the commit the release's `MANIFEST` was built from. Each is
//! read once per process.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};

use crate::lane::{Installed, LaneInputs};

/// `recipe-sha` hashes six files compiled into round-vm: milliseconds; this bounds a wedged spawn.
const RECIPE_DEADLINE: Duration = Duration::from_secs(5);

/// Whether a repo lands by rounds, as the registry says.
pub type LandsByRounds = dyn Fn(&str) -> bool + Send + Sync;

pub struct InstalledRelease {
    pub proc_: Arc<dyn Proc>,
    pub round_vm: String,
    /// The release root, where its `MANIFEST` sits; `None` when the process names none.
    pub release: Option<PathBuf>,
    pub rounds: Box<LandsByRounds>,
    read: OnceLock<Result<LaneInputs, String>>,
}

impl InstalledRelease {
    pub fn new(
        proc_: Arc<dyn Proc>,
        round_vm: String,
        release: Option<PathBuf>,
        rounds: Box<LandsByRounds>,
    ) -> InstalledRelease {
        InstalledRelease {
            proc_,
            round_vm,
            release,
            rounds,
            read: OnceLock::new(),
        }
    }

    fn image_digest(&self) -> Result<String, String> {
        let o = self.proc_.run(&ProcSpec {
            prog: self.round_vm.clone(),
            args: vec!["recipe-sha".into()],
            stdin: Vec::new(),
            deadline: RECIPE_DEADLINE,
        })?;
        let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();
        match o.code {
            Some(0) if !sha.is_empty() => Ok(sha),
            code => Err(format!(
                "{} recipe-sha exited {code:?}: {}",
                self.round_vm,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
        }
    }

    fn harness_version(&self) -> Result<String, String> {
        let root = self.release.as_ref().ok_or_else(|| {
            format!(
                "{} is not set: no release names the harness",
                spira_config::RELEASE_ENV
            )
        })?;
        let path = root.join("MANIFEST");
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        manifest_commit(&text).ok_or_else(|| format!("{} names no commit", path.display()))
    }
}

/// The `commit <sha>` line a release's `MANIFEST` opens with.
pub fn manifest_commit(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next() == Some("commit"))
            .then(|| w.next().map(str::to_string))
            .flatten()
    })
}

impl Installed for InstalledRelease {
    fn lane_inputs(&self, repo: &str) -> Result<Option<LaneInputs>, String> {
        if !(self.rounds)(repo) {
            return Ok(None);
        }
        self.read
            .get_or_init(|| {
                Ok(LaneInputs {
                    image_digest: self.image_digest()?,
                    harness_version: self.harness_version()?,
                })
            })
            .clone()
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::ProcOutput;
    use std::sync::Mutex;

    struct Answers(Mutex<Vec<Result<ProcOutput, String>>>, Mutex<Vec<ProcSpec>>);

    impl Proc for Answers {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            crate::lock(&self.1).push(spec.clone());
            crate::lock(&self.0).remove(0)
        }
    }

    fn out(code: Option<i32>, stdout: &str, stderr: &str) -> Result<ProcOutput, String> {
        Ok(ProcOutput {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }

    fn release(manifest: Option<&str>) -> testkit::TempDir {
        let t = testkit::TempDir::new("rc-installed");
        if let Some(m) = manifest {
            std::fs::write(t.path().join("MANIFEST"), m).unwrap();
        }
        t
    }

    fn installed(
        answers: Vec<Result<ProcOutput, String>>,
        root: Option<PathBuf>,
    ) -> (InstalledRelease, Arc<Answers>) {
        let p = Arc::new(Answers(Mutex::new(answers), Mutex::default()));
        let i = InstalledRelease::new(
            p.clone(),
            "/rel/bin/round-vm".into(),
            root,
            Box::new(|r| r == "alpha"),
        );
        (i, p)
    }

    #[test]
    fn a_round_repo_rides_the_release_recipe_and_manifest_commit_read_once() {
        let t = release(Some(
            "commit c0ffee\nbuilt 2026-11-01T00:00:00Z\nrepo spira\n",
        ));
        let (i, p) = installed(
            vec![out(Some(0), "ab12\n", "")],
            Some(t.path().to_path_buf()),
        );
        let want = LaneInputs {
            image_digest: "ab12".into(),
            harness_version: "c0ffee".into(),
        };
        assert_eq!(i.lane_inputs("alpha"), Ok(Some(want.clone())));
        assert_eq!(i.lane_inputs("alpha"), Ok(Some(want)));
        let calls = crate::lock(&p.1);
        assert_eq!(calls.len(), 1, "read once per process");
        assert_eq!(
            (calls[0].prog.as_str(), calls[0].args.as_slice()),
            ("/rel/bin/round-vm", &["recipe-sha".to_string()][..])
        );
    }

    #[test]
    fn a_repo_that_lands_without_rounds_reads_nothing() {
        let (i, p) = installed(vec![], None);
        assert_eq!(i.lane_inputs("beta"), Ok(None));
        assert!(crate::lock(&p.1).is_empty());
    }

    #[test]
    fn an_unreadable_recipe_or_manifest_is_refused_naming_it() {
        let (i, _) = installed(vec![out(Some(2), "", "no such verb")], None);
        assert!(i
            .lane_inputs("alpha")
            .unwrap_err()
            .contains("recipe-sha exited Some(2): no such verb"));
        let (i, _) = installed(vec![out(Some(0), "\n", "")], None);
        assert!(i
            .lane_inputs("alpha")
            .unwrap_err()
            .contains("recipe-sha exited Some(0)"));
        let (i, _) = installed(vec![Err("spawn failed".into())], None);
        assert_eq!(i.lane_inputs("alpha").unwrap_err(), "spawn failed");
        let (i, _) = installed(vec![out(Some(0), "ab12", "")], None);
        assert!(i
            .lane_inputs("alpha")
            .unwrap_err()
            .contains("SPIRA_RELEASE is not set"));
        let t = release(None);
        let (i, _) = installed(vec![out(Some(0), "ab12", "")], Some(t.path().to_path_buf()));
        assert!(i.lane_inputs("alpha").unwrap_err().contains("MANIFEST"));
        let t = release(Some("built 2026-11-01T00:00:00Z\n"));
        let (i, _) = installed(vec![out(Some(0), "ab12", "")], Some(t.path().to_path_buf()));
        assert!(i
            .lane_inputs("alpha")
            .unwrap_err()
            .contains("names no commit"));
    }
}
