
    // ---- layered config (one source, base:override) ----
    fn layer(dir: &std::path::Path, name: &str, text: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p.display().to_string()
    }

    #[test]
    fn an_override_changes_only_its_own_keys() {
        let d = testkit::TempDir::new("spira-config-layer-keys");
        let base = layer(d.path(), "base.toml", "[spira]\nid_prefix = \"sp\"\nlanes_max_live = 2\n");
        let over = layer(d.path(), "t.override.toml", "[spira]\nlanes_max_live = 5\n");
        let doc = load(std::path::Path::new(&format!("{base}:{over}"))).unwrap();
        let m = spira_string_map(&doc);
        assert_eq!(m.get("LANES_MAX_LIVE").map(String::as_str), Some("5"), "the override wins");
        assert_eq!(m.get("ID_PREFIX").map(String::as_str), Some("sp"), "the base fills the rest");
    }

    #[test]
    fn nested_tables_merge_key_by_key() {
        let d = testkit::TempDir::new("spira-config-layer-nested");
        let base = layer(d.path(), "base.toml", "[repo.alpha]\npath = \"/a\"\nmode = \"queue.local\"\nbase = \"local/main\"\n");
        let over = layer(d.path(), "o.toml", "[repo.alpha]\npath = \"/b\"\n");
        let doc = load(std::path::Path::new(&format!("{base}:{over}"))).unwrap();
        let rows = repos::rows_from_toml(&doc);
        assert_eq!(rows.iter().find(|r| r.name == "alpha").map(|r| r.path.as_str()), Some("/b"));
        assert_eq!(rows.iter().find(|r| r.name == "alpha").map(|r| r.base.as_str()), Some("local/main"), "unset keys keep the base's value");
    }

    #[test]
    fn a_missing_layer_is_refused_naming_it() {
        let d = testkit::TempDir::new("spira-config-layer-missing");
        let base = layer(d.path(), "base.toml", "[spira]\n");
        let gone = d.path().join("gone.toml").display().to_string();
        let err = load(std::path::Path::new(&format!("{base}:{gone}"))).unwrap_err();
        assert!(err.contains("gone.toml"), "{err}");
    }
    use super::*;

    #[test]
    fn the_launcher_path_is_the_release_then_the_system_dirs_and_unset_is_refused() {
        assert_eq!(
            release_path("/r/spira-releases/abc"),
            "/r/spira-releases/abc/bin:/r/spira-releases/abc/spira:/usr/local/bin:/usr/bin:/bin"
        );
        assert_eq!(release_path_from_env(Some("/h/")).unwrap(), "/h/bin:/h/spira:/usr/local/bin:/usr/bin:/bin");
        for v in [None, Some(""), Some("  ")] {
            assert!(release_path_from_env(v).unwrap_err().contains("SPIRA_RELEASE is not set"));
        }
    }

    #[test]
    fn release_path_with_tail_appends_after_the_system_dirs() {
        assert_eq!(
            release_path_with_tail("/r/spira-releases/abc", "/h/.local/bin:/h/.cargo/bin").unwrap(),
            "/r/spira-releases/abc/bin:/r/spira-releases/abc/spira:/usr/local/bin:/usr/bin:/bin:/h/.local/bin:/h/.cargo/bin"
        );
        // An empty (or all-whitespace) tail changes nothing — same as before this bead.
        for empty in ["", "   "] {
            assert_eq!(release_path_with_tail("/r/spira-releases/abc", empty).unwrap(), release_path("/r/spira-releases/abc"));
        }
    }

    #[test]
    fn release_path_with_tail_refuses_a_tail_entry_inside_a_release() {
        let e = release_path_with_tail("/r/spira-releases/abc", "/r/spira-releases/def/bin").unwrap_err();
        assert!(e.contains("spira-releases"), "{e}");
        assert!(e.contains("/r/spira-releases/def/bin"), "{e}");
    }

    #[test]
    fn release_path_with_tail_refuses_a_tail_entry_inside_a_checkout() {
        let dir = testkit::TempDir::new("spira-config-tail-checkout");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let e = release_path_with_tail("/r/spira-releases/abc", bin.to_str().unwrap()).unwrap_err();
        assert!(e.contains("checkout"), "{e}");
        assert!(e.contains(".git"), "{e}");
    }

    #[test]
    fn release_path_from_env_with_tail_composes_both_refusals() {
        assert_eq!(
            release_path_from_env_with_tail(Some("/h/"), "/x/y").unwrap(),
            "/h/bin:/h/spira:/usr/local/bin:/usr/bin:/bin:/x/y"
        );
        assert!(release_path_from_env_with_tail(None, "/x/y").unwrap_err().contains("SPIRA_RELEASE is not set"));
        assert!(release_path_from_env_with_tail(Some("/h/"), "/r/spira-releases/def").unwrap_err().contains("spira-releases"));
    }

    #[test]
    fn a_retired_key_the_registry_in_force_declares_is_kept() {
        let tmp = testkit::TempDir::new("retired-kept");
        let dir = tmp.path().to_path_buf();
        let conf_d = dir.join("conf.d");
        std::fs::create_dir_all(dir.join("conf.toml.d")).unwrap();
        std::fs::create_dir_all(&conf_d).unwrap();
        std::fs::write(conf_d.join("SPIRA_UNIT_TEST_SLOW_SECS"), "TYPE=u32\nDOC=d\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_UNIT_TEST_SLOW_SECS:=600}\"\nSPIRA_CONF_DEFAULT_EOF\n").unwrap();
        let declaring = registry::Registry::from_dirs(&dir).unwrap();
        let text = "[spira]\nunit_test_slow_secs = 9\n";
        let (doc, w) = validate_with(text, &declaring).unwrap();
        assert!(doc.spira.unwrap().0.contains_key("unit_test_slow_secs") && w.is_empty(), "{w:?}");
        std::fs::remove_dir_all(&conf_d).unwrap();
        std::fs::create_dir_all(&conf_d).unwrap();
        let (doc, w) = validate_with(text, &registry::Registry::from_dirs(&dir).unwrap()).unwrap();
        assert!(doc.spira.is_none_or(|s| !s.0.contains_key("unit_test_slow_secs")), "control: no registry declares it, so it is dropped");
        assert!(w.iter().any(|w| w.contains("unit_test_slow_secs is retired")), "{w:?}");
    }

    #[test]
    fn the_retired_lifecycle_switch_refuses_off_and_warns_on() {
        assert_eq!(check_lifecycle_switch_env(None), Ok(None));
        assert_eq!(check_lifecycle_switch_env(Some("")), Ok(None));
        assert!(check_lifecycle_switch_env(Some("1")).unwrap().unwrap().contains("retired"));
        assert!(check_lifecycle_switch_env(Some("true")).unwrap().is_some());
        for off in ["0", "false", "yes", "TRUE"] {
            let e = check_lifecycle_switch_env(Some(off)).unwrap_err();
            assert!(e.contains("sp-v62vn") && e.contains("Exit: remove it"), "{e}");
        }
        let (_, w) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nlifecycle_enforce = true\n").unwrap();
        assert!(w.iter().any(|w| w.contains("lifecycle_enforce is retired (sp-v62vn)")), "{w:?}");
        let e = validate("[spira]\nid_prefix = \"sp\"\nlifecycle_enforce = false\n").unwrap_err();
        assert!(e.contains("spira.lifecycle_enforce") && e.contains("no off") && e.contains("Exit:"), "{e}");
        assert!(validate("[spira]\nlifecycle_enforce = \"off\"\n").is_err());
    }

    #[test]
    fn valid_minimal_document() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmax_dwarves = 4\n").expect("valid");
        assert_eq!(doc.spira.unwrap().get_u32("SPIRA_MAX_DWARVES").unwrap(), Some(4));
    }

    #[test]
    fn unknown_key_names_its_path() {
        let (_, w) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nbogus = 1\n").unwrap();
        assert!(w.iter().any(|m| m.starts_with("spira.bogus") && m.contains("newer release")), "{w:?}");
        let err = validate("[spira]\nid_prefix = \"sp\"\nmax_dwarves = \"four\"\nbogus = 1\n").unwrap_err();
        assert!(err.starts_with("spira.max_dwarves"), "{err}");
    }

    #[test]
    fn wrong_type_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nmax_dwarves = \"four\"\n").unwrap_err();
        assert!(err.starts_with("spira.max_dwarves"), "{err}");
    }

    #[test]
    fn a_wrongly_typed_enum_names_its_path() {
        let err = validate("[spira]\nid_prefix = \"sp\"\nczar_stage_deadlock = 3\n").unwrap_err();
        assert!(err.starts_with("spira.czar_stage_deadlock"), "{err}");
    }

    #[test]
    fn missing_required_repo_field() {
        let err = validate("[repo.home]\nmode = \"push\"\n").unwrap_err();
        assert!(err.starts_with("repo.home"), "{err}");
    }

    #[test]
    fn missing_required_persona_field() {
        let err = validate("[persona.builder]\ntools = [\"Bash\"]\n").unwrap_err();
        assert!(err.starts_with("persona.builder"), "{err}");
    }

    #[test]
    fn the_inline_comment_scar_is_refused_unquoted() {
        // sp-upkae's own scar: an inline `#` comment after a bare, unquoted value used to
        // be silently absorbed into that value by conf.sh's KEY=value reader. TOML has no
        // such ambiguity to inherit: a bare word is not a legal value at all, so the same
        // line is a hard parse error here rather than a value nobody refused.
        let err = validate("[spira]\nid_prefix = \"sp\"\ndb = /home/x # a trailing comment\n").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn the_inline_comment_scar_is_harmless_quoted() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\ndb = \"/home/x\" # a trailing comment\n").expect("valid");
        assert_eq!(doc.spira.unwrap().get_str("SPIRA_DB").unwrap(), Some("/home/x".to_string()));
    }

    #[test]
    fn get_path_reads_nested_tables() {
        let doc =
            validate("[repo.home]\npath = \"/srv/checkouts/home\"\nmode = \"push\"\n").unwrap();
        assert_eq!(get_path(&doc, "repo.home.mode"), Some("push".to_string()));
        assert_eq!(get_path(&doc, "repo.home.base"), None);
    }

    #[test]
    fn export_sh_quotes_and_uppercases() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nhome_repo = \"a b\"\nmax_dwarves = 4\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("HOME_REPO='a b'\n"), "{out}");
        assert!(out.contains("MAX_DWARVES='4'\n"), "{out}");
    }

    #[test]
    fn export_sh_renders_a_toml_bool_as_the_word_true_not_1() {
        // sp-9hwim: a caller reading a bool key off export --sh (mail.sh's SPIRA_MAIL_MUTE)
        // must match on "1|true", the same spelling every other [spira] bool key already
        // uses — never a bare `= "1"`, which a TOML `true` would silently fail.
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmail_mute = true\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("MAIL_MUTE='true'\n"), "{out}");
    }

    #[test]
    fn set_path_on_empty_document_creates_the_table() {
        let doc = set_path(&SpiraToml::default(), "spira.prod", "/srv/x").unwrap();
        assert_eq!(doc.spira.unwrap().get_str("SPIRA_PROD").unwrap(), Some("/srv/x".to_string()));
    }

    #[test]
    fn set_path_replaces_without_disturbing_siblings() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nprod = \"/old\"\nmax_dwarves = 4\n").unwrap();
        let doc = set_path(&doc, "spira.prod", "/new").unwrap();
        let spira = doc.spira.unwrap();
        assert_eq!(spira.get_str("SPIRA_PROD").unwrap(), Some("/new".to_string()));
        assert_eq!(spira.get_u32("SPIRA_MAX_DWARVES").unwrap(), Some(4));
    }

    #[test]
    fn set_path_refuses_an_unknown_field() {
        let err = set_path(&SpiraToml::default(), "spira.bogus", "x").unwrap_err();
        assert!(err.starts_with("spira.bogus"), "{err}");
    }

    #[test]
    fn a_retired_key_warns_instead_of_erroring() {
        let (doc, warnings) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nqueue_local_gate = 1\n")
            .expect("a retired key must not be a hard error");
        assert!(doc.spira.is_some());
        assert!(
            warnings.iter().any(|w| w.contains("queue_local_gate") && w.contains("sp-vsob2")),
            "{warnings:?}"
        );
    }

    #[test]
    fn the_retired_repo_gate_key_warns_and_is_ignored() {
        let (doc, warnings) = validate_with_warnings(
            "[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngate = \"bash spira/x.sh\"\n",
        )
        .expect("a retired repo key must not be a hard error");
        assert!(doc.repo.contains_key("spira"));
        assert!(
            warnings.iter().any(|w| w.contains("repo.spira.gate") && w.contains("sp-quu2w")),
            "{warnings:?}"
        );
        let (_, w) = validate_with_warnings("[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngatee = 1\n").unwrap();
        assert!(w.iter().any(|m| m.starts_with("repo.spira.gatee") && !m.contains("retired")), "{w:?}");
    }

    #[test]
    fn unsetting_a_retired_key_removes_it_instead_of_refusing() {
        let text = "[repo.spira]\npath = \"/p\"\nmode = \"push\"\ngate = \"bash spira/x.sh\"\n";
        let doc = validate(text).unwrap();
        let out = toml::to_string_pretty(&unset_path(&doc, "repo.spira.gate").unwrap()).unwrap();
        assert!(!out.contains("gate"), "{out}");
        assert!(out.contains("path = \"/p\""), "{out}");
        assert!(unset_path(&doc, "spira.aeon_cpu_quota").is_ok()); // ffx-ok
        // Positive control: an unknown, never-retired key is still refused.
        assert!(unset_path(&doc, "repo.spira.gatee").is_err());
    }

    #[test]
    fn retired_cpu_quota_keys_warn_and_are_ignored() {
        // sp-b4oct: a live spira.toml still carrying the dwarf/landing CPU quota validates,
        // with one warning per key naming the retiring bead, and nothing exports them.
        let (doc, warnings) = validate_with_warnings(
            "[spira]\nid_prefix = \"sp\"\naeon_cpu_quota = \"400\"\nland_cpu_quota = \"70\"\n",
        )
        .expect("a retired CPU quota key must not be a hard error");
        for key in ["aeon_cpu_quota", "land_cpu_quota"] { // ffx-ok
            assert!(
                warnings.iter().any(|w| w.contains(key) && w.contains("sp-b4oct")),
                "no warning for {key}: {warnings:?}"
            );
        }
        let sh = export_sh(&doc);
        assert!(!sh.contains("CPU_QUOTA"), "a retired key leaked into export --sh: {sh}");
    }

    #[test]
    fn an_unknown_enum_value_loads_with_a_warning_and_refuses_only_its_reader() {
        let doc_text = "[spira]\nid_prefix = \"sp\"\nczar_stage_deadlock = \"someday\"\n[repo.r]\npath = \"/p\"\nmode = \"push\"\nlanes = [\"plan\", \"future-lane\"]\ngate_mode = \"some-future-mode\"\n";
        let (doc, w) = validate_with_warnings(doc_text).unwrap();
        for k in ["repo.r.gate_mode", "repo.r.lanes[1]", "spira.czar_stage_deadlock"] {
            assert!(w.iter().any(|m| m.starts_with(k)), "{k}: {w:?}");
        }
        assert_eq!(doc.repo["r"].lanes, vec![Lane::Plan]);
        let err = repo_gate_mode(&doc, "r").unwrap_err();
        assert!(err.contains("repo.r.gate_mode") && err.contains("some-future-mode"), "{err}");
        assert_eq!(repo_gate_mode(&doc, "other").unwrap(), GateMode::Suites);
        let err = validate("[repo.r]\npath = \"/p\"\nmode = \"teleport\"\n").unwrap_err();
        assert!(err.contains("repo.r.mode") && err.contains("teleport"), "{err}");
    }

    #[test]
    fn a_rewrite_never_drops_a_value_this_release_does_not_know() {
        let text = format!("{REPO_R}gate_mode = \"some-future-mode\"\n");
        let doc = validate(&text).unwrap();
        let edited = edit_text(&text, &doc, &set_path(&doc, "repo.r.format", "x").unwrap()).unwrap();
        assert!(edited.contains("some-future-mode") && edited.contains("format"), "{edited}");
        let dir = testkit::TempDir::new("unknown-value");
        let p = dir.path().join("spira.toml");
        let err = serialize_and_write(&p, &doc).unwrap_err();
        assert!(err.contains("repo.r.gate_mode") && !p.exists(), "{err}");
    }

    #[test]
    fn a_misspelt_key_warns_as_unknown_but_a_bad_known_value_errors() {
        let (_, w) = validate_with_warnings("[spira]\nid_prefix = \"sp\"\nqueue_local_gatee = 1\n").unwrap();
        assert!(w.iter().any(|m| m.starts_with("spira.queue_local_gatee") && !m.contains("retired")), "{w:?}");
        let err = validate("[spira]\nid_prefix = \"sp\"\nqueue_local_gatee = 1\nmax_dwarves = true\n").unwrap_err();
        assert!(err.starts_with("spira.max_dwarves"), "{err}");
    }

    #[test]
    fn missing_from_retirement_catches_a_dropped_key() {
        use std::collections::BTreeSet;
        let active: BTreeSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let mut history = active.clone();
        history.insert("queue_local_gate".to_string());
        // Dropped, but listed in RETIRED_SPIRA_KEYS — not an offender.
        assert!(missing_from_retirement(&history, &active).is_empty());

        // Planted offender: dropped from `active` and never retired.
        history.insert("never_retired".to_string());
        assert_eq!(
            missing_from_retirement(&history, &active),
            vec!["never_retired".to_string()]
        );
    }

    #[test]
    fn set_path_coerces_a_numeric_field() {
        let doc = set_path(&SpiraToml::default(), "spira.max_live_dwarves", "3").unwrap();
        assert_eq!(doc.spira.unwrap().get_u32("SPIRA_MAX_LIVE_DWARVES").unwrap(), Some(3));
    }

    #[test]
    fn set_path_coerces_a_bool_field() {
        let doc = set_path(&SpiraToml::default(), "spira.mail_mute", "true").unwrap();
        assert_eq!(doc.spira.unwrap().get_bool("SPIRA_MAIL_MUTE").unwrap(), Some(true));
    }

    #[test]
    fn set_path_keeps_a_numeric_looking_string_field_a_string() {
        // `instance` is a string field; a value that happens to parse as JSON must not be
        // coerced away from the string the schema actually wants.
        let doc = set_path(&SpiraToml::default(), "spira.instance", "123").unwrap();
        assert_eq!(doc.spira.unwrap().get_str("SPIRA_INSTANCE").unwrap(), Some("123".to_string()));
    }

    #[test]
    fn unset_path_removes_the_key_and_keeps_siblings() {
        let doc = validate("[spira]\nid_prefix = \"sp\"\nmax_live_dwarves = 3\nmax_dwarves = 4\n").unwrap();
        let doc = unset_path(&doc, "spira.max_live_dwarves").unwrap();
        // omitted entirely from the serialized document, not written as an empty value.
        let out = toml::to_string_pretty(&doc).unwrap();
        assert!(!out.contains("max_live_dwarves"), "{out}");
        let spira = doc.spira.unwrap();
        assert_eq!(spira.get_u32("SPIRA_MAX_LIVE_DWARVES").unwrap(), None);
        assert_eq!(spira.get_u32("SPIRA_MAX_DWARVES").unwrap(), Some(4));
    }

    // Env edits go through `testkit::env`, which serializes every test in this binary.
    static SCRATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn scratch_dir(tag: &str) -> testkit::TempDir {
        let n = SCRATCH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        testkit::TempDir::new(&format!("spira-config-lib-test-{tag}-{n}"))
    }

    #[test]
    fn find_under_locates_the_file() {
        let dir = scratch_dir("find-under-present");
        std::fs::write(dir.join(FILE_NAME), "[spira]\n").unwrap();
        assert_eq!(find_under(&dir), Some(dir.join(FILE_NAME)));
    }

    #[test]
    fn find_under_is_none_when_absent() {
        // POSITIVE CONTROL for find_under_locates_the_file: an empty scratch dir must not
        // report a file that was never written.
        let dir = scratch_dir("find-under-absent");
        assert_eq!(find_under(&dir), None);
    }

    #[test]
    fn discover_prefers_the_explicit_path_over_the_environment() {
        let _env = testkit::env(&[("SPIRA_TOML", Some("/should-not-be-used/spira.toml"))]);
        let explicit = PathBuf::from("/explicit/spira.toml");
        assert_eq!(discover(Some(explicit.clone())), Some(explicit));
    }

    #[test]
    fn discover_never_falls_back_through_spira_repo() {
        // REGRESSION (sp-hconl): discover() used to offer $SPIRA_REPO/spira.toml as a
        // candidate (sp-cx0mj); sp-9hwim's bash rewrite deliberately dropped that tier from
        // conf.sh a day later ("the running system must not read [from beside the checkout]
        // at all") and this function was never updated to match. A spira.toml sitting right
        // there, with nothing at the XDG/etc tiers, must resolve to None, not that file. The
        // tier logic itself is exercised in full in `locate`'s own tests; this is the
        // public-wrapper regression guard.
        if Path::new("/etc/spira/spira.toml").is_file() {
            eprintln!("skipping: this machine has a real /etc/spira/spira.toml");
            return;
        }
        let dir = scratch_dir("discover-repo");
        let repo_dir = dir.join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(repo_dir.join(FILE_NAME), "[spira]\n").unwrap();
        let empty_home = dir.join("home-empty");
        std::fs::create_dir_all(&empty_home).unwrap();
        let _env = testkit::env(&[
            ("SPIRA_TOML", None),
            ("XDG_CONFIG_HOME", None),
            ("SPIRA_REPO", repo_dir.to_str()),
            ("HOME", empty_home.to_str()),
        ]);
        assert_eq!(discover(None), None);
    }

    #[test]
    fn load_reads_and_validates() {
        let dir = scratch_dir("load-ok");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[spira]\nid_prefix = \"sp\"\nmax_dwarves = 4\n").unwrap();
        let doc = load(&path).expect("valid document");
        assert_eq!(doc.spira.unwrap().get_u32("SPIRA_MAX_DWARVES").unwrap(), Some(4));
    }

    #[test]
    fn load_names_the_path_on_a_parse_error() {
        let dir = scratch_dir("load-bad");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[spira]\nid_prefix = \"sp\"\nmax_dwarves = true\n").unwrap();
        let err = load(&path).unwrap_err();
        assert!(err.contains(&path.display().to_string()), "{err}");
    }

    #[test]
    fn set_paths_in_file_writes_both_or_neither() {
        let dir = testkit::TempDir::new("spira-config-setpaths");
        let path = dir.join("spira.toml");
        std::fs::write(&path, "[repo.r]\npath = \"/x\"\nmode = \"queue.local\"\nbase = \"local/main\"\n").unwrap();
        set_paths_in_file(&path, &[("repo.r.mode", "queue.forge"), ("repo.r.base", "origin/main")]).unwrap();
        let doc = load(&path).unwrap();
        assert_eq!(get_path(&doc, "repo.r.mode").as_deref(), Some("queue.forge"));
        assert_eq!(get_path(&doc, "repo.r.base").as_deref(), Some("origin/main"));
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(set_paths_in_file(&path, &[("repo.r.base", "local/main"), ("repo.r.mode", "bogus")]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------ gate_mode (sp-2ghui)

    const REPO_R: &str = "[repo.r]\npath = \"/x\"\nmode = \"queue.local\"\n";

    #[test]
    fn gate_mode_defaults_to_suites_when_absent() {
        let doc = validate(REPO_R).unwrap();
        assert_eq!(repo_gate_mode(&doc, "r").unwrap(), GateMode::Suites);
        assert_eq!(repo_gate_mode(&doc, "no-such-repo").unwrap(), GateMode::Suites);
        assert_eq!(get_path(&doc, "repo.r.gate_mode"), None);
    }

    #[test]
    fn gate_mode_reads_every_value() {
        for (v, want) in [("unit", GateMode::Unit), ("suites", GateMode::Suites), ("fences", GateMode::Fences)] {
            let doc = validate(&format!("{REPO_R}gate_mode = \"{v}\"\n")).unwrap();
            assert_eq!(repo_gate_mode(&doc, "r").unwrap(), want);
            assert_eq!(want.as_str(), v);
            assert_eq!(get_path(&doc, "repo.r.gate_mode").as_deref(), Some(v));
        }
    }

    #[test]
    fn gate_mode_refuses_anything_else() {
        for bad in ["\"Unit\"", "\"fast\"", "\"Fences\"", "\"fence\"", "\"\""] {
            let doc = validate(&format!("{REPO_R}gate_mode = {bad}\n")).unwrap();
            let err = repo_gate_mode(&doc, "r").unwrap_err();
            assert!(err.contains("repo.r.gate_mode"), "{bad}: {err}");
        }
        for bad in ["true", "1"] {
            let err = validate(&format!("{REPO_R}gate_mode = {bad}\n")).unwrap_err();
            assert!(err.contains("repo.r.gate_mode"), "{bad}: {err}");
        }
    }

    #[test]
    fn gate_mode_is_set_and_reverted_through_set_path() {
        let doc = validate(REPO_R).unwrap();
        let on = set_path(&doc, "repo.r.gate_mode", "unit").unwrap();
        assert_eq!(repo_gate_mode(&on, "r").unwrap(), GateMode::Unit);
        let text = toml::to_string_pretty(&on).unwrap();
        assert!(text.contains("gate_mode = \"unit\""), "{text}");
        assert!(set_path(&doc, "repo.r.gate_mode", "fast").is_err());
        let off = set_path(&on, "repo.r.gate_mode", "suites").unwrap();
        assert_eq!(repo_gate_mode(&off, "r").unwrap(), GateMode::Suites);
        let gone = unset_path(&on, "repo.r.gate_mode").unwrap();
        assert_eq!(repo_gate_mode(&gone, "r").unwrap(), GateMode::Suites);
        let fences = set_path(&doc, "repo.r.gate_mode", "fences").unwrap();
        assert_eq!(repo_gate_mode(&fences, "r").unwrap(), GateMode::Fences);
        assert!(toml::to_string_pretty(&fences).unwrap().contains("gate_mode = \"fences\""));
    }

    // ---- sp-oppza: migrate_goal_to_id_prefix — the automatic one-time upgrade migration ----

    #[test]
    fn id_prefix_from_goal_reads_the_prefix_and_refuses_what_is_not_bead_shaped() {
        assert_eq!(id_prefix_from_goal("sp-spira").as_deref(), Some("sp"));
        assert_eq!(id_prefix_from_goal("tt-own-thing").as_deref(), Some("tt"));
        for bad in ["nohyphen", "-leadinghyphen", "sp with space-x", ""] {
            assert_eq!(id_prefix_from_goal(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn migrate_derives_id_prefix_from_a_bead_shaped_goal_and_drops_goal() {
        let (out, msg) = migrate_goal_to_id_prefix("[spira]\ngoal = \"sp-spira\"\ndb = \"/db\"\n")
            .expect("parses")
            .expect("a migration applies");
        assert!(msg.contains("sp-k6m1m") && msg.contains("sp-oppza"), "{msg}");
        let (doc, _) = validate_strict(&out).expect("the migrated document passes the same check doctor/pre-activate run");
        let s = doc.spira.unwrap();
        assert_eq!(s.get_str("SPIRA_ID_PREFIX").unwrap().as_deref(), Some("sp"));
        assert_eq!(s.get_str("SPIRA_DB").unwrap().as_deref(), Some("/db"), "the rest of [spira] survives");
        assert!(!out.contains("goal"), "{out}");
    }

    #[test]
    fn migrate_is_a_true_no_op_once_id_prefix_is_set_by_hand() {
        // Production's own state: id_prefix already set BY HAND. Checked first, so a goal
        // left behind alongside it changes nothing — no rewrite, same as the retired-key
        // warning path already covers.
        assert_eq!(migrate_goal_to_id_prefix("[spira]\nid_prefix = \"sp\"\ngoal = \"sp-spira\"\n").unwrap(), None);
        assert_eq!(migrate_goal_to_id_prefix("[spira]\nid_prefix = \"tt\"\n").unwrap(), None);
    }

    #[test]
    fn migrate_is_a_no_op_with_nothing_to_migrate() {
        assert_eq!(migrate_goal_to_id_prefix("[spira]\ndb = \"/db\"\n").unwrap(), None, "no goal at all");
        assert_eq!(migrate_goal_to_id_prefix("[spira]\ngoal = \"nohyphen\"\n").unwrap(), None, "goal not bead-shaped");
        assert_eq!(migrate_goal_to_id_prefix("[repo.a]\npath = \"/a\"\nmode = \"push\"\n").unwrap(), None, "no [spira] table at all");
        assert_eq!(migrate_goal_to_id_prefix("").unwrap(), None, "empty document");
    }

    #[test]
    fn migrate_in_file_writes_atomically_and_a_second_run_is_silent() {
        let dir = testkit::TempDir::new("spira-config-migrate");
        let path = dir.join("spira.toml");
        std::fs::write(&path, "[spira]\ngoal = \"sp-spira\"\ndb = \"/db\"\n").unwrap();

        let msg = migrate_goal_to_id_prefix_in_file(&path).unwrap().expect("first run migrates");
        assert!(msg.contains("sp-oppza"), "{msg}");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("id_prefix = \"sp\""), "{after}");
        assert!(!after.contains("goal"), "{after}");

        // SECOND RUN IS SILENT: id_prefix is there now, so this is the true no-op path —
        // proof the migration ran exactly once, not on every invocation.
        assert_eq!(migrate_goal_to_id_prefix_in_file(&path).unwrap(), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after, "unchanged on the second run");
    }

    #[test]
    fn migrate_in_file_is_a_no_op_on_a_missing_or_empty_file() {
        let dir = testkit::TempDir::new("spira-config-migrate-missing");
        assert_eq!(migrate_goal_to_id_prefix_in_file(&dir.join("nonexistent.toml")).unwrap(), None);
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "").unwrap();
        assert_eq!(migrate_goal_to_id_prefix_in_file(&empty).unwrap(), None);
    }

    #[test]
    fn set_path_repo_land_writes_the_mode_field() {
        let doc = validate("[repo.home]\npath = \"/srv/home\"\nmode = \"push\"\n").unwrap();
        let doc = set_path(&doc, "repo.home.land", "queue.local").unwrap();
        assert_eq!(doc.repo["home"].mode, LandMode::QueueLocal);
        assert_eq!(get_path(&doc, "repo.home.land"), Some("queue.local".to_string()));
        assert_eq!(get_path(&doc, "repo.home.mode"), Some("queue.local".to_string()));
    }

    #[test]
    fn gate_string_with_quotes_dollar_and_newlines_round_trips_through_toml() {
        let gate = "echo \"hi\" && $(rm -rf /) # not really\nnext line\n\t'quoted'";
        let doc = set_path(
            &validate("[repo.home]\npath = \"/srv/home\"\nmode = \"push\"\n").unwrap(),
            "repo.home.base",
            gate,
        )
        .unwrap();
        let out = toml::to_string_pretty(&doc).expect("serializes");
        let reparsed = validate(&out).expect("the serialized document must still validate");
        assert_eq!(reparsed.repo["home"].base.as_deref(), Some(gate));
    }

    #[test]
    fn is_secret_shaped_matches_credential_shaped_names_case_insensitively() {
        for name in ["token", "BROKER_GH_TOKEN", "db_password", "Credential", "some_dsn"] {
            assert!(is_secret_shaped(name), "{name} should be secret-shaped");
        }
        for name in ["home_repo", "max_dwarves", "gate", "path"] {
            assert!(!is_secret_shaped(name), "{name} should not be secret-shaped");
        }
    }

    #[test]
    fn export_sh_omits_a_secret_shaped_field() {
        let doc = validate("[spira]\nhome_repo = \"a\"\nbroker_gh_token = \"ghp_x\"\n").unwrap();
        let out = export_sh(&doc);
        assert!(out.contains("HOME_REPO="), "{out}");
        assert!(!out.contains("ghp_x"), "{out}");
        assert!(!out.to_uppercase().contains("TOKEN"), "{out}");
    }

    #[test]
    fn atomic_write_start_leaves_the_destination_untouched_until_commit() {
        let tmp = testkit::TempDir::new("spira-config-test");
        let dir = tmp.path().to_path_buf();
        let dest = dir.join("spira.toml");
        std::fs::write(&dest, "original").unwrap();

        let pending = atomic_write_start(&dest, "replacement").expect("temp write succeeds");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "original");

        atomic_write_commit(pending).expect("commit succeeds");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "replacement");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_write_dropped_without_commit_leaves_no_temp_file_and_the_original_intact() {
        let tmp = testkit::TempDir::new("spira-config-test-drop");
        let dir = tmp.path().to_path_buf();
        let dest = dir.join("spira.toml");
        std::fs::write(&dest, "original").unwrap();

        {
            let _pending = atomic_write_start(&dest, "never committed").expect("temp write succeeds");
            // simulates a crash between the temp write and the rename: dropped, never committed
        }
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "original");
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover.is_empty(), "{leftover:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn backup_existing_copies_the_old_file_and_is_a_noop_when_absent() {
        let tmp = testkit::TempDir::new("spira-config-test-backup");
        let dir = tmp.path().to_path_buf();
        let dest = dir.join("spira.toml");

        assert_eq!(backup_existing(&dest).unwrap(), None);

        std::fs::write(&dest, "before").unwrap();
        let backup = backup_existing(&dest).unwrap().expect("a backup path");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "before");
        std::fs::remove_dir_all(&dir).ok();
    }
