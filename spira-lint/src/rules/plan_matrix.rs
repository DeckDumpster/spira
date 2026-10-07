//! `plan-matrix` — the test plan's gate fence: a suite change that orphans a use case's last
//! cover is refused, and so is a catalogue the coverage matrix cannot be built from. Ported
//! from `spira/plan-matrix-fence.sh` (sp-ufbkh). Contract: DESIGN.md.
//!
//! It judges the tree spira-lint walks (the gate tree, at the gate) against [`Tree::base`];
//! the bash fence was skipped at every gate because its caller compared `SPIRA_GATE_REPO`
//! (the repository) with the tree it sat in (the gate's worktree), and they never matched.

use crate::SyncCell as Cell;
use std::collections::BTreeMap;
use std::io::Write;
use std::process::Stdio;

use test_plan::{build_matrix, load_catalogues, orphan_violations, render_markdown, SuiteCoverage};

use crate::rules::covers_entries::covers_of;
use crate::{direct_child, Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "plan-matrix";
const CATALOGUE_DIR: &str = "docs/test-plan";

#[derive(Default)]
pub struct PlanMatrix {
    /// (use cases, suites here, suites at the base) after a clean check.
    checked: Cell<Option<(usize, usize, usize)>>,
}

/// `spira/test-*.sh`, directly in spira/.
fn is_suite(path: &str) -> bool {
    direct_child(path, "spira").is_some_and(|b| b.starts_with("test-") && b.ends_with(".sh"))
}

/// `suite_tier_of`: the first `# tier:` value before `set -` — the selector crate's parser
/// (sp-wx2tw).
pub use suite_select::header::tier_of;

/// One suite's declared coverage, as `suite-coverage-json.sh` renders it.
pub fn coverage(path: &str, text: &str) -> SuiteCoverage {
    SuiteCoverage {
        path: path.to_string(),
        tier: tier_of(text).filter(|t| !t.is_empty()),
        covers: covers_of(text).unwrap_or_default(),
    }
}

/// Every `spira/test-*.sh` as it stood at `rev`, read in one `git cat-file --batch`.
fn suites_at(tree: &Tree, rev: &str) -> Result<Vec<SuiteCoverage>, String> {
    let names = tree.git(&["ls-tree", "-r", "--name-only", rev, "--", "spira"])?;
    let paths: Vec<String> = String::from_utf8_lossy(&names)
        .lines()
        .filter(|p| is_suite(p))
        .map(str::to_string)
        .collect();
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&tree.root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("git cat-file: {e}"))?;
    let req = paths.iter().fold(String::new(), |acc, p| acc + rev + ":" + p + "\n");
    let mut stdin = child.stdin.take().ok_or("git cat-file: no stdin")?;
    let writer = std::thread::spawn(move || stdin.write_all(req.as_bytes()));
    let out = child.wait_with_output().map_err(|e| format!("git cat-file: {e}"))?;
    let _ = writer.join();
    if !out.status.success() {
        return Err("git cat-file --batch failed".into());
    }
    let buf = out.stdout;
    let mut at = 0;
    let mut suites = Vec::new();
    for p in &paths {
        let nl = buf[at..]
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("git cat-file: truncated output")?;
        let header = String::from_utf8_lossy(&buf[at..at + nl]).into_owned();
        at += nl + 1;
        let mut f = header.split(' ');
        let (_, kind, size) = (f.next(), f.next(), f.next());
        if kind != Some("blob") {
            return Err(format!("git cat-file: {rev}:{p}: {header}"));
        }
        let size: usize = size
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("git cat-file: bad header {header}"))?;
        let end = at + size;
        if end > buf.len() {
            return Err("git cat-file: truncated output".into());
        }
        suites.push(coverage(p, &String::from_utf8_lossy(&buf[at..end])));
        at = end + 1; // the newline after each object
    }
    Ok(suites)
}

