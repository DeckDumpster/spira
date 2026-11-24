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
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use testkit::TempDir;

const SPIRA_CONFIG: &str = "#!/bin/bash\ncase \"$1\" in resolve) cat \"$STUB_RESOLVED\" ;; env-bootstrap|check-bd) ;; *) exit 1 ;; esac\n";
const WATCHD: &str = "#!/bin/sh\n[ -n \"$SPIRA_TOML\" ] || { echo 'watchd: manifest unreadable: SPIRA_TOML is not set' >&2; exit 2; }\nprintf 'NAME UNIT HEALTH UNREAD\\nprobe daemon ok 0\\n'\n";
fn sys_path() -> String {
    format!("{}:{}", system_paths::USR_BIN, system_paths::BIN)
}

fn repo() -> PathBuf {
    testkit::manifest_dir().join("../..").canonicalize().unwrap()
}

struct Rig {
    dir: TempDir,
    toml: PathBuf,
    release: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = TempDir::new("launchers");
        let release = dir.join("rel");
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
        resolve(&self.release, &sys_path(), &self.toml.display().to_string(), self.dir.join("settings.json")).unwrap()
    }

    /// `sh -c <command>` under the environment the client gives it: a home, a bare `PATH`,
    /// and nothing of Spira's.
    fn as_client(&self, command: &str, stdin: &str) -> Output {
        let mut c = Command::new("bash");
        c.env_clear()
            .env("HOME", &*self.dir)
            .env("PATH", sys_path())
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
    fn launcher(&self, script: &str, profile: Profile) -> Output {
        let mut c = Command::new(repo().join(script));
        c.env_clear()
            .env("HOME", &*self.dir)
            .env("PATH", format!("{}:{}", self.release.join("bin").display(), sys_path()))
            .env("TMUX_TMPDIR", &*self.dir)
            .env("STUB_RESOLVED", self.dir.join("resolved.sh"))
            .arg("status")
            .stdin(Stdio::null());
        if profile != Profile::Nothing {
            c.env("SPIRA_RELEASE", &self.release);
        }
        if profile == Profile::Full {
            c.env("SPIRA_TOML", &self.toml);
        }
        c.output().unwrap()
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Profile {
    Full,
    WithoutToml,
    Nothing,
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
    link_built(&rig, "rebuild");
    let run = rig.launcher(script, Profile::Full);
    assert!(!text(&run).contains("SPIRA_TOML") && !text(&run).contains("refusing"), "{script} refused with its config set: {}", text(&run));
    assert!(matches!(run.status.code(), Some(0 | 1)), "{script} status: {}", text(&run));
    for profile in [Profile::WithoutToml, Profile::Nothing] {
        let bare = rig.launcher(script, profile);
        assert_ne!(bare.status.code(), Some(0), "{script} ran without its config: {}", text(&bare));
        if profile == Profile::WithoutToml {
            assert!(text(&bare).contains("SPIRA_TOML"), "{script} refused without saying why: {}", text(&bare));
        }
    }
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


fn link_built(rig: &Rig, name: &str) {
    std::os::unix::fs::symlink(testkit::bin_exe(name), rig.release.join("bin").join(name)).unwrap();
}

// covers: UC-operator-launchers-04
#[test_id(tcpd4mi)]
#[test]
fn the_ops_pane_renders_once_from_a_login_environment_and_names_the_missing_config_without_it() {
    let rig = Rig::new();
    link_built(&rig, "health");
    let health = |profile: bool| {
        let mut c = Command::new(rig.release.join("bin/health"));
        c.env_clear().env("HOME", &*rig.dir).env("PATH", sys_path()).env("SPIRA_RELEASE", &rig.release).arg("once").stdin(Stdio::null());
        if profile {
            c.env("SPIRA_TOML", &rig.toml);
        }
        c.output().unwrap()
    };
    let run = health(true);
    assert_eq!(run.status.code(), Some(0), "{}", text(&run));
    assert!(text(&run).contains("SPIRA"), "{}", text(&run));
    assert!(!text(&run).contains("SPIRA_TOML"), "{}", text(&run));
    assert!(text(&health(false)).contains("SPIRA_TOML is not set"), "the bare environment must be bare");
}

/// Runs `command` under `sh -c` on a pseudo-terminal that is its controlling one, as a tmux
/// pane does, and returns what it drew once `until` appears or the cap is reached.
fn on_a_pty(command: &str, env: &[(&str, &Path)], until: &[&str]) -> String {
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    let (mut master, mut slave) = (0, 0);
    let size = libc::winsize { ws_row: 50, ws_col: 160, ws_xpixel: 0, ws_ypixel: 0 };
    assert_eq!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &size) }, 0);
    let dup = |fd| Stdio::from(unsafe { OwnedFd::from_raw_fd(libc::dup(fd)) });
    let mut c = Command::new("sh");
    c.env_clear().env("TERM", "xterm").args(["-c", command]).stdin(dup(slave)).stdout(dup(slave)).stderr(dup(slave));
    for (k, v) in env {
        c.env(k, v);
    }
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY, 0);
            Ok(())
        });
    }
    let mut child = testkit::ChildGuard::spawn(&mut c);
    unsafe { libc::close(slave) };
    let mut reader = unsafe { std::fs::File::from_raw_fd(master) };
    let (mut seen, mut buf) = (String::new(), [0u8; 8192]);
    for _ in 0..100 {
        let mut p = libc::pollfd { fd: master, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut p, 1, 100) } > 0 {
            match reader.read(&mut buf) {
                Ok(n) if n > 0 => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
                _ => break,
            }
        }
        if until.iter().any(|u| seen.contains(u)) {
            break;
        }
    }
    child.kill();
    seen
}

// covers: UC-cockpit-observability-51
#[test_id(t04pbhv)]
#[test]
fn the_ops_pane_start_command_resolves_its_config_and_draws_the_lifecycle_view() {
    let rig = Rig::new();
    link_built(&rig, "lc-view");
    let conf = cockpit_ops::layout::Conf {
        spira_release: rig.release.display().to_string(),
        cock: PathBuf::new(),
        run: PathBuf::new(),
        cwd: String::new(),
        right_pct: 33,
        bottom_pct: 28,
        mail_cmd: None,
        heal_cooldown_secs: 60,
        idle_secs: 0,
        mouse_on: true,
        clipboard_on: true,
        spira_repo: String::new(),
        spira_conf: None,
    };
    let command = conf.health_cmd();
    let server_env = [("HOME", &*rig.dir as &Path), ("SPIRA_TOML", rig.toml.as_path())];
    let drawn = on_a_pty(&command, &server_env, &["refresh", "SPIRA_TOML is not set"]);
    assert!(drawn.contains("refresh"), "no frame: {drawn}");
    assert!(!drawn.contains("SPIRA_TOML is not set") && !drawn.contains("cannot resolve"), "{drawn}");

    let without_config = on_a_pty(&command, &server_env[..1], &["refresh", "SPIRA_TOML is not set"]);
    assert!(without_config.contains("SPIRA_TOML is not set"), "the pane drew a frame with no config: {without_config}");
}
