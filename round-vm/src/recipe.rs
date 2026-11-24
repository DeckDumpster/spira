//! The hash of everything a round image is built from. The CI image build tags the AMI with
//! it and the EC2 provider refuses an image whose tag differs from its own, so a release can
//! only run on the image its own recipe produces.

use sha2::{Digest, Sha256};
use std::path::Path;

/// The channel `rust-toolchain.toml` pins in `tree`, the one place the harness names a Rust.
pub fn tree_channel(tree: &Path) -> Option<String> {
    let text = std::fs::read_to_string(tree.join("rust-toolchain.toml")).ok()?;
    text.lines()
        .find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == "channel").then(|| v.trim().trim_matches('"').to_string())
        })
        .filter(|c| c.starts_with(|ch: char| ch.is_ascii_digit()))
}

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

pub fn tree_paths() -> &'static [&'static str] {
    &TREE_PATHS
}

/// Which check the round image preflight runs for a diff.
#[derive(Debug, PartialEq, Eq)]
pub enum PreflightPlan {
    Skip,
    PackageList { apt_line: String },
    FullSetup,
}

const AMI_SETUP: &str = "round-vm/ami-setup.sh";
const APT_PREFIX: &str = "apt-get install ";

/// `ami_setup_diff` is a `-U0` diff of `ami-setup.sh` without its `---`/`+++` headers.
pub fn preflight_plan(touched: &[&str], ami_setup_diff: &str, head_ami_setup: &str) -> PreflightPlan {
    let hit: Vec<&&str> = touched.iter().filter(|p| TREE_PATHS.contains(p)).collect();
    if hit.is_empty() {
        return PreflightPlan::Skip;
    }
    if hit.len() != 1 || *hit[0] != AMI_SETUP {
        return PreflightPlan::FullSetup;
    }
    let changed: Vec<&str> = ami_setup_diff.lines().filter(|l| l.starts_with('+') || l.starts_with('-')).collect();
    if changed.is_empty() || !changed.iter().all(|l| l[1..].starts_with(APT_PREFIX)) {
        return PreflightPlan::FullSetup;
    }
    let mut apt = head_ami_setup.lines().filter(|l| l.starts_with(APT_PREFIX));
    match (apt.next(), apt.next()) {
        (Some(line), None) => PreflightPlan::PackageList { apt_line: line.to_string() },
        _ => PreflightPlan::FullSetup,
    }
}

