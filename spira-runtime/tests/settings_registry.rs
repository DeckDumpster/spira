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
