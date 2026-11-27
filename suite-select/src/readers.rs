//! Repo paths outside a crate's directory that the crate's tests read: the one map the gate's
//! crate selection and the batcher's red attribution share.

/// `(path or directory prefix ending in '/', crates whose tests read it)`.
const READERS: &[(&str, &[&str])] = &[
    ("systemd/", &["release", "spira-world"]),
    ("spira/pre-activate.sh", &["release", "spira-lc"]),
    ("spira/runes/", &["spira-config", "dwarf"]),
    ("spira/conf.d/", CONFIG_READERS),
    ("spira/conf.toml.d/", CONFIG_READERS),
    ("spira/config-delta.toml", CONFIG_READERS),
    ("spira/config-delta.d/", CONFIG_READERS),
];

/// Crates whose tests read the tree's config registry or delta.
pub const REGISTRY_READERS: &[&str] = &["bead", "gate", "loom", "queue", "release", "spira-config", "spira-runtime", "watchdog"];
const CONFIG_READERS: &[&str] = REGISTRY_READERS;

/// The workspace crates whose tests read `path`, whatever directory the path sits in.
pub fn readers_of(path: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = vec![];
    for (key, crates) in READERS {
        let hit = if key.ends_with('/') { path.starts_with(key) } else { path == *key };
        if hit {
            for c in *crates {
                if !out.contains(c) {
                    out.push(c);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t2usiy1)]
    #[test]
    fn a_shipped_rune_is_read_by_spira_config_and_dwarf() {
        let r = readers_of("spira/runes/disabled/qa.rune");
        assert!(r.contains(&"spira-config") && r.contains(&"dwarf"), "{r:?}");
    }

    #[test_id(taaz8i7)]
    #[test]
    fn a_registry_key_and_a_delta_file_are_read_by_release_and_an_ordinary_source_file_by_none() {
        for p in ["spira/conf.d/SPIRA_TASK_MODES", "spira/config-delta.toml", "spira/config-delta.d/x.toml", "spira/conf.toml.d/y"] {
            assert!(readers_of(p).contains(&"release"), "{p}");
        }
        assert!(readers_of("release/src/lib.rs").is_empty());
        assert!(readers_of("spira/config-delta.toml.bak").is_empty());
    }

    #[test_id(tmq7m7y)]
    #[test]
    fn the_pre_activate_script_and_the_units_keep_their_readers() {
        assert_eq!(readers_of("spira/pre-activate.sh"), ["release", "spira-lc"]);
        assert_eq!(readers_of("systemd/a.service"), ["release", "spira-world"]);
    }
}
