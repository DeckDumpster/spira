//! [`Facts`] on the installed harness: the registry's round repos, their landing refs read with
//! `git`, the queue directory's round files, and the SUBMITTED beads through `spira-lc list`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use runtime::ports::{Proc, ProcSpec};
use spira_config::repos::Registry;

use crate::cutover_check::{Facts, QUEUE_FILES};
use crate::real::git::Locate;

const GIT_DEADLINE: Duration = Duration::from_secs(5);
const LC_DEADLINE: Duration = Duration::from_secs(5);

/// The registry's `queue.local` and `queue.forge` repos, in registry order.
pub fn round_repos(reg: &Registry) -> Result<Vec<String>, String> {
    if !reg.map_present() {
        return Err("no repository registry is declared".into());
    }
    Ok(reg.all().into_iter().filter(|r| reg.land_queued(r)).collect())
}

pub struct RealFacts {
    pub proc_: Arc<dyn Proc>,
    pub repos: Result<Vec<String>, String>,
    /// Each repo's checkout and landing ref, as the registry resolves them.
    pub locate: Box<Locate>,
    pub queue_dir: PathBuf,
    pub spira_lc: String,
}

type Ran = (Option<i32>, String, String);

impl RealFacts {
    fn run(&self, prog: &str, args: Vec<String>, deadline: Duration) -> Result<Ran, String> {
        let o = self.proc_.run(&ProcSpec {
            prog: prog.into(),
            args,
            stdin: vec![],
            deadline,
        })?;
        Ok((
            o.code,
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ))
    }

    fn git(&self, root: &str, args: &[&str]) -> Result<Ran, String> {
        let mut all = vec!["-C".to_string(), root.to_string()];
        all.extend(args.iter().map(|a| a.to_string()));
        self.run("git", all, GIT_DEADLINE)
    }
}

impl RealFacts {
    /// `repo`'s checkout and the commit its landing ref is at.
    fn landing(&self, repo: &str) -> Result<(String, String), String> {
        let (root, landref) = (self.locate)(repo)
            .ok_or_else(|| format!("{repo}: no checkout or landing ref resolves"))?;
        let spec = format!("{landref}^{{commit}}");
        match self.git(&root, &["rev-parse", "--verify", "--quiet", &spec])? {
            (Some(0), out, _) if !out.trim().is_empty() => Ok((root, out.trim().to_string())),
            (code, _, err) => Err(format!(
                "git rev-parse {landref} in {root} exited {code:?}: {err}"
            )),
        }
    }
}

impl Facts for RealFacts {
    fn round_repos(&self) -> Result<Vec<String>, String> {
        self.repos.clone()
    }

    fn at_landing(&self, repo: &str, path: &str) -> Result<Option<String>, String> {
        let (root, sha) = self.landing(repo)?;
        match self.git(&root, &["ls-tree", "--name-only", &sha, "--", path])? {
            (Some(0), out, _) if out.trim().is_empty() => return Ok(None),
            (Some(0), _, _) => {}
            (code, _, err) => {
                return Err(format!("git ls-tree {sha} {path} in {root} exited {code:?}: {err}"))
            }
        }
        match self.git(&root, &["cat-file", "-p", &format!("{sha}:{path}")])? {
            (Some(0), out, _) => Ok(Some(out)),
            (code, _, err) => Err(format!(
                "git cat-file {sha}:{path} in {root} exited {code:?}: {err}"
            )),
        }
    }

    fn names_at_landing(&self, repo: &str, dir: &str) -> Result<Vec<String>, String> {
        let (root, sha) = self.landing(repo)?;
        let tree = format!("{sha}:{dir}");
        match self.git(&root, &["ls-tree", "--name-only", &tree])? {
            (Some(0), out, _) => Ok(out.lines().filter(|l| !l.is_empty()).map(String::from).collect()),
            (code, _, err) => Err(format!("git ls-tree {tree} in {root} exited {code:?}: {err}")),
        }
    }

