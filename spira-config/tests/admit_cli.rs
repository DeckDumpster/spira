#[macro_use]
extern crate spira_test_id;
use std::path::Path;
use std::process::{Command, Output};

fn admit(run: &Path, inner: Option<&str>, args: &[&str]) -> Output {
    let mut c = Command::new(testkit::bin_exe("spira-admit"));
    c.env_clear().env("SPIRA_RUN", run).args(args);
    if let Some(i) = inner {
        c.env("SPIRA_ADMIT_INNER", i);
    }
    c.output().unwrap()
}

#[test_id(t5mltfu)]
#[test]
fn a_retired_wrapper_call_execs_the_inner_compiler_and_writes_no_pool_file() {
    let run = testkit::TempDir::new("admit-cli-wrapper");
    let out = admit(run.path(), Some(system_paths::BIN_ECHO), &["rustc", "--crate-name", "x"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "rustc --crate-name x\n");
    assert!(out.status.success());
    assert_eq!(std::fs::read_dir(run.path()).unwrap().count(), 0, "a pass-through writes no pool file");
}

#[test_id(t10x9nm)]
#[test]
fn without_an_inner_compiler_the_wrapper_execs_the_compiler_it_was_handed() {
    let run = testkit::TempDir::new("admit-cli-bare");
    let out = admit(run.path(), None, &[system_paths::BIN_ECHO, "-vV"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "-vV\n");
    assert_eq!(std::fs::read_dir(run.path()).unwrap().count(), 0);
}

#[test_id(twjd2oc)]
#[test]
fn help_and_no_argument_print_usage_and_exit_2() {
    let run = testkit::TempDir::new("admit-cli-usage");
    for args in [&[][..], &["help"][..], &["--help"][..]] {
        let out = admit(run.path(), Some(system_paths::BIN_ECHO), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("usage: spira-admit status"), "{args:?}");
    }
}
