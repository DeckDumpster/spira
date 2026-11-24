//! `config-delta` — a registry key added or dropped relative to the base is declared in
//! `spira/config-delta.toml`. Contract: DESIGN.md.

use std::collections::BTreeSet;

use crate::SyncCell as Cell;
use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "config-delta";
const DELTA: &str = "spira/config-delta.toml";
const DIRS: [&str; 2] = ["spira/conf.d", "spira/conf.toml.d"];

#[derive(Default)]
pub struct ConfigDelta {
    checked: Cell<Option<usize>>,
}

fn is_key_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

const TASK_MODES: &str = "spira/conf.d/SPIRA_TASK_MODES";

fn task_names(key_text: &str) -> Option<BTreeSet<String>> {
    let key = spira_config::registry::parse_key_text("SPIRA_TASK_MODES", key_text).ok()?;
    let list = key.default_expr()?;
    Some(
        list.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|e| !e.is_empty())
            .map(|e| e.split_once('=').map_or(e, |x| x.0).to_string())
            .collect(),
    )
}

fn undeclared_removed_tasks(tree: &Tree, base: &str, delta: &str) -> Vec<String> {
    let Some(prior) = tree.git(&["show", &format!("{base}:{TASK_MODES}")]).ok().and_then(|o| task_names(&String::from_utf8_lossy(&o))) else { return Vec::new() };
    let Some(head) = std::fs::read_to_string(tree.root.join(TASK_MODES)).ok().and_then(|t| task_names(&t)) else { return Vec::new() };
    let declared: BTreeSet<String> = delta
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("removed_tasks").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()))
        .unwrap_or_default();
    prior.into_iter().filter(|t| !head.contains(t) && !declared.contains(t)).collect()
}

pub fn config_path(file: &str) -> String {
    format!("spira.{}", file.strip_prefix("SPIRA_").unwrap_or(file).to_ascii_lowercase())
}

/// Word pairs by which a release renamed its vocabulary: a key dropped under the first word
/// and added under the second is a rename, which carries the operator's value.
const RENAMES: &[(&str, &str)] = &[("aeons", "dwarves"), ("aeon", "dwarf"), ("summon", "call")]; // ffx-ok

fn renamed_pairs(text: &str) -> Result<Vec<(String, String)>, String> {
    let v: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    Ok(v.get("renamed")
        .and_then(|r| r.as_table())
        .map(|t| t.iter().filter_map(|(f, to)| to.as_str().map(|to| (f.clone(), to.to_string()))).collect())
        .unwrap_or_default())
}

/// `(added, removed)` declared by a delta's text; a `[renamed]` entry declares its target
/// added and its source removed.
pub fn declared(text: &str) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let (added, removed) = spira_config::parse_config_delta(text)?;
    let (mut added, mut removed): (BTreeSet<String>, BTreeSet<String>) = (added.into_iter().collect(), removed.into_iter().collect());
    for (from, to) in renamed_pairs(text)? {
        removed.insert(from);
        added.insert(to);
    }
    Ok((added, removed))
}

/// `(removed, added)` pairs where the removed key maps to the added key through [`RENAMES`]
/// yet no `[renamed]` entry carries the value.
pub fn unrenamed(text: &str) -> Result<Vec<(String, String)>, String> {
    let (added, removed) = spira_config::parse_config_delta(text)?;
    let carried: BTreeSet<String> = renamed_pairs(text)?.into_iter().map(|(f, _)| f).collect();
    let mut out = Vec::new();
    for r in removed.iter().filter(|r| !carried.contains(*r)) {
        for (old, new) in RENAMES {
            if r.contains(old) {
                let twin = r.replacen(old, new, 1);
                if added.contains(&twin) {
                    out.push((r.clone(), twin));
                }
            }
        }
    }
    Ok(out)
}

