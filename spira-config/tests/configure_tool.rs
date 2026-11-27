//! `spira/configure.sh` is the subject, so the script itself runs under `bash`, with the built
//! `spira-config` on `PATH`. No shell suite may grow.

#[macro_use]
extern crate spira_test_id;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fresh {
    dir: testkit::TempDir,
    out: PathBuf,
}

fn fresh_box() -> Fresh {
    let dir = testkit::TempDir::new("configure-tool");
    let out = dir.path().join("spira.toml");
    Fresh { dir, out }
}

/// Runs the script with no TTY on stdin and nothing inherited from this process.
fn configure(b: &Fresh, args: &[&str], env: &[(&str, &str)]) -> Output {
    let d = b.dir.path();
    let tool = testkit::bin_exe("spira-config");
    let mut c = Command::new("bash");
    c.env_clear()
        .arg(testkit::manifest_dir().parent().unwrap().join("spira/configure.sh"))
        .args(args)
        .stdin(Stdio::null())
        .env("PATH", format!("{}:/usr/bin:/bin", tool.parent().unwrap().display()))
        .env("HOME", d.join("home"))
        .env("XDG_CONFIG_HOME", d.join("config"))
        .env("XDG_DATA_HOME", d.join("data"))
        .env("CONFIGURE_OUT", &b.out)
        // No registered default: without these `spira-config init` writes nothing.
        .env("CONFIGURE_GATE_TARGET_CAP_MIB", "55555")
        .env("CONFIGURE_METRICS_FLUSH_MINUTES", "7");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn assert_refused(b: &Fresh, o: &Output) {
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("SPIRA_DOLT_DATA"), "{err}");
    assert!(!b.out.exists(), "a refused run must write no config");
}

// covers: UC-config-store-preflight-07
#[test_id(t47gv8f)]
#[test]
fn an_empty_dolt_data_variable_is_refused_and_writes_no_config() {
    let b = fresh_box();
    let o = configure(&b, &[], &[("CONFIGURE_DOLT_DATA", "")]);
    assert_refused(&b, &o);
}

// covers: UC-config-store-preflight-07
#[test_id(tthwsot)]
#[test]
fn an_empty_dolt_data_flag_is_refused_and_writes_no_config() {
    let b = fresh_box();
    let o = configure(&b, &["--dolt-data", ""], &[]);
    assert_refused(&b, &o);
}

// covers: UC-config-store-preflight-07
#[test_id(tga4dpm)]
#[test]
fn an_unset_dolt_data_without_a_tty_takes_the_directory_under_the_data_home() {
    let b = fresh_box();
    let o = configure(&b, &[], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (doc, _) = spira_config::load_strict(&b.out).expect("what configure writes the reader accepts");
    let want = b.dir.path().join("data/spira/dolt").display().to_string();
    assert_eq!(spira_config::get_path(&doc, "spira.dolt_data"), Some(want));
    assert_eq!(spira_config::get_path(&doc, "spira.gate_target_cap_mib").as_deref(), Some("55555"));
    assert_eq!(spira_config::get_path(&doc, "spira.metrics_flush_minutes").as_deref(), Some("7"));
}
