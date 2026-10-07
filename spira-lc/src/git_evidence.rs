//! Content-on-base evidence for the migration classifier: an ancestry check, then a
//! merge-tree fallback — the algorithm lib.sh's bash content check (now retired) used,
//! ported here because the migration fixture requirement recorded on this bead
//! prohibits reaching for a commit-subject match instead. Three production shapes are
//! misclassified by exactly that (see `lifecycle::classify`'s own fixtures for the general
//! form): a subject that never matched `"spira: land <id>"` or `"<id>:..."`, on work that
//! was demonstrably on base by ancestry or by merge-tree content.

use std::collections::HashMap;
use std::path::Path;

fn git_output(repo: &Path, args: &[&str]) -> Option<String> {
    let out = spira_config::bounded::bounded("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// `git merge-base --is-ancestor <candidate> <base>` — the fact the migration classifier's
/// rule 1 (a legacy LANDED record whose tip is an ancestor of base) needs, and the fast path in [`content_on_base`] below.
pub fn is_ancestor(repo: &Path, candidate: &str, base: &str) -> bool {
    spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", candidate, base])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Ports lib.sh's old bash content check's two-step proof: ancestor first (cheap, and correct whenever
/// the exact commit is still reachable), then `merge-tree --write-tree` compared against
/// base's own tree — the check that survives a squash merge or a rebase that rewrote every
/// hash, because it asks "would merging this change anything?" instead of "is this exact
/// commit reachable?" Never inspects a commit subject or message.
pub fn content_on_base(repo: &Path, branch_or_tip: &str, base: &str) -> bool {
    if is_ancestor(repo, branch_or_tip, base) {
        return true;
    }
    let ahead: u64 = git_output(repo, &["rev-list", "--count", &format!("{base}..{branch_or_tip}")])
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if ahead == 0 {
        return false;
    }
    let Some(merged) = git_output(repo, &["merge-tree", "--write-tree", base, branch_or_tip]) else {
        return false;
    };
    let merged_tree = merged.lines().next().unwrap_or("").trim();
    if merged_tree.is_empty() {
        return false;
    }
    let Some(base_tree) = git_output(repo, &["rev-parse", &format!("{base}^{{tree}}")]) else {
        return false;
    };
    merged_tree == base_tree.trim()
}

/// Every bead a commit on `base` lands by a `spira: land <id>` line in its message, mapped to
/// the first such commit (the one nearest base's tip). One log walk for the whole repository,
/// not one per bead. A batch landing names its members this way after their branches are gone.
pub fn landing_lines(repo: &Path, base: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(log) = git_output(repo, &["log", base, "--format=%x01%H%n%B"]) else { return out };
    for chunk in log.split('\u{1}').skip(1) {
        let mut lines = chunk.lines();
        let Some(sha) = lines.next() else { continue };
        for line in lines {
            if let Some(rest) = line.strip_prefix("spira: land ") {
                for id in rest.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()) {
                    out.entry(id.to_string()).or_insert_with(|| sha.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::fs;

    struct ScratchRepo {
        dir: testkit::TempDir,
    }

    impl ScratchRepo {
        fn new(tag: &str) -> Self {
            let dir = testkit::TempDir::new(&format!("spira-lc-git-evidence-test-{tag}"));
            run(&dir, &["init", "-q", "-b", "main"]);
            run(&dir, &["config", "user.email", "test@example.invalid"]);
            run(&dir, &["config", "user.name", "test"]);
            ScratchRepo { dir }
        }

        fn commit(&self, file: &str, content: &str, message: &str) -> String {
            fs::write(self.dir.join(file), content).unwrap();
            run(&self.dir, &["add", file]);
            run(&self.dir, &["commit", "-q", "-m", message]);
            git_output(&self.dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
        }
    }

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(dir).args(args).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    #[test]
    fn an_ancestor_commit_is_content_on_base_with_no_subject_involved() {
        // Generalizes the two production shapes whose subject never matched
        // "spira: land <id>" or "<id>:..." but whose commit IS an ancestor of base.
        let repo = ScratchRepo::new("ancestor");
        repo.commit("f.txt", "base\n", "unrelated subject, not a landing marker");
        repo.commit("g.txt", "more\n", "fix(sp-xyz): unrelated conventional-commit subject");
        let tip = git_output(&repo.dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();

        // Model "this commit reached base" by fast-forwarding base up to it — the tip is
        // now a plain ancestor, with a subject that a subject-matching check would reject.
        run(&repo.dir, &["branch", "-f", "based-elsewhere", &tip]);
        assert!(content_on_base(&repo.dir, &tip, "based-elsewhere"), "an ancestor of base must prove content-on-base");
    }

    #[test]
    fn a_rewritten_hash_still_proves_content_on_base_via_merge_tree() {
        // Generalizes the third production shape: the branch's own commit is NOT an
        // ancestor of base (its hash was rewritten by a later rebase), but merging the
        // branch into base changes nothing, because the same content already landed under
        // a different commit. No subject is consulted anywhere in this check.
        let repo = ScratchRepo::new("rewritten");
        repo.commit("shared.txt", "line one\n", "initial");
        let branch_tip = repo.commit("fix.txt", "the fix\n", "a fix with no landing-marker subject at all");

        // Simulate the rewrite: base gets the SAME content under a brand new commit whose
        // hash differs from branch_tip (as a squash or a rebase would produce), while the
        // branch keeps pointing at its own, now-unreachable, original commit.
        run(&repo.dir, &["checkout", "-q", "-b", "rewritten-base", "HEAD~1"]);
        fs::write(repo.dir.join("fix.txt"), "the fix\n").unwrap();
        run(&repo.dir, &["add", "fix.txt"]);
        run(&repo.dir, &["commit", "-q", "-m", "the same fix, landed under a different hash"]);

        assert!(!is_ancestor(&repo.dir, &branch_tip, "rewritten-base"), "the original commit must NOT be an ancestor — its hash was rewritten");
        assert!(content_on_base(&repo.dir, &branch_tip, "rewritten-base"), "merge-tree must still prove the content is already there");
    }

    #[test]
    fn work_genuinely_outstanding_is_not_content_on_base() {
        let repo = ScratchRepo::new("outstanding");
        repo.commit("f.txt", "base\n", "initial");
        let base = git_output(&repo.dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        run(&repo.dir, &["checkout", "-q", "-b", "work"]);
        let tip = repo.commit("f.txt", "base\nplus new work\n", "real outstanding work");
        assert!(!content_on_base(&repo.dir, &tip, &base), "genuinely outstanding work must not be reported as landed");
    }

    #[test]
    fn a_landing_line_in_a_batch_body_maps_each_named_bead_to_that_commit() {
        let repo = ScratchRepo::new("landing-lines");
        repo.commit("a.txt", "a\n", "unrelated: sp-zzz mentioned in a subject only");
        let batch = repo.commit("b.txt", "b\n", "round-x: merge\n\nspira: land sp-one\nspira: land sp-two\nnot a spira: land sp-three line");
        let lines = landing_lines(&repo.dir, "main");
        assert_eq!(lines.get("sp-one"), Some(&batch));
        assert_eq!(lines.get("sp-two"), Some(&batch));
        assert_eq!(lines.get("sp-three"), None);
        assert_eq!(lines.get("sp-zzz"), None);
        assert!(landing_lines(&repo.dir, "no-such-ref").is_empty());
    }
}
