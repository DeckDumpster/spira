use super::*;

fn diff(id: &str, paths: &[&str], added: &str) -> MemberDiff {
    MemberDiff { id: id.into(), paths: paths.iter().map(|p| p.to_string()).collect(), added: added.into() }
}

const E0063: &str = "   Compiling spira-lc v0.1.0\nerror[E0063]: missing field `directed` in initializer of `BeadEventKind`\n   --> spira-lc/src/callers.rs:726:13\n    |\n726 |             BeadEventKind::Claim {\n";

#[test]
fn compiler_errors_carry_their_file_line_and_message() {
    let e = compile_errors(E0063);
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].file.as_str(), e[0].line), ("spira-lc/src/callers.rs", 726));
    assert!(e[0].message.contains("missing field `directed`"));
    assert!(compile_errors("warning: unused variable: `x`\n --> a/src/b.rs:1:1\n").is_empty(), "a warning is not a build red");
}

#[test]
fn an_error_at_a_file_only_one_member_touches_ejects_that_member() {
    let members = vec![diff("b", &["spira-lc/src/callers.rs"], "+    let x = 1;\n"), diff("a", &["doc/x.md"], "+prose\n")];
    let acts = attribute(&compile_errors(E0063), &members);
    assert_eq!(acts.len(), 1, "{acts:?}");
    let Action::Eject { id, line } = &acts[0] else { panic!("{acts:?}") };
    assert_eq!(id, "b");
    assert!(line.contains("callers.rs:726") && line.contains("E0063"), "{line}");
}

#[test]
fn an_error_naming_what_another_member_adds_sequences_the_caller_behind_it() {
    let members = vec![
        diff("a", &["spira-lc/src/events.rs"], "+pub struct BeadEventKind {\n+    directed: bool,\n"),
        diff("b", &["spira-lc/src/callers.rs"], "+            BeadEventKind::Claim {\n+                directed: false,\n"),
    ];
    let acts = attribute(&compile_errors(E0063), &members);
    assert_eq!(acts, vec![Action::Sequence { id: "b".into(), behind: vec!["a".into()], line: acts_line(&acts) }]);
}

fn acts_line(a: &[Action]) -> String {
    match &a[0] {
        Action::Sequence { line, .. } | Action::Eject { line, .. } => line.clone(),
    }
}

#[test]
fn a_shared_file_with_one_member_defining_the_missing_method_sequences_the_other() {
    let err = "error[E0599]: no method named `reclaim` found for struct `Pool` in the current scope\n --> q/src/pool.rs:40:10\n";
    let members = vec![diff("a", &["q/src/pool.rs"], "+    pub fn reclaim(&self) {}\n"), diff("b", &["q/src/pool.rs"], "+    p.reclaim();\n")];
    let acts = attribute(&compile_errors(err), &members);
    assert!(matches!(&acts[..], [Action::Sequence { id, behind, .. }] if id == "b" && behind == &vec!["a".to_string()]), "{acts:?}");
}

#[test]
fn an_error_no_member_touches_or_two_touch_with_no_provider_is_unmapped() {
    let members = vec![diff("a", &["x/src/a.rs"], "+1\n"), diff("b", &["x/src/a.rs"], "+2\n")];
    assert!(attribute(&compile_errors("error[E0425]: cannot find value `q` in this scope\n --> x/src/a.rs:3:1\n"), &members).is_empty());
    assert!(attribute(&compile_errors("error[E0425]: cannot find value `q` in this scope\n --> y/src/gone.rs:3:1\n"), &members).is_empty());
    assert!(attribute(&[], &members).is_empty(), "no parsed error is no verdict");
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
    assert!(defines("+    pub(crate) directed: Option<String>,", "directed"));
    assert!(defines("+pub fn directed(&self)", "directed"));
    assert!(!defines("+                directed: false,", "directed"));
    assert!(!defines("+                directed: String::new(),", "directed"));
    assert!(!defines("+    x.directed();", "directed"));
}
