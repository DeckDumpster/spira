//! The repo-relative paths a round image is built from. `round-vm`'s recipe hash reads exactly
//! these files (a test there holds the two together), and round admission uses them to give a
//! member that changes one a round of its own.

pub const PATHS: [&str; 6] = [
    "round-vm/src/template.sh",
    "round-vm/ami-setup.sh",
    "rust-toolchain.toml",
    "flake.nix",
    "flake.lock",
    "spira/testenv/Containerfile",
];

pub fn touches<S: AsRef<str>>(changed: &[S]) -> bool {
    changed.iter().any(|p| PATHS.contains(&p.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recipe_input_is_detected_among_others() {
        assert!(touches(&["README.md", "flake.lock"]));
        assert!(touches(&["round-vm/ami-setup.sh".to_string()]));
    }

    #[test]
    fn unrelated_and_empty_diffs_are_not_recipe_changes() {
        assert!(!touches(&["spira/queue.sh", "round-vm/src/pool.rs", "flake.nix.bak"]));
        assert!(!touches::<&str>(&[]));
    }
}