/// `(undeclared additions, undeclared removals)` between two key sets.
pub fn undeclared(
    base: &BTreeSet<String>,
    head: &BTreeSet<String>,
    added: &BTreeSet<String>,
    removed: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    (
        head.difference(base).filter(|k| !added.contains(*k)).cloned().collect(),
        base.difference(head).filter(|k| !removed.contains(*k)).cloned().collect(),
    )
}

impl Rule for ConfigDelta {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        DIRS.iter().any(|d| e.path.starts_with(&format!("{d}/"))) || e.path == DELTA || e.path.starts_with("spira/config-delta.d/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let base = tree.base_commit()?;
        let mut head = BTreeSet::new();
        let mut prior = BTreeSet::new();
        for d in DIRS {
            if let Ok(rd) = std::fs::read_dir(tree.root.join(d)) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if is_key_name(&n) && e.path().is_file() {
                        head.insert(config_path(&n));
                    }
                }
            }
            if let Ok(out) = tree.git(&["ls-tree", "--name-only", &base, &format!("{d}/")]) {
                for line in String::from_utf8_lossy(&out).lines() {
                    let n = line.rsplit('/').next().unwrap_or(line);
                    if is_key_name(n) {
                        prior.insert(config_path(n));
                    }
                }
            }
        }
        if head.is_empty() || prior.is_empty() {
            return Err(LintError::Refused(format!(
                "no config registry keys found ({} here, {} at the base) — nothing to compare",
                head.len(),
                prior.len()
            )));
        }
        let text = spira_config::delta::delta_text(&tree.root.join("spira")).map_err(|e| LintError::Refused(format!("{DELTA}: {e}")))?.unwrap_or_default();
        let (added, removed) =
            declared(&text).map_err(|e| LintError::Refused(format!("{DELTA} does not parse: {e}")))?;
        let typed = spira_config::delta_added(&text).and_then(|a| spira_config::check_delta(&tree.root.join("spira"), &a));
        let (no_add, no_rm) = undeclared(&prior, &head, &added, &removed);
        let mut out = Vec::new();
        if let Err(e) = typed {
            out.push(Finding { rule: NAME, path: DELTA.into(), line: None, message: format!("{DELTA} does not load under the typed schema: {e}") });
        }
        for k in no_add {
            out.push(Finding {
                rule: NAME,
                path: DELTA.into(),
                line: None,
                message: format!("registry key {k} is added but {DELTA} has no entry for it under its config table"),
            });
        }
        for k in no_rm {
            out.push(Finding {
                rule: NAME,
                path: DELTA.into(),
                line: None,
                message: format!("registry key {k} is removed but {DELTA} does not list it under removed"),
            });
        }
        for task in undeclared_removed_tasks(tree, &base, &text) {
            out.push(Finding {
                rule: NAME,
                path: TASK_MODES.into(),
                line: None,
                message: format!(
                    "task {task} is dropped from the SPIRA_TASK_MODES default but no config delta lists it under removed_tasks: the host's spira.task_modes still names it and the release build will refuse the landing; run spira-config retire --task {task} --bead <id>"
                ),
            });
        }
        for (from, to) in unrenamed(&text).map_err(|e| LintError::Refused(format!("{DELTA} does not parse: {e}")))? {
            out.push(Finding {
                rule: NAME,
                path: DELTA.into(),
                line: None,
                message: format!("{from} is removed and {to} added without a [renamed] entry: the operator's value would be re-defaulted, not carried"),
            });
        }
        if out.is_empty() {
            self.checked.set(Some(head.len()));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "registry keys".into()))
    }

    fn hint(&self) -> &'static str {
        "a release that adds or drops a registry key must declare it in spira/config-delta.toml ([spira] <key> = <value>, or removed = [\"spira.<key>\"]); otherwise activation has nothing to apply and the new release refuses the config in force"
    }
}

const FRAGMENTS: &str = "spira/config-delta.d/";
const FOLDED_NAME: &str = "config-delta-folded";

/// A branch never edits the folded delta; only the round's fold of fragments may.
pub struct ConfigDeltaFolded;

