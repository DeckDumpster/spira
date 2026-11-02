//! Content-on-base evidence for the migration classifier: an ancestry check, then a
//! merge-tree fallback — the algorithm lib.sh's bash content check (now retired) used,
//! ported here because the migration fixture requirement recorded on this bead
//! prohibits reaching for a commit-subject match instead. Three production shapes are
//! misclassified by exactly that (see `lifecycle::classify`'s own fixtures for the general
//! form): a subject that never matched `"spira: land <id>"` or `"<id>:..."`, on work that
//! was demonstrably on base by ancestry or by merge-tree content.

use std::collections::HashMap;
use std::path::Path;

pub(crate) fn git_output(repo: &Path, args: &[&str]) -> Option<String> {
    let out = spira_config::bounded::bounded("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Whether `sha`'s commit message names `bead_id` as a whole id, not as the prefix of a longer one
/// (`sp-x` inside `sp-x2` or the child `sp-x.1`).
pub fn commit_names_bead(repo: &Path, sha: &str, bead_id: &str) -> bool {
    let Some(message) = git_output(repo, &["log", "-1", "--format=%B", sha]) else { return false };
    let mut from = 0;
    while let Some(at) = message[from..].find(bead_id) {
        let start = from + at;
        let rest = &message[start + bead_id.len()..];
        let before_ok = message[..start].chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-'));
        let mut after = rest.chars();
        let after_ok = match after.next() {
            None => true,
            Some('.') => !after.next().is_some_and(|c| c.is_ascii_alphanumeric()),
            Some(c) => !(c.is_ascii_alphanumeric() || c == '-'),
        };
        if before_ok && after_ok {
            return true;
        }
        from = start + bead_id.len();
    }
    false
}

/// `git merge-base --is-ancestor <candidate> <base>` — the fast path in [`content_on_base`] below.
pub fn is_ancestor(repo: &Path, candidate: &str, base: &str) -> bool {
    spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", candidate, base])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn ref_resolves(repo: &Path, name: &str) -> bool {
    git_output(repo, &["rev-parse", "--verify", "--quiet", &format!("{name}^{{commit}}")]).is_some()
}

/// The commits reachable from `tip` and not from `base`; `None` when git cannot resolve either.
pub fn commits_above(repo: &Path, base: &str, tip: &str) -> Option<std::collections::BTreeSet<String>> {
    let out = git_output(repo, &["rev-list", tip, &format!("^{base}")])?;
    Some(out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
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
    let Some(merged) = git_output(repo, &["-c", &format!("attr.tree={base}"), "merge-tree", "--write-tree", base, branch_or_tip]) else {
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

/// [`content_on_base`] that also asks whether the change is still in `base`'s tree: a branch
/// that reached `base` and was later reverted is an ancestor, or merges clean, yet its change is
/// gone. The branch's own patch must reverse-apply to `base`'s tree.
pub fn change_in_force(repo: &Path, branch: &str, base: &str) -> bool {
    if !content_on_base(repo, branch, base) {
        return false;
    }
    let fork = if is_ancestor(repo, branch, base) { fork_of_merged(repo, branch, base) } else { git_output(repo, &["merge-base", branch, base]).map(|s| s.trim().to_string()) };
    let Some(fork) = fork.filter(|f| !f.is_empty()) else { return false };
    let Some(patch) = git_output(repo, &["diff", "--binary", &fork, branch]) else { return false };
    if patch.trim().is_empty() {
        return true;
    }
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let index = std::env::temp_dir().join(format!("spira-lc-in-force-{}-{seq}", std::process::id()));
    let with_index = |args: &[&str]| {
        let mut c = spira_config::bounded::bounded("git");
        c.arg("-C").arg(repo).env("GIT_INDEX_FILE", &index).args(args);
        c
    };
    let read = with_index(&["read-tree", base]).status().map(|s| s.success()).unwrap_or(false);
    let ok = read
        && with_index(&["apply", "-R", "--check", "--cached"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .and_then(|mut ch| {
                use std::io::Write;
                if let Some(mut si) = ch.stdin.take() {
                    let _ = si.write_all(patch.as_bytes());
                }
                ch.wait()
            })
            .map(|s| s.success())
            .unwrap_or(false);
    let _ = std::fs::remove_file(&index);
    ok
}

/// Where an already-merged `branch` forked: the merge-base of the merge commit that took it and
/// that merge's first parent; a fast-forward or rebase merge has no such commit, so its tip's parent.
fn fork_of_merged(repo: &Path, branch: &str, base: &str) -> Option<String> {
    let merges = git_output(repo, &["rev-list", "--merges", "--reverse", "--ancestry-path", &format!("{branch}..{base}")])?;
    for m in merges.lines() {
        let parents = git_output(repo, &["rev-list", "--parents", "-n1", m])?;
        let p: Vec<&str> = parents.split_whitespace().collect();
        if p.len() >= 3 && is_ancestor(repo, branch, p[2]) {
            return git_output(repo, &["merge-base", p[1], branch]).map(|s| s.trim().to_string());
        }
    }
    git_output(repo, &["rev-parse", &format!("{branch}^")]).map(|s| s.trim().to_string())
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
