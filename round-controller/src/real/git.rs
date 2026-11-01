//! [`Git`] on the repository's own checkout: its landing ref resolved through the registry, and
//! read with `git` spawned through [`Proc`].

use std::sync::Arc;
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};

use crate::ports::Git;

/// A read of a local ref takes milliseconds; this bounds a wedged filesystem.
const GIT_DEADLINE: Duration = Duration::from_secs(5);

/// Where `repo` lives and its landing ref's name, as the registry resolves them.
pub type Locate = dyn Fn(&str) -> Option<(String, String)> + Send + Sync;

pub struct RealGit {
    proc_: Arc<dyn Proc>,
    locate: Box<Locate>,
}

impl RealGit {
    pub fn new(proc_: Arc<dyn Proc>, locate: Box<Locate>) -> RealGit {
        RealGit { proc_, locate }
    }

    fn located(&self, repo: &str) -> Result<(String, String), String> {
        (self.locate)(repo).ok_or_else(|| format!("{repo}: no checkout or landing ref resolves"))
    }

    fn git(&self, root: &str, args: &[&str]) -> Result<(Option<i32>, String, String), String> {
        let mut all = vec!["-C".to_string(), root.to_string()];
        all.extend(args.iter().map(|a| a.to_string()));
        let out = self.proc_.run(&ProcSpec {
            prog: "git".into(),
            args: all,
            stdin: Vec::new(),
            deadline: GIT_DEADLINE,
        })?;
        Ok((
            out.code,
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

impl Git for RealGit {
    fn landing_sha(&self, repo: &str) -> Result<String, String> {
        let (root, landref) = self.located(repo)?;
        let spec = format!("{landref}^{{commit}}");
        match self.git(&root, &["rev-parse", "--verify", "--quiet", &spec])? {
            (Some(0), sha, _) if !sha.is_empty() => Ok(sha),
            (code, _, err) => Err(format!(
                "git rev-parse {landref} in {root} exited {code:?}: {err}"
            )),
        }
    }

    /// `merge-base --is-ancestor` exits 1 for "not an ancestor"; any other code is a fault.
    fn landed(&self, repo: &str, commit: &str) -> Result<bool, String> {
        let (root, landref) = self.located(repo)?;
        match self.git(&root, &["merge-base", "--is-ancestor", commit, &landref])? {
            (Some(0), _, _) => Ok(true),
            (Some(1), _, _) => Ok(false),
            (code, _, err) => Err(format!(
                "git merge-base --is-ancestor {commit} {landref} in {root} exited {code:?}: {err}"
            )),
        }
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
            self.1.lock().unwrap().push(spec.clone());
            self.0.lock().unwrap().remove(0)
        }
    }

    fn out(code: Option<i32>, stdout: &str, stderr: &str) -> Result<ProcOutput, String> {
        Ok(ProcOutput {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }

    fn git(answers: Vec<Result<ProcOutput, String>>) -> (RealGit, Arc<Answers>) {
        let p = Arc::new(Answers(Mutex::new(answers), Mutex::default()));
        let g = RealGit::new(
            p.clone(),
            Box::new(|r| (r == "lane").then(|| ("/src/lane".into(), "local/main".into()))),
        );
        (g, p)
    }

    #[test]
    fn the_landing_sha_is_the_landing_refs_commit_and_a_failed_read_is_named() {
        let (g, p) = git(vec![out(Some(0), "abc123\n", ""), out(Some(1), "", "")]);
        assert_eq!(g.landing_sha("lane").unwrap(), "abc123");
        assert_eq!(
            p.1.lock().unwrap()[0].args,
            [
                "-C",
                "/src/lane",
                "rev-parse",
                "--verify",
                "--quiet",
                "local/main^{commit}"
            ]
        );
        assert!(g.landing_sha("lane").unwrap_err().contains("local/main"));
        assert!(g.landing_sha("nowhere").unwrap_err().contains("nowhere"));
    }

    #[test]
    fn landed_is_ancestry_and_any_exit_but_zero_or_one_is_a_fault() {
        let (g, p) = git(vec![
            out(Some(0), "", ""),
            out(Some(1), "", ""),
            out(Some(128), "", "bad object h9"),
            Err("spawn failed".into()),
        ]);
        assert_eq!(g.landed("lane", "h1"), Ok(true));
        assert_eq!(g.landed("lane", "h2"), Ok(false));
        assert!(g
            .landed("lane", "h9")
            .unwrap_err()
            .contains("bad object h9"));
        assert_eq!(g.landed("lane", "h3").unwrap_err(), "spawn failed");
        assert_eq!(
            p.1.lock().unwrap()[0].args,
            [
                "-C",
                "/src/lane",
                "merge-base",
                "--is-ancestor",
                "h1",
                "local/main"
            ]
        );
    }
}