fn normalized(text: &str) -> Result<toml::Value, String> {
    fn sort(v: &mut toml::Value) {
        match v {
            toml::Value::Array(a) => {
                a.iter_mut().for_each(sort);
                a.sort_by_key(|x| x.to_string());
            }
            toml::Value::Table(t) => t.iter_mut().for_each(|(_, x)| sort(x)),
            _ => {}
        }
    }
    let mut v: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    sort(&mut v);
    Ok(v)
}

fn is_exact_fold(tree: &Tree, parent: &str, fold: &str, deleted: &[&str]) -> bool {
    if deleted.is_empty() {
        return false;
    }
    let show = |rev: &str, p: &str| tree.git(&["show", &format!("{rev}:{p}")]).map(|b| String::from_utf8_lossy(&b).into_owned());
    let mut parts = vec![(DELTA.to_string(), show(parent, DELTA).unwrap_or_default())];
    for p in deleted {
        match show(parent, p) {
            Ok(t) => parts.push((p.to_string(), t)),
            Err(_) => return false,
        }
    }
    let Ok(Some(merged)) = spira_config::delta::merge_delta_texts(&parts) else { return false };
    let Ok(folded) = show(fold, DELTA) else { return false };
    matches!((normalized(&merged), normalized(&folded)), (Ok(a), Ok(b)) if a == b)
}

fn is_fold_commit(tree: &Tree, commit: &str) -> bool {
    let parent = format!("{commit}^");
    let Ok(out) = tree.git(&["diff", "--name-status", "--no-renames", &parent, commit, "--", DELTA, FRAGMENTS]) else { return false };
    let text = String::from_utf8_lossy(&out);
    let mut delta_modified = false;
    let mut deleted = Vec::new();
    for (status, path) in text.lines().filter_map(|l| l.split_once('\t')) {
        if path == DELTA && status == "M" {
            delta_modified = true;
        } else if status == "D" && path.starts_with(FRAGMENTS) && path.ends_with(".toml") {
            deleted.push(path);
        } else {
            return false;
        }
    }
    delta_modified && is_exact_fold(tree, &parent, commit, &deleted)
}

