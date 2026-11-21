//! The one merge verdict for a tip against a base. The submit and the dwarf's exit check both
//! ask it, so they cannot disagree on the same tip and base.

#[derive(Debug, PartialEq)]
pub enum Merge {
    Clean,
    Conflict { paths: Vec<String>, text: String },
    Unknown(String),
}

/// Whether `tip` merges onto `base_fq` without a content conflict. `git` runs one git command in
/// the repository and returns its exit code and output.
pub fn merge_onto(git: &dyn Fn(&[&str]) -> (i32, String), base_fq: &str, tip: &str) -> Merge {
    let attr = format!("attr.tree={base_fq}");
    match git(&["-c", &attr, "merge-tree", "--write-tree", base_fq, tip]) {
        (0, _) => Merge::Clean,
        (1, text) => Merge::Conflict { paths: conflict_paths(&text), text },
        (_, out) => Merge::Unknown(out),
    }
}

/// The paths named by the `CONFLICT` lines of a merge-tree output.
pub fn conflict_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.starts_with("CONFLICT"))
        .filter_map(|l| {
            if let Some((_, p)) = l.split_once("Merge conflict in ") {
                return Some(p.trim());
            }
            let rest = l.split_once("): ")?.1;
            rest.split_once(" deleted in ").or_else(|| rest.split_once(" renamed ")).map(|(p, _)| p.trim())
        })
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn run(code: i32, out: &str) -> (Merge, Vec<String>) {
        let argv = RefCell::new(Vec::new());
        let m = merge_onto(
            &|a: &[&str]| {
                *argv.borrow_mut() = a.iter().map(|s| s.to_string()).collect();
                (code, out.to_string())
            },
            "base1",
            "tip1",
        );
        (m, argv.into_inner())
    }

    #[test_id(tw0ia8c)]
    #[test]
    fn exit_one_is_a_conflict_naming_its_paths() {
        let out = "t\n\nAuto-merging x\nCONFLICT (content): Merge conflict in state-store-sqlite/src/lib.rs\n";
        let (m, _) = run(1, out);
        assert_eq!(m, Merge::Conflict { paths: vec!["state-store-sqlite/src/lib.rs".into()], text: out.into() });
    }

    #[test_id(ta960zb)]
    #[test]
    fn exit_zero_is_clean() {
        assert_eq!(run(0, "tree\n").0, Merge::Clean);
    }

    #[test_id(tk2sknx)]
    #[test]
    fn any_other_exit_is_unknown() {
        assert_eq!(run(128, "fatal: bad\n").0, Merge::Unknown("fatal: bad\n".into()));
    }

    #[test_id(t1goa3d)]
    #[test]
    fn the_argv_is_exactly_the_merge_tree_write_tree_form() {
        let (_, argv) = run(0, "");
        assert_eq!(argv, ["-c", "attr.tree=base1", "merge-tree", "--write-tree", "base1", "tip1"]);
    }

    #[test_id(tju01nz)]
    #[test]
    fn conflict_paths_are_read_from_conflict_lines_only() {
        let text = "Auto-merging x\nCONFLICT (content): Merge conflict in deploy/ci.sh\nCONFLICT (add/add): Merge conflict in a/__init__.py\nCONFLICT (modify/delete): gone.rs deleted in HEAD and modified in b.\nnot a CONFLICT line\n";
        assert_eq!(conflict_paths(text), ["deploy/ci.sh", "a/__init__.py", "gone.rs"]);
        assert!(conflict_paths("clean\n").is_empty());
    }

    #[test_id(tg7c2nr)]
    #[test]
    fn two_hundred_conflicting_paths_are_all_read() {
        let text: String = (0..200).map(|i| format!("CONFLICT (content): Merge conflict in f{i}.rs\n")).collect();
        assert_eq!(conflict_paths(&text).len(), 200);
    }
}
