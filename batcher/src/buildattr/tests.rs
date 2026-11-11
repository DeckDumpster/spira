use super::*;

fn diff(id: &str, paths: &[&str], added: &str) -> MemberDiff {
    MemberDiff {
        id: id.into(),
        paths: paths.iter().map(|p| p.to_string()).collect(),
        added: added.into(),
    }
}

const E0063: &str = "   Compiling spira-lc v0.1.0\nerror[E0063]: missing field `directed` in initializer of `BeadEventKind`\n   --> spira-lc/src/callers.rs:726:13\n    |\n726 |             BeadEventKind::Claim {\n";

#[test]
fn compiler_errors_carry_their_file_line_and_message() {
    let e = compile_errors(E0063);
    assert_eq!(e.len(), 1);
    assert_eq!(
        (e[0].file.as_str(), e[0].line),
        ("spira-lc/src/callers.rs", 726)
    );
    assert!(e[0].message.contains("missing field `directed`"));
    assert!(
        compile_errors("warning: unused variable: `x`\n --> a/src/b.rs:1:1\n").is_empty(),
        "a warning is not a build red"
    );
}

#[test]
fn an_error_at_a_file_only_one_member_touches_ejects_that_member() {
    let members = vec![
        diff("b", &["spira-lc/src/callers.rs"], "+    let x = 1;\n"),
        diff("a", &["doc/x.md"], "+prose\n"),
    ];
    let acts = attribute(&compile_errors(E0063), &members);
    assert_eq!(acts.len(), 1, "{acts:?}");
    let Action::Eject { id, line } = &acts[0] else {
        panic!("{acts:?}")
    };
    assert_eq!(id, "b");
    assert!(
        line.contains("callers.rs:726") && line.contains("E0063"),
        "{line}"
    );
}

#[test]
fn an_error_naming_what_another_member_adds_sequences_the_caller_behind_it() {
    let members = vec![
        diff(
            "a",
            &["spira-lc/src/events.rs"],
            "+pub struct BeadEventKind {\n+    directed: bool,\n",
        ),
        diff(
            "b",
            &["spira-lc/src/callers.rs"],
            "+            BeadEventKind::Claim {\n+                directed: false,\n",
        ),
    ];
    let acts = attribute(&compile_errors(E0063), &members);
    assert_eq!(
        acts,
        vec![Action::Sequence {
            id: "b".into(),
            behind: vec!["a".into()],
            line: acts_line(&acts)
        }]
    );
}

fn acts_line(a: &[Action]) -> String {
    match &a[0] {
        Action::Sequence { line, .. } | Action::Eject { line, .. } => line.clone(),
    }
}

#[test]
fn a_shared_file_with_one_member_defining_the_missing_method_sequences_the_other() {
    let err = "error[E0599]: no method named `reclaim` found for struct `Pool` in the current scope\n --> q/src/pool.rs:40:10\n";
    let members = vec![
        diff("a", &["q/src/pool.rs"], "+    pub fn reclaim(&self) {}\n"),
        diff("b", &["q/src/pool.rs"], "+    p.reclaim();\n"),
    ];
    let acts = attribute(&compile_errors(err), &members);
    assert!(
        matches!(&acts[..], [Action::Sequence { id, behind, .. }] if id == "b" && behind == &vec!["a".to_string()]),
        "{acts:?}"
    );
}

#[test]
fn an_error_no_member_touches_or_two_touch_with_no_provider_is_unmapped() {
    let members = vec![
        diff("a", &["x/src/a.rs"], "+1\n"),
        diff("b", &["x/src/a.rs"], "+2\n"),
    ];
    assert!(attribute(
        &compile_errors("error[E0425]: cannot find value `q` in this scope\n --> x/src/a.rs:3:1\n"),
        &members
    )
    .is_empty());
    assert!(attribute(
        &compile_errors(
            "error[E0425]: cannot find value `q` in this scope\n --> y/src/gone.rs:3:1\n"
        ),
        &members
    )
    .is_empty());
    assert!(
        attribute(&[], &members).is_empty(),
        "no parsed error is no verdict"
    );
}