    fn queue_files(&self) -> Result<Vec<String>, String> {
        let dir = &self.queue_dir;
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        };
        let mut out = Vec::new();
        for e in rd {
            let e = e.map_err(|e| format!("{}: {e}", dir.display()))?;
            let repo = e.file_name().to_string_lossy().into_owned();
            for f in QUEUE_FILES {
                let path = e.path().join(f);
                if path.try_exists().map_err(|err| format!("{}: {err}", path.display()))? {
                    out.push(format!("{repo}/{f}"));
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn submitted(&self, repo: &str) -> Result<usize, String> {
        let args = ["list", "--state", "SUBMITTED", "--repo", repo]
            .map(String::from)
            .to_vec();
        match self.run(&self.spira_lc, args, LC_DEADLINE)? {
            (Some(0), out, _) => serde_json::from_str::<Vec<serde_json::Value>>(out.trim())
                .map(|v| v.len())
                .map_err(|e| format!("spira-lc list answered no JSON array: {e}")),
            (code, out, err) => Err(format!(
                "spira-lc list exited {code:?}: {} {err}",
                out.trim()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Mutex;

    use runtime::ports::ProcOutput;

    use super::*;

    /// Answers each call by its first two arguments after `-C <root>`, else by the program.
    #[derive(Default)]
    struct Script {
        answers: BTreeMap<String, Result<ProcOutput, String>>,
        seen: Mutex<Vec<ProcSpec>>,
    }

    fn out(code: i32, stdout: &str, stderr: &str) -> Result<ProcOutput, String> {
        Ok(ProcOutput {
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    impl Proc for Script {
        fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
            crate::lock(&self.seen).push(spec.clone());
            let key = match spec.prog.as_str() {
                "git" => spec.args[2].clone(),
                p => p.to_string(),
            };
            self.answers
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Err(format!("unscripted {key}")))
        }
    }

    struct Under {
        facts: RealFacts,
        script: Arc<Script>,
    }

    impl std::ops::Deref for Under {
        type Target = RealFacts;
        fn deref(&self) -> &RealFacts {
            &self.facts
        }
    }

    fn facts(answers: &[(&str, Result<ProcOutput, String>)], queue_dir: &Path) -> Under {
        let s = Script {
            answers: answers
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            ..Script::default()
        };
        let script = Arc::new(s);
        let facts = RealFacts {
            proc_: script.clone(),
            repos: Ok(vec!["alpha".into()]),
            locate: Box::new(|r| (r == "alpha").then(|| ("/r/alpha".into(), "local/main".into()))),
            queue_dir: queue_dir.to_path_buf(),
            spira_lc: "spira-lc".into(),
        };
        Under { facts, script }
    }

    #[test]
    fn the_round_repos_are_the_queued_ones_and_no_registry_is_refused() {
        let map = "alpha | /a | queue.local | local/main | |\nbeta | /b | push | main | |\ngamma | /g | queue.forge | origin/main | |\n";
        let reg = Registry::new(Some(map), &BTreeMap::new(), Path::new("/h/alpha"));
        assert_eq!(round_repos(&reg), Ok(vec!["alpha".into(), "gamma".into()]));
        let none = Registry::new(None, &BTreeMap::new(), Path::new("/h/alpha"));
        assert!(round_repos(&none).unwrap_err().contains("no repository registry"));
    }

    #[test]
    fn a_file_at_the_landing_ref_is_read_and_an_absent_one_is_none() {
        let d = testkit::TempDir::new("rc-facts");
        let f = facts(
            &[
                ("rev-parse", out(0, "abc\n", "")),
                ("ls-tree", out(0, "gate.steps\n", "")),
                ("cat-file", out(0, "step make\n", "")),
            ],
            &d,
        );
        assert_eq!(f.at_landing("alpha", "gate.steps"), Ok(Some("step make\n".into())));
        let f = facts(&[("rev-parse", out(0, "abc\n", "")), ("ls-tree", out(0, "", ""))], &d);
        assert_eq!(f.at_landing("alpha", "gate.steps"), Ok(None));
    }

    #[test]
    fn a_landing_ref_or_file_git_cannot_read_is_refused_naming_the_call() {
        let d = testkit::TempDir::new("rc-facts");
        let f = facts(&[], &d);
        assert!(f.at_landing("ghost", "x").unwrap_err().contains("no checkout"));
        assert!(f.at_landing("alpha", "x").unwrap_err().contains("unscripted rev-parse"));
        let f = facts(&[("rev-parse", out(128, "", "unknown revision"))], &d);
        assert!(f.at_landing("alpha", "x").unwrap_err().contains("unknown revision"));
        let f = facts(&[("rev-parse", out(0, "abc", "")), ("ls-tree", out(128, "", "bad tree"))], &d);
        assert!(f.at_landing("alpha", "x").unwrap_err().contains("git ls-tree abc x"));
        let f = facts(
            &[
                ("rev-parse", out(0, "abc", "")),
                ("ls-tree", out(0, "x\n", "")),
                ("cat-file", out(128, "", "bad object")),
            ],
            &d,
        );
        assert!(f.at_landing("alpha", "x").unwrap_err().contains("bad object"));
    }

    #[test]
    fn the_names_under_a_directory_of_the_landing_ref_are_listed() {
        let d = testkit::TempDir::new("rc-facts");
        let f = facts(&[("rev-parse", out(0, "abc\n", "")), ("ls-tree", out(0, "test-a.sh\nlib.sh\n", ""))], &d);
        assert_eq!(f.names_at_landing("alpha", "spira"), Ok(vec!["test-a.sh".into(), "lib.sh".into()]));
        let spec = crate::lock(&f.script.seen).last().cloned().unwrap();
        assert_eq!(spec.args.join(" "), "-C /r/alpha ls-tree --name-only abc:spira");
        let f = facts(&[("rev-parse", out(0, "abc\n", "")), ("ls-tree", out(128, "", "not a tree"))], &d);
        assert!(f.names_at_landing("alpha", "spira").unwrap_err().contains("not a tree"));
        assert!(f.names_at_landing("ghost", "spira").unwrap_err().contains("no checkout"));
    }

    #[test]
    fn every_open_or_prepared_file_under_the_queue_directory_is_named() {
        let d = testkit::TempDir::new("rc-facts");
        assert_eq!(facts(&[], &d.join("absent")).queue_files(), Ok(vec![]));
        for (repo, file) in [("beta", "prepared"), ("alpha", "open"), ("alpha", "round")] {
            std::fs::create_dir_all(d.join(repo)).unwrap();
            std::fs::write(d.join(repo).join(file), "").unwrap();
        }
        assert_eq!(
            facts(&[], &d).queue_files(),
            Ok(vec!["alpha/open".into(), "beta/prepared".into()])
        );
        std::fs::write(d.join("plain"), "").unwrap();
        assert!(facts(&[], &d.join("plain")).queue_files().unwrap_err().contains("plain"));
    }

    #[test]
    fn the_submitted_beads_are_counted_from_spira_lc_and_a_bad_answer_is_refused() {
        let d = testkit::TempDir::new("rc-facts");
        let f = facts(&[("spira-lc", out(0, "[{\"bead_id\":\"sp-a\"},{\"bead_id\":\"sp-b\"}]\n", ""))], &d);
        assert_eq!(f.submitted("alpha"), Ok(2));
        let spec = crate::lock(&f.script.seen).last().cloned().unwrap();
        assert_eq!(
            (spec.prog.as_str(), spec.args.join(" ")),
            ("spira-lc", "list --state SUBMITTED --repo alpha".into())
        );
        let f = facts(&[("spira-lc", out(0, "not json", ""))], &d);
        assert!(f.submitted("alpha").unwrap_err().contains("no JSON array"));
        let f = facts(&[("spira-lc", out(2, "", "cannot tell"))], &d);
        assert!(f.submitted("alpha").unwrap_err().contains("cannot tell"));
    }
}
