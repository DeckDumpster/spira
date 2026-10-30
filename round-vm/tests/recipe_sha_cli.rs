use std::process::Command;

#[test]
fn recipe_sha_of_a_tree_matches_the_release_that_is_built_from_it() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_round-vm")).args(args).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let sha = run(&["recipe-sha"]);
    assert_eq!(sha.len(), 64, "{sha}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{sha}");
    assert_eq!(run(&["recipe-sha", "--tree", root.to_str().unwrap()]), sha);
}
