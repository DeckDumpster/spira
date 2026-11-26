//! A build directory donated by one gate tree and adopted by another at a different path
//! rebuilds only the crate whose source changed, with real cargo.

#[macro_use]
extern crate spira_test_id;
use std::path::Path;
use std::process::Command;

fn checked(c: &mut Command) -> String {
    let o = c.output().unwrap();
    assert!(o.status.success(), "{c:?}: {}", String::from_utf8_lossy(&o.stderr));
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn run(dir: &Path, prog: &str, args: &[&str]) -> String {
    checked(
        Command::new(prog)
            .current_dir(dir)
            .env_remove("CARGO_TARGET_DIR")
            .args(args)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t"),
    )
}

// Empty, not removed: removing lets a cargo config's wrapper apply.
fn pinned(c: &mut Command) -> &mut Command {
    c.env("RUSTC_WRAPPER", "").env("RUSTC_WORKSPACE_WRAPPER", "")
}

fn cargo(dir: &Path) -> Command {
    let mut c = Command::new("cargo");
    pinned(c.current_dir(dir).env_remove("CARGO_TARGET_DIR"));
    c
}

fn cargo_build(dir: &Path) -> String {
    checked(cargo(dir).args(["build", "--color", "never"]))
}

fn compiled(out: &str) -> Vec<String> {
    out.lines().filter_map(|l| l.trim().strip_prefix("Compiling ")).filter_map(|l| l.split(' ').next()).map(String::from).collect()
}

fn seeded(t: &testkit::TempDir, change: &str, contents: &str) -> Vec<String> {
    let (root, a, b) = (t.path().join("root"), t.path().join("tree-a"), t.path().join("tree-b"));
    std::fs::create_dir_all(a.join("leaf/src")).unwrap();
    std::fs::create_dir_all(a.join("base/src")).unwrap();
    std::fs::write(a.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\nmembers = [\"base\", \"leaf\"]\n").unwrap();
    std::fs::write(a.join("base/Cargo.toml"), "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    std::fs::write(a.join("base/src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
    std::fs::write(a.join("leaf/Cargo.toml"), "[package]\nname = \"leaf\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nbase = { path = \"../base\" }\n").unwrap();
    std::fs::write(a.join("leaf/src/lib.rs"), "pub fn g() -> u8 { base::f() }\n").unwrap();
    std::fs::write(a.join(".gitignore"), "target/\n").unwrap();
    run(&a, "git", &["init", "-q"]);
    run(&a, "git", &["add", "."]);
    run(&a, "git", &["commit", "-qm", "base"]);

    let dest = root.join("tree-a").join("debug");
    std::fs::create_dir_all(a.join("target")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::os::unix::fs::symlink(&dest, a.join("target/debug")).unwrap();
    let build = |tree: &Path| compiled(&cargo_build(tree));
    assert_eq!(build(&a).len(), 2);
    assert!(gate::warm::donate_tree(&a, "r", 1 << 20).is_some());

    run(t.path(), "git", &["clone", "-q", a.to_str().unwrap(), b.to_str().unwrap()]);
    std::fs::write(b.join(change), contents).unwrap();
    run(&b, "git", &["commit", "-qam", "change"]);
    let bdest = root.join("tree-b").join("debug");
    std::fs::create_dir_all(&bdest).unwrap();
    std::fs::create_dir_all(b.join("target")).unwrap();
    std::os::unix::fs::symlink(&bdest, b.join("target/debug")).unwrap();
    let (_, line) = gate::warm::warm_start(&root, "r", &bdest, &b).unwrap();
    assert!(line.contains("warm from"), "{line}");

    build(&b)
}

#[test_id(tiv14kr)]
#[test]
fn an_adopted_build_directory_rebuilds_only_the_crate_that_changed() {
    let t = testkit::TempDir::new("gate-warm-leaf");
    assert_eq!(seeded(&t, "leaf/src/lib.rs", "pub fn g() -> u8 { base::f() + 1 }\n"), ["leaf"]);
}

#[test_id(ts471dk)]
#[test]
fn an_adopted_build_directory_rebuilds_the_dependents_of_a_changed_crate() {
    let t = testkit::TempDir::new("gate-warm-dep");
    assert_eq!(seeded(&t, "base/src/lib.rs", "pub fn f() -> u8 { 2 }\n"), ["base", "leaf"]);
}

#[test_id(tpyjntf)]
#[test]
fn an_adopted_build_directory_never_serves_a_binary_with_the_donors_path_baked_in() {
    let t = testkit::TempDir::new("gate-warm-path");
    let (root, a, b) = (t.path().join("root"), t.path().join("tree-a"), t.path().join("tree-b"));
    std::fs::create_dir_all(a.join("pin/src")).unwrap();
    std::fs::write(a.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\nmembers = [\"pin\"]\n").unwrap();
    std::fs::write(a.join("pin/Cargo.toml"), "[package]\nname = \"pin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    std::fs::write(a.join("pin/src/main.rs"), "fn main() { print!(\"{}\", env!(\"CARGO_MANIFEST_DIR\")); }\n").unwrap();
    std::fs::write(a.join(".gitignore"), "target/\n").unwrap();
    run(&a, "git", &["init", "-q"]);
    run(&a, "git", &["add", "."]);
    run(&a, "git", &["commit", "-qm", "base"]);
    let link = |tree: &Path, name: &str| {
        let dest = root.join(name).join("debug");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::create_dir_all(tree.join("target")).unwrap();
        std::os::unix::fs::symlink(&dest, tree.join("target/debug")).unwrap();
        dest
    };
    link(&a, "tree-a");
    cargo_build(&a);
    assert_eq!(run(&a, a.join("target/debug/pin").to_str().unwrap(), &[]), a.join("pin").to_str().unwrap());
    assert!(gate::warm::donate_tree(&a, "r", 1 << 20).is_some());

    run(t.path(), "git", &["clone", "-q", a.to_str().unwrap(), b.to_str().unwrap()]);
    let bdest = link(&b, "tree-b");
    gate::warm::warm_start(&root, "r", &bdest, &b).unwrap();
    cargo_build(&b);
    assert_eq!(run(&b, b.join("target/debug/pin").to_str().unwrap(), &[]), b.join("pin").to_str().unwrap());
}

#[test_id(t95hy6w)]
#[test]
fn a_unit_cached_by_sccache_in_one_tree_is_recompiled_in_another() {
    let Some(sccache) = spira_config::build::find_on(&std::env::var("PATH").unwrap_or_default(), "sccache") else {
        assert!(std::env::var_os("SPIRA_ALLOW_NO_SCCACHE").is_some(), "sccache is not on PATH: this test pins sccache's own per-tree key and cannot run without it (set SPIRA_ALLOW_NO_SCCACHE=1 to skip)");
        return;
    };
    let t = testkit::TempDir::new("gate-sccache-path");
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port().to_string();
    let cache = t.path().join("cache");
    let sc = sccache.to_str().unwrap();
    let env: Vec<(&str, &str)> = vec![
        ("RUSTC_WRAPPER", sc),
        ("SCCACHE_DIR", cache.to_str().unwrap()),
        ("SCCACHE_SERVER_PORT", &port),
        ("SCCACHE_IGNORE_SERVER_IO_ERROR", "1"),
        ("SCCACHE_WEBDAV_ENDPOINT", ""),
    ];
    let build = |tree: &Path| {
        let o = Command::new("cargo").current_dir(tree).env_remove("CARGO_TARGET_DIR").envs(env.iter().cloned()).args(["build", "--color", "never"]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let o = Command::new(tree.join("target/debug/pin")).output().unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let (a, b) = (t.path().join("tree-a"), t.path().join("tree-b"));
    for tree in [&a, &b] {
        std::fs::create_dir_all(tree.join("lib/src")).unwrap();
        std::fs::create_dir_all(tree.join("pin/src")).unwrap();
        std::fs::write(tree.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\nmembers = [\"lib\", \"pin\"]\n").unwrap();
        std::fs::write(tree.join("lib/Cargo.toml"), "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::write(tree.join("lib/src/lib.rs"), "pub fn dir() -> &'static str { env!(\"CARGO_MANIFEST_DIR\") }\n").unwrap();
        std::fs::write(tree.join("pin/Cargo.toml"), "[package]\nname = \"pin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nlib = { path = \"../lib\" }\n").unwrap();
        std::fs::write(tree.join("pin/src/main.rs"), "fn main() { print!(\"{}\", lib::dir()); }\n").unwrap();
    }
    let (from_a, from_b) = (build(&a), build(&b));
    let _ = Command::new(sc).arg("--stop-server").envs(env.iter().cloned()).output();
    assert_eq!(from_a, a.join("lib").to_str().unwrap());
    assert_eq!(from_b, b.join("lib").to_str().unwrap());
}

#[test_id(tkstl5q)]
#[test]
fn a_scratch_build_runs_no_compiler_wrapper_the_parent_names() {
    let t = testkit::TempDir::new("gate-warm-wrapper");
    std::fs::create_dir_all(t.path().join("src")).unwrap();
    std::fs::write(t.path().join("Cargo.toml"), "[package]\nname = \"one\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
    std::fs::write(t.path().join("src/lib.rs"), "").unwrap();
    let mut c = cargo(t.path());
    for pin in ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"] {
        assert!(c.get_envs().any(|(k, v)| k == pin && v.is_some_and(|v| v.is_empty())), "cargo() leaves {pin} inherited");
    }
    c.env("RUSTC_WRAPPER", "false");
    checked(pinned(&mut c).args(["build", "--color", "never"]));
}
