    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn the_victorialogs_url_is_exported_to_every_child_that_ships_or_counts() {
        assert!(EXPORT_KEYS.contains(&"SPIRA_VICTORIALOGS_URL"));
        assert!(EXPORT_KEYS.windows(2).all(|w| w[0] < w[1]), "the export list stays sorted and without repeats");
    }

    #[test]
    fn resolve_or_say_returns_empty_on_a_missing_conf_d() {
        let ws = testkit::TempDir::new("spira-config-resolve-or-say");
        let home = ws.path().join("no-such-home");
        let env = BTreeMap::new();
        assert!(resolve_for_process(&home, &home, &env).is_err(), "positive control: this home must fail to resolve");
        assert_eq!(resolve_or_say("test", &home, &home, &env), Resolved::default());
    }

    /// THE ONE SOURCE: a key spira.toml does not declare is refused by name — never
    /// defaulted, never read from the environment (per Ryan 2026-10-05).
    #[test]
    fn an_undeclared_key_is_refused_by_name_even_when_the_environment_sets_it() {
        let ws = testkit::TempDir::new("spira-config-resolve-undeclared");
        let (home, repo) = fixture_home_repo(&ws);
        let doc = crate::unset_path(&crate::fixture_doc(&BTreeMap::new()), "spira.run").unwrap();
        let e = env(&[("HOME", "/h"), ("SPIRA_RUN", "/from/the/environment")]);
        let err = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(&doc), conf_d: &home.join("conf.d") })
            .expect_err("an undeclared spira.run must refuse");
        let msg = err.to_string();
        assert!(msg.contains("SPIRA_RUN") && msg.contains("spira.run"), "{msg}");
        assert!(!msg.contains("/from/the/environment"), "the environment must not be read: {msg}");
    }

    #[test]
    fn an_empty_runes_dir_resolves_to_the_homes_runes_and_reaches_sh_all() {
        let ws = testkit::TempDir::new("spira-config-resolve-runes-dir");
        let (home, repo) = fixture_home_repo(&ws);
        let e = env(&[("HOME", "/h")]);
        let doc = crate::fixture_doc(&env(&[("SPIRA_RUNES_DIR", "")]));
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(&doc), conf_d: &home.join("conf.d") }).unwrap();
        let want = format!("{}/runes", home.display());
        assert_eq!(r.get("SPIRA_RUNES_DIR"), want);
        assert!(r.to_sh_all().contains(&format!("SPIRA_RUNES_DIR='{want}'")), "{}", r.to_sh_all());

        let declared = crate::fixture_doc(&env(&[("SPIRA_RUNES_DIR", "/elsewhere/runes")]));
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(&declared), conf_d: &home.join("conf.d") }).unwrap();
        assert_eq!(r.get("SPIRA_RUNES_DIR"), "/elsewhere/runes");
    }

    #[test]
    fn sh_all_never_assigns_the_launchers_release_over_the_environment() {
        let ws = testkit::TempDir::new("spira-config-resolve-release");
        let (home, repo) = fixture_home_repo(&ws);
        std::fs::write(home.join("conf.d/SPIRA_RELEASE"), "TYPE=string\nGROUP=runtime\nDOC=the release a task resolves tools from\n").unwrap();
        let e = env(&[("HOME", "/h"), ("SPIRA_RELEASE", "/staged/release")]);
        let doc = crate::fixture_doc(&env(&[("SPIRA_RELEASE", "/installed/current")]));
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(&doc), conf_d: &home.join("conf.d") }).unwrap();
        assert_eq!(r.get("SPIRA_RELEASE"), "/installed/current", "the in-process reader still sees the declared key");
        let sh = r.to_sh_all();
        assert!(sh.contains("SPIRA_RUN="), "positive control: sh-all carries the other keys: {sh}");
        assert!(!sh.lines().any(|l| l.starts_with("SPIRA_RELEASE=")), "{sh}");
    }

    fn fixture_home_repo(workspaces: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let repo = workspaces.join("spira-harness");
        std::fs::create_dir_all(repo.join("spira/conf.d")).unwrap();
        let home = repo.join("spira");
        (home, repo)
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }









    #[test]
    fn derive_home_repo_env_override_wins() {
        let ws = testkit::TempDir::new("spira-config-derive-repo-env");
        let (home, _repo) = fixture_home_repo(&ws);
        let e = env(&[("SPIRA_REPO", "/explicit/override")]);
        assert_eq!(derive_home_repo(&home, &e), Path::new("/explicit/override"));
    }

    // The tests' own `git init` needs PATH; derive_repo_filesystem itself reads only the
    // filesystem now, so a poisoned map (GIT_DIR below) cannot steer it.
    fn env_with_path(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut m = env(pairs);
        m.entry("PATH".to_string()).or_insert_with(|| std::env::var("PATH").unwrap_or_default());
        m
    }

    #[test]
    fn derive_home_repo_falls_back_to_parent_outside_a_checkout() {
        let ws = testkit::TempDir::new("spira-config-derive-repo-noparent");
        let (home, repo) = fixture_home_repo(&ws);
        // deliberately NOT a git repo: no `git init`
        let e = env_with_path(&[]);
        let got = derive_home_repo(&home, &e).canonicalize().unwrap();
        let want = repo.canonicalize().unwrap();
        assert_eq!(got, want);
    }


    #[test]
    fn a_declared_mail_readers_beats_the_registered_default() {
        let ws = testkit::TempDir::new("spira-config-mail-readers");
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let repo = home.parent().unwrap().to_path_buf();
        let toml = crate::fixture_toml_file(ws.path(), &env(&[("SPIRA_MAIL_READERS", "concierge=echo wake")]));
        let e = env(&[("HOME", "/h"), ("SPIRA_TOML", toml.to_str().unwrap())]);
        let resolved = resolve_for_process(&home, &repo, &e).unwrap();
        assert_eq!(resolved.values.get("SPIRA_MAIL_READERS").map(String::as_str), Some("concierge=echo wake"));
    }

    /// An env override still wins outright, exactly as `SPIRA_RUN` always has — this is
    /// what makes a systemd unit, which sets it explicitly, keep working unchanged.
    #[test]
    fn resolve_ask_label_reads_the_declaration_never_the_environment() {
        let ws = testkit::TempDir::new("spira-config-ask-label");
        // The REAL registry, where SPIRA_ASK_LABEL is a key.
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let toml = crate::fixture_toml_file(ws.path(), &env(&[("SPIRA_ASK_LABEL", "declared-ask")]));
        let e = env(&[("HOME", "/h"), ("SPIRA_TOML", toml.to_str().unwrap()), ("SPIRA_ASK_LABEL", "from-the-environment")]);
        assert_eq!(resolve_ask_label(&e, &home).unwrap(), "declared-ask");
    }




