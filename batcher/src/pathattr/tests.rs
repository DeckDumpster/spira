use super::*;

fn m(id: &str, paths: &[&str]) -> MemberPaths {
    MemberPaths { id: id.into(), paths: paths.iter().map(|p| p.to_string()).collect() }
}

fn red(suite: &str, evidence: &str, own: &[&str]) -> Red {
    Red { suite: suite.into(), evidence: evidence.into(), own: own.iter().map(|g| g.to_string()).collect() }
}

fn round() -> Vec<MemberPaths> {
    vec![
        m("sp-2yclhz", &["sentinel/src/check4.rs", "sentinel/src/audit.rs", "sentinel/src/lib.rs"]),
        m("sp-73vuz3", &["systemd/spira-nix-gc.service", "systemd/spira-nix-gc.timer"]),
        m("sp-38qf5k", &["batcher-cut/src/pace.rs", "batcher-cut/src/io.rs"]),
        m("sp-other", &["doc/notes.md", "sentinel/src/lib.rs"]),
    ]
}

fn eject_ids(v: &Verdict) -> Vec<&str> {
    match v {
        Verdict::Eject(o) => o.iter().map(|(id, _)| id.as_str()).collect(),
        Verdict::Ambiguous(_) => vec![],
    }
}

#[test]
fn r_auto_58_fence_names_check4() {
    let r = red("fence: event-taxonomy", "event-taxonomy: sentinel/src/check4.rs: no call site emits bead.poisoned", &[]);
    assert_eq!(eject_ids(&attribute(&[r], &round())), ["sp-2yclhz"]);
}

#[test]
fn r_auto_58_recert_names_units() {
    let r = red("doctor", "world_planes.rs:39 units with no valid Plane=: [\"spira-nix-gc.service\", ...] and doctor spira-nix-gc-prod.service's ExecStart", &[]);
    assert_eq!(eject_ids(&attribute(&[r], &round())), ["sp-73vuz3"]);
}

#[test]
fn r_auto_59_two_reds_two_owners() {
    let a = red("test-wire-token", "grep: /w/sentinel/src/audit.rs: No such file", &[]);
    let b = red("test-batcher-cut-x", "wanted [PR 1 opened] in [no round cut: no trigger]", &["batcher-cut/"]);
    let v = attribute(&[a, b], &round());
    assert_eq!(eject_ids(&v), ["sp-2yclhz", "sp-38qf5k"]);
}

#[test]
fn a_red_naming_two_members_is_ambiguous_with_both_listed() {
    let r = red("s", "sentinel/src/audit.rs and batcher-cut/src/io.rs both failed", &[]);
    match attribute(&[r], &round()) {
        Verdict::Ambiguous(rows) => assert_eq!(rows[0].candidates.keys().collect::<Vec<_>>(), ["sp-2yclhz", "sp-38qf5k"]),
        v => panic!("{v:?}"),
    }
}

#[test]
fn a_red_naming_no_member_pauses() {
    let r = red("s", "assertion failed at unrelated/thing.rs:3", &["elsewhere/"]);
    assert!(matches!(attribute(&[r], &round()), Verdict::Ambiguous(_)));
}

#[test]
fn one_ambiguous_red_pauses_the_round_even_if_another_is_owned() {
    let a = red("a", "sentinel/src/check4.rs", &[]);
    let b = red("b", "nothing here", &[]);
    assert!(matches!(attribute(&[a, b], &round()), Verdict::Ambiguous(_)));
}

#[test]
fn no_reds_is_ambiguous() {
    assert!(matches!(attribute(&[], &round()), Verdict::Ambiguous(_)));
}

#[test]
fn generic_bare_basenames_implicate_nobody() {
    let r = red("s", "error in lib.rs:12", &[]);
    assert!(matches!(attribute(&[r], &round()), Verdict::Ambiguous(_)));
}

#[test]
fn evidence_outranks_own_paths() {
    let r = red("s", "sentinel/src/check4.rs missing", &["batcher-cut/"]);
    assert_eq!(eject_ids(&attribute(&[r], &round())), ["sp-2yclhz"]);
}

#[test]
fn parses_reds_file() {
    let t = "@@ a own=x/,y*.sh\nline 1\n@@ b\nline 2\n";
    let r = parse_reds(t).unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].own, ["x/", "y*.sh"]);
    assert_eq!(r[1].evidence, "line 2\n");
    assert!(parse_reds("stray\n").is_err());
}

#[test]
fn glob_matches() {
    assert!(glob("test-*.sh", "test-a.sh"));
    assert!(!glob("test-*.sh", "test-a.rs"));
    assert!(glob("a/", "a/b/c"));
    assert!(!glob("a/", "ab/c"));
}