impl Rule for PlanMatrix {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_suite(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let base_rev = tree.base_commit()?;
        let base = tree.base.as_deref().unwrap_or_default();
        let prev = suites_at(tree, &base_rev).map_err(LintError::Refused)?;
        if prev.is_empty() {
            return Err(LintError::Refused(format!(
                "the base {base} has no spira/test-*.sh — nothing to judge a deletion against"
            )));
        }
        let mut cur = Vec::new();
        for e in crate::scope(tree, self)? {
            if let Some(c) = tree.content(e) {
                cur.push(coverage(&e.path, &String::from_utf8_lossy(c)));
            }
        }
        let dir = tree.root.join(CATALOGUE_DIR);
        if !dir.is_dir() {
            return Err(LintError::Refused(format!("no catalogue directory {CATALOGUE_DIR}")));
        }
        let finding = |message: String| Finding { rule: NAME, path: CATALOGUE_DIR.into(), line: None, message };
        let cats = match load_catalogues(&dir) {
            Ok(c) => c,
            // A catalogue the matrix cannot be built from: each error is a finding.
            Err(errs) => {
                let root = format!("{}/", tree.root.display());
                return Ok(errs.into_iter().map(|e| finding(e.replace(&root, ""))).collect());
            }
        };
        let use_cases: usize = cats.iter().map(|c| c.catalogue.use_case.len()).sum();
        if use_cases == 0 {
            return Err(LintError::Refused(format!("{CATALOGUE_DIR} declares no use case — nothing to check")));
        }
        let out: Vec<Finding> = orphan_violations(&cats, &prev, &cur).into_iter().map(finding).collect();
        // The matrix is built and rendered in memory, as plan-matrix.sh does to disk: a
        // catalogue that loads but cannot be rendered is caught here, not by the next reader.
        let doc = build_matrix(&cats, &cur, &BTreeMap::new());
        if render_markdown(&doc).is_empty() {
            return Err(LintError::Refused("the coverage matrix rendered empty".into()));
        }
        if out.is_empty() {
            self.checked.set(Some((use_cases, cur.len(), prev.len())));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked
            .get()
            .map(|(n, here, base)| (n, format!("use-cases ({here} suites here, {base} at the base)")))
    }

    fn hint(&self) -> &'static str {
        "a use case lost its last covering suite: cover it again, or mark it [use_case.uncovered] in its catalogue"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use crate::testutil::TempDir;

    const CAT: &str = "api_version = \"1\"\narea = \"demo\"\n\n[[use_case]]\nid = \"UC-demo-01\"\ntier = \"T0\"\nstatement = \"one\"\n\n[[use_case]]\nid = \"UC-demo-02\"\ntier = \"T1\"\nstatement = \"two\"\n";

    /// A repository whose base commit has two suites, each the last cover of one use case.
    fn repo() -> TempDir {
        let t = TempDir::new("plan-matrix");
        t.git_init();
        t.write("docs/test-plan/demo.toml", CAT);
        t.write("spira/test-a.sh", "#!/bin/sh\n# tier: T0\n# covers: spira/a.sh UC-demo-01\nset -u\n");
        t.write("spira/test-b.sh", "#!/bin/sh\n# tier: T1\n# covers: UC-demo-02\nset -u\n");
        t.git(&["add", "."]);
        t.git(&["commit", "-qm", "base"]);
        t.git(&["tag", "base"]);
        t
    }

    fn check(t: &TempDir, base: Option<&str>) -> crate::testutil::Checked {
        let tree = Tree::from_git(t.path()).unwrap().with_base(base.map(str::to_string));
        let r = PlanMatrix::default();
        let out = r.check(&tree);
        (out, r.checked())
    }

    #[test]
    fn an_unchanged_tree_checks_every_use_case_and_says_so() {
        let t = repo();
        let (out, checked) = check(&t, Some("base"));
        assert_eq!(out, Ok(vec![]));
        assert_eq!(checked, Some((2, "use-cases (2 suites here, 2 at the base)".into())));
    }

    #[test]
    fn deleting_a_use_cases_last_cover_is_a_finding() {
        let t = repo();
        t.git(&["rm", "-q", "spira/test-b.sh"]);
        let (out, checked) = check(&t, Some("base"));
        let out = out.unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].message.starts_with("UC-demo-02: lost its last covering suite"), "{}", out[0]);
        assert_eq!(checked, None, "no positive control on a red");
    }

    #[test]
    fn an_uncovered_marker_or_a_new_cover_keeps_it_clean() {
        let t = repo();
        t.git(&["rm", "-q", "spira/test-b.sh"]);
        t.write("spira/test-c.sh", "#!/bin/sh\n# tier: T1\n# covers: UC-demo-02\n");
        assert_eq!(check(&t, Some("base")).0, Ok(vec![]));
    }

    #[test]
    fn no_base_refuses_rather_than_compare_against_nothing() {
        let t = repo();
        let (out, checked) = check(&t, None);
        assert!(matches!(out, Err(LintError::Refused(ref m)) if m.contains("SPIRA_GATE_BASE")), "{out:?}");
        assert_eq!(checked, None);
        let (out, _) = check(&t, Some("no-such-ref"));
        assert!(matches!(out, Err(LintError::Refused(ref m)) if m.contains("does not resolve")), "{out:?}");
    }

    #[test]
    fn a_base_with_no_suites_refuses() {
        let t = repo();
        t.git(&["commit", "-q", "--allow-empty", "-m", "x"]);
        let empty = TempDir::new("plan-matrix-empty");
        empty.git_init();
        empty.write("README", "x\n");
        empty.git(&["add", "."]);
        empty.git(&["commit", "-qm", "e"]);
        empty.write("docs/test-plan/demo.toml", CAT);
        empty.write("spira/test-a.sh", "# covers: UC-demo-01\n");
        let (out, _) = check(&empty, Some("HEAD"));
        assert!(matches!(out, Err(LintError::Refused(ref m)) if m.contains("no spira/test-*.sh")), "{out:?}");
    }

    #[test]
    fn a_malformed_catalogue_is_a_finding() {
        let t = repo();
        t.write("docs/test-plan/demo.toml", "api_version = \"1\"\narea = \"demo\"\nbogus = 1\n");
        let out = check(&t, Some("base")).0.unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].message.contains("bogus"), "{}", out[0]);
    }

    /// The rule reads the tree it is handed — the gate's worktree — not the repository the
    /// worktree belongs to: a deletion made only in the worktree is seen.
    #[test]
    fn it_judges_the_worktree_it_walks_not_the_main_checkout() {
        let t = repo();
        let wt = TempDir::new("plan-matrix-wt");
        let wt_path = wt.path().join("gate-tree");
        t.git(&["worktree", "add", "-q", "--detach", wt_path.to_str().unwrap(), "base"]);
        let st = Command::new("git")
            .arg("-C")
            .arg(&wt_path)
            .args(["rm", "-q", "spira/test-a.sh"])
            .status()
            .unwrap();
        assert!(st.success());
        let tree = Tree::from_git(&wt_path).unwrap().with_base(Some("base".into()));
        let out = PlanMatrix::default().check(&tree).unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].message.starts_with("UC-demo-01"), "{}", out[0]);
        // …and the main checkout, untouched, is clean.
        assert_eq!(check(&t, Some("base")).0, Ok(vec![]));
    }

    #[test]
    fn tier_of_stops_at_set_and_takes_the_first() {
        assert_eq!(tier_of("# tier: T2\n# tier: T3\n"), Some("T2".into()));
        assert_eq!(tier_of("set -u\n# tier: T2\n"), None);
        assert_eq!(tier_of("#tier:T1\n"), Some("T1".into()));
    }
}