fn digest<'a>(inputs: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut h = Sha256::new();
    for ((name, text), path) in inputs.zip(TREE_PATHS) {
        let text = &spira_config::recipe::image_text(path, text);
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
    #[test_id(tqmrp6w)]
    #[test]
    fn the_hash_is_a_stable_sha256_hex_digest() {
        let s = super::super::sha();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(s, super::super::sha());
    }

    fn tree_with(name: &str, containerfile: &str) -> testkit::TempDir {
        let root = testkit::TempDir::new(name);
        for (path, (_, text)) in super::TREE_PATHS.iter().zip(super::INPUTS.iter()) {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            let body = if *path == spira_config::recipe::CONTAINERFILE { containerfile } else { text };
            std::fs::write(full, body).unwrap();
        }
        root
    }

    #[test_id(tqfo2d8)]
    #[test]
    fn a_containerfile_comment_keeps_the_hash_and_a_run_line_moves_it() {
        let cf = "# top\nFROM a\nRUN x\n";
        let a = super::super::sha_of_tree(&tree_with("recipe-cf-a", cf)).unwrap();
        let b = super::super::sha_of_tree(&tree_with("recipe-cf-b", &cf.replace("# top", "# other"))).unwrap();
        let c = super::super::sha_of_tree(&tree_with("recipe-cf-c", &cf.replace("RUN x", "RUN y"))).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test_id(t29xeez)]
    #[test]
    fn a_comment_in_any_other_input_still_moves_the_hash() {
        let cf = "FROM a\n";
        let a = tree_with("recipe-oth-a", cf);
        let b = tree_with("recipe-oth-b", cf);
        let f = b.join("round-vm/ami-setup.sh");
        let t = std::fs::read_to_string(&f).unwrap();
        std::fs::write(&f, format!("{t}\n# a comment\n")).unwrap();
        assert_ne!(super::super::sha_of_tree(&a).unwrap(), super::super::sha_of_tree(&b).unwrap());
    }

    #[test_id(tz86os2)]
    #[test]
    fn a_tree_missing_a_recipe_input_is_refused_naming_it() {
        let root = testkit::TempDir::new("recipe-missing-tree");
        let e = super::super::sha_of_tree(&root.join("nonexistent-recipe-tree")).unwrap_err();
        assert!(e.contains("template.sh"), "{e}");
    }

    #[test_id(temd32h)]
    #[test]
    fn the_tree_channel_follows_the_file_and_ignores_a_floating_one() {
        let tmp = testkit::TempDir::new("tc-pin");
        let d = tmp.path();
        assert_eq!(super::super::tree_channel(d), None);
        std::fs::write(d.join("rust-toolchain.toml"), "[toolchain]\nchannel = \"9.8.7\"\n").unwrap();
        assert_eq!(super::super::tree_channel(d).as_deref(), Some("9.8.7"));
        std::fs::write(d.join("rust-toolchain.toml"), "[toolchain]\nchannel = \"stable\"\n").unwrap();
        assert_eq!(super::super::tree_channel(d), None);
    }

    const APT: &str = "apt-get install -y -qq podman git bubblewrap";
    const HEAD: &str = "#!/bin/bash\napt-get update -qq\napt-get install -y -qq podman git bubblewrap\nsystemctl enable ssh\n";
    const APT_DIFF: &str = "-apt-get install -y -qq podman git\n+apt-get install -y -qq podman git bubblewrap\n";
    const AMI: &str = "round-vm/ami-setup.sh";

    fn plan(touched: &[&str], diff: &str, head: &str) -> super::PreflightPlan {
        super::preflight_plan(touched, diff, head)
    }

    // covers: UC-test-infrastructure-44
    #[test_id(ta0ijhe)]
    #[test]
    fn an_apt_line_only_change_installs_the_package_list() {
        assert_eq!(plan(&[AMI], APT_DIFF, HEAD), super::super::PreflightPlan::PackageList { apt_line: APT.into() });
    }

    #[test_id(tobyxuz)]
    #[test]
    fn an_apt_line_plus_another_line_runs_the_full_setup() {
        let diff = format!("{APT_DIFF}-systemctl enable ssh\n+systemctl enable --now ssh\n");
        assert_eq!(plan(&[AMI], &diff, HEAD), super::super::PreflightPlan::FullSetup);
    }

    #[test_id(tjilywy)]
    #[test]
    fn a_second_apt_line_runs_the_full_setup() {
        let head = format!("{HEAD}apt-get install -y -qq vim\n");
        assert_eq!(plan(&[AMI], "+apt-get install -y -qq vim\n", &head), super::super::PreflightPlan::FullSetup);
    }

    #[test_id(tq1dk02)]
    #[test]
    fn a_removed_apt_line_runs_the_full_setup() {
        let head = "#!/bin/bash\nsystemctl enable ssh\n";
        assert_eq!(plan(&[AMI], "-apt-get install -y -qq podman git\n", head), super::super::PreflightPlan::FullSetup);
    }

    #[test_id(t2dqs33)]
    #[test]
    fn another_line_only_runs_the_full_setup() {
        let diff = "-systemctl enable ssh\n+systemctl enable --now ssh\n";
        assert_eq!(plan(&[AMI], diff, HEAD), super::super::PreflightPlan::FullSetup);
    }

    #[test_id(tt8t9ze)]
    #[test]
    fn an_empty_ami_setup_diff_runs_the_full_setup() {
        assert_eq!(plan(&[AMI], "", HEAD), super::super::PreflightPlan::FullSetup);
    }

    #[test_id(tdx6gni)]
    #[test]
    fn a_touched_flake_lock_runs_the_full_setup() {
        assert_eq!(plan(&["flake.lock"], "", HEAD), super::super::PreflightPlan::FullSetup);
    }

    #[test_id(t3ikkwt)]
    #[test]
    fn ami_setup_and_flake_lock_together_run_the_full_setup() {
        assert_eq!(plan(&[AMI, "flake.lock"], APT_DIFF, HEAD), super::super::PreflightPlan::FullSetup);
    }

    #[test_id(tjxsoke)]
    #[test]
    fn an_unrelated_file_skips() {
        assert_eq!(plan(&["unrelated", "round-vm/src/other.rs"], "", ""), super::super::PreflightPlan::Skip);
    }

    #[test_id(te8ipc0)]
    #[test]
    fn every_tree_path_is_a_recipe_input_and_nothing_else_is() {
        for p in super::super::tree_paths() {
            assert_ne!(plan(&[p], "", ""), super::super::PreflightPlan::Skip, "{p}");
        }
        assert_eq!(super::super::tree_paths().len(), super::super::INPUTS.len());
        assert_eq!(plan(&["round-vm/src/recipe.rs"], "", ""), super::super::PreflightPlan::Skip);
    }
}
