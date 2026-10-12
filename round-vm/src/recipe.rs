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

pub fn sha() -> String {
    let mut h = Sha256::new();
    for (name, text) in INPUTS {
        h.update(format!("{name}\0{}\0", text.len()));
        h.update(text.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_hash_is_a_stable_sha256_hex_digest() {
        let s = super::sha();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(s, super::sha());
    }
}
