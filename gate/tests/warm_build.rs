//! A build directory donated by one gate tree and adopted by another at a different path
//! rebuilds only the crate whose source changed (sp-143lod), with real cargo.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, prog: &str, args: &[&str]) -> String {
    let o = Command::new(prog)
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .args(args)
        .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(o.status.success(), "{prog} {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn compiled(out: &str) -> Vec<String> {
    out.lines().filter_map(|l| l.trim().strip_prefix("Compiling ")).filter_map(|l| l.split(' ').next()).map(String::from).collect()
}

fn seeded(t: &testkit::TempDir, change: &str, contents: &str) -> Vec<String> {
    let (root, a, b) = (t.path().join("root"), t.path().join("tree-a"), t.path().join("tree-b"));
    std::fs::create_dir_all(a.join("leaf/src")).unwrap();
    std::fs::create_dir_all(a.join("base/src")).unwrap();
    std::fs::write(a.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\nmembers = [\"base\", \"leaf\"]\n[profile.dwarf]\ninherits = \"dev\"\n").unwrap();
    std::fs::write(a.join("base/Cargo.toml"), "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    std::fs::write(a.join("base/src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
    std::fs::write(a.join("leaf/Cargo.toml"), "[package]\nname = \"leaf\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nbase = { path = \"../base\" }\n").unwrap();
    std::fs::write(a.join("leaf/src/lib.rs"), "pub fn g() -> u8 { base::f() }\n").unwrap();
    std::fs::write(a.join(".gitignore"), "target/\n").unwrap();
    run(&a, "git", &["init", "-q"]);
    run(&a, "git", &["add", "."]);
    run(&a, "git", &["commit", "-qm", "base"]);

    let dest = root.join("tree-a").join("dwarf");
    std::fs::create_dir_all(a.join("target")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::os::unix::fs::symlink(&dest, a.join("target/dwarf")).unwrap();
    let build = |tree: &Path| {
        let o = run(tree, "cargo", &["build", "--profile", "dwarf", "--color", "never"]);
        compiled(&o)
    };
    assert_eq!(build(&a).len(), 2);
    assert!(gate::warm::donate_tree(&a));

    run(t.path(), "git", &["clone", "-q", a.to_str().unwrap(), b.to_str().unwrap()]);
    std::fs::write(b.join(change), contents).unwrap();
    run(&b, "git", &["commit", "-qam", "change"]);
    let bdest = root.join("tree-b").join("dwarf");
    std::fs::create_dir_all(&bdest).unwrap();
    std::fs::create_dir_all(b.join("target")).unwrap();
    std::os::unix::fs::symlink(&bdest, b.join("target/dwarf")).unwrap();
    let line = gate::warm::warm_start(&root, &bdest, &b).unwrap();
    assert!(line.contains("warm from"), "{line}");

    build(&b)
}

#[test]
fn an_adopted_build_directory_rebuilds_only_the_crate_that_changed() {
    let t = testkit::TempDir::new("gate-warm-leaf");
    assert_eq!(seeded(&t, "leaf/src/lib.rs", "pub fn g() -> u8 { base::f() + 1 }\n"), ["leaf"]);
}

#[test]
fn an_adopted_build_directory_rebuilds_the_dependents_of_a_changed_crate() {
    let t = testkit::TempDir::new("gate-warm-dep");
    assert_eq!(seeded(&t, "base/src/lib.rs", "pub fn f() -> u8 { 2 }\n"), ["base", "leaf"]);
}
