//! The repo-relative paths a round image is built from. `round-vm`'s recipe hash reads exactly
//! these files (a test there holds the two together), and a round whose image preflight goes red
//! ejects the members that change one.

pub const PATHS: [&str; 6] = [
    "round-vm/src/template.sh",
    "round-vm/ami-setup.sh",
    "rust-toolchain.toml",
    "flake.nix",
    "flake.lock",
    "spira/testenv/Containerfile",
];

/// The files whose change moves the testenv image tag a tree needs.
pub const TAG_INPUTS: [&str; 5] = [
    "spira/testenv/Containerfile",
    "spira/deps.toml",
    "rust-toolchain.toml",
    "testenv/src/container.rs",
    "spira-config/src/recipe.rs",
];

pub fn touches_tag_input<S: AsRef<str>>(changed: &[S]) -> bool {
    changed.iter().any(|p| TAG_INPUTS.contains(&p.as_ref()))
}

pub fn touches<S: AsRef<str>>(changed: &[S]) -> bool {
    changed.iter().any(|p| PATHS.contains(&p.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t4q6ucl)]
    #[test]
    fn a_recipe_input_is_detected_among_others() {
        assert!(touches(&["README.md", "flake.lock"]));
        assert!(touches(&["round-vm/ami-setup.sh".to_string()]));
    }

    #[test_id(tri2cac)]
    #[test]
    fn a_tag_input_is_detected_among_others() {
        assert!(touches_tag_input(&["README.md", "spira/deps.toml"]));
        assert!(!touches_tag_input(&["spira/queue.sh", "flake.lock"]));
        assert!(!touches_tag_input::<&str>(&[]));
    }

    #[test_id(t5ebw64)]
    #[test]
    fn unrelated_and_empty_diffs_are_not_recipe_changes() {
        assert!(!touches(&["spira/queue.sh", "round-vm/src/pool.rs", "flake.nix.bak"]));
        assert!(!touches::<&str>(&[]));
    }
}
