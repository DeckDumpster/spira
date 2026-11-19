use std::process::Command;

#[test]
fn canary_is_not_a_verb() {
    let home = spira_config::resolve::locate_home_for_process().expect("harness home");
    let units = testkit::TempDir::new("release-canary-verb");
    for verb in ["canary", "canary-worker"] {
        let out = Command::new(testkit::bin_exe("release"))
            .arg(verb)
            .env_clear()
            .env("SPIRA_HOME", &home)
            .env("SPIRA_TOML", home.join("testlib/complete.toml"))
            .env("SPIRA_UNIT_DIR", units.path())
            .output()
            .expect("run release");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{verb} is a usage error: {err}");
        assert!(err.contains(&format!("unknown subcommand {verb}")), "{verb} is refused as unknown: {err}");
    }
}
