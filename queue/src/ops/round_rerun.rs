//! A pass whose only red was a test the registry has since disabled as flipping re-runs just the
//! step that held it (law-a-round-lands-only-on-a-green-pass); every other pass is a full one.

use std::fs;
use std::path::{Path, PathBuf};

use super::round::{phase_reds, suite_statuses, unjudgeable, BLOCKING};

pub(super) const INTEGRATION_STEP: &str = "integration-tests";
const DENY_LIST: &str = "deny-list";
const INTEGRATION_FILES: [&str; 3] = ["integration.json", "integration-tests.out", "integration-progress"];

/// What the round's last pass left behind, read once.
pub(super) struct Prior<'a> {
    pub trees: &'a str,
    pub head_tree: Option<&'a str>,
    pub red: &'a str,
    pub results: &'a Path,
    pub passed_deny: &'a Path,
    pub wt: &'a Path,
}

/// The one step to run again, when the last pass was red on a single test now disabled in the
/// kept deny list, on the tree this pass will build; `None` is a full pass.
pub(super) fn step_to_rerun(p: &Prior) -> Option<String> {
    let last = p.trees.split_whitespace().last()?.split_once('=')?.1;
    if p.head_tree != Some(last) {
        return None;
    }
    let phases = phase_reds(p.results);
    let mut found = Vec::new();
    suite_statuses(p.results, &mut found, 0);
    let blocking: Vec<&(String, String)> = found.iter().filter(|(_, s)| BLOCKING.contains(&s.as_str())).collect();
    let added = added_rows(&fs::read_to_string(p.passed_deny).ok()?, &fs::read_to_string(p.results.join(DENY_LIST)).ok()?)?;
    let step = match (&phases[..], &blocking[..]) {
        ([], [(name, _)]) if name.ends_with(".sh") && p.red == name.as_str() => {
            added.iter().any(|(test, _)| test == &format!("suite:{name}")).then(|| name.clone())?
        }
        ([one], []) if one.stage == INTEGRATION_STEP && !one.before_suites && p.red == "unit-tests" => {
            let ids = failing_ids(&one.failed)?;
            ids.iter().all(|id| added.iter().any(|(test, _)| test == id)).then(|| INTEGRATION_STEP.to_string())?
        }
        _ => return None,
    };
    let rest: Vec<(String, String)> = found
        .into_iter()
        .map(|(n, s)| if n == step { (n, "ok".to_string()) } else { (n, s) })
        .collect();
    unjudgeable(p.wt, &rest).is_none().then_some(step)
}

/// The (test, bead) of each row `kept` adds to `passed`, every one a `disabled` row; `None` when
/// the kept list dropped a row of the passed one or added any other kind of row.
fn added_rows(passed: &str, kept: &str) -> Option<Vec<(String, String)>> {
    let rows = |text: &str| -> Vec<String> { text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect() };
    let (passed, kept) = (rows(passed), rows(kept));
    let mut left = kept.clone();
    for row in &passed {
        let at = left.iter().position(|k| k == row)?;
        left.remove(at);
    }
    left.iter()
        .map(|row| {
            let mut f = row.split('\t');
            (f.next()? == "disabled").then_some(())?;
            Some((f.next()?.to_string(), f.next().unwrap_or_default().to_string()))
        })
        .collect()
}

/// `test:<id>` of each `FAIL <binary> <test>` line, `None` when any line names no registry id.
fn failing_ids(failed: &[String]) -> Option<Vec<String>> {
    let ids: Vec<String> = failed
        .iter()
        .map(|line| {
            let mut w = line.split_whitespace();
            let (_status, _binary, test, None) = (w.next()?, w.next()?, w.next()?, w.next()) else { return None };
            let id = test.rsplit("::").next()?;
            let shaped = id.len() == 7 && id.starts_with('t') && id[1..].bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase());
            shaped.then(|| format!("test:{id}"))
        })
        .collect::<Option<_>>()?;
    (!ids.is_empty()).then_some(ids)
}

/// Lays the re-run's `scratch` results over the pass's `kept` ones: the step's earlier files go,
/// its new ones come, the deny list and every other step stay.
pub(super) fn replace_step_results(kept: &Path, scratch: &Path, step: &str) {
    drop_step_files(kept, step, 0);
    copy_over(scratch, kept);
}

fn drop_step_files(dir: &Path, step: &str, depth: u32) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let (path, name) = (e.path(), e.file_name().to_string_lossy().into_owned());
        if path.is_dir() {
            if depth < 2 {
                drop_step_files(&path, step, depth + 1);
            }
        } else if (step == INTEGRATION_STEP && INTEGRATION_FILES.contains(&name.as_str())) || name.starts_with(&format!("{step}.")) {
            let _ = fs::remove_file(&path);
        }
    }
}

fn copy_over(from: &Path, to: &Path) {
    let _ = fs::create_dir_all(to);
    for e in fs::read_dir(from).into_iter().flatten().flatten() {
        let (src, dst): (PathBuf, PathBuf) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_over(&src, &dst);
        } else if e.file_name() != DENY_LIST {
            let _ = fs::copy(&src, &dst);
        }
    }
}
