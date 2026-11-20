//! `rename-vocabulary`'s plan over a statute store.

use std::collections::BTreeMap;

use rule::code::{parse_toc, Pointers, Retired};
use rule::vocab::{plan, Entry};

const TOC: &str = "[[part]]\nnumber = 1\nname = \"A\"\nslug = \"a\"\nscope = \"s\"\nslugs = [\"law-y\"]\n";

fn store(rows: &[(&str, &str)]) -> BTreeMap<String, String> {
    rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn an_unnumbered_slug_renames_a_text_term_amends_and_clean_or_other_family_keys_are_left() {
    let book = store(&[("law-one-aeon-one-worktree", "One worktree."), ("law-x", "summon it"), ("law-y", "Clean."), ("sop-z", "aeon")]); // ffx-ok
    let got = plan(&book, &parse_toc(TOC).unwrap(), &Pointers::default());
    assert_eq!(
        got,
        vec![
            Entry::Rename { from: "law-one-aeon-one-worktree".into(), to: "law-one-dwarf-one-worktree".into(), text: "One worktree.".into(), numbered: false }, // ffx-ok
            Entry::Amend { key: "law-x".into(), text: "call it".into() },
        ]
    );
}

#[test]
fn a_renamed_slug_in_the_toc_or_a_pointer_is_numbered() {
    let book = store(&[("law-aeon-a", "A."), ("law-aeon-b", "B.")]); // ffx-ok
    let toc = parse_toc(&TOC.replace("law-y", "law-aeon-a")).unwrap(); // ffx-ok
    let pointers = Pointers { retired: vec![Retired { slug: "law-aeon-b".into(), reason: "r".into(), replaced_by: None, on: "d".into() }], ..Pointers::default() }; // ffx-ok
    let got = plan(&book, &toc, &pointers);
    assert!(got.iter().all(|e| matches!(e, Entry::Rename { numbered: true, .. })), "{got:?}");
    assert_eq!(got.len(), 2);
}

#[test]
fn a_term_without_replacement_is_unresolved_and_a_taken_target_is_a_collision() {
    let book = store(&[("law-m", "A magus."), ("law-aeon-q", "Q."), ("law-dwarf-q", "Other.")]); // ffx-ok
    let got = plan(&book, &[], &Pointers::default());
    assert_eq!(
        got,
        vec![
            Entry::Collision { from: "law-aeon-q".into(), to: "law-dwarf-q".into() }, // ffx-ok
            Entry::Unresolved { key: "law-m".into(), terms: vec!["magus".into()] }, // ffx-ok
        ]
    );
}
