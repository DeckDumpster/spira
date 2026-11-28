use super::*;

#[test_id(t4oa2rs)]
#[test]
fn requirements_follow_cargos_ranges() {
    let cases = [
        ("0.3", "0.3.9", true),
        ("0.3", "0.4.0", false),
        ("1", "1.9.0", true),
        ("1", "2.0.0", false),
        ("^1.2.3", "1.2.2", false),
        ("~1.2", "1.2.9", true),
        ("~1.2", "1.3.0", false),
        ("=1.2.3", "1.2.4", false),
        (">=1.2, <1.5", "1.4.9", true),
        (">=1.2, <1.5", "1.5.0", false),
        ("0.0.3", "0.0.4", false),
        ("*", "9.9.9", true),
        ("1.*", "1.7.0", true),
    ];
    for (req, v, want) in cases {
        assert_eq!(semver::admits(req, v), Some(want), "{req} vs {v}");
    }
    assert_eq!(semver::admits("not a req", "1.0.0"), None);
}

#[test_id(t24ghdh)]
#[test]
fn the_requirement_and_lock_are_read_from_the_lines_naming_the_crate() {
    let m = "[dependencies]\nserde = \"9\"\ndumpster-kit-extra = \"7\"\ndumpster-kit = { version = \"0.3\", registry = \"r\" }\n";
    assert_eq!(parse_requirement(m, "dumpster-kit").as_deref(), Some("0.3"));
    assert_eq!(parse_requirement("dumpster-kit = \"1.2\"\n", "dumpster-kit").as_deref(), Some("1.2"));
    assert_eq!(parse_requirement("dumpster-kit = { git = \"u\" }", "dumpster-kit"), None);
    let lock = "[[package]]\nname = \"dumpster-kit-extra\"\nversion = \"7.0.0\"\n\n[[package]]\nname = \"dumpster-kit\"\nversion = \"0.3.4\"\n";
    assert_eq!(parse_locked(lock, "dumpster-kit").as_deref(), Some("0.3.4"));
    assert_eq!(parse_locked(lock, "absent"), None);
}

#[test_id(tpnbefn)]
#[test]
fn the_newest_release_ignores_yanked_and_pre_release_versions() {
    let idx = "{\"vers\":\"0.3.0\"}\n{\"vers\":\"0.10.0\"}\n{\"vers\":\"0.11.0\",\"yanked\":true}\n{\"vers\":\"0.12.0-rc.1\"}\n";
    assert_eq!(newest_release(idx).as_deref(), Some("0.10.0"));
    assert_eq!(newest_release("garbage"), None);
}

mod registry {
    use crate::real::index_path;

    #[test_id(tpxt7gr)]
    #[test]
    fn index_paths_follow_the_sparse_layout() {
        assert_eq!(index_path("a"), "1/a");
        assert_eq!(index_path("ab"), "2/ab");
        assert_eq!(index_path("abc"), "3/a/abc");
        assert_eq!(index_path("Dumpster-Kit"), "du/mp/dumpster-kit");
    }
}
