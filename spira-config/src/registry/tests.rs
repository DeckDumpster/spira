    #[test]
    fn a_home_missing_either_directory_is_refused_by_path() {
        let dir = testkit::TempDir::new("spira-config-registry-home");
        let conf_d = dir.join("conf.d");
        std::fs::create_dir_all(&conf_d).unwrap();
        assert_eq!(Registry::load(dir.path()).unwrap_err(), format!("no config registry at {}", dir.join("conf.toml.d").display()));
        std::fs::remove_dir(&conf_d).unwrap();
        std::fs::create_dir_all(dir.join("conf.toml.d")).unwrap();
        assert_eq!(Registry::load(dir.path()).unwrap_err(), format!("no config registry at {}", conf_d.display()));
    }

    #[test]
    fn an_unknown_type_is_refused() {
        let dir = testkit::TempDir::new("spira-config-registry-type");
        std::fs::write(dir.join("SPIRA_X"), "TYPE=float\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\nSPIRA_CONF_DEFAULT_EOF\n").unwrap();
        assert!(load(dir.path()).unwrap_err().contains("TYPE=\"float\""));
    }

    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn declares_key_sees_a_source_in_either_directory_and_not_an_absent_one() {
        let dir = testkit::TempDir::new("declares-key");
        for d in SOURCE_DIRS {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("conf.d/SPIRA_A"), "").unwrap();
        std::fs::write(dir.path().join("conf.toml.d/SPIRA_B"), "").unwrap();
        assert!(declares_key(dir.path(), "SPIRA_A") && declares_key(dir.path(), "SPIRA_B"));
        assert!(!declares_key(dir.path(), "SPIRA_C"));
    }

    fn key(name: &str, body: &str) -> RegistryKey {
        RegistryKey {
            name: name.to_string(),
            ty: "string".into(),
            group: "test".into(),
            doc: String::new(),
            required: false,
            default_body: body.to_string(),
            field: field_of(name),
            toml_only: false,
            max: None,
            schema_doc: None,
        }
    }

    /// sp-1cdgq round 3: a missing directory is now a NAMED error, not a silent empty
    /// registry — this is the test that used to assert the opposite. Two base reds
    /// (sp-8qm8g, sp-1cdgq) came from a test fixture's `--home` lacking `conf.d`, and
    /// `resolve()` quietly proceeding on the ~29 hand-written keys alone masked both until
    /// something that specifically needed a registry-resolved key went looking.
    #[test]
    fn missing_directory_is_a_named_error_not_an_empty_registry() {
        let dir = testkit::TempDir::new("spira-config-registry-missing");
        let missing = dir.join("does-not-exist");
        let err = load(&missing).unwrap_err();
        assert_eq!(err, format!("no config registry at {}", missing.display()));
    }

    /// An EXISTING but empty directory is still not an error — a release that genuinely
    /// ships zero registry files (or a fixture that only cares about the hand-written keys)
    /// keeps working. Only "cannot even read this path" is refused.
    #[test]
    fn existing_empty_directory_is_still_an_empty_registry() {
        let dir = testkit::TempDir::new("spira-config-registry-empty");
        let empty = dir.join("conf.d");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(load(&empty).unwrap(), BTreeMap::new());
    }

    #[test]
    fn parses_type_group_doc_and_default_body() {
        let dir = testkit::TempDir::new("spira-config-registry-parse");
        std::fs::write(
            dir.join("SPIRA_EXAMPLE"),
            "TYPE=u32\nGROUP=test\nDOC=an example\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_EXAMPLE:=7}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        let reg = load(dir.path()).unwrap();
        let k = &reg["SPIRA_EXAMPLE"];
        assert_eq!(k.ty, "u32");
        assert_eq!(k.group, "test");
        assert_eq!(k.doc, "an example");
        assert_eq!(k.default_expr(), Some("7"));
    }

    #[test]
    fn a_lowercase_or_dotfile_name_is_skipped() {
        let dir = testkit::TempDir::new("spira-config-registry-skip");
        std::fs::write(dir.join(".gitkeep"), "").unwrap();
        std::fs::write(dir.join("readme.md"), "").unwrap();
        let reg = load(dir.path()).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn unterminated_heredoc_is_refused() {
        let dir = testkit::TempDir::new("spira-config-registry-unterminated");
        std::fs::write(dir.join("SPIRA_BAD"), "DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\nno close\n").unwrap();
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn procedural_and_no_default_stubs_have_no_expr() {
        let k1 = key("SPIRA_X", "    # PROCEDURAL — ...\n");
        let k2 = key("SPIRA_Y", "    # NO DEFAULT.\n");
        assert_eq!(k1.default_expr(), None);
        assert_eq!(k2.default_expr(), None);
    }

    #[test]
    fn default_expr_is_brace_balanced_for_a_nested_substitution() {
        let k = key(
            "SPIRA_OVERRIDES",
            "    : \"${SPIRA_OVERRIDES:=${XDG_CONFIG_HOME:-$HOME/.config}/spira/overrides}\"\n",
        );
        assert_eq!(
            k.default_expr(),
            Some("${XDG_CONFIG_HOME:-$HOME/.config}/spira/overrides")
        );
    }

    #[test]
    fn topo_order_runs_a_dependency_after_what_it_names() {
        let mut reg = BTreeMap::new();
        reg.insert("SPIRA_B".to_string(), key("SPIRA_B", "    : \"${SPIRA_B:=$SPIRA_A/x}\"\n"));
        reg.insert("SPIRA_A".to_string(), key("SPIRA_A", "    : \"${SPIRA_A:=1}\"\n"));
        let order = topo_order(&reg).unwrap();
        let pos_a = order.iter().position(|k| k == "SPIRA_A").unwrap();
        let pos_b = order.iter().position(|k| k == "SPIRA_B").unwrap();
        assert!(pos_a < pos_b, "{order:?}");
    }

    #[test]
    fn topo_order_refuses_a_cycle() {
        let mut reg = BTreeMap::new();
        reg.insert("SPIRA_A".to_string(), key("SPIRA_A", "    : \"${SPIRA_A:=$SPIRA_B}\"\n"));
        reg.insert("SPIRA_B".to_string(), key("SPIRA_B", "    : \"${SPIRA_B:=$SPIRA_A}\"\n"));
        assert!(topo_order(&reg).is_err());
    }

    #[test]
    fn stub_keys_are_excluded_from_topo_order() {
        let mut reg = BTreeMap::new();
        reg.insert("SPIRA_A".to_string(), key("SPIRA_A", "    : \"${SPIRA_A:=1}\"\n"));
        reg.insert("SPIRA_STUB".to_string(), key("SPIRA_STUB", "    # NO DEFAULT.\n"));
        let order = topo_order(&reg).unwrap();
        assert_eq!(order, vec!["SPIRA_A".to_string()]);
    }
