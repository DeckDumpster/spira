use queue::ops::round::{suite_statuses, BLOCKING};
use spira_sim::roundvm::{self, parse_verdict, write_verdict, Verdict, FAULT, VERDICT_FILE};
use std::path::{Path, PathBuf};

struct W {
    _t: testkit::TempDir,
    world: PathBuf,
    tree: PathBuf,
    results: PathBuf,
}

fn exe(p: &Path) {
    testkit::write_exe(p, "#!/bin/sh\nexit 0\n");
}

/// A world with a release holding two executables (and one non-executable file), and a
/// round tree with a three-suite corpus plus a file testenv's glob does not take.
fn world() -> W {
    let t = testkit::TempDir::new("simrvm");
    let world = t.join("world");
    let rel = t.join("rel/bin");
    std::fs::create_dir_all(&rel).unwrap();
    exe(&rel.join("queue"));
    exe(&rel.join("spira-lc"));
    std::fs::write(rel.join("README"), "not a binary").unwrap();
    std::fs::create_dir_all(&world).unwrap();
    std::fs::write(world.join(spira_sim::world::MARKER), "").unwrap();
    std::os::unix::fs::symlink(t.join("rel"), world.join("release")).unwrap();
    let tree = t.join("round-wt");
    std::fs::create_dir_all(tree.join("spira")).unwrap();
    for s in ["test-a.sh", "test-b.sh", "test-c.sh", "helper.sh"] {
        std::fs::write(tree.join("spira").join(s), "").unwrap();
    }
    let results = t.join("results");
    W { _t: t, world, tree, results }
}

fn none(_: &str) -> Option<String> {
    None
}

fn args(w: &W) -> Vec<String> {
    vec!["run".into(), w.tree.display().to_string(), "--results-dir".into(), w.results.display().to_string()]
}

/// What `queue round certify` reads back: each suite and its status, by queue's own reader.
fn read_back(w: &W) -> Vec<(String, String)> {
    let mut found = Vec::new();
    suite_statuses(&w.results, &mut found, 0);
    found.sort();
    found
}

fn reds(found: &[(String, String)]) -> Vec<String> {
    found.iter().filter(|(_, s)| BLOCKING.contains(&s.as_str())).map(|(n, _)| n.clone()).collect()
}

fn installed(w: &W) -> Vec<String> {
    let dir = w.tree.join("target/release");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().filter(|e| queue::ops::simple::is_executable(&e.path())).map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

#[test]
fn green_writes_an_ok_result_per_suite_and_installs_the_release() {
    let w = world();
    write_verdict(&w.world, &Verdict::Green).unwrap();
    let (code, err) = roundvm::run(&w.world, &args(&w), &none, 946684800);
    assert_eq!(code, 0, "{err}");
    let found = read_back(&w);
    let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["test-a.sh", "test-b.sh", "test-c.sh"]);
    assert!(found.iter().all(|(_, s)| s == "ok"), "{found:?}");
    assert!(reds(&found).is_empty());
    let line = std::fs::read_to_string(w.results.join("test-a.sh.result")).unwrap();
    assert_eq!(line.split_whitespace().count(), 7, "testenv-batch's 7-field line: {line:?}");
    assert_eq!(installed(&w), ["queue", "spira-lc"], "what round land's binaries check accepts");
}

#[test]
fn the_batchers_own_flags_are_accepted_and_suites_narrows_the_results() {
    let w = world();
    write_verdict(&w.world, &Verdict::Green).unwrap();
    let mut a = vec!["run".to_string(), w.tree.display().to_string()];
    for (k, v) in [("--suites", "test-a.sh,test-c.sh"), ("--maxpar", "8"), ("--toolchain", "1.90"), ("--attr-spool", "/spool"), ("--base", "local/main")] {
        a.extend([k.to_string(), v.to_string()]);
    }
    a.extend(["--results-dir".to_string(), w.results.display().to_string()]);
    let (code, err) = roundvm::run(&w.world, &a, &none, 946684800);
    assert_eq!(code, 0, "{err}");
    let names: Vec<String> = read_back(&w).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["test-a.sh", "test-c.sh"]);
    let (code, err) = roundvm::run(&w.world, &["run".into(), w.tree.display().to_string(), "--frobnicate".into(), "1".into()], &none, 0);
    assert_eq!(code, FAULT);
    assert!(err.contains("does not take --frobnicate"), "{err}");
}

