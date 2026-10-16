use super::*;

fn dir(n: &str) -> testkit::TempDir {
    testkit::TempDir::new(&format!("ui-{n}"))
}

fn ui(argv: &[&str], image: Option<&str>) -> Ui {
    Ui { argv: argv.iter().map(|s| s.to_string()).collect(), image: image.map(String::from), dir: None, timeout: None }
}

#[test]
fn the_playwright_dir_is_the_first_that_holds_a_config() {
    let t = dir("find");
    assert_eq!(playwright_dir(t.path(), None), None);
    std::fs::create_dir_all(t.path().join("tests/visual")).unwrap();
    std::fs::write(t.path().join("tests/visual/playwright.config.ts"), "").unwrap();
    std::fs::write(t.path().join("playwright.config.ts"), "").unwrap();
    assert_eq!(playwright_dir(t.path(), None).as_deref(), Some("tests/visual"));
    assert_eq!(playwright_dir(t.path(), Some(".")).as_deref(), Some("."));
    assert_eq!(playwright_dir(t.path(), Some("nowhere")), None);
}

#[test]
fn the_version_comes_from_the_lock_before_the_range() {
    let t = dir("ver");
    std::fs::write(t.path().join("package.json"), r#"{"devDependencies":{"@playwright/test":"^1.56.0"}}"#).unwrap();
    assert_eq!(playwright_version(t.path()).as_deref(), Some("1.56.0"));
    std::fs::write(t.path().join("package-lock.json"), r#"{"packages":{"node_modules/@playwright/test":{"version":"1.57.2"}}}"#).unwrap();
    assert_eq!(playwright_version(t.path()).as_deref(), Some("1.57.2"));
    std::fs::write(t.path().join("package-lock.json"), "{}").unwrap();
    std::fs::write(t.path().join("package.json"), r#"{"devDependencies":{"@playwright/test":"latest"}}"#).unwrap();
    assert_eq!(playwright_version(t.path()), None);
}

#[test]
fn the_image_is_pinned_to_the_version_unless_declared() {
    assert_eq!(image_for(None, Some("1.57.2")).unwrap(), "mcr.microsoft.com/playwright:v1.57.2-noble");
    assert_eq!(image_for(Some(&ui(&[], Some("x:1"))), Some("1.57.2")).unwrap(), "x:1");
    assert!(image_for(None, None).is_err());
}

#[test]
fn a_declared_argv_replaces_the_default() {
    let a = argv_for(None, "tests/visual");
    assert!(a[2].starts_with("cd tests/visual && npm ci") && a[2].contains("$UI_SPECS"), "{a:?}");
    assert_eq!(argv_for(Some(&ui(&["serve"], None)), "d"), vec!["serve".to_string()]);
}

#[test]
fn a_spec_that_could_be_a_flag_or_a_command_is_refused() {
    for bad in ["--update-snapshots", "a; rm -rf /", "$(x)", ""] {
        assert!(!plausible_spec(bad), "{bad}");
    }
    assert!(plausible_spec("collection-window.spec.ts"));
}
