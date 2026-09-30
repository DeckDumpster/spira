//! The selection's behaviour, ported from `spira/test-select.sh` (parts A–Q, retired with
//! `select.sh`, sp-wx2tw). Each test names the part it replaces.

use super::*;
use crate::corpus::{Corpus, Suite};

fn suite(name: &str, header: &str) -> Suite {
    Suite::parse(name, header).unwrap()
}

fn corpus(suites: &[(&str, &str)]) -> Corpus {
    Corpus::new(suites.iter().map(|(n, h)| suite(n, h)).collect())
}

fn m(paths: &[&str]) -> Vec<Change> {
    paths.iter().map(|p| Change::modified(p)).collect()
}

fn no_fns(_: &str) -> Vec<String> {
    vec![]
}

fn run(c: &Corpus, ch: &[Change], o: &Options) -> Result<Selection, Fail> {
    select(c, ch, &mut no_fns, &Buckets::default(), o)
}

/// Suites A (covers covered.sh), B (covers other.sh), C (no covers: always runs),
/// D (covers Makefile, which is plumbing).
fn abcd() -> Corpus {
    corpus(&[
        ("test-fx-a.sh", "# covers: covered.sh\n"),
        ("test-fx-b.sh", "# covers: other.sh\n"),
        ("test-fx-c.sh", "#!/bin/bash\nset -u\n"),
        ("test-fx-d.sh", "# covers: Makefile\n"),
    ])
}

fn fallback() -> Options {
    Options::default()
}

fn nofallback() -> Options {
    Options {
        no_all_fallback: true,
        ..Options::default()
    }
}

#[test]
fn a_all_is_every_suite() {
    let s = all(&abcd(), &fallback());
    assert_eq!(s.suites, ["test-fx-a.sh", "test-fx-b.sh", "test-fx-c.sh", "test-fx-d.sh"]);
    assert_eq!(s.mode, Mode::All);
}

#[test]
fn b_a_covered_change_selects_its_suite_and_the_always_run_suites() {
    let s = run(&abcd(), &m(&["covered.sh"]), &fallback()).unwrap();
    assert_eq!(s.suites, ["test-fx-a.sh", "test-fx-c.sh"]);
    assert_eq!(s.mode, Mode::Diff);
    assert_eq!(s.log, ["select: 2 suite(s) selected"]);
}

#[test]
fn c_an_unmapped_change_falls_back_to_all_unless_told_not_to() {
    let s = run(&abcd(), &m(&["no-suite-owns-this.go"]), &fallback()).unwrap();
    assert_eq!(s.mode, Mode::All);
    assert_eq!(s.suites.len(), 4);
    assert!(s.log[0].contains("no-suite-owns-this.go → [all: unmapped]"));
    assert!(s.log.last().unwrap().contains("running all 4 suites"));
    // F: --no-all-fallback — only covered + always-run, mode diff, the file is unplaced.
    let s = run(&abcd(), &m(&["no-suite-owns-this.go"]), &nofallback()).unwrap();
    assert_eq!(s.suites, ["test-fx-c.sh"]);
    assert_eq!(s.mode, Mode::Diff);
    assert_eq!(s.unplaced, ["no-suite-owns-this.go"]);
}

#[test]
fn d_an_empty_diff_selects_only_the_always_run_suites() {
    let s = run(&abcd(), &[], &fallback()).unwrap();
    assert_eq!(s.suites, ["test-fx-c.sh"]);
    assert_eq!(s.mode, Mode::Diff);
    let o = Options {
        no_nocov: true,
        ..Options::default()
    };
    assert!(run(&abcd(), &[], &o).unwrap().suites.is_empty());
}

