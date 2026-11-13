//! `VmRunner`'s fence probe against a real git tree and the real `gate fence-run`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use batcher::core::Member;
use batcher::drive::RoundRunner;
use batcher_cut::io::{lifecycle_tests_env, Land, Repo};
use batcher_cut::vm::VmRunner;

fn gate_bin() -> PathBuf {
    let gate = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("gate");
    assert!(gate.is_file(), "the build that runs this test builds gate beside it: {}", gate.display());
    gate
}

fn member(id: &str, tip: &str) -> Member {
    Member { id: id.into(), tip: tip.into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: Default::default(), blocked_by: vec![] }
}

#[test]
fn a_red_fence_on_a_member_subset_leaves_its_own_output_and_a_green_one_leaves_none() {
    let d = testkit::TempDir::new("fence-green");
    let mut env = lifecycle_tests_env(d.path());
    env.gate_bin = gate_bin();
    let toml = spira_config::process::fixture_toml(d.path(), &[]).display().to_string();
    let release = d.path().join("release").display().to_string();
    let _env = testkit::env(&[("SPIRA_TOML", Some(&toml)), (spira_config::RELEASE_ENV, Some(&release))]);
    let repo = Repo { name: "fx".into(), path: d.path().join("repo"), base: "main".into(), forge: PathBuf::new(), land: Land::Local };
    let r = &repo.path;
    fs::create_dir_all(r.join("spira")).unwrap();
    testkit::git(r, &["init", "-q", "-b", "main"]);
    fs::write(r.join("gate.steps"), "step bash spira/poisoned.sh\n").unwrap();
    fs::write(r.join("spira/poisoned.sh"), "if [ -e poison ]; then echo \"poisoned.sh: poison present at callers.rs:726\" >&2; exit 1; fi\n").unwrap();
    let commit = |msg: &str| {
        testkit::git(r, &["add", "-A"]);
        testkit::git(r, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", msg]);
        testkit::git(r, &["rev-parse", "HEAD"]).trim().to_string()
    };
    let base = commit("base");
    let branch = |name: &str, file: &str| {
        testkit::git(r, &["checkout", "-q", "-b", name, "main"]);
        fs::write(r.join(file), "x\n").unwrap();
        commit(name)
    };
    let (bad, good) = (branch("bad", "poison"), branch("good", "harmless"));
    testkit::git(r, &["checkout", "-q", "main"]);
    let (bad, good) = (member("sp-bad", &bad), member("sp-good", &good));
    let mut run = VmRunner::new(&env, &repo, &repo.path, &base, "r1", BTreeMap::new()).unwrap();
    run.set_members(&[bad.clone(), good.clone()]);

    assert_eq!(run.filter_green("fence-poisoned", std::slice::from_ref(&good)), Some(true));
    assert_eq!(run.probe_output(), "", "a green fence leaves nothing to carry");
    assert_eq!(run.filter_green("fence-poisoned", std::slice::from_ref(&bad)), Some(false));
    let said = run.probe_output();
    assert!(said.contains("poison present at callers.rs:726"), "{said}");
    assert_eq!(run.probe_output(), "", "the output is handed over once");
}
