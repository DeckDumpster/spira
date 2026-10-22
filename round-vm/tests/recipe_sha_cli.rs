use std::process::Command;

#[test]
fn cargo_run_package_prints_recipe_digest() {
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "--locked", "-p", "round-vm", "--", "recipe-sha"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let sha = String::from_utf8(out.stdout).unwrap();
    let sha = sha.trim();
    assert_eq!(sha.len(), 64, "{sha}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{sha}");
}

#[test]
fn recipe_sha_of_a_tree_matches_the_release_that_is_built_from_it() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_round-vm")).args(args).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    assert_eq!(run(&["recipe-sha", "--tree", root.to_str().unwrap()]), run(&["recipe-sha"]));
}
