//! The selector's test-plan cells (`docs/test-plan/test-infrastructure.toml`, 01 and 03–07),
//! driven through the same entry points the CLI and testenv call, over an in-memory git.

use crate::corpus::{Corpus, Suite};
use crate::header;
use crate::io::{select_diff, Git};
use crate::select::{self, parse_name_status, report_text, unclaimed_tracked, Buckets, Fail, Mode, Options, Selection};
use crate::{refuse, Refusal};
use std::path::Path;

struct FakeGit {
    name_status: String,
    unified0: String,
    head_text: String,
}

impl FakeGit {
    fn diff(name_status: &str) -> FakeGit {
        FakeGit { name_status: name_status.into(), unified0: String::new(), head_text: String::new() }
    }
}

impl Git for FakeGit {
    fn diff_name_status(&self, _: &Path, _: &str, _: &str) -> Result<String, Refusal> {
        Ok(self.name_status.clone())
    }
    fn diff_raw(&self, _: &Path, _: &str, _: &str) -> Result<String, Refusal> {
        Ok(String::new())
    }
    fn diff_u0(&self, _: &Path, _: &str, _: &str, _: &str) -> Option<String> {
        (!self.unified0.is_empty()).then(|| self.unified0.clone())
    }
    fn show(&self, _: &Path, _: &str, _: &str) -> Option<String> {
        (!self.head_text.is_empty()).then(|| self.head_text.clone())
    }
    fn ls_tree(&self, _: &Path, _: &str) -> Result<Vec<String>, Refusal> {
        refuse("not used")
    }
    fn ls_files(&self, _: &Path) -> Result<Vec<String>, Refusal> {
        refuse("not used")
    }
}

fn corpus(suites: &[(&str, &str)]) -> Corpus {
    Corpus::new(suites.iter().map(|(n, h)| Suite::parse(n, h).unwrap()).collect())
}

fn opts(no_all_fallback: bool) -> Options {
    Options { no_all_fallback, ..Options::default() }
}

fn diff_select(g: &FakeGit, c: &Corpus, o: &Options) -> Result<Selection, Fail> {
    select_diff(g, Path::new("."), c, "base", "head", &Buckets::default(), o)
}

fn files_select(list: &str, c: &Corpus, o: &Options) -> Result<Selection, Fail> {
    select::select(c, &parse_name_status(list), &mut |_| vec![], &Buckets::default(), o)
}

fn names(s: Result<Selection, Fail>) -> Vec<String> {
    let mut v = s.unwrap().suites;
    v.sort();
    v
}

// covers: UC-test-infrastructure-01
#[test_id(te0zfyw)]
#[test]
fn the_header_parser_reads_every_declaration_and_nothing_from_an_undeclared_file() {
    let h = "#!/usr/bin/env bash\n# tier: T2\n# covers: spira/one.sh spira/two.sh\n# covers: spira/ignored.sh\n\
             # requires: testenv, podman\n# exclusive: yes\n# selects-on: added,mode\nset -uo pipefail\n";
    assert_eq!(header::covers_of(h).unwrap(), ["spira/one.sh", "spira/two.sh"]);
    assert_eq!(header::tier_of(h).as_deref(), Some("T2"));
    assert_eq!(header::requires_of(h), ["testenv", "podman"]);
    assert_eq!(header::requires_of("# requires: testenv podman\n"), ["testenv", "podman"]);
    assert_eq!(header::exclusive_of(h).as_deref(), Some("yes"));
    assert_eq!(header::selects_on_of(h), ["added", "mode"]);
    // No declaration, or a file that could not be read (empty text): everything is empty.
    for bare in ["", "#!/usr/bin/env bash\nset -uo pipefail\n"] {
        assert_eq!(header::covers_of(bare), None);
        assert_eq!(header::tier_of(bare), None);
        assert!(header::requires_of(bare).is_empty());
        assert_eq!(header::exclusive_of(bare), None);
        assert!(header::selects_on_of(bare).is_empty());
    }
}

fn plan_corpus() -> Corpus {
    corpus(&[
        ("test-a.sh", "# tier: T1\n# covers: spira/alpha.sh\n"),
        ("test-s.sh", "# tier: T1\n# covers: spira/alpha.sh\n# selects-on: added,mode\n"),
        ("test-n.sh", "# tier: T1\n"),
        ("test-b.sh", "# tier: T1\n# covers: spira/beta.sh\n"),
    ])
}

// covers: UC-test-infrastructure-03
#[test_id(the43be)]
#[test]
fn selection_over_a_changed_set_follows_covers_fallback_inert_and_selects_on() {
    let c = plan_corpus();
    let plain = opts(false);
    assert_eq!(names(files_select("spira/alpha.sh\n", &c, &plain)), ["test-a.sh", "test-n.sh"]);
    let unmapped = files_select("Makefile\n", &c, &plain).unwrap();
    assert_eq!((unmapped.mode, unmapped.suites.len()), (Mode::All, 4));
    let held = files_select("Makefile\n", &c, &opts(true)).unwrap();
    assert_eq!((held.mode, held.suites), (Mode::Diff, vec!["test-n.sh".to_string()]));
    assert_eq!(names(files_select("", &c, &plain)), ["test-n.sh"]);
    assert_eq!(names(files_select("notes.md\n", &c, &plain)), ["test-n.sh"]);
    assert!(!names(files_select("spira/alpha.sh\n", &c, &plain)).contains(&"test-s.sh".to_string()));
    assert!(names(files_select("A\tspira/alpha.sh\n", &c, &plain)).contains(&"test-s.sh".to_string()));
    let mut mode = parse_name_status("spira/alpha.sh\n");
    mode[0].mode_changed = true;
    let fired = select::select(&c, &mode, &mut |_| vec![], &Buckets::default(), &plain).unwrap();
    assert!(fired.suites.contains(&"test-s.sh".to_string()));
    // `--base/--head` and `--files` take the same path to the same answer.
    let g = FakeGit::diff("M\tspira/alpha.sh\n");
    assert_eq!(names(diff_select(&g, &c, &plain)), names(files_select("spira/alpha.sh\n", &c, &plain)));
}

