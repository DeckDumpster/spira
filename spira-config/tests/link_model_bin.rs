//! `spira-config link-model-bin <release>` through the real binary (sp-jq4wq): the one
//! entry a staged release reaches `release_env::link_model_bin` by — testenv's stage script
//! and the suite library call the tree's OWN spira-config, so a staged release's `model-bin/`
//! is the tree under test's list, never a copy baked into whichever testenv happens to be
//! installed (the installed one predated sp-zf4q3 and staged no `model-bin/` at all).

use spira_config::release_env::{model_bin_problems, MODEL_BINS, MODEL_BIN_DIR};
use std::fs;
use std::path::Path;
use std::process::Command;

fn link(release: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spira-config"))
        .arg("link-model-bin")
        .arg(release)
        .output()
        .expect("spira-config runs")
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

/// A release staged the way testenv stages one (bin/ holding every workspace binary, `work`
/// among tools that call bd) and no model-bin/: after link-model-bin, model-bin/ holds
/// exactly MODEL_BINS, each a relative link to ../bin/<name>, and release verify's own check
/// finds nothing. A second call (every suite of a batch may make it) is a no-op, not an error.
#[test]
fn a_staged_release_gets_a_model_bin_holding_exactly_the_allowed_list() {
    let d = testkit::TempDir::new("spira-config-link-model-bin");
    let rel = d.join("spira-release-x");
    fs::create_dir_all(rel.join("bin")).unwrap();
    for b in MODEL_BINS.iter().copied().chain(["bd", "bead", "spira-claim", "mail"]) {
        testkit::write_exe(rel.join("bin").join(b), "#!/bin/sh\n");
    }
    assert!(!model_bin_problems(&rel).is_empty(), "precondition: no model-bin/ is a problem");

    let o = link(&rel);
    assert!(o.status.success(), "link-model-bin failed: {}", String::from_utf8_lossy(&o.stderr));
    let dir = rel.join(MODEL_BIN_DIR);
    let mut want: Vec<String> = MODEL_BINS.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(entries(&dir), want, "model-bin/ holds exactly MODEL_BINS");
    for b in MODEL_BINS {
        assert_eq!(fs::read_link(dir.join(b)).unwrap(), Path::new("../bin").join(b));
    }
    assert_eq!(model_bin_problems(&rel), Vec::<String>::new());

    let again = link(&rel);
    assert!(again.status.success(), "a second link-model-bin must succeed: {}", String::from_utf8_lossy(&again.stderr));
    assert_eq!(entries(&dir), want);
}

/// A release whose bin/ ships none of MODEL_BINS (a repository other than the harness) gets
/// no model-bin/ at all, and that is success.
#[test]
fn a_release_without_model_binaries_gets_no_model_bin() {
    let d = testkit::TempDir::new("spira-config-link-model-bin-none");
    let rel = d.join("rel");
    fs::create_dir_all(rel.join("bin")).unwrap();
    testkit::write_exe(rel.join("bin/other"), "#!/bin/sh\n");
    let o = link(&rel);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!rel.join(MODEL_BIN_DIR).exists());
}

#[test]
fn a_missing_argument_is_a_usage_error() {
    let o = Command::new(env!("CARGO_BIN_EXE_spira-config")).arg("link-model-bin").output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("usage: spira-config link-model-bin <release>"));
}
