//! Proof of work on rework: the checks an ejection named, and the passing runs recorded for
//! them at an exact tip. A bead sent back red is resubmitted only when each named check has a
//! recorded pass at the tip being submitted.
//!
//! Layout under `$SPIRA_RUN/proof/<bead>/`: `failing` (one check per line, newest ejection
//! only) and `pass/<tip>/<check>` (the tool's exit code and summary line).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub fn valid_check(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_tip(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn bead_dir(run: &Path, bead: &str) -> io::Result<PathBuf> {
    if !valid_id(bead) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a bead id: {bead:?}")));
    }
    Ok(run.join("proof").join(bead))
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.{}", path.file_name().and_then(|n| n.to_str()).unwrap_or("x"), std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// Replaces the bead's failing checks with `checks` (invalid names are an error, not dropped).
pub fn record_failing(run: &Path, bead: &str, checks: &[String]) -> io::Result<()> {
    if let Some(bad) = checks.iter().find(|c| !valid_check(c)) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a check name: {bad:?}")));
    }
    let mut seen: Vec<&str> = Vec::new();
    for c in checks {
        if !seen.contains(&c.as_str()) {
            seen.push(c);
        }
    }
    if seen.is_empty() {
        return Ok(());
    }
    write_atomic(&bead_dir(run, bead)?.join("failing"), &(seen.join("\n") + "\n"))
}

pub fn failing(run: &Path, bead: &str) -> Vec<String> {
    let Ok(dir) = bead_dir(run, bead) else { return Vec::new() };
    fs::read_to_string(dir.join("failing")).unwrap_or_default().lines().map(str::trim).filter(|l| valid_check(l)).map(String::from).collect()
}

/// The ejection's record is consumed when the bead is submitted with every check proven.
pub fn clear_failing(run: &Path, bead: &str) {
    if let Ok(dir) = bead_dir(run, bead) {
        let _ = fs::remove_file(dir.join("failing"));
    }
}

pub fn record_run(run: &Path, bead: &str, tip: &str, check: &str, exit: i32, summary: &str) -> io::Result<()> {
    if !valid_tip(tip) || !valid_check(check) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad tip or check name"));
    }
    let line: String = summary.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(300).collect();
    write_atomic(&bead_dir(run, bead)?.join("pass").join(tip).join(check), &format!("exit={exit}\nsummary={line}\n"))
}

/// Proven: a run recorded at exactly this tip, exit 0.
pub fn proven(run: &Path, bead: &str, tip: &str, check: &str) -> bool {
    if !valid_tip(tip) || !valid_check(check) {
        return false;
    }
    let Ok(dir) = bead_dir(run, bead) else { return false };
    fs::read_to_string(dir.join("pass").join(tip).join(check)).is_ok_and(|t| t.lines().next() == Some("exit=0"))
}

/// A run of this check at exactly this tip is on record, whatever its exit.
pub fn recorded(run: &Path, bead: &str, tip: &str, check: &str) -> bool {
    if !valid_tip(tip) || !valid_check(check) {
        return false;
    }
    bead_dir(run, bead).is_ok_and(|dir| dir.join("pass").join(tip).join(check).is_file())
}

pub fn unproven(run: &Path, bead: &str, tip: &str) -> Vec<String> {
    failing(run, bead).into_iter().filter(|c| !proven(run, bead, tip, c)).collect()
}

/// Checks that are not suites of the landing ref's corpus: the round's own steps and fences.
fn is_step(check: &str) -> bool {
    check.starts_with("fence-") || matches!(check, "spira-lint" | "workspace-build" | "unit-tests" | "config-delta" | "testenv")
}

