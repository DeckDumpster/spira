use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    o
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn branch_adding(dir: &Path, name: &str, migration: &str, rule: &str) {
    assert!(git(dir, &["checkout", "-q", "-b", name, "base"]).status.success());
    write(dir, &format!("lifecycle/migrations/{migration}"), "SELECT 1;\n");
    write(dir, &format!("spira-lint/src/rules/{rule}.rs"), "pub fn rules() -> Vec<Box<dyn crate::Rule>> { vec![] }\n");
    assert!(git(dir, &["add", "-A"]).status.success());
    assert!(git(dir, &["commit", "-q", "-m", name]).status.success());
}

#[test]
fn two_branches_each_adding_a_migration_and_a_rule_merge_without_conflict() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap();
    let tmp = testkit::TempDir::new("no-shared-lists");
    let dir = tmp.path().to_path_buf();
    assert!(git(&dir, &["init", "-q", "-b", "main"]).status.success());
    for rel in ["spira-lint/src/lib.rs", "spira-lint/src/rules/mod.rs", "spira-lc/src/migrate.rs"] {
        write(&dir, rel, &std::fs::read_to_string(root.join(rel)).unwrap());
    }
    for e in std::fs::read_dir(root.join("lifecycle/migrations")).unwrap() {
        let p = e.unwrap().path();
        write(&dir, &format!("lifecycle/migrations/{}", p.file_name().unwrap().to_string_lossy()), &std::fs::read_to_string(&p).unwrap());
    }
    assert!(git(&dir, &["add", "-A"]).status.success());
    assert!(git(&dir, &["commit", "-q", "-m", "base"]).status.success());
    assert!(git(&dir, &["tag", "base"]).status.success());
    branch_adding(&dir, "a", "9990-a.sql", "rule_a");
    branch_adding(&dir, "b", "9990-b.sql", "rule_b");
    let merged = git(&dir, &["merge", "--no-edit", "-q", "a"]);
    let out = String::from_utf8_lossy(&merged.stdout).to_string() + &String::from_utf8_lossy(&merged.stderr);
    assert!(merged.status.success(), "the two branches conflict: {out}");
}
