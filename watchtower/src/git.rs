//! The handful of read-only git calls the checks need. Each mirrors one bash line exactly
//! (`git [-C repo] rev-parse --verify --quiet <ref>`, `merge-base --is-ancestor`, `log
//! --oneline`) — no repo library, because the bash never used one either.


fn git(repo: Option<&str>, args: &[&str]) -> std::process::Output {
    let mut cmd = spira_config::bounded::bounded("git");
    if let Some(r) = repo {
        cmd.arg("-C").arg(r);
    }
    cmd.args(args);
    cmd.output().unwrap_or_else(|_| std::process::Output {
        status: std::process::ExitStatus::default(),
        stdout: Vec::new(),
        stderr: Vec::new(),
    })
}

/// `git [-C repo] rev-parse --verify --quiet <ref>` — exit 0 means the ref exists.
pub fn ref_exists(repo: Option<&str>, refname: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", refname])
        .status
        .success()
}

/// `git [-C repo] merge-base --is-ancestor <tip> <onto>` — exit 0 means `tip` is already an
/// ancestor of (already merged into) `onto`.
pub fn is_ancestor(repo: Option<&str>, tip: &str, onto: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", tip, onto])
        .status
        .success()
}

/// Count of `git [-C repo] log --oneline <rev>` lines matching any of `needles` — the bash's
/// `git log --oneline "$ref" | grep -E '(a|b)' | wc -l`. A failed `log` (bad ref, no repo)
/// yields 0, same as the bash's `|| _tc_async_on_main=0`.
pub fn oneline_log_matches(repo: Option<&str>, rev: &str, needles: &[&str]) -> u32 {
    let out = git(repo, &["log", "--oneline", rev]);
    if !out.status.success() {
        return 0;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter(|l| needles.iter().any(|n| l.contains(n)))
        .count() as u32
}

/// `spira_landref "$repo"` — the base ref `repo` lands on. In-process via
/// [`crate::seams::registry`] (sp-k6lku, "wave 4.13") and `spira_config::repos::landref`;
/// was `. "$SPIRA_HOME/lib.sh"; spira_landref "$repo"`.
pub fn spira_landref(spira_home: &str, repo: &str) -> Option<String> {
    let reg = crate::seams::registry(spira_home);
    spira_config::repos::landref(&reg, repo)
}