fn git_out(root: &str, args: &[&str]) -> Result<String, String> {
    // batch-job: one git read of the landing ref
    let out = std::process::Command::new("git").arg("-C").arg(root).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The suites `landref` contains in `root`, with the commit read: a repository's declared
/// tiers, else the `spira/test-*.sh` files, as testenv resolves them for a tree.
pub fn landed_suites(root: &str, landref: &str) -> Result<(String, Vec<String>), String> {
    let commit = git_out(root, &["rev-parse", "--verify", &format!("{landref}^{{commit}}")])?.trim().to_string();
    let has = |path: &str| git_out(root, &["cat-file", "-e", &format!("{commit}:{path}")]).is_ok();
    let decl = ".spira/testenv.toml";
    if has(decl) && !has("spira/testenv/Containerfile") {
        let text = git_out(root, &["show", &format!("{commit}:{decl}")])?;
        let doc: toml::Value = crate::parse_foreign(&text).map_err(|e| format!("{decl}: {e}"))?;
        let names = doc.get("tier").and_then(|t| t.as_table()).map(|t| t.keys().cloned().collect()).unwrap_or_default();
        return Ok((commit, names));
    }
    let listing = git_out(root, &["ls-tree", "--name-only", &commit, "spira/"])?;
    let names = listing.lines().filter_map(|l| l.strip_prefix("spira/")).filter(|n| n.starts_with("test-") && n.ends_with(".sh")).map(String::from).collect();
    Ok((commit, names))
}

/// Removes from the bead's failing checks every suite not in `landed` and returns them: a suite
/// the landing ref no longer has cannot be run, so it cannot be required.
pub fn would_drop(check: &str) -> bool {
    !is_step(check)
}

pub fn drop_deleted(run: &Path, bead: &str, landed: &[String]) -> io::Result<Vec<String>> {
    let all = failing(run, bead);
    let (gone, kept): (Vec<String>, Vec<String>) = all.into_iter().partition(|c| !is_step(c) && !landed.contains(c));
    if gone.is_empty() {
        return Ok(gone);
    }
    if kept.is_empty() {
        clear_failing(run, bead);
    } else {
        write_atomic(&bead_dir(run, bead)?.join("failing"), &(kept.join("\n") + "\n"))?;
    }
    Ok(gone)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T1: &str = "1111111111111111111111111111111111111111";
    const T2: &str = "2222222222222222222222222222222222222222";

    fn run() -> testkit::TempDir {
        testkit::TempDir::new("proof")
    }

    #[test]
    fn a_pass_proves_only_its_own_check_at_its_own_tip() {
        let d = run();
        let r = d.path().to_path_buf();
        record_failing(&r, "sp-a", &["test-x.sh".into(), "test-y.sh".into(), "test-x.sh".into()]).unwrap();
        assert_eq!(failing(&r, "sp-a"), ["test-x.sh", "test-y.sh"]);
        assert_eq!(unproven(&r, "sp-a", T1), ["test-x.sh", "test-y.sh"]);
        record_run(&r, "sp-a", T1, "test-x.sh", 0, "ok\n12 passed\n").unwrap();
        assert_eq!(unproven(&r, "sp-a", T1), ["test-y.sh"]);
        assert_eq!(unproven(&r, "sp-a", T2), ["test-x.sh", "test-y.sh"], "a pass at another tip proves nothing");
        record_run(&r, "sp-a", T1, "test-y.sh", 1, "1 failed").unwrap();
        assert_eq!(unproven(&r, "sp-a", T1), ["test-y.sh"], "a red run is a record, not a proof");
        assert!(fs::read_to_string(r.join("proof/sp-a/pass").join(T1).join("test-x.sh")).unwrap().contains("summary=12 passed"));
        clear_failing(&r, "sp-a");
        assert!(unproven(&r, "sp-a", T1).is_empty());
    }

    #[test]
    fn a_suite_the_landing_ref_lacks_is_dropped_from_the_requirement_and_steps_stay() {
        let d = run();
        let r = d.path().to_path_buf();
        record_failing(&r, "sp-a", &["test-gone.sh".into(), "test-kept.sh".into(), "unit-tests".into(), "fence-x".into()]).unwrap();
        let gone = drop_deleted(&r, "sp-a", &["test-kept.sh".into()]).unwrap();
        assert_eq!(gone, ["test-gone.sh"]);
        assert_eq!(failing(&r, "sp-a"), ["test-kept.sh", "unit-tests", "fence-x"]);
        record_failing(&r, "sp-b", &["test-gone.sh".into()]).unwrap();
        assert_eq!(drop_deleted(&r, "sp-b", &[]).unwrap(), ["test-gone.sh"]);
        assert!(failing(&r, "sp-b").is_empty(), "nothing left to require");
    }

    #[test]
    fn landed_suites_reads_the_ref_not_the_checkout() {
        let d = run();
        let root = d.path().to_str().unwrap().to_string();
        let g = |a: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&root).args(a).status().unwrap().success());
        g(&["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(d.path().join("spira")).unwrap();
        std::fs::write(d.path().join("spira/test-a.sh"), "").unwrap();
        std::fs::write(d.path().join("spira/test-b.sh"), "").unwrap();
        g(&["add", "."]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x"]);
        g(&["rm", "-q", "spira/test-b.sh"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "y"]);
        std::fs::write(d.path().join("spira/test-c.sh"), "").unwrap();
        let (commit, names) = landed_suites(&root, "main").unwrap();
        assert_eq!(names, ["test-a.sh"]);
        assert_eq!(commit.len(), 40);
    }

    #[test]
    fn names_that_climb_out_of_the_store_are_refused() {
        let d = run();
        let r = d.path().to_path_buf();
        assert!(record_failing(&r, "sp-a", &["../x".into()]).is_err());
        assert!(record_failing(&r, "../sp-a", &["x".into()]).is_err());
        assert!(record_run(&r, "sp-a", "not-a-tip", "x", 0, "").is_err());
        assert!(!proven(&r, "sp-a", T1, "../x"));
    }
}