#[test]
fn one_unmapped_error_makes_the_whole_verdict_unmapped() {
    let members = vec![diff("a", &["x/src/a.rs"], "+1\n")];
    let text = "error[E0425]: cannot find value `q`\n --> x/src/a.rs:3:1\nerror[E0425]: cannot find value `r`\n --> base/src/z.rs:1:1\n";
    assert!(attribute(&compile_errors(text), &members).is_empty());
}

#[test]
fn an_initializer_is_not_a_definition_of_the_field_it_sets() {
    assert!(defines("+    directed: bool,", "directed"));
    assert!(defines(
        "+    pub(crate) directed: Option<String>,",
        "directed"
    ));
    assert!(defines("+pub fn directed(&self)", "directed"));
    assert!(!defines("+                directed: false,", "directed"));
    assert!(!defines(
        "+                directed: String::new(),",
        "directed"
    ));
    assert!(!defines("+    x.directed();", "directed"));
}

#[test]
fn a_member_defining_a_same_named_field_in_another_crate_is_no_provider() {
    let members = vec![
        diff("a", &["other-crate/src/s.rs"], "+    directed: bool,\n"),
        diff("b", &["spira-lc/src/callers.rs"], "+    x\n"),
    ];
    let acts = attribute(&compile_errors(E0063), &members);
    assert!(
        matches!(&acts[..], [Action::Eject { id, .. }] if id == "b"),
        "{acts:?}"
    );
}

#[test]
fn several_errors_in_real_compiler_output_each_map_and_a_bare_summary_line_is_skipped() {
    let text = "error[E0599]: no method named `x` found\n  --> a/src/one.rs:3:9\n   |\nerror[E0308]: mismatched types\n  --> b/src/two.rs:8:5\n\nerror: could not compile `a` (lib) due to 2 previous errors\n";
    let e = compile_errors(text);
    assert_eq!(
        e.iter()
            .map(|e| (e.file.as_str(), e.line))
            .collect::<Vec<_>>(),
        vec![("a/src/one.rs", 3), ("b/src/two.rs", 8)]
    );
    let members = vec![
        diff("p", &["a/src/one.rs"], "+1\n"),
        diff("q", &["b/src/two.rs"], "+2\n"),
    ];
    assert_eq!(attribute(&e, &members).len(), 2);
}

#[test]
fn the_recipe_paths_are_the_ones_the_image_workflow_builds_on() {
    let yml = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../.github/workflows/round-ami-build.yml"
    ))
    .unwrap();
    let mut triggers: Vec<&str> = yml
        .lines()
        .skip_while(|l| !l.contains("paths:"))
        .skip(1)
        .map_while(|l| l.trim().strip_prefix("- "))
        .collect();
    let mut ours = super::RECIPE_PATHS.to_vec();
    triggers.sort_unstable();
    ours.sort_unstable();
    assert_eq!(ours, triggers);
}

#[test]
fn only_a_signal_exit_of_the_runner_is_a_signal_death() {
    assert!(is_signal_death(
        "round-vm: PHASE-FAILED phase=suites rc=134"
    ));
    assert!(is_signal_death(
        "the remote runner exited 139, outside testenv's contract"
    ));
    assert!(!is_signal_death("round-vm: PHASE-FAILED phase=suites rc=2"));
    assert!(!is_signal_death(
        "round-vm: PHASE-FAILED phase=build rc=134"
    ));
    assert!(!is_signal_death("round-vm: harness fault — over-cap: exceeded the 600s wall bound (OVER-CAP) PHASE-FAILED phase=suites rc=137"));
    assert!(!is_signal_death(
        "the remote runner exited 101, outside testenv's contract"
    ));
}

#[test]
fn runner_touchers_match_the_directories_and_not_their_lookalikes() {
    let ds = [
        diff("a", &["round-vm/src/run.rs"], ""),
        diff("b", &["testenv/src/x.rs"], ""),
        diff("c", &["round-vm-extra/x"], ""),
        diff("d", &["doc/round-vm/x"], ""),
    ];
    let ids: Vec<&str> = runner_touchers(&ds).iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
}
