//! The repo-relative paths a round image is built from. `round-vm`'s recipe hash reads exactly
//! these files (a test there holds the two together), and a round whose image preflight goes red
//! ejects the members that change one.

pub const CONTAINERFILE: &str = "spira/testenv/Containerfile";

pub const PATHS: [&str; 6] = [
    "round-vm/src/template.sh",
    "round-vm/ami-setup.sh",
    "rust-toolchain.toml",
    "flake.nix",
    "flake.lock",
    CONTAINERFILE,
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

/// The bytes of a recipe input that change the image it builds. Only the Containerfile loses
/// anything (blank and comment lines); the others hold heredocs, `#` inside strings, or JSON.
pub fn image_text(path: &str, text: &str) -> String {
    if path != CONTAINERFILE {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if let Some(c) = t.strip_prefix('#') {
            if is_directive(c) {
                return text.to_string();
            }
            continue;
        }
        if t.trim().is_empty() {
            continue;
        }
        if t.contains("<<") {
            return text.to_string();
        }
        out.push_str(line);
    }
    out
}

fn is_directive(comment: &str) -> bool {
    let c = comment.trim_start().to_ascii_lowercase();
    ["escape", "syntax", "check"].iter().any(|k| {
        c.strip_prefix(k).is_some_and(|r| r.trim_start().starts_with('='))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CF: &str = "# top\nFROM a\n\n  # note\nRUN x \\\n  # inner\n  && y\n";

    #[test_id(tx344un)]
    #[test]
    fn the_containerfile_loses_blank_and_comment_lines_only() {
        assert_eq!(image_text(CONTAINERFILE, CF), "FROM a\nRUN x \\\n  && y\n");
    }

    #[test_id(tkl14s3)]
    #[test]
    fn a_heredoc_or_parser_directive_keeps_the_containerfile_whole() {
        for t in [
            "RUN cat <<EOF\n# body\nEOF\n",
            "# escape=`\nFROM a\n",
            "#Syntax = x\nFROM a\n",
            "FROM a\n# check=skip=all\n",
        ] {
            assert_eq!(image_text(CONTAINERFILE, t), t);
        }
    }

    #[test_id(t9ajee6)]
    #[test]
    fn every_other_recipe_input_is_returned_unchanged() {
        let t = "# c\n\ncat <<EOF\n\"# s\"\n";
        for p in PATHS.iter().filter(|p| **p != CONTAINERFILE) {
            assert_eq!(image_text(p, t), t, "{p}");
        }
    }

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
