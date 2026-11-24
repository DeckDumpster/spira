//! Proof of work on rework: the checks an ejection named, and the passing runs recorded for
//! them at an exact tip. A bead sent back red is resubmitted only when each named check has a
//! recorded pass at the tip being submitted.
//!
//! Layout under `$SPIRA_RUN/proof/<bead>/`: `failing` (one check per line, newest ejection
//! only), `failing-tests/<check>` (the registry ids of the tests that check failed, one per
//! line) and `pass/<tip>/<check>` (the tool's exit code and summary line).

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

/// Keeps the registry ids of the tests `check` failed in the newest ejection (replacing any
/// earlier record); no ids keeps nothing, so the check is judged whole.
pub fn record_failing_tests(run: &Path, bead: &str, check: &str, ids: &[String]) -> io::Result<()> {
    if !valid_check(check) || ids.iter().any(|i| i.is_empty() || i.contains('\n')) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad check or test id"));
    }
    let path = bead_dir(run, bead)?.join("failing-tests").join(check);
    if ids.is_empty() {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    write_atomic(&path, &(ids.join("\n") + "\n"))
}

pub fn failing_tests(run: &Path, bead: &str, check: &str) -> Vec<String> {
    let Ok(dir) = bead_dir(run, bead) else { return Vec::new() };
    if !valid_check(check) {
        return Vec::new();
    }
    fs::read_to_string(dir.join("failing-tests").join(check)).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(String::from).collect()
}

pub fn record_run(run: &Path, bead: &str, tip: &str, check: &str, exit: i32, summary: &str) -> io::Result<()> {
    if !valid_tip(tip) || !valid_check(check) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad tip or check name"));
    }
    let line: String = summary.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(300).collect();
    let dir = bead_dir(run, bead)?.join("pass").join(tip);
    write_atomic(&dir.join(format!("{check}.out")), summary)?;
    write_atomic(&dir.join(check), &format!("exit={exit}\nsummary={line}\n"))
}

/// Where `record_run` keeps the whole output of a run of `check` at `tip`.
pub fn run_output_path(run: &Path, bead: &str, tip: &str, check: &str) -> io::Result<PathBuf> {
    if !valid_tip(tip) || !valid_check(check) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad tip or check name"));
    }
    Ok(bead_dir(run, bead)?.join("pass").join(tip).join(format!("{check}.out")))
}

/// Keeps the deny-list snapshot a run of `check` at `tip` is judged by, beside its record.
pub fn record_deny_list(run: &Path, bead: &str, tip: &str, check: &str, text: &str) -> io::Result<PathBuf> {
    if !valid_tip(tip) || !valid_check(check) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad tip or check name"));
    }
    let path = bead_dir(run, bead)?.join("pass").join(tip).join(format!("{check}.deny-list"));
    write_atomic(&path, text)?;
    Ok(path)
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
    crate::round_step::is_step(check) || check == "testenv"
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
/// Drops every failing suite `landref` no longer contains, from the bead's failing checks and
/// from the gate's `ejected/<bead>` sidecar, recording one fact per suite through `record` first.
/// A landing ref that cannot be read drops nothing. Returns the suites dropped.
pub fn drop_deleted_suites(run: &Path, bead: &str, root: &str, landref: &str, record: &dyn Fn(&str) -> Result<(), String>) -> Result<Vec<String>, String> {
    let sidecar = run.join("ejected").join(bead);
    let sidecar_text = fs::read_to_string(&sidecar).unwrap_or_default();
    let named: Vec<String> = sidecar_text.lines().next().unwrap_or("").split(|c: char| c == ',' || c.is_whitespace()).filter(|w| !w.is_empty()).map(String::from).collect();
    if failing(run, bead).is_empty() && named.is_empty() {
        return Ok(Vec::new());
    }
    let Ok((commit, landed)) = landed_suites(root, landref) else { return Ok(Vec::new()) };
    let mut gone: Vec<String> = failing(run, bead).into_iter().chain(named.iter().cloned()).filter(|c| !is_step(c) && !landed.contains(c)).collect();
    let mut seen = Vec::new();
    gone.retain(|c| !seen.contains(c) && {
        seen.push(c.clone());
        true
    });
    for name in &gone {
        record(&format!("suite {name} deleted on {landref} at {commit}; requirement dropped")).map_err(|e| format!("the dropped suite {name} could not be recorded: {e}"))?;
    }
    if gone.is_empty() {
        return Ok(gone);
    }
    remove_checks(run, bead, &gone)?;
    Ok(gone)
}