fn funcs_corpus() -> Corpus {
    corpus(&[
        ("test-fa.sh", "# covers: spira/funcs.sh#fa\n"),
        ("test-fb.sh", "# covers: spira/funcs.sh#fb\n"),
        ("test-funcs.sh", "# covers: spira/funcs.sh\n"),
        ("test-n.sh", "# tier: T1\n"),
    ])
}

// covers: UC-test-infrastructure-04
#[test_id(tuw0t6p)]
#[test]
fn a_function_cover_narrows_to_its_hunk_and_a_global_hunk_selects_every_claimant() {
    let c = funcs_corpus();
    let head = "fa() {\n    echo A\n}\nfb() {\n    echo b\n}\ntop=1\n";
    let mut g = FakeGit::diff("M\tspira/funcs.sh\n");
    g.head_text = head.into();
    g.unified0 = "@@ -2 +2 @@\n-    echo a\n+    echo A\n".into();
    assert_eq!(names(diff_select(&g, &c, &opts(true))), ["test-fa.sh", "test-funcs.sh", "test-n.sh"]);
    g.unified0 = "@@ -7 +7 @@\n-top=0\n+top=1\n".into();
    assert_eq!(names(diff_select(&g, &c, &opts(true))), ["test-fa.sh", "test-fb.sh", "test-funcs.sh", "test-n.sh"]);
    // A plain file list carries no hunks, so nothing narrows.
    assert_eq!(names(files_select("spira/funcs.sh\n", &c, &opts(true))), ["test-fa.sh", "test-fb.sh", "test-funcs.sh", "test-n.sh"]);
}

// covers: UC-test-infrastructure-05
#[test_id(tepe7pc)]
#[test]
fn an_unclaimed_source_file_fails_selection_and_the_report_lists_unclaimed_and_unplaced() {
    let c = corpus(&[("test-b.sh", "# covers: spira/beta.sh\n")]);
    match files_select("spira/gamma.sh\nMakefile\n", &c, &opts(false)) {
        Err(Fail::Unclaimed { files, unplaced }) => {
            assert_eq!(files, ["spira/gamma.sh"]);
            assert_eq!(unplaced, ["Makefile"]);
            let tracked = ["spira/beta.sh".to_string(), "spira/gamma.sh".to_string()];
            let report = report_text(&unplaced, &unclaimed_tracked(&c, &tracked));
            assert_eq!(report, "unplaced:Makefile\nunclaimed:spira/gamma.sh\n");
        }
        other => panic!("an unclaimed source file must fail selection, got {other:?}"),
    }
    assert!(files_select("spira/beta.sh\n", &c, &opts(false)).is_ok());
}

// covers: UC-test-infrastructure-06
#[test_id(ttumduv)]
#[test]
fn a_branch_diff_selects_the_union_of_its_members_and_falls_back_to_all_on_an_unmapped_one() {
    let c = corpus(&[
        ("test-a.sh", "# covers: spira/alpha.sh\n"),
        ("test-b.sh", "# covers: spira/beta.sh\n"),
        ("test-n.sh", "# tier: T1\n"),
        ("test-x.sh", "# covers: spira/other.sh\n"),
    ]);
    let union = FakeGit::diff("M\tspira/beta.sh\nM\tspira/alpha.sh\n");
    let got = diff_select(&union, &c, &opts(true)).unwrap();
    assert_eq!(got.mode, Mode::Diff);
    assert_eq!(names(Ok(got)), ["test-a.sh", "test-b.sh", "test-n.sh"]);
    let unmapped = FakeGit::diff("M\tspira/beta.sh\nA\tMakefile\n");
    let got = diff_select(&unmapped, &c, &opts(false)).unwrap();
    assert_eq!((got.mode, got.suites.len()), (Mode::All, 4));
}

// covers: UC-test-infrastructure-07
#[test_id(tvbtyza)]
#[test]
fn every_consumer_links_the_suite_select_crate_and_no_private_selector_remains() {
    let root = spira_config::resolve::checkout_root_for_process().unwrap();
    let links = |manifest: &str| {
        manifest.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("suite-select") && l.contains("path") && l.contains("\"../suite-select\"")
        })
    };
    // The matcher is shown able to refuse before its silence is believed.
    assert!(links("[dependencies]\nsuite-select = { path = \"../suite-select\" }\n"));
    assert!(!links("[dependencies]\nserde = \"1\"\n"));
    for krate in ["testenv", "batcher-cut", "gate"] {
        let manifest = std::fs::read_to_string(root.join(krate).join("Cargo.toml")).unwrap();
        assert!(links(&manifest), "{krate} must select through the suite-select crate");
    }
    let gate_yml = std::fs::read_to_string(root.join(".github/workflows/gate.yml")).unwrap();
    assert!(gate_yml.contains("cargo build --release -p suite-select"), "gate.yml must build and select with suite-select");
    for private in ["select.sh", "gate-touched.sh", "gate-budget-select.sh", "gate-spira.sh", "suite-select.sh"] {
        assert!(!root.join("spira").join(private).exists(), "private selector spira/{private} is back");
    }
}
