//! The submit fence against a dwarf's session scratch (`.runtime/session-tmp-*`) entering history.

use spira_config::{RUNTIME_DIR, SESSION_TMP_PREFIX};

const NAMED: usize = 5;

/// The added paths lying under a session scratch directory at the worktree root.
pub fn added_scratch(added: &[String]) -> Vec<String> {
    added
        .iter()
        .filter(|p| {
            let mut parts = p.split('/');
            parts.next() == Some(RUNTIME_DIR) && parts.next().is_some_and(|d| d.starts_with(SESSION_TMP_PREFIX))
        })
        .cloned()
        .collect()
}

/// Judges the branch at `root` against `land`: only added paths count, so a branch that deletes scratch passes.
pub fn gate(root: &std::path::Path, land: &str) -> Result<(), String> {
    let out = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-only", "--no-renames", "--diff-filter=A", &format!("{land}...HEAD")])
        .output()
        .ok()
        .filter(|o| o.status.success());
    refusal(out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()))
}

/// `added`: the paths the branch adds against its landing ref, `None` when they cannot be read.
pub fn refusal(added: Option<String>) -> Result<(), String> {
    let Some(added) = added else { return Err("cannot read the branch's diff against its landing ref to look for session scratch".into()) };
    let lines: Vec<String> = added.lines().map(String::from).collect();
    let bad = added_scratch(&lines);
    if bad.is_empty() {
        return Ok(());
    }
    let shown = bad.iter().take(NAMED).cloned().collect::<Vec<_>>().join(", ");
    let more = if bad.len() > NAMED { ", ..." } else { "" };
    Err(format!("the branch adds {} path(s) under {RUNTIME_DIR}/{SESSION_TMP_PREFIX}*, a session's scratch: {shown}{more}", bad.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(t102pg2)]
    #[test]
    fn session_scratch_paths_are_returned() {
        let got = added_scratch(&v(&[".runtime/session-tmp-225736/.tmpX/a.rm", ".runtime/session-tmp-1/claude-1000/t.output"]));
        assert_eq!(got.len(), 2);
    }

    #[test_id(tnlsio5)]
    #[test]
    fn paths_that_only_resemble_scratch_are_not() {
        assert!(added_scratch(&v(&["src/.runtime/session-tmp-1/x", ".runtime/spira/state", "fixtures/a.rm"])).is_empty());
    }

    #[test_id(tor30wh)]
    #[test]
    fn an_unreadable_diff_refuses() {
        assert!(refusal(None).is_err());
        assert!(refusal(Some(String::new())).is_ok());
    }

    #[test_id(t9jys8l)]
    #[test]
    fn the_refusal_names_the_count_and_the_first_paths() {
        let all: String = (0..7).map(|i| format!(".runtime/session-tmp-9/f{i}\n")).collect();
        let e = refusal(Some(all)).unwrap_err();
        assert!(e.contains("7 path(s)") && e.contains("f0") && e.contains("f4") && !e.contains("f5"));
    }
}