#[test]
fn red_marks_exactly_the_named_suites_blocking() {
    let w = world();
    std::fs::write(w.world.join(VERDICT_FILE), "red test-b.sh test-new.sh\n").unwrap();
    let (code, err) = roundvm::run(&w.world, &args(&w), &none, 946684800);
    assert_eq!(code, 1, "{err}");
    let found = read_back(&w);
    assert_eq!(reds(&found), ["test-b.sh", "test-new.sh"]);
    assert_eq!(found.iter().filter(|(_, s)| s == "ok").count(), 2);
    assert_eq!(installed(&w), ["queue", "spira-lc"], "a red round still built");
}

#[test]
fn a_scripted_fault_leaves_no_verdict() {
    let w = world();
    write_verdict(&w.world, &Verdict::Fault("pool exhausted".into())).unwrap();
    let (code, err) = roundvm::run(&w.world, &args(&w), &none, 0);
    assert_eq!(code, FAULT);
    assert!(err.contains("pool exhausted"), "{err}");
    assert!(read_back(&w).is_empty());
    assert!(installed(&w).is_empty());
}

/// Planted controls: a verdict the stub cannot read must never come back as green. Each
/// exits round-vm's own-failure code with no result written and nothing installed.
#[test]
fn an_absent_or_malformed_verdict_is_never_green() {
    for text in [None, Some(""), Some("# only a comment\n"), Some("gren\n"), Some("green test-a.sh\n"), Some("red\n"), Some("red ../escape\n"), Some("ok\n")] {
        let w = world();
        if let Some(t) = text {
            std::fs::write(w.world.join(VERDICT_FILE), t).unwrap();
        }
        let (code, err) = roundvm::run(&w.world, &args(&w), &none, 0);
        assert_eq!(code, FAULT, "{text:?}: {err}");
        assert!(read_back(&w).is_empty(), "{text:?} wrote results");
        assert!(installed(&w).is_empty(), "{text:?} installed binaries");
    }
}

#[test]
fn green_with_nothing_to_report_or_install_is_a_fault() {
    let w = world();
    write_verdict(&w.world, &Verdict::Green).unwrap();
    std::fs::remove_dir_all(w.tree.join("spira")).unwrap();
    let (code, err) = roundvm::run(&w.world, &args(&w), &none, 0);
    assert_eq!(code, FAULT, "an empty corpus: {err}");
    assert!(read_back(&w).is_empty());

    let w = world();
    write_verdict(&w.world, &Verdict::Green).unwrap();
    std::fs::remove_file(w.world.join("release")).unwrap();
    let (code, err) = roundvm::run(&w.world, &args(&w), &none, 0);
    assert_eq!(code, FAULT, "no release: {err}");
    assert!(read_back(&w).is_empty());
}

#[test]
fn the_release_falls_back_to_spira_sim_release() {
    let w = world();
    let rel = w.world.join("release").canonicalize().unwrap();
    std::fs::remove_file(w.world.join("release")).unwrap();
    write_verdict(&w.world, &Verdict::Green).unwrap();
    let env = |k: &str| (k == roundvm::RELEASE_ENV).then(|| rel.display().to_string());
    let (code, err) = roundvm::run(&w.world, &args(&w), &env, 0);
    assert_eq!(code, 0, "{err}");
    assert_eq!(installed(&w), ["queue", "spira-lc"]);
}

#[test]
fn outside_a_world_it_refuses() {
    let w = world();
    write_verdict(&w.world, &Verdict::Green).unwrap();
    std::fs::remove_file(w.world.join(spira_sim::world::MARKER)).unwrap();
    assert_eq!(roundvm::run(&w.world, &args(&w), &none, 0).0, FAULT);
    assert!(read_back(&w).is_empty());
}

#[test]
fn verdicts_round_trip() {
    for v in [Verdict::Green, Verdict::Red(vec!["test-a.sh".into(), "test-b.sh".into()]), Verdict::Fault("vm lost".into())] {
        let t = testkit::TempDir::new("simrvm");
        write_verdict(&t, &v).unwrap();
        assert_eq!(parse_verdict(&std::fs::read_to_string(t.join(VERDICT_FILE)).unwrap()).unwrap(), v);
    }
}

/// The binary answers by name, as `queue round certify` calls it: `timeout ... round-vm run`
/// found first on the world's PATH.
#[test]
fn the_sim_binary_answers_as_round_vm() {
    let w = world();
    let bin = w.world.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_sim"), bin.join("round-vm")).unwrap();
    std::fs::write(w.world.join(VERDICT_FILE), "red test-c.sh\n").unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let o = std::process::Command::new("timeout")
        .args(["-k", "10", "60", "round-vm", "run"])
        .arg(&w.tree)
        .arg("--results-dir")
        .arg(&w.results)
        .env("PATH", path)
        .env(roundvm::WORLD_ENV, &w.world)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(reds(&read_back(&w)), ["test-c.sh"]);
}
