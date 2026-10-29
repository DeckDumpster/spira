//! The settings resolve through the real config door over the shipped registry, not a stub:
//! every stubbed test answered SPIRA_REPO, so production refused it at start (sp-6gsvrm).

#[test]
fn check_config_resolves_every_setting_against_the_shipped_registry() {
    let dir = testkit::TempDir::new("runtime-settings-registry");
    let sock = dir.join("lc.sock").display().to_string();
    let env = spira_config::process::fixture_metrics_env(&dir, "", &[("SPIRA_LC_SOCKET", &sock)]);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_spira-runtime"))
        .arg("check-config")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .envs(env)
        .output()
        .unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "check-config refused the shipped registry: {stderr}");
    assert!(stdout.starts_with("ok "), "{stdout}");
}

fn runtime(verb: &str, release: &std::path::Path, toml: &std::path::Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_spira-runtime"))
        .arg(verb)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("SPIRA_RELEASE", release)
        .env("SPIRA_TOML", toml)
        .output()
        .unwrap()
}

#[test]
fn a_release_without_its_registry_is_refused_by_path_at_check_config_and_before_serve_binds() {
    let dir = testkit::TempDir::new("runtime-no-registry");
    let sock = dir.join("lc.sock");
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_LC_SOCKET", &sock.display().to_string())]);
    let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let release = dir.join("release");
    std::fs::create_dir_all(release.join("spira")).unwrap();
    std::os::unix::fs::symlink(real.join("conf.toml.d"), release.join("spira/conf.toml.d")).unwrap();

    for verb in ["check-config", "serve"] {
        let out = runtime(verb, &release, &toml);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{verb}: {stderr}");
        assert!(stderr.contains(&format!("{}", release.join("spira/conf.d").display())), "{verb} did not name the registry path: {stderr}");
    }
    assert!(!sock.exists(), "serve bound a socket before refusing");

    std::os::unix::fs::symlink(real.join("conf.d"), release.join("spira/conf.d")).unwrap();
    let out = runtime("check-config", &release, &toml);
    assert!(out.status.success(), "positive control: with the registry present check-config passes: {}", String::from_utf8_lossy(&out.stderr));
}
