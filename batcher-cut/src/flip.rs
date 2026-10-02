//! A flip's deletion (law-a-test-that-flips-is-deleted): the suite file, its use cases'
//! `[use_case.uncovered]` marks citing the follow-up bead, and its tier-budget allowlist row,
//! committed into the round. Only text edits live here; git and the bead store are `io`'s.

use std::fs;
use std::path::Path;

pub const ALLOWLIST: &str = "spira/tier-budget-allowlist";
pub const CATALOGUES: &str = "docs/test-plan";

/// `suite`'s row removed from allowlist `text`; `None` when it had none.
pub fn without_allowlist_row(text: &str, suite: &str) -> Option<String> {
    let row = |l: &str| l.split('\t').next() == Some(suite);
    if !text.lines().any(row) {
        return None;
    }
    let mut out: String = text.lines().filter(|l| !row(l)).collect::<Vec<_>>().join("\n");
    out.push('\n');
    Some(out)
}

/// Catalogue `text` with use case `uc` marked uncovered, citing `bead`; `None` when `uc` is
/// not declared here or is already marked.
pub fn mark_uncovered(text: &str, uc: &str, bead: &str, suite: &str, date: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|l| l.trim() == format!("id = \"{uc}\""))?;
    let end = lines[at..].iter().position(|l| l.trim().is_empty() || l.starts_with('[')).map_or(lines.len(), |i| at + i);
    if lines[end..].iter().find(|l| !l.trim().is_empty()).is_some_and(|l| l.starts_with("[use_case.uncovered]")) {
        return None;
    }
    let reason = format!("{suite} flipped (red once, green on every alone re-run) and the batcher deleted it from the round");
    let mark = [
        "[use_case.uncovered]".to_string(),
        format!("reason = \"{reason}\""),
        format!("date = \"{date}\""),
        format!("bead = \"{bead}\""),
    ];
    let mut out: Vec<String> = lines[..end].iter().map(|s| s.to_string()).collect();
    out.push(String::new());
    out.extend(mark);
    out.extend(lines[end..].iter().map(|s| s.to_string()));
    let mut s = out.join("\n");
    s.push('\n');
    Some(s)
}

/// The `UC-` ids a suite's `# covers:` line names.
pub fn use_cases_of(suite_text: &str) -> Vec<String> {
    suite_select::header::covers_of(suite_text).unwrap_or_default().into_iter().filter(|t| t.starts_with("UC-")).collect()
}

