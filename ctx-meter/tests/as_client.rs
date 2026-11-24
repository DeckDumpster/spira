//! The status line command the harness registers, run the way the client runs it: the text
//! `session_hook` writes, under `env_clear`, never a path this target assembled. Each case also
//! runs it broken, because a launcher that passes either way proves nothing.

#[macro_use]
extern crate spira_test_id;
use release::session_hook::{resolve, Paths};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use testkit::TempDir;

fn sys_path() -> String {
    format!("{}:{}", system_paths::USR_BIN, system_paths::BIN)
}

fn repo() -> PathBuf {
    testkit::manifest_dir().join("..").canonicalize().unwrap()
}

struct Rig {
    dir: TempDir,
    toml: PathBuf,
    release: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = TempDir::new("ctx-meter-client");
        let release = dir.join("rel");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        std::os::unix::fs::symlink(repo().join("spira"), release.join("spira")).unwrap();
        let (prod, run) = (release.join("spira").display().to_string(), dir.join("run").display().to_string());
        let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_PROD", &prod), ("SPIRA_RUN", &run), ("SPIRA_DB", "")]);
        Rig { dir, toml, release }
    }

    fn with_binary(self) -> Rig {
        std::os::unix::fs::symlink(testkit::bin_exe("ctx-meter"), self.release.join("bin/ctx-meter")).unwrap();
        self
    }

    fn paths(&self) -> Paths {
        resolve(&self.release, &sys_path(), &self.toml.display().to_string(), self.dir.join("settings.json"), &self.dir.join("rt"), "").unwrap()
    }

    /// `bash -c <command>` under the environment the client gives it: a home, a bare `PATH`,
    /// and nothing of Spira's.
    fn as_client(&self, command: &str, stdin: &str) -> Output {
        let mut child = Command::new("bash")
            .env_clear()
            .env("HOME", &*self.dir)
            .env("PATH", sys_path())
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// The registered command with its `--run <dir>` removed.
fn without_run(command: &str) -> String {
    let words: Vec<&str> = command.split(' ').collect();
    let at = words.iter().position(|w| *w == "--run").unwrap();
    words.iter().enumerate().filter(|(i, _)| *i != at && *i != at + 1).map(|(_, w)| *w).collect::<Vec<_>>().join(" ")
}

const METER_IN: &str = r#"{"context_window": {"current_usage": {}, "total_input_tokens": 12345}}"#;

// covers: UC-operator-launchers-06, UC-operator-channel-47
#[test_id(tf1gnae)]
#[test]
fn the_registered_status_line_command_prints_the_meter_as_the_client_runs_it() {
    let rig = Rig::new().with_binary();
    let command = rig.paths().meter_command();
    let out = rig.as_client(&command, METER_IN);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("ctx 12k"), "{}", text(&out));

    let broken = rig.as_client(&without_run(&command), METER_IN);
    assert_eq!(broken.status.code(), Some(2), "the meter ran without --run: {}", text(&broken));
    assert!(text(&broken).contains("usage"), "it refused without saying why: {}", text(&broken));
    assert!(broken.stdout.is_empty(), "{}", text(&broken));
}

#[test_id(tx7kd2m)]
#[test]
fn the_registered_command_fails_where_the_release_has_no_meter_binary() {
    let rig = Rig::new();
    let out = rig.as_client(&rig.paths().meter_command(), METER_IN);
    assert_ne!(out.status.code(), Some(0), "{}", text(&out));
    assert!(out.stdout.is_empty(), "{}", text(&out));
}
