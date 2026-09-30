//! `plan-lint` — every suite declares its tier and its UC coverage, and every UC id it names
//! exists in the typed catalogue. Ported from `spira/plan-lint.sh`'s default (`--lint`) mode
//! (sp-pppt0). Contract: DESIGN.md.
//!
//! `plan-lint.sh` itself is not deleted: its `--orphans` mode is `plan-matrix` (this crate,
//! ported earlier under sp-ufbkh) and its `--check`/`--gaps` modes stay bash-callable
//! utilities (`--gaps` is a report, never a failure, and has no gate role to port). This
//! rule is the first gate coverage for the header-and-UC-id check: the bash fence was never
//! wired into the gate string at all, so violations reached the tree unchecked, and the
//! corpus accumulated about 130 suites with no `# tier:` and a handful of stale UC ids.
//! [`ALLOW_FILE`] grandfathers today's offenders in (whole suite, shrink-only, the
//! `testlib-migrated`/`tmp-leak` pattern) so the rule can gate every *new* suite without
//! failing every branch on debt it did not add.

use std::cell::Cell;
use std::collections::BTreeSet;

use test_plan::load_catalogues;

use crate::rules::covers_entries::covers_of;
use crate::rules::plan_matrix::tier_of;
use crate::{direct_child, Entry, Finding, LintError, Rule, Tree};

pub struct PlanLint {
    checked: Cell<Option<usize>>,
}

impl Default for PlanLint {
    fn default() -> Self {
        PlanLint { checked: Cell::new(None) }
    }
}

const NAME: &str = "plan-lint";
const CATALOGUE_DIR: &str = "docs/test-plan";
const ALLOW_FILE: &str = "spira-lint/plan-lint-allow";

/// `spira/test-*.sh`, directly in spira/.
fn is_suite(path: &str) -> bool {
    direct_child(path, "spira").is_some_and(|b| b.starts_with("test-") && b.ends_with(".sh"))
}

