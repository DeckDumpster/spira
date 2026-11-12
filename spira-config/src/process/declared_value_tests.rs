    use super::declared_value;
    use std::collections::BTreeMap;

    #[test]
    fn a_declared_key_answers_the_same_as_the_full_resolve_and_the_full_resolve_owns_the_rest() {
        let dir = testkit::TempDir::new("spira-config-declared-value");
        let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let declare: BTreeMap<String, String> = [("SPIRA_INSTANCE", "prod"), ("SPIRA_ASK_LABEL", "pinned-ask"), ("SPIRA_AGENT", "pinned-agent")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let toml = crate::fixture_toml_file(dir.path(), &declare);
        let env: BTreeMap<String, String> = [("HOME", "/h"), ("SPIRA_TOML", toml.to_str().unwrap())].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let full = crate::resolve::resolve_for_process(&tree, &tree, &env).expect("the fixture resolves");
        let merged = crate::merge_layers(toml.to_str().unwrap()).unwrap();

        let answered: Vec<_> = full.values.iter().filter_map(|(k, v)| declared_value(&merged, k).map(|d| (k, v, d))).collect();
        assert!(answered.len() > 100, "positive control: most keys answer from the declaration, got {}", answered.len());
        for (k, full_v, fast_v) in &answered {
            assert_eq!(*full_v, fast_v, "{k}");
        }
        assert_eq!(declared_value(&merged, "SPIRA_ASK_LABEL").as_deref(), Some("pinned-ask"));
        assert_eq!(declared_value(&merged, "SPIRA_AGENT"), None, "a computed key takes the full resolve");
        assert_eq!(declared_value(&merged, "NOT_A_SPIRA_KEY"), None);
        assert_eq!(declared_value(&merged, "SPIRA_NO_SUCH_KEY_ANYWHERE"), None, "an undeclared key is the full resolve's refusal");
    }

    #[test]
    fn a_confined_instance_takes_the_full_resolve_so_containment_still_runs() {
        let dir = testkit::TempDir::new("spira-config-declared-value-confined");
        let declare: BTreeMap<String, String> = [("SPIRA_INSTANCE", "scratch"), ("SPIRA_ASK_LABEL", "x")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let toml = crate::fixture_toml_file(dir.path(), &declare);
        let merged = crate::merge_layers(toml.to_str().unwrap()).unwrap();
        assert_eq!(declared_value(&merged, "SPIRA_ASK_LABEL"), None);
    }

    #[test]
    fn declared_value_leaves_the_registry_check_to_cfg() {
        let dir = testkit::TempDir::new("spira-config-declared-unregistered");
        let toml = dir.path().join("spira.toml");
        std::fs::write(&toml, "[spira]\ninstance = \"prod\"\nnot_a_registered_key = \"typo\"\n").unwrap();
        let merged = crate::merge_layers(toml.to_str().unwrap()).unwrap();
        assert_eq!(declared_value(&merged, "SPIRA_NOT_A_REGISTERED_KEY").as_deref(), Some("typo"));
    }
