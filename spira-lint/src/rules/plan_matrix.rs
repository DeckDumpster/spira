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

use test_plan::{
    build_matrix, coverage_gaps, load_catalogues, orphan_violations, render_markdown,
    unknown_uc_violations, SuiteCoverage,
};

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

/// A Rust source's `// covers: UC-…` lines as one T0 cover, as `suite-coverage-json.sh` reads them.
fn rust_coverage(path: &str, text: &str) -> Option<SuiteCoverage> {
    let mut covers: Vec<String> = Vec::new();
    for l in text
        .lines()
        .filter(|l| l.trim_start().starts_with("// covers: UC-"))
    {
        for tok in l.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
            if tok.starts_with("UC-") && !covers.iter().any(|c| c == tok) {
                covers.push(tok.to_string());
            }
        }
    }
    (!covers.is_empty()).then(|| SuiteCoverage {
        path: path.to_string(),
        tier: Some("T0".into()),
        covers,
    })
}

/// Every Rust source's `// covers: UC-…` cover as it stood at `rev`.
fn rust_covers_at(tree: &Tree, rev: &str) -> Result<Vec<SuiteCoverage>, String> {
    let out = match tree.git(&[
        "grep",
        "-I",
        "--no-color",
        "-E",
        "^[[:space:]]*// covers: UC-",
        rev,
        "--",
        "*.rs",
    ]) {
        Ok(o) => o,
        Err(e) if e.ends_with(": ") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let prefix = format!("{rev}:");
    let mut by_path: BTreeMap<String, String> = BTreeMap::new();
    for l in String::from_utf8_lossy(&out).lines() {
        let l = l.strip_prefix(&prefix).unwrap_or(l);
        if let Some((path, line)) = l.split_once(':') {
            let t = by_path.entry(path.to_string()).or_default();
            t.push_str(line);
            t.push('\n');
        }
    }
    Ok(by_path
        .iter()
        .filter_map(|(p, t)| rust_coverage(p, t))
        .collect())
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
    // batch-job: reads every suite blob in one process; stalls for tens of seconds on a loaded host; hang guard only
    let mut child = std::process::Command::new("timeout")
        .arg(crate::GIT_HANG_GUARD_SECS)
        .arg("git")
        .arg("-C")
        .arg(&tree.root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("git cat-file: {e}"))?;
    let req = paths
        .iter()
        .fold(String::new(), |acc, p| acc + rev + ":" + p + "\n");
    let mut stdin = child.stdin.take().ok_or("git cat-file: no stdin")?;
    let writer = std::thread::spawn(move || stdin.write_all(req.as_bytes()));
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git cat-file: {e}"))?;
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
        let mut prev = suites_at(tree, &base_rev).map_err(LintError::Refused)?;
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
        let suites_here = cur.len();
        let suites_base = prev.len();
        prev.extend(rust_covers_at(tree, &base_rev).map_err(LintError::Refused)?);
        let mut rust_here = Vec::new();
        for e in tree.entries.iter().filter(|e| e.path.ends_with(".rs")) {
            let Some(c) = tree.content(e) else { continue };
            if let Some(sc) = rust_coverage(&e.path, &String::from_utf8_lossy(c)) {
                rust_here.push(sc);
            }
        }
        let dir = tree.root.join(CATALOGUE_DIR);
        if !dir.is_dir() {
            return Err(LintError::Refused(format!(
                "no catalogue directory {CATALOGUE_DIR}"
            )));
        }
        let finding = |message: String| Finding {
            rule: NAME,
            path: CATALOGUE_DIR.into(),
            line: None,
            message,
        };
        let cats = match load_catalogues(&dir) {
            Ok(c) => c,
            // A catalogue the matrix cannot be built from: each error is a finding.
            Err(errs) => {
                let root = format!("{}/", tree.root.display());
                return Ok(errs
                    .into_iter()
                    .map(|e| finding(e.replace(&root, "")))
                    .collect());
            }
        };
        let use_cases: usize = cats.iter().map(|c| c.catalogue.use_case.len()).sum();
        if use_cases == 0 {
            return Err(LintError::Refused(format!(
                "{CATALOGUE_DIR} declares no use case — nothing to check"
            )));
        }
        let undeclared: Vec<String> = unknown_uc_violations(&cats, &rust_here)
            .into_iter()
            .map(|m| {
                format!(
                    "{m} — add the use case to docs/test-plan/<area>.toml, or name a declared one"
                )
            })
            .collect();
        cur.extend(rust_here);
        for s in &mut prev {
            for c in &mut s.covers {
                *c = crate::rules::ffx_vocabulary::renamed(c);
            }
        }
        let orphans = orphan_violations(&cats, &prev, &cur);
        let gaps = coverage_gaps(&cats, &cur).into_iter().filter(|g| {
            !orphans
                .iter()
                .any(|o| g.starts_with(&format!("gap: {} ", o.split(':').next().unwrap_or(""))))
        });
        let out: Vec<Finding> = orphans
            .iter()
            .cloned()
            .chain(gaps)
            .chain(undeclared)
            .map(finding)
            .collect();
        // The matrix is built and rendered in memory, as plan-matrix.sh does to disk: a
        // catalogue that loads but cannot be rendered is caught here, not by the next reader.
        let doc = build_matrix(&cats, &cur, &BTreeMap::new());
        if render_markdown(&doc).is_empty() {
            return Err(LintError::Refused(
                "the coverage matrix rendered empty".into(),
            ));
        }
        if out.is_empty() {
            self.checked
                .set(Some((use_cases, suites_here, suites_base)));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|(n, here, base)| {
            (
                n,
                format!("use-cases ({here} suites here, {base} at the base)"),
            )
        })
    }

    fn hint(&self) -> &'static str {
        "a use case lost its last cover (suite header or Rust `// covers: UC-…` line) or has none, or a Rust test names an undeclared id: cover it again, mark it [use_case.uncovered], or declare the id in docs/test-plan/<area>.toml"
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(PlanMatrix::default())]
}

#[cfg(test)]
mod tests {
    use super::*;



    #[test]
    fn tier_of_stops_at_set_and_takes_the_first() {
        assert_eq!(tier_of("# tier: T2\n# tier: T3\n"), Some("T2".into()));
        assert_eq!(tier_of("set -u\n# tier: T2\n"), None);
        assert_eq!(tier_of("#tier:T1\n"), Some("T1".into()));
    }
}
