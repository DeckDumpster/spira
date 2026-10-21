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
