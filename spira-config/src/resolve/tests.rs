    use super::*;
    use std::collections::BTreeMap;

    #[test_id(tuav0fm)]
    #[test]
    fn the_victorialogs_url_is_exported_to_every_child_that_ships_or_counts() {
        assert!(EXPORT_KEYS.contains(&"SPIRA_VICTORIALOGS_URL"));
        assert!(EXPORT_KEYS.windows(2).all(|w| w[0] < w[1]), "the export list stays sorted and without repeats");
    }

    #[test_id(tjqc804)]
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
    #[test_id(t3hzyas)]
    #[test]
    fn an_undeclared_key_is_refused_by_name_even_when_the_environment_sets_it() {
        let ws = testkit::TempDir::new("spira-config-resolve-undeclared");
        let (home, repo) = fixture_home_repo(&ws);
        let doc = crate::unset_path(&crate::fixture_doc(&BTreeMap::new()), "spira.run").unwrap();
        let e = env(&[("HOME", "h"), ("SPIRA_RUN", "from/the/environment")]);
        let err = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(&doc), conf_d: &home.join("conf.d") })
            .expect_err("an undeclared spira.run must refuse");
        let msg = err.to_string();
        assert!(msg.contains("SPIRA_RUN") && msg.contains("spira.run"), "{msg}");
        assert!(!msg.contains("from/the/environment"), "the environment must not be read: {msg}");
    }

    #[test_id(tqogl9u)]
    #[test]
    fn an_empty_runes_dir_resolves_to_the_homes_runes_and_reaches_sh_all() {
        let ws = testkit::TempDir::new("spira-config-resolve-runes-dir");
        let (home, repo) = fixture_home_repo(&ws);
        let e = env(&[("HOME", "h")]);
        let doc = crate::fixture_doc(&env(&[("SPIRA_RUNES_DIR", "")]));
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(&doc), conf_d: &home.join("conf.d") }).unwrap();
        let want = format!("{}/runes", home.display());
        assert_eq!(r.get("SPIRA_RUNES_DIR"), want);
        assert!(r.to_sh_all().contains(&format!("SPIRA_RUNES_DIR='{want}'")), "{}", r.to_sh_all());

        let declared = crate::fixture_doc(&env(&[("SPIRA_RUNES_DIR", "elsewhere/runes")]));
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(&declared), conf_d: &home.join("conf.d") }).unwrap();
        assert_eq!(r.get("SPIRA_RUNES_DIR"), "elsewhere/runes");
    }

    #[test_id(tmha7ct)]
    #[test]
    fn a_relative_broker_socket_is_placed_under_the_run_dir_and_is_never_defaulted() {
        let ws = testkit::TempDir::new("spira-config-resolve-broker");
        let (home, repo) = fixture_home_repo(&ws);
        let e = env(&[("HOME", "h")]);
        let go = |doc: &crate::SpiraToml| resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(doc), conf_d: &home.join("conf.d") });
        let doc = crate::fixture_doc(&e);

        let r = go(&crate::set_path(&doc, "spira.broker_socket", "nested/b.sock").unwrap()).unwrap();
        assert_eq!(r.get("SPIRA_BROKER_SOCKET"), format!("{}/nested/b.sock", r.get("SPIRA_RUN")));

        let absolute = format!("{}/b.sock", ws.path().display());
        let r = go(&crate::set_path(&doc, "spira.broker_socket", &absolute).unwrap()).unwrap();
        assert_eq!(r.get("SPIRA_BROKER_SOCKET"), absolute);

        let err = go(&crate::unset_path(&doc, "spira.broker_socket").unwrap()).expect_err("an undeclared socket must refuse").to_string();
        assert!(err.contains("spira.broker_socket"), "{err}");
    }

    #[test_id(t00tz2g)]
    #[test]
    fn sh_all_never_assigns_the_launchers_release_over_the_environment() {
        let ws = testkit::TempDir::new("spira-config-resolve-release");
        let (home, repo) = fixture_home_repo(&ws);
        std::fs::write(home.join("conf.d/SPIRA_RELEASE"), "TYPE=string\nGROUP=runtime\nDOC=the release a task resolves tools from\n").unwrap();
        let e = env(&[("HOME", "h"), ("SPIRA_RELEASE", "staged/release")]);
        let doc = crate::fixture_doc(&env(&[("SPIRA_RELEASE", "installed/current")]));
        let r = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(&doc), conf_d: &home.join("conf.d") }).unwrap();
        assert_eq!(r.get("SPIRA_RELEASE"), "installed/current", "the in-process reader still sees the declared key");
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









    #[test_id(trgdky0)]
    #[test]
    fn derive_home_repo_env_override_wins() {
        let ws = testkit::TempDir::new("spira-config-derive-repo-env");
        let (home, _repo) = fixture_home_repo(&ws);
        let e = env(&[("SPIRA_REPO", "explicit/override")]);
        assert_eq!(derive_home_repo(&home, &e), Path::new("explicit/override"));
    }

    // The tests' own `git init` needs PATH; derive_repo_filesystem itself reads only the
    // filesystem now, so a poisoned map (GIT_DIR below) cannot steer it.
    fn env_with_path(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut m = env(pairs);
        m.entry("PATH".to_string()).or_insert_with(|| std::env::var("PATH").unwrap_or_default());
        m
    }

    #[test_id(tzmdpn2)]
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


    #[test_id(tq90g8s)]
    #[test]
    fn a_declared_mail_readers_beats_the_registered_default() {
        let ws = testkit::TempDir::new("spira-config-mail-readers");
        let home = crate::resolve::locate_home_for_process().unwrap();
        let repo = home.parent().unwrap().to_path_buf();
        let toml = crate::fixture_toml_file(ws.path(), &env(&[("SPIRA_MAIL_READERS", "concierge=echo wake")]));
        let e = env(&[("HOME", "h"), ("SPIRA_TOML", toml.to_str().unwrap())]);
        let resolved = resolve_for_process(&home, &repo, &e).unwrap();
        assert_eq!(resolved.values.get("SPIRA_MAIL_READERS").map(String::as_str), Some("concierge=echo wake"));
    }

    /// An env override still wins outright, exactly as `SPIRA_RUN` always has — this is
    /// what makes a systemd unit, which sets it explicitly, keep working unchanged.
    #[test_id(ti3o1t6)]
    #[test]
    fn resolve_ask_label_reads_the_declaration_never_the_environment() {
        let ws = testkit::TempDir::new("spira-config-ask-label");
        // The REAL registry, where SPIRA_ASK_LABEL is a key.
        let home = crate::resolve::locate_home_for_process().unwrap();
        let toml = crate::fixture_toml_file(ws.path(), &env(&[("SPIRA_ASK_LABEL", "declared-ask")]));
        let e = env(&[("HOME", "h"), ("SPIRA_TOML", toml.to_str().unwrap()), ("SPIRA_ASK_LABEL", "from-the-environment")]);
        assert_eq!(resolve_ask_label(&e, &home).unwrap(), "declared-ask");
    }

    fn shipped_conf_d() -> std::path::PathBuf {
        crate::resolve::locate_home_for_process().unwrap().join("conf.d")
    }

    fn declared(pairs: &[(&str, &str)]) -> crate::SpiraToml {
        [("spira.home_repo", "h"), ("spira.repo_map", "nowhere/repo-map")]
            .iter()
            .chain(pairs)
            .fold(crate::SpiraToml::default(), |d, (k, v)| crate::set_path(&d, k, v).unwrap())
    }

    fn with_defaults(repo: &Path, doc: &crate::SpiraToml, e: &BTreeMap<String, String>) -> Resolved {
        let home = repo.join("spira");
        resolve_with_defaults(ResolveInput { env: e, home: &home, repo, toml: Some(doc), conf_d: &shipped_conf_d() }).unwrap()
    }

    // covers: UC-config-store-preflight-01
    #[test_id(tge5msq)]
    #[test]
    fn a_declared_key_beats_its_derived_default_and_the_environment_never_speaks() {
        let ws = testkit::TempDir::new("spira-config-precedence");
        let (home, repo) = fixture_home_repo(&ws);
        let e = env(&[("HOME", "h"), ("SPIRA_INSTANCE", "from-the-environment")]);

        let derived = with_defaults(&repo, &declared(&[]), &e);
        assert_eq!(derived.get("SPIRA_INSTANCE"), "prod", "nothing declared: the derived default");

        let doc = declared(&[("spira.instance", "acc")]);
        assert_eq!(with_defaults(&repo, &doc, &e).get("SPIRA_INSTANCE"), "acc", "the declaration beats the default and the environment");

        let refused = resolve(ResolveInput { env: &e, home: &home, repo: &repo, toml: Some(&doc), conf_d: &shipped_conf_d() });
        assert!(refused.is_err(), "reading a config that leaves a key undeclared is refused, not defaulted");
    }

    #[test_id(tcvpxi7)]
    #[test]
    fn spira_claude_is_a_deprecated_alias_that_only_an_undeclared_agent_falls_back_to() {
        let ws = testkit::TempDir::new("spira-config-claude-alias");
        let (_, repo) = fixture_home_repo(&ws);
        let e = env(&[("HOME", "h"), ("SPIRA_CLAUDE", "opt/old-agent")]);

        let aliased = with_defaults(&repo, &declared(&[]), &e);
        assert_eq!(aliased.get("SPIRA_AGENT"), "opt/old-agent");
        assert_eq!(aliased.warnings, vec!["spira: SPIRA_CLAUDE is deprecated; rename it to SPIRA_AGENT in spira.conf".to_string()]);

        let doc = declared(&[("spira.agent", "opt/new-agent")]);
        let declared_wins = with_defaults(&repo, &doc, &e);
        assert_eq!(declared_wins.get("SPIRA_AGENT"), "opt/new-agent");
        assert!(declared_wins.warnings.is_empty(), "{:?}", declared_wins.warnings);

        let neither = with_defaults(&repo, &declared(&[]), &env(&[("HOME", "h")]));
        assert_eq!(neither.get("SPIRA_AGENT"), "claude");
        assert!(neither.warnings.is_empty());
    }

    // covers: UC-config-store-preflight-03
    #[test_id(tbf12m9)]
    #[test]
    fn derived_defaults_follow_the_install_location_and_the_whole_shipped_key_set_resolves() {
        let ws = testkit::TempDir::new("spira-config-derived-location");
        let (_home, repo) = fixture_home_repo(&ws);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let e = env(&[("HOME", "h")]);
        let doc = declared(&[]);
        let r = with_defaults(&repo, &doc, &e);
        assert_eq!(r.get("SPIRA_WORKSPACES"), ws.path().to_str().unwrap(), "a git checkout's workspaces root is its parent directory");
        assert!(r.values.len() > 100, "every key of the shipped registry resolves from its default: {}", r.values.len());
        assert!(r.values.values().all(|v| !v.contains("$SPIRA_")), "no derived default is left an unexpanded reference");

        let moved = testkit::TempDir::new("spira-config-derived-location-moved");
        let repo2 = moved.path().join("spira-harness");
        std::fs::create_dir_all(repo2.join(".git")).unwrap();
        assert_eq!(with_defaults(&repo2, &doc, &e).get("SPIRA_WORKSPACES"), moved.path().to_str().unwrap());
    }

    // covers: UC-config-store-preflight-04
    #[test_id(ts1kxk9)]
    #[test]
    fn prod_keeps_unqualified_paths_and_a_non_prod_instance_gets_its_own_database_and_runtime_directory() {
        let ws = testkit::TempDir::new("spira-config-instance-paths");
        let (_home, repo) = fixture_home_repo(&ws);
        let e = env(&[("HOME", "h")]);
        let prod = with_defaults(&repo, &declared(&[]), &e);
        assert_eq!(prod.get("SPIRA_DB"), "h/.local/share/spira/db");
        assert_eq!(prod.get("SPIRA_RUN"), "h/.local/share/spira/run");

        let acc = with_defaults(&repo, &declared(&[("spira.instance", "acc")]), &e);
        assert_eq!(acc.get("SPIRA_DB"), "h/.local/share/spira-acc/db");
        assert_eq!(acc.get("SPIRA_RUN"), "h/.local/share/spira-acc/run");
    }

#[test_id(tfc1vyp)]
#[test]
fn checkout_root_is_the_parent_of_the_home() {
    let env = |k: &str, v: &str| BTreeMap::from([(k.to_string(), v.to_string())]);
    assert_eq!(checkout_root(&env("SPIRA_HOME", "w/spira")), Ok(PathBuf::from("w")));
    assert_eq!(checkout_root(&env("SPIRA_RELEASE", "r")), Ok(PathBuf::from("r")));
    assert!(checkout_root(&BTreeMap::new()).unwrap_err().contains("refusing to search"));
    assert!(checkout_root(&env("SPIRA_HOME", "spira")).unwrap_err().contains("has no parent"));
}

#[test_id(tibluub)]
#[test]
fn checkout_root_refuses_a_home_with_no_parent() {
    let env = BTreeMap::from([("SPIRA_HOME".to_string(), "/".to_string())]);
    assert!(checkout_root(&env).unwrap_err().contains("has no parent"));
}

#[test_id(tr0x835)]
#[test]
fn checkout_root_for_process_reads_the_process_environment() {
    let _g = testkit::env(&[("SPIRA_HOME", Some("w/spira"))]);
    assert_eq!(checkout_root_for_process(), Ok(PathBuf::from("w")));
}
