    use super::*;

    #[test]
    fn registry_reads_repo_tables_from_spira_toml_when_no_map_file_exists() {
        let doc = crate::validate("[repo.alpha]\npath = \"/tmp/alpha\"\nmode = \"queue.local\"\nbase = \"local/main\"\nlanes = [\"plan\", \"spike\"]\n").unwrap();
        let rows = rows_from_toml(&doc);
        assert_eq!(names(&rows), vec!["alpha".to_string()]);
        assert_eq!(field(&rows, "alpha", Column::Path), "/tmp/alpha");
        assert_eq!(field(&rows, "alpha", Column::Land), "queue.local");
        assert_eq!(field(&rows, "alpha", Column::Base), "local/main");
        assert_eq!(field(&rows, "alpha", Column::Lanes), "plan,spike");
        assert_eq!(field(&rows, "alpha", Column::Gate), "");
        assert_eq!(field(&rows, "nope", Column::Path), "");
    }

    /// The repo map is the one spira.toml declares (spira.repo_map) — never discovered from
    /// XDG, a legacy spira.conf or a shipped example (per Ryan 2026-10-05).
    #[test]
    fn registry_env_takes_the_declared_map() {
        let t = testkit::TempDir::new("repos-registry-env");
        let map = t.path().join("my-repo-map");
        std::fs::write(&map, "spira|/nowhere|local|local/main\n").unwrap();
        let toml = crate::fixture_toml_file(t.path(), &[("SPIRA_REPO_MAP".to_string(), map.display().to_string())].into_iter().collect());
        let home = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let mut env = std::collections::BTreeMap::new();
        env.insert("HOME".to_string(), t.path().display().to_string());
        env.insert("SPIRA_REPO".to_string(), t.path().display().to_string());
        env.insert("SPIRA_TOML".to_string(), toml.display().to_string());
        let repo = std::path::PathBuf::from(env.get("SPIRA_REPO").unwrap());
        if let Err(e) = crate::resolve::resolve_for_process(&home, &repo, &env) { panic!("resolve: {e}"); }
        let out = registry_env(env, &home);
        assert_eq!(out.get("SPIRA_REPO_MAP").map(String::as_str), Some(map.to_str().unwrap()), "{out:?}");
    }

    /// The REGISTERED keys (`SPIRA_REPO_MAP`, `SPIRA_HOME_REPO`) never come from the
    /// environment (per Ryan 2026-10-05: one source of config) — an inherited copy is
    /// dropped and only what config resolves survives (here nothing: no SPIRA_TOML).
    /// `SPIRA_REPO` is a per-copy fact, not config, and is kept; `SPIRA_REPO_DERIVED`
    /// (sp-8bhnr) is always recomputed from `home`, never the stale "0".
    #[test]
    fn registry_env_ignores_the_environments_copy_of_a_registered_key() {
        let mut env = std::collections::BTreeMap::new();
        for (k, v) in [("SPIRA_REPO_MAP", "/a"), ("SPIRA_HOME_REPO", "h"), ("SPIRA_REPO", "/r"), ("SPIRA_REPO_DERIVED", "0"), ("SPIRA_TOML", "/nonexistent/spira.toml")] {
            env.insert(k.to_string(), v.to_string());
        }
        let home = std::path::Path::new("/nonexistent/spira");
        let out = registry_env(env.clone(), home);
        assert_eq!(out.get("SPIRA_REPO_MAP"), None, "{out:?}");
        assert_eq!(out.get("SPIRA_HOME_REPO"), None, "{out:?}");
        assert_eq!(out.get("SPIRA_REPO").map(String::as_str), Some("/r"));
        assert_eq!(out.get("SPIRA_REPO_DERIVED").map(String::as_str), Some("/nonexistent/spira"));
    }

    /// sp-8bhnr (P0, LOOP-STOPPING): CHECK6's own dispatch forwards its OWN resolved
    /// `SPIRA_REPO`/`SPIRA_REPO_MAP`/`SPIRA_HOME_REPO` into the landing-pass worker it
    /// starts, but never `SPIRA_REPO_DERIVED` (`sentinel/src/dispatch.rs`'s `setenv` list
    /// omits it). Sentinel's own `home` under its unit is a release's bundled, non-checkout
    /// `spira/`, so its own `SPIRA_REPO` derives to the release ROOT — and when that gets
    /// forwarded into landing-pass, which shares the SAME `SPIRA_HOME`,
    /// [`repo_override`] used to read the missing `SPIRA_REPO_DERIVED` as a deliberate
    /// override and hand back the release root for the home repo outright, instead of its
    /// real configured checkout. Seen red against the pre-fix code (which never backfills
    /// `SPIRA_REPO_DERIVED` at all — `resolve()` never puts it in `Resolved::values`).
    #[test]
    fn registry_resolves_the_configured_checkout_despite_a_forwarded_repo_without_its_derived_pair() {
        let t = testkit::TempDir::new("repos-registry-sp-8bhnr");

        // The REAL configured checkout for the home repo "spira" (production: spira.toml
        // and repo-map both name the real harness checkout; here, a fixture equivalent).
        let checkout = t.path().join("checkouts/spira");
        std::fs::create_dir_all(&checkout).unwrap();
        let map = t.path().join("repo-map");
        std::fs::write(&map, format!("spira | {} | queue.local | local/main |  |\n", checkout.display())).unwrap();

        // The release's own bundled `spira/` — NOT a git checkout (a release layout, not a
        // clone), exactly sp-8bhnr's own repro. Only `conf.d` needs to be real (resolve()
        // reads it unconditionally); borrow this checkout's own, real one.
        let release = t.path().join("spira-releases/deadbeef");
        let release_spira = release.join("spira");
        std::fs::create_dir_all(&release_spira).unwrap();
        std::os::unix::fs::symlink(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d"),
            release_spira.join("conf.d"),
        )
        .unwrap();

        // CHECK6's own forwarded env: SPIRA_REPO = sentinel's own derived value for THIS
        // SAME home (the release root — `derive_repo_filesystem` falls back to `home`'s
        // parent when `git -C home rev-parse --show-toplevel` fails), SPIRA_REPO_MAP and
        // SPIRA_HOME_REPO forwarded too, SPIRA_REPO_DERIVED deliberately absent.
        let home_dir = t.path().join("userhome");
        let mut env = std::collections::BTreeMap::new();
        env.insert("HOME".to_string(), home_dir.display().to_string());
        env.insert("SPIRA_REPO".to_string(), release.canonicalize().unwrap_or_else(|_| release.clone()).display().to_string());
        // The map and the home repo are DECLARED config (the one source), not forwarded env.
        let toml = crate::process::fixture_toml(t.path(), &[("SPIRA_REPO_MAP", &map.display().to_string()), ("SPIRA_HOME_REPO", "spira")]);
        env.insert("SPIRA_TOML".to_string(), toml.display().to_string());

        let reg = Registry::from_env(env, &release_spira);
        assert_eq!(
            reg.root("spira"),
            Some(checkout.display().to_string()),
            "the home repo 'spira' must resolve to its configured checkout, not the forwarded SPIRA_REPO (the release root)"
        );
    }


    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const MAP: &str = "\
# a comment line, skipped
home | /h/home | push | origin/main |  | .claude/gate-home.sh
narrow | /h/narrow
service | /h/service | pr | origin/master | cargo fmt --all | cargo fmt --all -- --check
lanes-on | /h/lanes | push | origin/main | | bash x.sh | plan,incident
lanes-empty | /h/lanesempty | push | origin/main | | bash x.sh |
oldshape | /h/old | pr | fmt-tool | gate --flag
";

    #[test]
    fn parse_skips_comments_and_short_rows_but_keeps_a_two_field_row() {
        let rows = parse(MAP);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["home", "narrow", "service", "lanes-on", "lanes-empty", "oldshape"]);
        let narrow = rows.iter().find(|r| r.name == "narrow").unwrap();
        assert_eq!(narrow.path, "/h/narrow");
        assert_eq!(narrow.land, "");
        assert_eq!(narrow.base, "");
        assert_eq!(narrow.format, "");
        assert_eq!(narrow.gate, "");
    }

    #[test]
    fn six_field_row_reads_base_format_and_gate_by_name_not_position() {
        let rows = parse(MAP);
        let home = rows.iter().find(|r| r.name == "home").unwrap();
        assert_eq!(home.path, "/h/home");
        assert_eq!(home.land, "push");
        assert_eq!(home.base, "origin/main");
        assert_eq!(home.format, "");
        assert_eq!(home.gate, ".claude/gate-home.sh");
    }

    #[test]
    fn gate_may_itself_contain_pipes() {
        let rows = parse(MAP);
        let svc = rows.iter().find(|r| r.name == "service").unwrap();
        assert_eq!(svc.gate, "cargo fmt --all -- --check");
    }

    #[test]
    fn five_field_row_predating_base_reads_format_at_column_four() {
        let rows = parse(MAP);
        let old = rows.iter().find(|r| r.name == "oldshape").unwrap();
        assert_eq!(old.base, "");
        assert_eq!(old.format, "fmt-tool");
        assert_eq!(old.gate, "gate --flag");
    }

    #[test]
    fn a_trailing_identifier_field_is_lanes_not_gate() {
        let rows = parse(MAP);
        let r = rows.iter().find(|r| r.name == "lanes-on").unwrap();
        assert_eq!(r.gate, "bash x.sh");
        assert_eq!(r.lanes, "plan,incident");
    }

    #[test]
    fn an_explicit_empty_trailing_field_is_still_a_lanes_column() {
        let rows = parse(MAP);
        let r = rows.iter().find(|r| r.name == "lanes-empty").unwrap();
        assert_eq!(r.gate, "bash x.sh");
        assert_eq!(r.lanes, "");
    }

    #[test]
    fn field_is_blank_for_an_unknown_name_not_an_error() {
        let rows = parse(MAP);
        assert_eq!(field(&rows, "ghost", Column::Path), "");
    }

    #[test]
    fn names_preserves_file_order() {
        let rows = parse(MAP);
        assert_eq!(names(&rows), vec!["home", "narrow", "service", "lanes-on", "lanes-empty", "oldshape"]);
    }

    #[test]
    fn home_repo_prefers_the_explicit_key_then_falls_back_to_a_basename() {
        assert_eq!(home_repo(&env(&[("SPIRA_HOME_REPO", "brain")]), Path::new("/x/y")), "brain");
        assert_eq!(home_repo(&env(&[("SPIRA_REPO", "/w/harness")]), Path::new("/x/y")), "harness");
        assert_eq!(home_repo(&env(&[]), Path::new("/x/spira")), "spira");
        assert_eq!(home_repo(&env(&[("SPIRA_HOME_REPO", "")]), Path::new("/x/spira/")), "spira");
    }

    #[test]
    fn registry_root_prefers_the_map_and_refuses_an_unmapped_name() {
        let env = env(&[("SPIRA_HOME_REPO", "home")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        assert_eq!(reg.root("home"), Some("/h/home".to_string()));
        assert_eq!(reg.root("ghost"), None);
        assert_eq!(reg.root(""), Some("/h/home".to_string()), "empty name defaults to the home repo");
    }

    #[test]
    fn registry_root_takes_an_explicit_spira_repo_override_for_the_home_repo_only() {
        let env = env(&[("SPIRA_HOME_REPO", "home"), ("SPIRA_REPO", "/override/path"), ("SPIRA_REPO_DERIVED", "/derived")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        assert_eq!(reg.root("home"), Some("/override/path".to_string()));
        // a non-home name is never overridden, even though SPIRA_REPO is set
        assert_eq!(reg.root("service"), Some("/h/service".to_string()));
    }

    #[test]
    fn registry_root_treats_spira_repo_equal_to_derived_as_no_override() {
        let env = env(&[("SPIRA_HOME_REPO", "home"), ("SPIRA_REPO", "/derived"), ("SPIRA_REPO_DERIVED", "/derived")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        // falls through to the map, which has home at /h/home, not /derived
        assert_eq!(reg.root("home"), Some("/h/home".to_string()));
    }

    #[test]
    fn registry_land_defaults_to_push_and_normalizes_queue_forge() {
        let env = env(&[]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        assert_eq!(reg.land("home"), "push");
        assert_eq!(reg.land("ghost"), "push");
        let qf = "q | /q | queue.forge | origin/main | |\n";
        let reg2 = Registry::new(Some(qf), &env, Path::new("/h/home"));
        assert_eq!(reg2.land("q"), "queue");
        assert!(reg2.land_queued("q"));
        assert!(!reg2.land_queued("ghost"));
        assert!(reg2.land_forge_queued("q"));
        let ql = "l | /l | queue.local | main | |\n";
        let reg3 = Registry::new(Some(ql), &env, Path::new("/h/home"));
        assert!(reg3.land_queued("l") && !reg3.land_forge_queued("l"));
        assert!(!reg3.land_forge_queued("ghost"));
    }

    #[test]
    fn registry_land_ignores_a_missing_map_rather_than_refusing() {
        let env = env(&[]);
        let reg = Registry::new(None, &env, Path::new("/h/home"));
        assert_eq!(reg.land("anything"), "push");
        assert!(!reg.map_present());
    }

    #[test]
    fn registry_field_lookups_refuse_outright_when_the_map_is_absent() {
        let base_env = env(&[("SPIRA_HOME_REPO", "home")]);
        let reg = Registry::new(None, &base_env, Path::new("/h/home"));
        assert_eq!(reg.field("home", Column::Path), None);
        assert_eq!(reg.gate("home"), None);
        assert_eq!(reg.format("home"), None);
        assert_eq!(reg.base("home"), None);
        // root is unaffected when the override already answers it...
        let env_ov = env(&[("SPIRA_HOME_REPO", "home"), ("SPIRA_REPO", "/ov"), ("SPIRA_REPO_DERIVED", "/d")]);
        let reg_ov = Registry::new(None, &env_ov, Path::new("/h/home"));
        assert_eq!(reg_ov.root("home"), Some("/ov".to_string()));
        // ...but refuses otherwise, same as any other repo_field-based lookup.
        assert_eq!(reg.root("home"), None);
    }

    #[test]
    fn registry_all_lists_the_home_repo_first_and_excludes_it_from_the_rest() {
        let env = env(&[("SPIRA_HOME_REPO", "home")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        let all = reg.all();
        assert_eq!(all[0], "home");
        assert_eq!(all.iter().filter(|n| n.as_str() == "home").count(), 1);
        assert!(all.contains(&"service".to_string()));
    }

    #[test]
    fn registry_all_always_lists_the_home_repo_even_with_no_map_at_all() {
        let env = env(&[("SPIRA_HOME_REPO", "home")]);
        let reg = Registry::new(None, &env, Path::new("/h/home"));
        assert_eq!(reg.all(), vec!["home".to_string()]);
    }

    #[test]
    fn registry_name_at_matches_literally_and_the_override_maps_to_home() {
        let env = env(&[("SPIRA_HOME_REPO", "home"), ("SPIRA_REPO", "/ov"), ("SPIRA_REPO_DERIVED", "/d")]);
        let reg = Registry::new(Some(MAP), &env, Path::new("/h/home"));
        assert_eq!(reg.name_at("/ov"), Some("home".to_string()));
        assert_eq!(reg.name_at("/h/service"), Some("service".to_string()));
        assert_eq!(reg.name_at("/nowhere"), None);
        assert_eq!(reg.name_at(""), None);
    }

    // ---- base refs (family W, sp-o88bx) ---------------------------------------------------

    fn registry_for(map: &str, home_name: &str) -> Registry {
        Registry::new(Some(map), &env(&[("SPIRA_HOME_REPO", home_name)]), Path::new("/nonexistent"))
    }

    #[test]
    fn ref_branch_strips_up_to_the_first_slash_or_passes_through() {
        assert_eq!(ref_branch("origin/master"), "master");
        assert_eq!(ref_branch("local/main"), "main");
        assert_eq!(ref_branch("main"), "main");
    }

    #[test]
    fn landref_refuses_an_unmapped_name_and_a_missing_git_dir() {
        let reg = registry_for("r | /nope\n", "r");
        assert_eq!(landref(&reg, "ghost"), None, "unmapped name");
        assert_eq!(landref(&reg, "r"), None, "mapped but not a real checkout");
    }

    #[test]
    fn publish_mode_is_pr_unless_the_row_declares_push() {
        let mut reg = Registry::new(None, &env(&[]), Path::new("q"));
        assert_eq!(publish_mode(&reg, "q"), crate::PublishMode::Pr);
        let doc: crate::SpiraToml = toml::from_str("[publish.q]\nmode = \"push\"\n").unwrap();
        reg = reg.with_publish(doc.publish);
        assert_eq!(publish_mode(&reg, "q"), crate::PublishMode::Push);
        assert_eq!(publish_mode(&reg, "other"), crate::PublishMode::Pr);
        assert!(toml::from_str::<crate::SpiraToml>("[publish.q]\nmode = \"ssh\"\n").is_err());
    }

    #[test]
    fn publish_forge_refuses_when_the_land_ref_itself_does_not_resolve() {
        let reg = registry_for("q | /nope\n", "q");
        assert_eq!(publish_forge(&reg, "q", &env(&[])), None);
    }
