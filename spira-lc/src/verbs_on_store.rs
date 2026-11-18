//! With no control store configured, every verb that once read the dolt lifecycle database says
//! so; the same verbs against a real store are `tests/verbs_on_store.rs`.

fn run(args: &[&str]) -> (i32, String) {
    crate::dispatch(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
}

fn world() -> (testkit::TempDir, testkit::EnvGuard) {
    let dir = testkit::TempDir::new("spira-lc-verbs-on-store");
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_LC_STORE_PATH", "")]);
    let env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap()))]);
    (dir, env)
}

#[test]
fn with_no_control_store_configured_every_ported_verb_is_cannot_tell_and_never_a_dolt_read() {
    let (_dir, _env) = world();
    for verb in [&["fact", "add", "sp-a", "--kind", "claimed", "--actor", "a"][..], &["fact", "list"], &["facts-query", "SELECT 1 FROM events"], &["journal", "state"], &["ops-view", "ops_live"], &["ops-bead", "sp-a"], &["churn-facts", "--since", "0"], &["mend-watch"], &["event-continuity"]] {
        let (rc, out) = run(verb);
        assert_eq!(rc, 2, "{verb:?}: {out}");
        assert!(out.contains("cannot tell") && out.contains("SPIRA_LC_STORE_PATH"), "{verb:?}: {out}");
    }
}

#[test]
fn the_deleted_verbs_answer_usage() {
    let (_dir, _env) = world();
    for verb in [&["ops-snapshot"][..], &["pause-sweep"], &["ready-set"], &["pane-facets", "inflow"]] {
        let (_, out) = run(verb);
        assert!(out.starts_with("usage: spira-lc"), "{verb:?}: {out}");
        for name in ["ops-snapshot", "pause-sweep", "ready-set", "pane-facets"] {
            assert!(!out.contains(name), "{verb:?}: usage names {name}");
        }
    }
}

#[test]
fn facts_is_no_longer_a_verb_and_gets_the_general_usage() {
    let (_dir, _env) = world();
    let (rc, out) = run(&["facts"]);
    assert_eq!(rc, 2);
    assert!(out.starts_with("usage: spira-lc"), "{out}");
}
