use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{parse_stacked_id, Merge, Probe};

pub struct GitProbe {
    pub repo: PathBuf,
    pub base_ref: String,
    pub work: PathBuf,
    pub lint: bool,
    pub state: Box<dyn Fn(&str) -> Result<String, String>>,
}

fn git(dir: &Path) -> Command {
    let mut c = spira_config::bounded::bounded("git");
    c.arg("-C").arg(dir);
    c
}

fn out(c: &mut Command, what: &str) -> Result<String, String> {
    let o = c.output().map_err(|e| format!("{what}: {e}"))?;
    if !o.status.success() {
        return Err(format!("{what} exited {}: {}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("").trim()));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn checkout(repo: &Path, wt: &Path, commit: &str) -> Result<(), String> {
    if wt.join(".git").exists() {
        return out(git(wt).args(["checkout", "-q", "--detach", "--force", commit]), "git checkout").map(|_| ());
    }
    std::fs::create_dir_all(wt.parent().unwrap_or(wt)).map_err(|e| format!("{}: {e}", wt.display()))?;
    out(git(repo).args(["worktree", "add", "-q", "--detach", "--force"]).arg(wt).arg(commit), "git worktree add").map(|_| ())
}

fn is_noise(l: &str) -> bool {
    l.trim().is_empty() || l.starts_with("fence: ") || l.contains("clean —")
}

fn lint_lines(c: &mut Command, wt: &Path, what: &str) -> Result<Vec<String>, String> {
    let o = c.output().map_err(|e| format!("{what}: {e}"))?;
    let all = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    match o.status.code() {
        Some(0) => Ok(Vec::new()),
        Some(1) => Ok(all.lines().filter(|l| !is_noise(l)).map(|l| l.replace(&wt.display().to_string(), "<wt>")).collect()),
        code => Err(format!("{what} exited {code:?}: {}", all.lines().last().unwrap_or("").trim())),
    }
}

/// Findings in `merged` that `base` does not already carry.
pub fn new_findings(merged: &[String], base: &[String]) -> Vec<String> {
    merged.iter().filter(|l| !base.contains(l)).cloned().collect()
}

impl Probe for GitProbe {
    fn base(&self) -> Result<String, String> {
        out(git(&self.repo).args(["rev-parse", "--verify", &format!("{}^{{commit}}", self.base_ref)]), "git rev-parse").map(|s| s.trim().to_string())
    }

    fn merge(&self, tip: &str) -> Result<Merge, String> {
        let o = git(&self.repo)
            .args(["merge-tree", "--write-tree", "--name-only", "--no-messages", &self.base_ref, tip])
            .output()
            .map_err(|e| format!("git merge-tree: {e}"))?;
        let text = String::from_utf8_lossy(&o.stdout).into_owned();
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("").to_string();
        let hex = first.len() >= 40 && first.bytes().all(|b| b.is_ascii_hexdigit());
        match o.status.code() {
            Some(1) if hex => Ok(Merge::Conflict(lines.take_while(|l| !l.is_empty()).map(str::to_string).collect())),
            Some(0) if hex => {
                let base = self.base()?;
                let c = out(git(&self.repo).args(["commit-tree", &first, "-p", &base, "-p", tip, "-m", "sift merge"]), "git commit-tree")?;
                Ok(Merge::Clean(c.trim().to_string()))
            }
            code => Err(format!("git merge-tree exited {code:?}: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("").trim())),
        }
    }

    fn patch_id(&self, tip: &str) -> Result<String, String> {
        let diff = git(&self.repo).args(["diff", &format!("{}...{tip}", self.base_ref)]).output().map_err(|e| format!("git diff: {e}"))?;
        if !diff.status.success() {
            return Err("git diff failed".into());
        }
        let mut child = git(&self.repo).args(["patch-id", "--stable"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| format!("git patch-id: {e}"))?;
        child.stdin.take().ok_or("no stdin")?.write_all(&diff.stdout).map_err(|e| format!("git patch-id: {e}"))?;
        let o = child.wait_with_output().map_err(|e| format!("git patch-id: {e}"))?;
        if !o.status.success() {
            return Err("git patch-id failed".into());
        }
        Ok(String::from_utf8_lossy(&o.stdout).split_whitespace().next().unwrap_or("").to_string())
    }

    fn stacked_on(&self, id: &str, tip: &str) -> Result<Vec<String>, String> {
        let log = out(git(&self.repo).args(["log", "--format=%s", &format!("{}..{tip}", self.base_ref)]), "git log")?;
        let mut ids: Vec<String> = log.lines().filter_map(|s| parse_stacked_id(s, id)).collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    fn state(&self, id: &str) -> Result<String, String> {
        (self.state)(id)
    }

    fn lint(&self, merged: &str) -> Result<Vec<String>, String> {
        if !self.lint {
            return Ok(Vec::new());
        }
        let base = self.base()?;
        let target = self.work.join("lint-target");
        let (wt_m, wt_b) = (self.work.join("lint-merged"), self.work.join("lint-base"));
        checkout(&self.repo, &wt_m, merged)?;
        checkout(&self.repo, &wt_b, &base)?;
        // batch-job: a cargo build of the tree under test runs as long as the build does
        let mut build = Command::new("timeout");
        build.arg("1800").args(["cargo", "build", "--release", "-p", "spira-lint"]).current_dir(&wt_m).env("CARGO_TARGET_DIR", &target);
        out(&mut build, "cargo build -p spira-lint")?;
        let bin = target.join("release/spira-lint");
        let run = |wt: &Path| {
            // batch-job: a lint walk of the tracked tree
            let mut c = Command::new("timeout");
            c.arg("600").arg(&bin).arg("--root").arg(wt).arg("--base").arg(&base).current_dir(wt).env("SPIRA_GATE_BASE", &base);
            lint_lines(&mut c, wt, "spira-lint")
        };
        Ok(new_findings(&run(&wt_m)?, &run(&wt_b)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Merge, Probe};

    fn g(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn commit(dir: &Path, file: &str, body: &str, msg: &str) -> String {
        std::fs::write(dir.join(file), body).unwrap();
        g(dir, &["add", "-A"]);
        g(dir, &["commit", "-q", "-m", msg]);
        g(dir, &["rev-parse", "HEAD"])
    }

    fn fixture() -> (testkit::TempDir, GitProbe, [String; 3]) {
        let d = testkit::TempDir::new("sift-git");
        let p = d.path().to_path_buf();
        g(&p, &["init", "-q", "-b", "main"]);
        commit(&p, "f", "one\n", "root");
        g(&p, &["branch", "side"]);
        commit(&p, "f", "two\n", "base moves");
        g(&p, &["checkout", "-q", "side"]);
        let conflict = commit(&p, "f", "other\n", "sp-other: edits the same line");
        g(&p, &["checkout", "-q", "-b", "clean", "main"]);
        let clean = commit(&p, "g", "new\n", "spira: land sp-bead — adds g");
        let probe = GitProbe { repo: p.clone(), base_ref: "main".into(), work: p.join(".sift"), lint: true, state: Box::new(|_| Ok("SUBMITTED".into())) };
        (d, probe, [conflict, clean, "x".into()])
    }

    #[test]
    fn merge_reports_a_conflict_with_its_files_and_a_clean_merge_as_a_commit() {
        let (_d, probe, [conflict, clean, _]) = fixture();
        assert!(matches!(probe.merge(&conflict), Ok(Merge::Conflict(f)) if f == ["f"]));
        assert!(matches!(probe.merge(&clean), Ok(Merge::Clean(c)) if c.len() == 40));
        assert!(probe.merge("nonesuch").is_err());
    }

    #[test]
    fn patch_id_is_the_same_for_a_rebased_copy_and_stacked_ids_are_read_off_the_log() {
        let (d, probe, [_, clean, _]) = fixture();
        let p = d.path();
        g(p, &["checkout", "-q", "-b", "copy", "main"]);
        let copy = commit(p, "g", "new\n", "different subject");
        assert_eq!(probe.patch_id(&clean).unwrap(), probe.patch_id(&copy).unwrap());
        assert_eq!(probe.stacked_on("sp-me", &clean).unwrap(), ["sp-bead"]);
        assert!(probe.stacked_on("sp-bead", &clean).unwrap().is_empty());
    }

    #[test]
    fn a_probe_with_lint_off_builds_nothing_and_reports_nothing() {
        let (d, mut probe, [_, clean, _]) = fixture();
        probe.lint = false;
        assert_eq!(probe.lint(&clean).unwrap(), Vec::<String>::new());
        assert!(!d.path().join(".sift").exists());
    }

    #[test]
    fn only_findings_absent_from_the_base_are_new() {
        let m = vec!["a".to_string(), "b".to_string()];
        assert_eq!(new_findings(&m, &["a".to_string()]), ["b"]);
    }
}