impl Rule for ConfigDeltaFolded {
    fn name(&self) -> &'static str {
        FOLDED_NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == DELTA || e.path.starts_with(FRAGMENTS)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let base = tree.base_commit()?;
        let out = tree
            .git(&["diff", "--name-status", "--no-renames", &base, "--", DELTA, FRAGMENTS])
            .map_err(|e| LintError::Refused(format!("cannot diff {DELTA} against the base: {e}")))?;
        let text = String::from_utf8_lossy(&out);
        if !text.lines().filter_map(|l| l.split_once('\t')).any(|(_, p)| p == DELTA) {
            return Ok(Vec::new());
        }
        let range = format!("{base}..HEAD");
        let listed = tree
            .git(&["rev-list", "--no-merges", &range, "--", DELTA])
            .map_err(|e| LintError::Refused(format!("cannot list the commits touching {DELTA}: {e}")))?;
        let listed = String::from_utf8_lossy(&listed);
        let commits: Vec<&str> = listed.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let ok = !commits.is_empty()
            && commits.iter().all(|c| is_fold_commit(tree, c))
            && match (tree.git(&["show", &format!("{}:{DELTA}", commits[0])]), normalized(&tree.read_text(DELTA))) {
                (Ok(b), Ok(w)) => normalized(&String::from_utf8_lossy(&b)).is_ok_and(|l| l == w),
                _ => false,
            };
        if ok {
            return Ok(Vec::new());
        }
        Ok(vec![Finding {
            rule: FOLDED_NAME,
            path: DELTA.into(),
            line: None,
            message: format!("{DELTA} is edited by this branch; write the change to {FRAGMENTS}<bead>.toml instead"),
        }])
    }

    fn hint(&self) -> &'static str {
        "the folded delta is edited only by the round's fold of fragments; a branch adds spira/config-delta.d/<bead>.toml (`spira-config retire <key> --bead <bead>` writes it) so two branches never share a line"
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ConfigDelta::default()),
        Box::new(ConfigDeltaFolded),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test_id(tbvfhnk)]
    #[test]
    fn a_retired_key_is_declared_removed_so_the_rule_is_silent() {
        let t = TempDir::new("config-delta-retire");
        for k in ["SPIRA_OLD_KNOB", "SPIRA_KEPT"] {
            t.write(&format!("spira/conf.d/{k}"), "TYPE=string\nDOC=d\n");
        }
        t.write(DELTA, "removed = []\n");
        t.write("spira/testlib/complete.toml", "[spira]\n");
        t.write("spira/conf.toml.d/SPIRA_TOML_ONLY", "TYPE=string\nDOC=d\n");
        for args in [&["init", "-q", "-b", "main"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(t.path()).args(args).status().unwrap().success());
        }
        let rule = ConfigDelta::default();
        let tree = || Tree::from_git(t.path()).unwrap().with_base(Some("main".into()));
        t.remove("spira/conf.d/SPIRA_OLD_KNOB");
        assert!(rule.check(&tree()).unwrap().iter().any(|f| f.message.contains("registry key spira.old_knob is removed")), "control: an undeclared removal is found");
        t.write("spira/conf.d/SPIRA_OLD_KNOB", "TYPE=string\nDOC=d\n");
        spira_config::retire::retire_key(t.path(), "spira.old_knob", "sp-x").unwrap();
        let found = rule.check(&tree()).unwrap();
        assert!(found.is_empty(), "{:?}", found.iter().map(|f| &f.message).collect::<Vec<_>>());
    }

    fn task_tree(base_modes: &str, head_modes: &str) -> (TempDir, String) {
        let t = TempDir::new("config-delta-tasks");
        let key = |m: &str| format!("TYPE=string\nDOC=d\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${{SPIRA_TASK_MODES:={m}}}\"\nSPIRA_CONF_DEFAULT_EOF\n");
        t.write("spira/conf.toml.d/SPIRA_TOML_ONLY", "TYPE=string\nDOC=d\n");
        t.write("spira/conf.d/SPIRA_TASK_MODES", &key(base_modes));
        t.write(DELTA, "removed = []\n");
        t.write("spira/testlib/complete.toml", "[spira]\n");
        for args in [&["init", "-q", "-b", "main"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(t.path()).args(args).status().unwrap().success());
        }
        t.write("spira/conf.d/SPIRA_TASK_MODES", &key(head_modes));
        (t, String::new())
    }

    #[test_id(t2af9ez)]
    #[test]
    fn a_task_dropped_from_the_default_without_removed_tasks_is_found_and_declaring_it_clears_it() {
        let (t, _) = task_tree("a=act b=act", "a=act");
        let rule = ConfigDelta::default();
        let tree = || Tree::from_git(t.path()).unwrap().with_base(Some("main".into()));
        let found = rule.check(&tree()).unwrap();
        assert_eq!(found.len(), 1, "{:?}", found.iter().map(|f| &f.message).collect::<Vec<_>>());
        assert!(found[0].message.contains("task b is dropped") && found[0].message.contains("spira-config retire --task b"), "{}", found[0].message);
        t.write("spira/config-delta.d/x.toml", "removed_tasks = [\"b\"]\n");
        let found = rule.check(&tree()).unwrap();
        assert!(found.is_empty(), "{:?}", found.iter().map(|f| &f.message).collect::<Vec<_>>());
    }

    #[test_id(ty5p5dd)]
    #[test]
    fn a_side_without_a_task_modes_file_skips_the_task_check() {
        let (t, _) = task_tree("a=act b=act", "a=act");
        t.remove("spira/conf.d/SPIRA_TASK_MODES");
        let tree = Tree::from_git(t.path()).unwrap().with_base(Some("main".into()));
        let found = ConfigDelta::default().check(&tree).unwrap();
        assert!(found.iter().all(|f| f.path != TASK_MODES), "{:?}", found.iter().map(|f| &f.message).collect::<Vec<_>>());
    }

    /// `commits`: (rev, status rows against its parent, DELTA at the rev, DELTA at its parent).
    fn folded_tree(
        t: &TempDir,
        status: &'static str,
        base_file: &'static str,
        frags: &'static [(&'static str, &'static str)],
        commits: &'static [(&'static str, &'static str, &'static str, &'static str)],
    ) -> Tree {
        Tree::from_paths(t.path(), [DELTA], Vec::<String>::new()).with_base(Some("main".into())).with_git(Box::new(move |a| match a {
            ["rev-parse", ..] | ["merge-base", ..] => Ok(b"B\n".to_vec()),
            ["diff", "--name-status", "--no-renames", _, "--", ..] => Ok(status.as_bytes().to_vec()),
            ["diff", "--name-status", "--no-renames", p, c, "--", ..] => commits
                .iter()
                
                .find(|(r, ..)| format!("{r}^") == *p && r == c)
                .map(|(_, st, ..)| st.as_bytes().to_vec())
                .ok_or_else(|| format!("unexpected {a:?}")),
            ["rev-list", "--no-merges", "B..HEAD", "--", ..] => Ok(commits.iter().rev().map(|(r, ..)| format!("{r}\n")).collect::<String>().into_bytes()),
            ["show", spec] => {
                let (rev, path) = spec.split_once(':').ok_or("bad spec")?;
                if path == DELTA {
                    if rev == "B" {
                        return Ok(base_file.as_bytes().to_vec());
                    }
                    if let Some((_, _, d, _)) = commits.iter().find(|(r, ..)| *r == rev) {
                        return Ok(d.as_bytes().to_vec());
                    }
                    if let Some((.., d)) = commits.iter().find(|(r, ..)| format!("{r}^") == rev) {
                        return Ok(d.as_bytes().to_vec());
                    }
                }
                frags.iter().find(|(p, _)| *p == path).map(|(_, c)| c.as_bytes().to_vec()).ok_or_else(|| format!("unexpected {a:?}"))
            }
            other => Err(format!("unexpected {other:?}")),
        }))
    }

    const FRAG_A: &[(&str, &str)] = &[("spira/config-delta.d/sp-a.toml", "removed = [\"spira.b\"]\n")];
    const FOLD_ST: &str = "M\tspira/config-delta.toml\nD\tspira/config-delta.d/sp-a.toml\n";
    const A: &str = "removed = [\"spira.a\"]\n";
    const AB: &str = "removed = [\"spira.a\", \"spira.b\"]\n";

    #[test_id(t53gblw)]
    #[test]
    fn a_branch_editing_the_folded_delta_gets_one_finding() {
        let t = TempDir::new("folded-edit");
        t.write(DELTA, A);
        let f = ConfigDeltaFolded.check(&folded_tree(&t, "M\tspira/config-delta.toml\n", "", &[], &[("E", "M\tspira/config-delta.toml\n", A, "")])).unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.contains("spira/config-delta.d/<bead>.toml"));
    }

    #[test_id(tl3am9w)]
    #[test]
    fn a_tree_whose_diff_is_exactly_a_fold_gets_none() {
        let t = TempDir::new("folded-fold");
        t.write(DELTA, AB);
        let tree = folded_tree(&t, FOLD_ST, A, FRAG_A, &[("F", FOLD_ST, AB, A)]);
        assert!(ConfigDeltaFolded.check(&tree).unwrap().is_empty());
    }

    #[test_id(td11eis)]
    #[test]
    fn a_fold_whose_fragment_supersedes_a_base_value_gets_none_and_any_other_value_one() {
        const BASE: &str = "[spira]\nk = 1\n";
        const FRAG: &[(&str, &str)] = &[("spira/config-delta.d/sp-a.toml", "[spira]\nk = 2\n")];
        let t = TempDir::new("folded-supersede");
        t.write(DELTA, "[spira]\nk = 2\n");
        let ok = folded_tree(&t, FOLD_ST, BASE, FRAG, &[("F", FOLD_ST, "[spira]\nk = 2\n", BASE)]);
        assert!(ConfigDeltaFolded.check(&ok).unwrap().is_empty());
        let bad = folded_tree(&t, FOLD_ST, BASE, FRAG, &[("F", FOLD_ST, "[spira]\nk = 3\n", BASE)]);
        assert_eq!(ConfigDeltaFolded.check(&bad).unwrap().len(), 1);
    }

    #[test_id(tcy5csv)]
    #[test]
    fn a_round_fold_of_a_fragment_its_own_member_added_gets_none() {
        let t = TempDir::new("folded-member");
        t.write(DELTA, AB);
        let tree = folded_tree(&t, "M\tspira/config-delta.toml\n", A, FRAG_A, &[("F", FOLD_ST, AB, A)]);
        assert!(ConfigDeltaFolded.check(&tree).unwrap().is_empty());
    }

    #[test_id(t0yaxm1)]
    #[test]
    fn a_hand_edit_after_a_round_fold_is_still_refused() {
        let t = TempDir::new("folded-hand");
        const EDIT: &str = "removed = [\"spira.a\", \"spira.b\", \"spira.c\"]\n";
        t.write(DELTA, EDIT);
        let tree = folded_tree(&t, "M\tspira/config-delta.toml\n", A, FRAG_A, &[("F", FOLD_ST, AB, A), ("H", "M\tspira/config-delta.toml\n", EDIT, AB)]);
        assert_eq!(ConfigDeltaFolded.check(&tree).unwrap().len(), 1);
    }

    #[test_id(tccj3hc)]
    #[test]
    fn a_fold_that_drops_an_entry_is_refused() {
        let t = TempDir::new("folded-drop");
        t.write(DELTA, A);
        let tree = folded_tree(&t, FOLD_ST, A, FRAG_A, &[("F", FOLD_ST, A, A)]);
        assert_eq!(ConfigDeltaFolded.check(&tree).unwrap().len(), 1, "a fold that drops the fragment's entry is an edit");
    }

    #[test_id(t1yefca)]
    #[test]
    fn a_commit_adding_a_fragment_beside_the_fold_is_not_a_fold() {
        let t = TempDir::new("folded-extra");
        t.write(DELTA, AB);
        const EXTRA_ST: &str = "M\tspira/config-delta.toml\nD\tspira/config-delta.d/sp-a.toml\nA\tspira/config-delta.d/sp-b.toml\n";
        let tree = folded_tree(&t, "M\tspira/config-delta.toml\n", A, FRAG_A, &[("F", EXTRA_ST, AB, A)]);
        assert_eq!(ConfigDeltaFolded.check(&tree).unwrap().len(), 1);
    }

    #[test_id(tapbtig)]
    #[test]
    fn a_worktree_edit_on_top_of_an_exact_fold_is_refused() {
        let t = TempDir::new("folded-worktree");
        t.write(DELTA, "removed = [\"spira.a\", \"spira.b\", \"spira.z\"]\n");
        let tree = folded_tree(&t, "M\tspira/config-delta.toml\n", A, FRAG_A, &[("F", FOLD_ST, AB, A)]);
        assert_eq!(ConfigDeltaFolded.check(&tree).unwrap().len(), 1);
    }

    #[test_id(tapt6c1)]
    #[test]
    fn a_branch_adding_only_a_fragment_gets_none() {
        let t = TempDir::new("folded-frag");
        let tree = folded_tree(&t, "A\tspira/config-delta.d/sp-a.toml\n", "", &[], &[]);
        assert!(ConfigDeltaFolded.check(&tree).unwrap().is_empty());
    }
}