/// Delete each `(suite, bead)` from the tree at `wt`. Returns the paths changed or removed,
/// repo-relative, for `git add -A`'s companion check; a suite that is not there is an error,
/// so a stale name never reads as deleted.
pub fn apply(wt: &Path, flips: &[(String, String)], date: &str) -> Result<Vec<String>, String> {
    let mut touched = vec![];
    let mut ucs: Vec<(String, String, String)> = vec![];
    for (suite, bead) in flips {
        let rel = format!("spira/{suite}");
        let text = fs::read_to_string(wt.join(&rel)).map_err(|e| format!("{rel}: {e}"))?;
        for uc in use_cases_of(&text) {
            ucs.push((uc, bead.clone(), suite.clone()));
        }
        fs::remove_file(wt.join(&rel)).map_err(|e| format!("{rel}: {e}"))?;
        touched.push(rel);
        let al = wt.join(ALLOWLIST);
        if let Ok(t) = fs::read_to_string(&al) {
            if let Some(n) = without_allowlist_row(&t, suite) {
                fs::write(&al, n).map_err(|e| format!("{ALLOWLIST}: {e}"))?;
                touched.push(ALLOWLIST.to_string());
            }
        }
    }
    let still_covered: Vec<String> = fs::read_dir(wt.join("spira"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("test-"))
                .filter_map(|e| fs::read_to_string(e.path()).ok())
                .flat_map(|t| use_cases_of(&t))
                .collect()
        })
        .unwrap_or_default();
    let dir = wt.join(CATALOGUES);
    let tomls: Vec<_> = fs::read_dir(&dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect()).unwrap_or_default();
    for (uc, bead, suite) in ucs.iter().filter(|(uc, _, _)| !still_covered.contains(uc)) {
        for p in &tomls {
            let Ok(t) = fs::read_to_string(p) else { continue };
            if let Some(n) = mark_uncovered(&t, uc, bead, suite, date) {
                fs::write(p, n).map_err(|e| format!("{}: {e}", p.display()))?;
                touched.push(format!("{CATALOGUES}/{}", p.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
    }
    touched.sort();
    touched.dedup();
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAT: &str = "area = \"x\"\n\n[[use_case]]\nid = \"UC-x-01\"\ntier = \"T1\"\nstatement = \"s\"\n\n[[use_case]]\nid = \"UC-x-02\"\ntier = \"T1\"\nstatement = \"t\"\n";

    #[test]
    fn marks_only_the_named_use_case_and_cites_the_bead() {
        let n = mark_uncovered(CAT, "UC-x-01", "sp-b1", "test-a.sh", "2026-10-02").unwrap();
        let (first, second) = n.split_once("id = \"UC-x-02\"").unwrap();
        assert!(first.contains("[use_case.uncovered]") && first.contains("bead = \"sp-b1\""), "{n}");
        assert!(!second.contains("uncovered"), "{n}");
        assert!(mark_uncovered(&n, "UC-x-01", "sp-b2", "t", "d").is_none(), "already marked");
        assert!(mark_uncovered(CAT, "UC-x-09", "sp-b1", "t", "d").is_none(), "not declared here");
    }

    #[test]
    fn marks_the_last_use_case_in_a_file() {
        let n = mark_uncovered(CAT, "UC-x-02", "sp-b1", "test-a.sh", "2026-10-02").unwrap();
        assert!(n.trim_end().ends_with("bead = \"sp-b1\""), "{n}");
    }

    #[test]
    fn drops_exactly_the_suites_allowlist_row() {
        let t = "# c\ntest-a.sh\tT1\t3.0\ntest-ab.sh\tT1\t2.0\n";
        assert_eq!(without_allowlist_row(t, "test-a.sh").unwrap(), "# c\ntest-ab.sh\tT1\t2.0\n");
        assert!(without_allowlist_row(t, "test-z.sh").is_none());
    }

    #[test]
    fn apply_deletes_the_suite_marks_its_use_cases_and_keeps_covered_ones() {
        let d = testkit::TempDir::new("flip-apply");
        fs::create_dir_all(d.join("spira")).unwrap();
        fs::create_dir_all(d.join(CATALOGUES)).unwrap();
        fs::write(d.join("spira/test-flip.sh"), "#!/bin/bash\n# covers: spira/x.sh UC-x-01 UC-x-02\nset -uo pipefail\n").unwrap();
        fs::write(d.join("spira/test-keep.sh"), "#!/bin/bash\n# covers: UC-x-02\nset -uo pipefail\n").unwrap();
        fs::write(d.join(ALLOWLIST), "test-flip.sh\tT1\t3.0\ntest-keep.sh\tT1\t1.0\n").unwrap();
        fs::write(d.join(CATALOGUES).join("x.toml"), CAT).unwrap();
        let touched = apply(&d, &[("test-flip.sh".into(), "sp-b1".into())], "2026-10-02").unwrap();
        assert!(!d.join("spira/test-flip.sh").exists());
        assert_eq!(fs::read_to_string(d.join(ALLOWLIST)).unwrap(), "test-keep.sh\tT1\t1.0\n");
        let cat = fs::read_to_string(d.join(CATALOGUES).join("x.toml")).unwrap();
        let (one, two) = cat.split_once("id = \"UC-x-02\"").unwrap();
        assert!(one.contains("sp-b1") && !two.contains("uncovered"), "{cat}");
        assert_eq!(touched.len(), 3, "{touched:?}");
        assert!(apply(&d, &[("test-flip.sh".into(), "sp-b1".into())], "d").is_err(), "a suite already gone is an error");
    }
}
