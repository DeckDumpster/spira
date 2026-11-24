//! The shell scripts are the subject (the binary-spawning exception): `hold.sh` and `unhold.sh`
//! run under `bash` against a stub `spira-lc` and a throwaway run directory. The liveness the
//! reaper reads from what they leave behind is `hel::reap`'s own tests.

#[macro_use]
extern crate spira_test_id;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn root() -> PathBuf {
    testkit::manifest_dir().join("..").canonicalize().unwrap()
}

struct Fixture {
    tmp: testkit::TempDir,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(hb) = std::fs::read_to_string(self.run_dir().join("hold-sp-h1.hb")) {
            let _ = Command::new("kill").arg(hb.trim()).status();
        }
    }
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = testkit::TempDir::new("hold-scripts");
        let d = tmp.path();
        std::fs::create_dir_all(d.join("run")).unwrap();
        std::fs::create_dir_all(d.join("stubs")).unwrap();
        testkit::write_exe(d.join("stubs/spira-lc"), &format!("#!/bin/bash\necho \"$*\" >> {}/lc.log\n", d.display()));
        std::fs::write(d.join("o.toml"), format!("[spira]\nrun = \"{}/run\"\n", d.display())).unwrap();
        Fixture { tmp }
    }

    fn run_dir(&self) -> PathBuf {
        self.tmp.path().join("run")
    }

    fn lc_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.tmp.path().join("lc.log")).unwrap_or_default().lines().map(String::from).collect()
    }

    fn script(&self, name: &str, args: &[&str]) -> Output {
        let d = self.tmp.path();
        let config_bin = testkit::bin_exe("spira-config");
        Command::new("bash")
            .arg(root().join("spira").join(name))
            .args(args)
            .env_clear()
            .env("HOME", d)
            .env("PATH", format!("{}/stubs:{}:/usr/bin:/bin", d.display(), config_bin.parent().unwrap().display()))
            .env("SPIRA_TOML", format!("{}:{}", root().join("spira/testlib/complete.toml").display(), d.join("o.toml").display()))
            .env("SPIRA_HOLD_HEARTBEAT", "3600")
            .output()
            .unwrap()
    }
}

fn gone(pid: &str) -> bool {
    let out = Command::new("ps").args(["-o", "stat=", "-p", pid]).output().unwrap();
    let stat = String::from_utf8(out.stdout).unwrap();
    stat.trim().is_empty() || stat.trim_start().starts_with('Z')
}

// covers: UC-ops-detection-remediation-34
#[test_id(tui3vkf)]
#[test]
fn hold_writes_pidfile_and_heartbeat_refuses_a_double_hold_and_unhold_removes_both() {
    let fx = Fixture::new();
    let me = std::process::id().to_string();
    let pidfile = fx.run_dir().join("hold-sp-h1.pid");
    let hbfile = fx.run_dir().join("hold-sp-h1.hb");

    let held = fx.script("hold.sh", &["sp-h1", "--pid", &me]);
    assert_eq!(held.status.code(), Some(0), "{}", String::from_utf8_lossy(&held.stderr));
    assert_eq!(std::fs::read_to_string(&pidfile).unwrap().trim(), me);
    let hb = std::fs::read_to_string(&hbfile).unwrap().trim().to_string();
    assert!(!gone(&hb), "the heartbeat is alive while held");
    assert_eq!(fx.lc_calls(), [format!("hold sp-h1 manual manual hold via hold.sh (pid {me}) hold-sp-h1")]);

    let again = fx.script("hold.sh", &["sp-h1", "--pid", &me]);
    assert_eq!(again.status.code(), Some(1), "a second hold of a held bead is refused");
    assert_eq!(std::fs::read_to_string(&pidfile).unwrap().trim(), me);

    let freed = fx.script("unhold.sh", &["sp-h1"]);
    assert_eq!(freed.status.code(), Some(0), "{}", String::from_utf8_lossy(&freed.stderr));
    assert!(!pidfile.exists() && !hbfile.exists());
    assert_eq!(fx.lc_calls().last().unwrap(), "unhold sp-h1 manual hold-sp-h1");
    let deadline = Instant::now() + Duration::from_secs(5);
    // clock-safe: caps the wait for the heartbeat's exit, which no handle signals (it is not our child)
    while !gone(&hb) && Instant::now() < deadline {
        // clock-safe: the poll interval of that same capped wait
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(gone(&hb), "unhold kills the heartbeat");
}