/// Removes `gone` from the bead's failing checks, its recorded failing tests and the gate's
/// `ejected/<bead>` sidecar.
fn remove_checks(run: &Path, bead: &str, gone: &[String]) -> Result<(), String> {
    let sidecar = run.join("ejected").join(bead);
    let named: Vec<String> = fs::read_to_string(&sidecar).unwrap_or_default().lines().next().unwrap_or("").split(|c: char| c == ',' || c.is_whitespace()).filter(|w| !w.is_empty()).map(String::from).collect();
    let kept: Vec<String> = failing(run, bead).into_iter().filter(|c| !gone.contains(c)).collect();
    if kept.is_empty() {
        clear_failing(run, bead);
    } else {
        write_atomic(&bead_dir(run, bead).map_err(|e| e.to_string())?.join("failing"), &(kept.join("\n") + "\n")).map_err(|e| e.to_string())?;
    }
    for c in gone {
        if valid_check(c) {
            let _ = record_failing_tests(run, bead, c, &[]);
        }
    }
    let kept_sidecar: Vec<&String> = named.iter().filter(|c| !gone.contains(c)).collect();
    if kept_sidecar.is_empty() {
        let _ = fs::remove_file(&sidecar);
    } else {
        write_atomic(&sidecar, &kept_sidecar.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(",")).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Drops `checks` from the bead's requirement: its failing checks, their recorded tests and the
/// `ejected/<bead>` sidecar. The caller has already recorded why.
pub fn drop_disabled_checks(run: &Path, bead: &str, checks: &[String]) -> Result<(), String> {
    remove_checks(run, bead, checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T1: &str = "1111111111111111111111111111111111111111";
    const T2: &str = "2222222222222222222222222222222222222222";

    fn run() -> testkit::TempDir {
        testkit::TempDir::new("proof")
    }

    #[test_id(thselih)]
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

    #[test_id(tg3dpa4)]
    #[test]
    fn failing_tests_round_trip_per_check_and_an_empty_record_keeps_nothing() {
        let d = run();
        let r = d.path().to_path_buf();
        let ids = vec!["test:t0atest".to_string(), "test:t0bint0".to_string()];
        record_failing_tests(&r, "sp-a", "unit-tests", &ids).unwrap();
        assert_eq!(failing_tests(&r, "sp-a", "unit-tests"), ids);
        assert!(failing_tests(&r, "sp-a", "other").is_empty());
        assert!(record_failing_tests(&r, "sp-a", "../x", &ids).is_err());
        record_failing_tests(&r, "sp-a", "unit-tests", &[]).unwrap();
        assert!(failing_tests(&r, "sp-a", "unit-tests").is_empty());
    }

    #[test_id(tqksz7i)]
    #[test]
    fn dropping_a_disabled_check_leaves_the_other_checks_in_place() {
        let d = run();
        let r = d.path().to_path_buf();
        record_failing(&r, "sp-a", &["unit-tests".into(), "test-y.sh".into()]).unwrap();
        record_failing_tests(&r, "sp-a", "unit-tests", &["test:t0unitb".to_string()]).unwrap();
        record_failing_tests(&r, "sp-a", "test-y.sh", &["test:t0unitc".to_string()]).unwrap();
        fs::create_dir_all(r.join("ejected")).unwrap();
        fs::write(r.join("ejected/sp-a"), "unit-tests,test-y.sh").unwrap();
        drop_disabled_checks(&r, "sp-a", &["unit-tests".to_string()]).unwrap();
        assert_eq!(failing(&r, "sp-a"), ["test-y.sh"]);
        assert!(failing_tests(&r, "sp-a", "unit-tests").is_empty());
        assert_eq!(failing_tests(&r, "sp-a", "test-y.sh"), ["test:t0unitc"]);
        assert_eq!(fs::read_to_string(r.join("ejected/sp-a")).unwrap(), "test-y.sh");
        drop_disabled_checks(&r, "sp-a", &["test-y.sh".to_string()]).unwrap();
        assert!(failing(&r, "sp-a").is_empty());
        assert!(!r.join("ejected/sp-a").exists());
    }

    fn repo_without(dir: &Path) -> String {
        let root = dir.join("repo");
        fs::create_dir_all(root.join("spira")).unwrap();
        let g = |a: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&root).args(a).status().unwrap().success());
        g(&["init", "-q", "-b", "main"]);
        for n in ["test-gone.sh", "test-kept.sh"] {
            fs::write(root.join("spira").join(n), "").unwrap();
        }
        g(&["add", "."]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x"]);
        g(&["rm", "-q", "spira/test-gone.sh"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "y"]);
        root.to_str().unwrap().to_string()
    }

    #[test_id(tniumpn)]
    #[test]
    fn a_forced_suite_the_landing_ref_lacks_is_dropped_with_its_reason_and_steps_stay() {
        let d = run();
        let root = repo_without(d.path());
        let r = d.path().join("run");
        record_failing(&r, "sp-a", &["test-gone.sh".into(), "test-kept.sh".into(), "unit-tests".into(), "fence-x".into(), "bare-suites".into(), "release-verify".into()]).unwrap();
        fs::create_dir_all(r.join("ejected")).unwrap();
        fs::write(r.join("ejected/sp-a"), "test-gone.sh,test-kept.sh").unwrap();
        let facts = std::cell::RefCell::new(Vec::new());
        let record = |c: &str| {
            facts.borrow_mut().push(c.to_string());
            Ok(())
        };
        let gone = drop_deleted_suites(&r, "sp-a", &root, "main", &record).unwrap();
        assert_eq!(gone, ["test-gone.sh"]);
        assert_eq!(failing(&r, "sp-a"), ["test-kept.sh", "unit-tests", "fence-x", "bare-suites", "release-verify"]);
        assert_eq!(fs::read_to_string(r.join("ejected/sp-a")).unwrap(), "test-kept.sh");
        let commit = git_out(&root, &["rev-parse", "main"]).unwrap();
        assert_eq!(*facts.borrow(), [format!("suite test-gone.sh deleted on main at {}; requirement dropped", commit.trim())]);
        assert!(drop_deleted_suites(&r, "sp-a", &root, "main", &record).unwrap().is_empty(), "a second submit has nothing to drop");
    }

    #[test_id(tj1ajck)]
    #[test]
    fn a_requirement_only_a_deleted_suite_held_is_cleared_with_its_sidecar() {
        let d = run();
        let root = repo_without(d.path());
        let r = d.path().join("run");
        record_failing(&r, "sp-b", &["test-gone.sh".into()]).unwrap();
        fs::create_dir_all(r.join("ejected")).unwrap();
        fs::write(r.join("ejected/sp-b"), "test-gone.sh").unwrap();
        drop_deleted_suites(&r, "sp-b", &root, "main", &|_| Ok(())).unwrap();
        assert!(failing(&r, "sp-b").is_empty() && !r.join("ejected/sp-b").exists());
    }

    #[test_id(t046d4j)]
    #[test]
    fn a_recorder_that_fails_leaves_the_requirement_in_place() {
        let d = run();
        let root = repo_without(d.path());
        let r = d.path().join("run");
        record_failing(&r, "sp-c", &["test-gone.sh".into()]).unwrap();
        assert!(drop_deleted_suites(&r, "sp-c", &root, "main", &|_| Err("down".into())).is_err());
        assert_eq!(failing(&r, "sp-c"), ["test-gone.sh"]);
    }

    #[test_id(tqdz3sc)]
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

    #[test_id(te9sxvc)]
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