#[test]
fn h_the_report_names_unplaced_and_unclaimed_files() {
    let c = corpus(&[
        ("test-u.sh", "# covers: util.sh\n"),
        ("test-f.sh", "# covers: lib.sh#foo\n"),
    ]);
    let tracked: Vec<String> = ["util.sh", "orphan.sh", "lib.sh", "x.py", "README.md", "spira/test-u.sh"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // A function-level claim claims its file (the bash matched `lib.sh#foo` verbatim).
    assert_eq!(unclaimed_tracked(&c, &tracked), ["orphan.sh", "x.py"]);
    assert_eq!(
        report_text(&["a.go".into()], &["orphan.sh".into()]),
        "unplaced:a.go\nunclaimed:orphan.sh\n"
    );
}

#[test]
fn i_l_inert_files_select_nothing_and_never_fall_back() {
    let c = corpus(&[("test-fx-i.sh", "# covers: some.sh\n")]);
    let s = run(&c, &m(&["README.md", "CHANGES.txt"]), &fallback()).unwrap();
    assert!(s.suites.is_empty());
    // Positive control: with no inert bucket README.md is unmapped and falls back.
    let b = Buckets {
        inert: vec!["__none__".into()],
        ..Buckets::default()
    };
    let s = select(&c, &m(&["README.md"]), &mut no_fns, &b, &fallback()).unwrap();
    assert_eq!(s.suites, ["test-fx-i.sh"]);
    assert_eq!(s.mode, Mode::All);
    // L: docs-only → the always-run suite only.
    let c = corpus(&[("test-fx-i.sh", "# covers: some.sh\n"), ("test-nc.sh", "set -u\n")]);
    assert_eq!(run(&c, &m(&["docs/x.md"]), &fallback()).unwrap().suites, ["test-nc.sh"]);
}

#[test]
fn j_an_unclaimed_source_file_fails_naming_it_and_a_deleted_one_does_not() {
    let c = corpus(&[("test-o.sh", "# covers: other.sh\n")]);
    let e = run(&c, &m(&["spira/new.sh", "x.go"]), &nofallback()).unwrap_err();
    assert_eq!(
        e,
        Fail::Unclaimed {
            files: vec!["spira/new.sh".into()],
            unplaced: vec!["x.go".into()]
        }
    );
    // Claimed → selected.
    let c2 = corpus(&[("test-o.sh", "# covers: other.sh\n"), ("test-n.sh", "# covers: spira/new.sh\n")]);
    assert_eq!(run(&c2, &m(&["spira/new.sh"]), &nofallback()).unwrap().suites, ["test-n.sh"]);
    // Deleted → not the branch's fault; unplaced (and the fallback, when enabled).
    let del = parse_name_status("D\tspira/gone.sh\n");
    let s = run(&c, &del, &nofallback()).unwrap();
    assert_eq!(s.unplaced, ["spira/gone.sh"]);
    assert_eq!(run(&c, &del, &fallback()).unwrap().mode, Mode::All);
    // A deleted file still selects the suite that names it.
    let s = run(&c2, &parse_name_status("D\tspira/new.sh\n"), &nofallback()).unwrap();
    assert_eq!(s.suites, ["test-n.sh"]);
}

#[test]
fn m_function_level_covers_narrow_to_the_changed_function() {
    let c = corpus(&[
        ("test-ma.sh", "# covers: hub.sh#foo\n"),
        ("test-mb.sh", "# covers: hub.sh#bar\n"),
    ]);
    let mut foo = |_: &str| vec!["foo".to_string()];
    let s = select(&c, &m(&["hub.sh"]), &mut foo, &Buckets::default(), &nofallback()).unwrap();
    assert_eq!(s.suites, ["test-ma.sh"]);
    // A hunk outside any declared function: both (cannot narrow further).
    let mut other = |_: &str| vec!["baz".to_string()];
    let s = select(&c, &m(&["hub.sh"]), &mut other, &Buckets::default(), &nofallback()).unwrap();
    assert_eq!(s.suites, ["test-ma.sh", "test-mb.sh"]);
    // --files (no narrowing): both.
    assert_eq!(run(&c, &m(&["hub.sh"]), &nofallback()).unwrap().suites.len(), 2);
}

#[test]
fn m_changed_functions_maps_hunks_onto_function_blocks() {
    let head = "#!/bin/bash\nfoo() {\n  echo a\n}\nglobal=1\nbar() {\n  echo b\n}\n";
    // A hunk on line 3 (inside foo).
    assert_eq!(changed_functions("@@ -3 +3 @@\n-x\n+y\n", head), ["foo"]);
    // Line 7 (bar) and line 5 (global code: no function).
    assert_eq!(changed_functions("@@ -5,0 +5,1 @@\n@@ -7 +7,1 @@\n", head), ["bar"]);
    assert!(changed_functions("@@ -5 +5 @@\n", head).is_empty());
    // A pure deletion (count 0) names nothing; the def line counts; the brace does not.
    assert!(changed_functions("@@ -3 +2,0 @@\n", head).is_empty());
    assert_eq!(changed_functions("@@ -2 +2 @@\n", head), ["foo"]);
    assert!(changed_functions("@@ -4 +4 @@\n", head).is_empty());
}

#[test]
fn n_a_batch_diff_selects_the_union_and_falls_back_on_an_unmapped_member() {
    let c = corpus(&[
        ("test-alpha.sh", "# covers: alpha.sh\n"),
        ("test-beta.sh", "# covers: beta.sh\n"),
        ("test-other.sh", "# covers: other.sh\n"),
    ]);
    let s = run(&c, &m(&["alpha.sh", "beta.sh"]), &fallback()).unwrap();
    assert_eq!(s.suites, ["test-alpha.sh", "test-beta.sh"]);
    let s = run(&c, &m(&["alpha.sh", "mystery.go"]), &fallback()).unwrap();
    assert_eq!(s.mode, Mode::All);
    assert_eq!(s.suites.len(), 3);
}

#[test]
fn o_p_selects_on_fires_only_on_its_events() {
    let c = corpus(&[
        ("test-fx-o-inv.sh", "# covers: spira/*.sh\n# selects-on: added,mode\nset -u\n"),
        ("test-edit.sh", "# covers: spira/x.sh\n"),
    ]);
    // An added file matching its glob: fires (and the covers claim keeps it from being
    // unclaimed).
    let s = run(&c, &parse_name_status("A\tspira/new.sh\n"), &nofallback()).unwrap();
    assert_eq!(s.suites, ["test-fx-o-inv.sh"]);
    // A plain filename (status unknown → modified): does not fire, but is claimed.
    let s = run(&c, &m(&["spira/new.sh"]), &nofallback()).unwrap();
    assert!(s.suites.is_empty(), "{:?}", s.suites);
    // A mode change fires.
    let mut ch = m(&["spira/x.sh"]);
    ch[0].mode_changed = true;
    let s = run(&c, &ch, &nofallback()).unwrap();
    assert_eq!(s.suites, ["test-edit.sh", "test-fx-o-inv.sh"]);
    // An edit to an inert file does not.
    assert!(run(&c, &m(&["README.md"]), &nofallback()).unwrap().suites.is_empty());
}

#[test]
fn q_a_plumbing_file_falls_back_even_when_claimed() {
    let s = run(&abcd(), &m(&["Makefile"]), &fallback()).unwrap();
    assert_eq!(s.mode, Mode::All);
    assert!(s.log[0].contains("Makefile → [all: plumbing]"));
    // Positive control: without the plumbing bucket only its claimant runs.
    let b = Buckets {
        plumbing: vec!["__none__".into()],
        ..Buckets::default()
    };
    let s = select(&abcd(), &m(&["Makefile"]), &mut no_fns, &b, &fallback()).unwrap();
    assert_eq!(s.suites, ["test-fx-d.sh", "test-fx-c.sh"]);
    // Q3: --no-all-fallback suppresses it too.
    let s = run(&abcd(), &m(&["Makefile"]), &nofallback()).unwrap();
    assert_eq!(s.suites, ["test-fx-d.sh", "test-fx-c.sh"]);
}

#[test]
fn tiers_keep_listed_and_untiered_suites_everywhere() {
    let c = corpus(&[
        ("test-t1.sh", "# tier: T1\n# covers: a.sh\n"),
        ("test-t2.sh", "# tier: T2\n# covers: a.sh\n"),
        ("test-nt.sh", "# covers: a.sh\n"),
    ]);
    let o = Options {
        tiers: parse_tiers("T0,T1"),
        ..nofallback()
    };
    assert_eq!(run(&c, &m(&["a.sh"]), &o).unwrap().suites, ["test-nt.sh", "test-t1.sh"]);
    assert_eq!(all(&c, &o).suites, ["test-nt.sh", "test-t1.sh"]);
    assert_eq!(parse_tiers(""), None);
}

#[test]
fn name_status_and_raw_parsing() {
    let c = parse_name_status("M\ta.sh\nA\tb.sh\nD\tc.sh\nR100\told.sh\tnew.sh\nbare.sh\n\n");
    let p: Vec<&str> = c.iter().map(|x| x.path.as_str()).collect();
    assert_eq!(p, ["a.sh", "b.sh", "c.sh", "old.sh", "new.sh", "bare.sh"]);
    assert!(c[1].added && !c[0].added && c[2].deleted);
    assert!(!c[5].added && !c[5].deleted);
    // Paths with blanks stay one path.
    assert_eq!(parse_name_status("M\ta b.sh\n")[0].path, "a b.sh");
    let raw = ":100644 100755 abc def M\tspira/x.sh\n:000000 100644 000 abc A\tspira/n.sh\n:100644 100644 a b M\ty.sh\n";
    assert_eq!(parse_mode_changes(raw), ["spira/x.sh"]);
}

#[test]
fn buckets_read_the_environment_overrides() {
    let env = |k: &str| match k {
        "SPIRA_SELECT_INERT" => Some("*.go".to_string()),
        "SPIRA_SELECT_SOURCE" => Some(String::new()),
        _ => None,
    };
    let b = Buckets::from_env(&env);
    assert_eq!(b.inert, ["*.go"]);
    assert_eq!(b.source, ["spira/*.sh"], "empty means the default, as ${{VAR:-default}}");
    assert!(b.plumbing.contains(&"spira/lib.sh".to_string()));
}
