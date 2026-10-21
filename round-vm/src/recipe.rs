//! The hash of everything a round image is built from. The CI image build tags the AMI with
//! it and the EC2 provider refuses an image whose tag differs from its own, so a release can
//! only run on the image its own recipe produces.

use sha2::{Digest, Sha256};

const INPUTS: [(&str, &str); 6] = [
    ("template.sh", include_str!("template.sh")),
    ("ami-setup.sh", include_str!("../ami-setup.sh")),
    ("rust-toolchain.toml", include_str!("../../rust-toolchain.toml")),
    ("flake.nix", include_str!("../../flake.nix")),
    ("flake.lock", include_str!("../../flake.lock")),
    ("Containerfile", include_str!("../../spira/testenv/Containerfile")),
];

/// Where each input sits in a source tree, in `INPUTS` order.
const TREE_PATHS: [&str; 6] = [
    "round-vm/src/template.sh",
    "round-vm/ami-setup.sh",
    "rust-toolchain.toml",
    "flake.nix",
    "flake.lock",
    "spira/testenv/Containerfile",
];

fn digest<'a>(inputs: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut h = Sha256::new();
    for (name, text) in inputs {
        h.update(format!("{name}\0{}\0", text.len()));
        h.update(text.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha() -> String {
    digest(INPUTS.into_iter())
}

/// The hash of the recipe a source tree carries, which is what an image built from that tree is tagged with.
pub fn sha_of_tree(root: &std::path::Path) -> Result<String, String> {
    let mut texts = Vec::new();
    for path in TREE_PATHS {
        let full = root.join(path);
        texts.push(std::fs::read_to_string(&full).map_err(|e| format!("{}: {e}", full.display()))?);
    }
    Ok(digest(INPUTS.iter().zip(&texts).map(|((name, _), text)| (*name, text.as_str()))))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_hashed_files_are_the_admission_path_list() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        assert_eq!(super::INPUTS.len(), spira_config::recipe::PATHS.len());
        for ((_, text), path) in super::INPUTS.iter().zip(spira_config::recipe::PATHS) {
            assert_eq!(*text, std::fs::read_to_string(root.join(path)).unwrap(), "{path}");
        }
    }

    #[test]
    fn the_hash_is_a_stable_sha256_hex_digest() {
        let s = super::sha();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(s, super::sha());
    }

    #[test]
    fn a_tree_hashes_as_the_release_built_from_it_does() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        assert_eq!(super::sha_of_tree(&root).unwrap(), super::sha());
    }

    #[test]
    fn a_tree_missing_a_recipe_input_is_refused_naming_it() {
        let e = super::sha_of_tree(std::path::Path::new("/nonexistent-recipe-tree")).unwrap_err();
        assert!(e.contains("template.sh"), "{e}");
    }
}
