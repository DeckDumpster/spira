    use super::*;

    #[test]
    fn resolve_tool_without_spira_release_refuses_naming_the_key() {
        let e = resolve_tool("spira-lc", None).unwrap_err();
        assert!(e.contains("SPIRA_RELEASE is not set"), "{e}");
        assert!(resolve_tool("spira-lc", Some("")).unwrap_err().contains("SPIRA_RELEASE"));
    }

    #[test]
    fn resolve_tool_never_resolves_on_path_or_by_bare_name() {
        let t = testkit::TempDir::new("resolve-tool-nopath");
        std::fs::create_dir_all(t.join("bin")).unwrap();
        let e = resolve_tool("sh", Some(&t.path().display().to_string())).unwrap_err();
        assert!(e.contains("SPIRA_RELEASE") && e.contains("sh is not in"), "{e}");
    }

    #[test]
    fn resolve_tool_finds_bin_then_spira_under_the_release() {
        let t = testkit::TempDir::new("resolve-tool");
        for p in [t.join("bin/spira-lc"), t.join("spira/gate.sh"), t.join("bin/queue"), t.join("spira/queue")] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            testkit::write_exe(&p, "");
        }
        let rel = t.path().display().to_string();
        assert_eq!(resolve_tool("spira-lc", Some(&rel)).unwrap(), format!("{rel}/bin/spira-lc"));
        assert_eq!(resolve_tool("gate.sh", Some(&rel)).unwrap(), format!("{rel}/spira/gate.sh"));
        assert_eq!(resolve_tool("queue", Some(&format!("{rel}/"))).unwrap(), format!("{rel}/bin/queue"));
    }

    #[test]
    fn launcher_path_without_spira_release_refuses_naming_the_key() {
        let e = launcher_path(None, Some("/h"), "").unwrap_err();
        assert!(e.contains("SPIRA_RELEASE is not set"), "{e}");
    }

    #[test]
    fn launcher_path_without_home_refuses_naming_the_key() {
        let e = launcher_path(Some("/r"), None, "").unwrap_err();
        assert!(e.contains("HOME is not set"), "{e}");
    }

    #[test]
    fn child_path_env_never_carries_the_inherited_path() {
        let got = child_path_env(Some("/opt/rel"), Some("/h"), "/box/bin").unwrap();
        let path = got.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str());
        assert_eq!(path, Some("/opt/rel/bin:/opt/rel/spira:/h/.local/lib/spira-env/bin:/usr/local/bin:/usr/bin:/bin:/box/bin"));
        let rel = got.iter().find(|(k, _)| k == crate::RELEASE_ENV).map(|(_, v)| v.as_str());
        assert_eq!(rel, Some("/opt/rel"));
    }

    #[test]
    fn a_build_child_runs_from_the_stage_it_builds() {
        let got = stage_child_path_env(Path::new("/w/stage"));
        assert_eq!(got, vec![(crate::RELEASE_ENV.to_string(), "/w/stage".to_string()), ("PATH".to_string(), "/w/stage/bin:/w/stage/spira:/usr/local/bin:/usr/bin:/bin".to_string())]);
    }

    #[test]
    fn child_path_env_refuses_a_tail_inside_a_release() {
        assert!(child_path_env(Some("/opt/rel"), Some("/h"), "/x/spira-releases/abc/bin").is_err());
    }
