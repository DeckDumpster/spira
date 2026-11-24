//! Every command the harness hands to something outside itself, run the way that something
//! runs it: the text the harness registers, under `env_clear`, never a path this target
//! assembled. The scripts are the subject; the programs beside them that other crates own
//! (`spira-config`, `watchd`) are stubs, because an integration target of this crate is not
//! built with those binaries. Each case also runs the command broken, because a launcher
//! that passes either way proves nothing.

#[macro_use]
extern crate spira_test_id;
use release::session_hook::{resolve, Paths};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use testkit::TempDir;

const SPIRA_CONFIG: &str = "#!/bin/bash\ncase \"$1\" in resolve) cat \"$STUB_RESOLVED\" ;; env-bootstrap|check-bd) ;; *) exit 1 ;; esac\n";
const WATCHD: &str = "#!/bin/sh\n[ -n \"$SPIRA_TOML\" ] || { echo 'watchd: SPIRA_TOML is not set' >&2; exit 2; }\nprintf 'NAME UNIT HEALTH UNREAD\\nprobe daemon ok 0\\n'\n";
const SYS_PATH: &str = "/usr/bin:/bin";

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
        let dir = TempDir::new("launchers");
        let release = dir.join("releases/current");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        std::os::unix::fs::symlink(repo().join("spira"), release.join("spira")).unwrap();
        let (run, mail) = (dir.join("run"), dir.join("mail"));
        let (prod, run_s, mail_s) = (release.join("spira").display().to_string(), run.display().to_string(), mail.display().to_string());
        let toml = spira_config::process::fixture_toml(
            &dir,
            &[("SPIRA_PROD", &prod), ("SPIRA_RUN", &run_s), ("SPIRA_MAIL", &mail_s), ("SPIRA_DB", "")],
        );
        let doc = spira_config::validate(&std::fs::read_to_string(&toml).unwrap()).unwrap();
        let home = repo().join("spira");
        let env: BTreeMap<String, String> = [("HOME".to_string(), dir.display().to_string())].into();
        let resolved = spira_config::resolve::resolve(spira_config::resolve::ResolveInput {
            env: &env,
            home: &home,
            repo: &repo(),
            toml: Some(&doc),
            conf_d: &home.join("conf.d"),
        })
        .unwrap_or_else(|_| panic!("the fixture config resolves"));
        std::fs::write(dir.join("resolved.sh"), resolved.to_sh_all()).unwrap();
        for (name, body) in [("spira-config", SPIRA_CONFIG), ("watchd", WATCHD)] {
            testkit::write_exe(release.join("bin").join(name), body);
        }
        Rig { dir, toml, release }
    }

    fn paths(&self) -> Paths {
        resolve(&self.release, SYS_PATH, &self.toml.display().to_string(), self.dir.join("settings.json")).unwrap()
    }

    /// `sh -c <command>` under the environment the client gives it: a home, a bare `PATH`,
    /// and nothing of Spira's.
    fn as_client(&self, command: &str, stdin: &str) -> Output {
        let mut c = Command::new("bash");
        c.env_clear()
            .env("HOME", &*self.dir)
            .env("PATH", SYS_PATH)
            .env("STUB_RESOLVED", self.dir.join("resolved.sh"))
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// A launcher at the repository root, run from the operator's login environment: `SPIRA_TOML`
    /// and `SPIRA_RELEASE` when `profile`, nothing when not.
    fn launcher(&self, script: &str, profile: bool) -> Output {
        let mut c = Command::new(repo().join(script));
        c.env_clear()
            .env("HOME", &*self.dir)
            .env("PATH", format!("{}:{SYS_PATH}", self.release.join("bin").display()))
            .env("TMUX_TMPDIR", &*self.dir)
            .env("STUB_RESOLVED", self.dir.join("resolved.sh"))
            .arg("status")
            .stdin(Stdio::null());
        if profile {
            c.env("SPIRA_TOML", &self.toml).env("SPIRA_RELEASE", &self.release);
        }
        c.output().unwrap()
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// The registered command with its `SPIRA_TOML` assignment removed.
fn unconfigured(command: &str) -> String {
    command.split(' ').filter(|w| !w.starts_with("SPIRA_TOML=")).collect::<Vec<_>>().join(" ")
}

fn assert_launcher_reports_a_state_and_refuses_bare(script: &str) {
    let rig = Rig::new();
    let run = rig.launcher(script, true);
    assert!(!text(&run).contains("SPIRA_TOML"), "{script} named a missing config: {}", text(&run));
    assert!(matches!(run.status.code(), Some(0 | 1)), "{script} status: {}", text(&run));
    let bare = rig.launcher(script, false);
    assert_ne!(bare.status.code(), Some(0), "{script} ran with nothing set: {}", text(&bare));
}

// covers: UC-operator-launchers-02
#[test_id(tsnhuni)]
#[test]
fn cockpit_status_runs_from_a_login_environment_and_refuses_bare() {
    assert_launcher_reports_a_state_and_refuses_bare("cockpit.sh");
}

// covers: UC-operator-launchers-03
#[test_id(t5y02ii)]
#[test]
fn concierge_status_runs_from_a_login_environment_and_refuses_bare() {
    assert_launcher_reports_a_state_and_refuses_bare("concierge.sh");
}

// covers: UC-operator-launchers-05, UC-operator-channel-48
#[test_id(tiga8ed)]
#[test]
fn the_registered_session_start_command_prints_the_watcher_summary_as_the_client_runs_it() {
    let rig = Rig::new();
    let command = rig.paths().hook_command();
    let hook_in = r#"{"hook_event_name":"SessionStart","source":"startup"}"#;
    let out = rig.as_client(&command, hook_in);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("probe"), "no watcher summary: {}", text(&out));

    let broken = rig.as_client(&unconfigured(&command), hook_in);
    assert!(!text(&broken).contains("probe"), "the summary survived without SPIRA_TOML: {}", text(&broken));
}

// covers: UC-operator-launchers-06, UC-operator-channel-47
#[test_id(tf1gnae)]
#[test]
fn the_registered_status_line_command_prints_the_meter_as_the_client_runs_it() {
    let rig = Rig::new();
    let command = rig.paths().meter_command();
    let meter_in = r#"{"context_window": {"current_usage": {}, "total_input_tokens": 12345}}"#;
    let out = rig.as_client(&command, meter_in);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!out.stdout.is_empty() && !text(&out).contains("SPIRA_TOML"), "{}", text(&out));

    let broken = rig.as_client(&unconfigured(&command), meter_in);
    assert_ne!(broken.status.code(), Some(0), "the meter ran without SPIRA_TOML: {}", text(&broken));
}

