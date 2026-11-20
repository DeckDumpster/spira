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

#[test]
fn an_em_dash_before_an_article_does_not_stop_the_plan() {
    let book = store(&[("law-e", "One rule—an aeon per worktree.")]); // ffx-ok
    let got = plan(&book, &[], &Pointers::default());
    assert_eq!(got, vec![Entry::Amend { key: "law-e".into(), text: "One rule—a dwarf per worktree.".into() }]); // ffx-ok
}

#[test]
fn a_slug_article_and_a_bare_summons_are_planned_by_grammar() {
    let book = store(&[("law-an-aeon-x", "A summons."), ("law-w", "summons reopened")]); // ffx-ok
    let got = plan(&book, &[], &Pointers::default());
    assert_eq!(
        got,
        vec![
            Entry::Rename { from: "law-an-aeon-x".into(), to: "law-a-dwarf-x".into(), text: "A call.".into(), numbered: false }, // ffx-ok
            Entry::Unresolved { key: "law-w".into(), terms: vec!["summons".into()] }, // ffx-ok
        ]
    );
}

#[test]
fn the_dry_run_prints_only_the_changed_sentences_old_then_new_and_a_rename_as_from_to() {
    let before = store(&[("law-a", "Keep this. An aeon acts."), ("law-aeon-b", "Same.")]); // ffx-ok
    let got = plan(&before, &[], &Pointers::default());
    let report = rule::vocab::dry_run_report(&before, &got);
    assert_eq!(report, "law-a\n- An aeon acts.\n+ A dwarf acts.\n\nlaw-aeon-b → law-dwarf-b\n\n"); // ffx-ok
}

#[test]
fn every_rename_leaves_a_pointer_and_an_unnumbered_old_slug_is_numbered_first() {
    use rule::code::{catch_up, check_pointers, resolve};
    use rule::vocab::record_renames;
    let toc = parse_toc(TOC).unwrap();
    let renamed = [("law-an-aeon-x".to_string(), "law-a-dwarf-x".to_string(), false)]; // ffx-ok
    let (toc, pointers) = record_renames(&toc, &Pointers::default(), &renamed, "d").unwrap();
    let book = store(&[("law-a-dwarf-x", "X.")]);
    let toc = catch_up(&toc, &pointers, &book, None).unwrap();
    assert!(check_pointers(&toc, &pointers).is_empty());
    let r = resolve(&toc, &pointers, "law-an-aeon-x").unwrap(); // ffx-ok
    assert_eq!(r.slug, "law-a-dwarf-x");
    assert_eq!(r.hops.len(), 1, "{:?}", r.hops);

    let numbered = [("law-y".to_string(), "law-z".to_string(), true)];
    let (toc2, p2) = record_renames(&parse_toc(TOC).unwrap(), &Pointers::default(), &numbered, "d").unwrap();
    assert_eq!(toc2.iter().flat_map(|p| &p.slugs).filter(|s| *s == "law-y").count(), 1);
    assert_eq!(p2.retired.len(), 1);
    let toc2 = catch_up(&toc2, &p2, &store(&[("law-z", "Z.")]), None).unwrap();
    assert!(check_pointers(&toc2, &p2).is_empty());
    assert_eq!(resolve(&toc2, &p2, "law-y").unwrap().hops.len(), 1);
}