impl Rule for PlanLint {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_suite(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let suites = crate::scope(tree, self)?;
        let allow: Vec<(usize, String)> = tree
            .read_text(ALLOW_FILE)
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim_start();
                !(t.is_empty() || t.starts_with('#'))
            })
            .map(|(i, l)| (i + 1, l.trim().to_string()))
            .collect();
        let listed = |p: &str| allow.iter().any(|(_, a)| a == p);

        let dir = tree.root.join(CATALOGUE_DIR);
        let known: BTreeSet<String> = if dir.is_dir() {
            match load_catalogues(&dir) {
                Ok(cats) => cats
                    .iter()
                    .flat_map(|lc| lc.catalogue.use_case.iter())
                    .map(|uc| uc.id.clone())
                    .collect(),
                Err(errs) => {
                    let root = format!("{}/", tree.root.display());
                    return Ok(errs
                        .into_iter()
                        .map(|e| Finding {
                            rule: NAME,
                            path: CATALOGUE_DIR.into(),
                            line: None,
                            message: e.replace(&root, ""),
                        })
                        .collect());
                }
            }
        } else {
            BTreeSet::new()
        };

        let mut out = Vec::new();
        let mut offenders = Vec::new();
        for e in &suites {
            let Some(c) = tree.content(e) else { continue };
            let text = String::from_utf8_lossy(c);
            let has_tier = tier_of(&text).is_some_and(|t| !t.is_empty());
            let has_covers = covers_of(&text).is_some_and(|c| !c.is_empty());
            let mut here = Vec::new();
            if !has_tier {
                here.push("missing # tier:".to_string());
            }
            if !has_covers {
                here.push("missing # covers:".to_string());
            }
            if let Some(toks) = covers_of(&text) {
                for tok in toks.iter().filter(|t| t.starts_with("UC-")) {
                    if !known.contains(tok) {
                        here.push(format!("unknown UC id on # covers: {tok}"));
                    }
                }
            }
            if here.is_empty() {
                continue;
            }
            offenders.push(e.path.clone());
            if listed(&e.path) {
                continue;
            }
            for message in here {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: None, message });
            }
        }
        for (line, p) in &allow {
            if !offenders.iter().any(|o| o == p) {
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("lists {p}, which no longer needs an exception — remove the line (the list only shrinks)"),
                });
            }
        }
        if out.is_empty() {
            self.checked.set(Some(suites.len()));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "suites".to_string()))
    }

    fn hint(&self) -> &'static str {
        "Every suite declares # tier: and # covers:, and every UC id on # covers: exists in \
docs/test-plan/*.toml. Add the header, fix the id, or add the use case to its catalogue. \
spira-lint/plan-lint-allow grandfathers today's offenders in and only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const CAT: &str = "api_version = \"test-plan/v1\"\narea = \"demo\"\n\n[[use_case]]\nid = \"UC-demo-01\"\ntier = \"T0\"\nstatement = \"one\"\n";

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        PlanLint::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn missing_headers_are_named_and_clearing_them_passes() {
        let t = TempDir::new("pl-missing");
        t.write("docs/test-plan/demo.toml", CAT);
        t.write("spira/test-planted.sh", "#!/usr/bin/env bash\necho hello\n");
        let got = run(&t, &["docs/test-plan/demo.toml", "spira/test-planted.sh"]).unwrap();
        assert_eq!(
            got,
            vec![
                "plan-lint: spira/test-planted.sh: missing # tier:".to_string(),
                "plan-lint: spira/test-planted.sh: missing # covers:".to_string(),
            ]
        );
    }

    #[test]
    fn an_unknown_uc_id_is_refused_and_a_known_one_passes() {
        let t = TempDir::new("pl-uc");
        t.write("docs/test-plan/demo.toml", CAT);
        t.write(
            "spira/test-planted.sh",
            "#!/usr/bin/env bash\n# tier: T1\n# covers: spira/dispatch.sh UC-demo-99\necho hi\n",
        );
        let got = run(&t, &["docs/test-plan/demo.toml", "spira/test-planted.sh"]).unwrap();
        assert_eq!(got, vec!["plan-lint: spira/test-planted.sh: unknown UC id on # covers: UC-demo-99"]);

        t.write(
            "spira/test-planted.sh",
            "#!/usr/bin/env bash\n# tier: T1\n# covers: spira/dispatch.sh UC-demo-01\necho hi\n",
        );
        assert!(run(&t, &["docs/test-plan/demo.toml", "spira/test-planted.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_malformed_catalogue_is_a_finding_not_a_refusal() {
        let t = TempDir::new("pl-bad-cat");
        t.write("docs/test-plan/demo.toml", "api_version = \"test-plan/v1\"\narea = \"demo\"\nbogus = 1\n");
        t.write("spira/test-a.sh", "# tier: T0\n# covers: spira/a.sh\n");
        let got = run(&t, &["docs/test-plan/demo.toml", "spira/test-a.sh"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("bogus"), "{}", got[0]);
    }

    #[test]
    fn no_catalogue_directory_still_checks_headers() {
        let t = TempDir::new("pl-no-cat");
        t.write("spira/test-a.sh", "echo\n");
        let got = run(&t, &["spira/test-a.sh"]).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
    }

    #[test]
    fn no_suites_matched_refuses() {
        let t = TempDir::new("pl-none");
        t.write("spira/lib.sh", "echo\n");
        assert_eq!(run(&t, &["spira/lib.sh"]), Err(LintError::EmptyScope));
    }

    #[test]
    fn a_listed_offender_is_silenced_and_a_stale_entry_is_refused() {
        let t = TempDir::new("pl-allow");
        t.write("docs/test-plan/demo.toml", CAT);
        t.write("spira/test-old.sh", "echo hello\n");
        t.write("spira/test-fixed.sh", "# tier: T0\n# covers: spira/a.sh\n");
        t.write(ALLOW_FILE, "# header\nspira/test-old.sh\nspira/test-fixed.sh\n");
        let got = run(
            &t,
            &["docs/test-plan/demo.toml", "spira/test-old.sh", "spira/test-fixed.sh", ALLOW_FILE],
        )
        .unwrap();
        assert_eq!(
            got,
            vec![format!(
                "plan-lint: {ALLOW_FILE}:3: lists spira/test-fixed.sh, which no longer needs an exception — remove the line (the list only shrinks)"
            )]
        );
    }

    #[test]
    fn a_clean_tree_reports_checked() {
        let t = TempDir::new("pl-clean");
        t.write("docs/test-plan/demo.toml", CAT);
        t.write("spira/test-a.sh", "# tier: T0\n# covers: spira/a.sh UC-demo-01\n");
        let tree = Tree::from_paths(t.path(), ["docs/test-plan/demo.toml", "spira/test-a.sh"], std::iter::empty::<&str>());
        let r = PlanLint::default();
        assert_eq!(r.check(&tree), Ok(vec![]));
        assert_eq!(r.checked(), Some((1, "suites".to_string())));
    }
}
