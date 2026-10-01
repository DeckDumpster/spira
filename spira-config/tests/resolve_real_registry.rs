//! Parity smoke test for `spira_config::resolve` (sp-eekjm) against the REAL `spira/conf.d`
//! registry this harness ships — not a hand-built fixture. Every one of the ~297 keys
//! `spira/conf-gen.sh` would otherwise fold into `conf.d.defaults.generated.sh` must resolve
//! without error here, under a realistic `SPIRA_HOME`/`SPIRA_REPO`, the same parity proof the
//! wave-brief asks for before a bash caller is ever pointed at this code (a later bead,
//! sp-ubcgo).

use std::path::Path;

fn conf_d() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira").join("conf.d")
}

#[test]
fn every_real_registry_key_resolves_without_error() {
    let conf_d = conf_d();
    assert!(conf_d.is_dir(), "expected {} to exist in this checkout", conf_d.display());

    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");

    let mut env = std::collections::BTreeMap::new();
    env.insert("HOME".to_string(), "/home/parity-test".to_string());

    let resolved = spira_config::resolve::resolve(spira_config::resolve::ResolveInput {
        env: &env,
        home: &home,
        repo: &repo,
        toml: None,
        conf_d: &conf_d,
    })
    .expect("every real conf.d default should evaluate (see eval.rs's module doc for the shapes it supports)");

    // Every key the registry names has SOME resolved value (possibly empty) — resolve never
    // silently drops a key the registry knows about.
    let registry = spira_config::registry::load(&conf_d).unwrap();
    for key in registry.keys() {
        assert!(resolved.values.contains_key(key), "{key} has no resolved value");
    }

    // The per-copy keys the bead names are resolved in-process (for SPIRA_REPO_MAP, SPIRA_
    // FAYTHS, SPIRA_MAX_AEONS) or not resolved at all (SPIRA_HOME, SPIRA_REPO) — either way,
    // NONE of them appear in the typed export set `resolve --sh` would print.
    let sh_text = resolved.to_sh(spira_config::resolve::EXPORT_KEYS);
    let exported: std::collections::BTreeSet<&str> =
        sh_text.lines().filter_map(|l| l.split('=').next()).collect();
    for forbidden in ["SPIRA_HOME", "SPIRA_REPO", "SPIRA_REPO_MAP", "SPIRA_FAYTHS", "SPIRA_MAX_AEONS"] {
        assert!(!exported.contains(forbidden), "{forbidden} leaked into the typed export set");
    }
    assert!(!resolved.values.keys().any(|k| k == "SPIRA_HOME" || k == "SPIRA_REPO"));
}
