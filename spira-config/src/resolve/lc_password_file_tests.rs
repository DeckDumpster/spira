    use super::lc_password_file;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn an_absent_variable_resolves_to_the_default_credential_when_it_exists() {
        let t = testkit::TempDir::new("lcpw");
        let d = t.path().to_path_buf();
        std::fs::create_dir_all(d.join("spira")).unwrap();
        std::fs::write(d.join("spira/spira-lc.credential"), "pw").unwrap();
        let got = lc_password_file(&env(&[("XDG_CONFIG_HOME", d.to_str().unwrap())]));
        assert_eq!(got, format!("{}/spira/spira-lc.credential", d.display()));
        let empty = lc_password_file(&env(&[("XDG_CONFIG_HOME", d.to_str().unwrap()), ("SPIRA_LC_PASSWORD_FILE", "")]));
        assert_eq!(empty, got);
    }

    #[test]
    fn with_no_credential_anywhere_it_is_empty_and_an_explicit_value_wins() {
        assert_eq!(lc_password_file(&env(&[("XDG_CONFIG_HOME", "/nonexistent-lcpw")])), "");
        assert_eq!(lc_password_file(&env(&[("SPIRA_LC_PASSWORD_FILE", "/x")])), "/x");
    }
