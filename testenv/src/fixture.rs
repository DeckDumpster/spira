//! The container tier: one batch container, its install, the shared test-DB baseline and
//! the per-suite exec environment (DESIGN.md §4.2). Everything goes through
//! [`ContainerRuntime`], so the orchestration is tested against a fake.

use crate::record::Mode;
use crate::runtime::{ContainerRuntime, ExecOutcome, ExecRequest};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

// Baked into the image; must agree with the Containerfile (and testenv.sh).
pub const SPIRA_USER: &str = "spirauser";
pub const USER_RUNTIME: &str = "/run/user/1001";
pub const CONTAINER_CARGO: &str = "/var/spira/cargo";
pub const CONTAINER_CARGO_TARGET: &str = "/var/spira/cargo/target";
pub const WORKSPACE: &str = "/workspace";

pub const BASELINE_SCRIPT: &str = ". /workspace/spira/testdb.sh && testdb_up batch_baseline && printf \"TESTDB_NAME=%s\\nTESTDB_DIR=%s\\nTESTDB_BASELINE=%s\\nTESTDB_BD=%s\\nTESTDB_BIN=%s\\nTESTDB_MODE=%s\\n\" \"$TESTDB_NAME\" \"$TESTDB_DIR\" \"${TESTDB_BASELINE:-}\" \"$TESTDB_BD\" \"${TESTDB_BIN:-}\" \"${TESTDB_MODE:-}\"";

/// A harness fault in the container tier, with the exit status it maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// rc 2: the container did not come up, or probe failed.
    Up(String),
    /// rc 3: configure / suspend / install failed.
    Install(String),
}

impl Fault {
    pub fn rc(&self) -> i32 {
        match self {
            Fault::Up(_) => 2,
            Fault::Install(_) => 3,
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Fault::Up(m) | Fault::Install(m) => m,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Dead { detail: String },
}

/// The shared test-DB baseline, when it built completely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestDb {
    pub name: String,
    pub dir: String,
    pub baseline: String,
    pub bd: String,
    pub bin: String,
    pub mode: String,
}

impl TestDb {
    /// Parse testdb_up's `KEY=value` report; None unless NAME and BASELINE are both set.
    pub fn parse(out: &str) -> Option<TestDb> {
        let mut t = TestDb::default();
        for l in out.lines() {
            let Some((k, v)) = l.split_once('=') else {
                continue;
            };
            let v = v.to_string();
            match k {
                "TESTDB_NAME" => t.name = v,
                "TESTDB_DIR" => t.dir = v,
                "TESTDB_BASELINE" => t.baseline = v,
                "TESTDB_BD" => t.bd = v,
                "TESTDB_BIN" => t.bin = v,
                "TESTDB_MODE" => t.mode = v,
                _ => {}
            }
        }
        (!t.name.is_empty() && !t.baseline.is_empty()).then_some(t)
    }

    /// Shared: each suite copies the baseline (~26 ms). None: each runs bd init (~6 s).
    pub fn env(shared: Option<&TestDb>) -> Vec<(String, String)> {
        let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
        match shared {
            Some(t) => vec![
                kv("TESTDB_SHARED", "1"),
                kv("TESTDB_NAME", &t.name),
                kv("TESTDB_DIR", &t.dir),
                kv("TESTDB_BASELINE", &t.baseline),
                kv("TESTDB_BD", &t.bd),
                kv("TESTDB_BIN", &t.bin),
                kv("TESTDB_MODE", &t.mode),
            ],
            None => vec![
                kv("TESTDB_SHARED", "0"),
                kv("TESTDB_NAME", ""),
                kv("TESTDB_DIR", ""),
            ],
        }
    }
}

/// podman's own pre-exec refusal: the `--user` account vanished from the container.
pub fn is_user_account_fault(output: &str) -> bool {
    match output.find("unable to find user") {
        Some(i) => output[i..].contains("passwd file"),
        None => false,
    }
}

pub struct Session<'a> {
    pub rt: &'a dyn ContainerRuntime,
    pub name: String,
    pub instance: String,
    /// `/workspace/target/<profile-dir>` — the artifact under test, as the container sees it.
    pub artifacts: String,
    pub liveness_retries: u32,
    pub liveness_sleep: Duration,
}

fn kv(k: &str, v: impl Into<String>) -> (String, String) {
    (k.to_string(), v.into())
}

impl<'a> Session<'a> {
    pub fn new(rt: &'a dyn ContainerRuntime, instance: &str, profile_dir: &str) -> Self {
        Session {
            rt,
            name: format!("spira-batch-{instance}"),
            instance: instance.to_string(),
            artifacts: format!("{WORKSPACE}/target/{profile_dir}"),
            liveness_retries: 3,
            liveness_sleep: Duration::from_secs(3),
        }
    }

    /// `/tmp/spira-batch-<instance>`: the batch's SPIRA_RUN inside the container.
    pub fn batch_run(&self) -> String {
        format!("/tmp/spira-batch-{}", self.instance)
    }

    pub fn testdb_data(&self) -> String {
        format!("{}/testdb", self.batch_run())
    }

    /// The artifact contract every exec that runs harness code carries (DESIGN.md §5).
    pub fn artifact_env(&self) -> Vec<(String, String)> {
        vec![
            kv("SPIRA_ARTIFACTS", &self.artifacts),
            kv("SPIRA_ARTIFACTS_ROOT", WORKSPACE),
            kv(
                "SPIRA_TEST_PLAN_BIN",
                format!("{}/test-plan", self.artifacts),
            ),
        ]
    }

    fn user_env(&self) -> Vec<(String, String)> {
        vec![
            kv("XDG_RUNTIME_DIR", USER_RUNTIME),
            kv(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={USER_RUNTIME}/bus"),
            ),
            kv("CARGO_HOME", CONTAINER_CARGO),
            kv("CARGO_TARGET_DIR", CONTAINER_CARGO_TARGET),
        ]
    }

    fn as_user(&self, argv: &[&str], env: Vec<(String, String)>) -> ExecRequest {
        ExecRequest::new(&self.name, argv)
            .user(SPIRA_USER)
            .env(&env)
    }

    /// `testenv.sh up --name <n> --checkout <worktree>`, then `testenv.sh probe`.
    pub fn up(&self, checkout: &Path) -> Result<(), Fault> {
        let up = self.rt.testenv(&[
            "up".into(),
            "--name".into(),
            self.name.clone(),
            "--checkout".into(),
            checkout.display().to_string(),
        ]);
        if !up.ok() {
            return Err(Fault::Up(format!(
                "container {} did not come up",
                self.name
            )));
        }
        let probe = self
            .rt
            .testenv(&["probe".into(), "--name".into(), self.name.clone()]);
        if !probe.ok() {
            return Err(Fault::Up(format!(
                "probe failed — user systemd not available in {}",
                self.name
            )));
        }
        Ok(())
    }

    pub fn configure_request(&self) -> ExecRequest {
        let mut env = self.user_env();
        env.extend([
            kv("CONFIGURE_PROD", format!("{WORKSPACE}/spira")),
            kv("CONFIGURE_MAX_AEONS", "1"),
            kv("CONFIGURE_MAX_LIVE_AEONS", "1"),
            kv("CONFIGURE_LOOM_ADDR", "127.0.0.1:7300"),
            kv("CONFIGURE_DOLT_DATA", ""),
        ]);
        env.extend(self.artifact_env());
        self.as_user(&["bash", "/workspace/spira/configure.sh"], env)
    }

    pub fn suspend_request(&self, unit: &str) -> ExecRequest {
        let mut env = vec![
            kv("XDG_RUNTIME_DIR", USER_RUNTIME),
            kv("SPIRA_RUN", self.batch_run()),
        ];
        env.extend(self.artifact_env());
        self.as_user(
            &[
                "bash",
                "/workspace/spira/ctrl.sh",
                "suspend",
                unit,
                "--reason",
                "image rustc too old for lockfile v4",
                "--owner",
                "sp-fud1",
            ],
            env,
        )
    }

    pub fn install_request(&self) -> ExecRequest {
        let mut env = self.user_env();
        env.extend([
            kv("SPIRA_PROD", format!("{WORKSPACE}/spira")),
            kv("SPIRA_INSTALL_FORCE", "1"),
            kv("SPIRA_RUN", self.batch_run()),
            kv("SPIRA_TESTDB_DATA", self.testdb_data()),
        ]);
        env.extend(self.artifact_env());
        self.as_user(
            &["bash", "/workspace/systemd/install.sh", &self.instance],
            env,
        )
    }

    /// configure, suspend the Rust-backed units the image cannot build, install.
    pub fn install(&self, log: &dyn Fn(&str)) -> Result<(), Fault> {
        log(&format!("configure inside {}", self.name));
        let out = self.rt.exec(&self.configure_request());
        if !out.ok() {
            log(&format!("configure rc={} output tail:\n{}", out.rc, out.tail(40)));
            return Err(Fault::Install("configure failed — harness fault".into()));
        }
        log(&format!(
            "suspending Rust-backed units inside {} (image rustc too old for lockfile v4)",
            self.name
        ));
        for unit in ["spira-loom", "spira-cockpit"] {
            if !self.rt.exec(&self.suspend_request(unit)).ok() {
                return Err(Fault::Install(format!(
                    "ctrl suspend failed for {unit} — harness fault"
                )));
            }
        }
        log(&format!(
            "install instance {} inside {}",
            self.instance, self.name
        ));
        let out = self.rt.exec(&self.install_request());
        if !out.ok() {
            log(&format!("install rc={} output tail:\n{}", out.rc, out.tail(40)));
            return Err(Fault::Install("install failed — harness fault".into()));
        }
        Ok(())
    }

    /// Each distinct token checked once with `command -v` inside the container.
    pub fn unmet_requirements<'t>(
        &self,
        tokens: impl IntoIterator<Item = &'t str>,
    ) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut unmet = BTreeSet::new();
        for t in tokens {
            if t.is_empty() || t == "testenv" || !seen.insert(t.to_string()) {
                continue;
            }
            let req = self.as_user(
                &["bash", "-c", "command -v \"$1\" >/dev/null 2>&1", "_", t],
                vec![kv("XDG_RUNTIME_DIR", USER_RUNTIME)],
            );
            if !self.rt.exec(&req).ok() {
                unmet.insert(t.to_string());
            }
        }
        unmet
    }

    pub fn baseline_request(&self) -> ExecRequest {
        let mut env = vec![
            kv("XDG_RUNTIME_DIR", USER_RUNTIME),
            kv("SPIRA_TESTDB_DATA", self.testdb_data()),
        ];
        env.extend(self.artifact_env());
        self.as_user(&["bash", "-c", BASELINE_SCRIPT], env)
    }

    /// `testenv testdb template` with the artifact under test: the server-mode template is
    /// built once here, so no server-mode suite waits on it (DESIGN-testdb.md §2.1).
    pub fn testdb_template_request(&self) -> ExecRequest {
        let mut env = vec![kv("XDG_RUNTIME_DIR", USER_RUNTIME)];
        env.extend(self.artifact_env());
        let exe = format!("{}/testenv", self.artifacts);
        self.as_user(
            &[&exe, "testdb", "template", "--bd", "bd", "--dolt", "dolt"],
            env,
        )
    }

    /// Build the shared baseline once; None falls back to per-suite databases.
    pub fn baseline(&self) -> (Option<TestDb>, ExecOutcome) {
        let out = self.rt.exec(&self.baseline_request());
        let db = if out.ok() {
            TestDb::parse(&out.output)
        } else {
            None
        };
        (db, out)
    }

    /// Per-suite SPIRA_RUN in parallel mode (`n` is the 1-based launch index).
    pub fn suite_run(&self, n: usize) -> String {
        format!("/tmp/spira-batch-{}-{n}", self.instance)
    }

    pub fn suite_home(&self, n: usize) -> String {
        format!("{}/home", self.suite_run(n))
    }

    /// The bd call log a suite's run leaves (read into bd_calls/bd_ms).
    pub fn bd_log(&self, mode: Mode, n: usize, suite: &str) -> String {
        match mode {
            Mode::Serial => format!("{}/bd/{suite}.log", self.batch_run()),
            Mode::Parallel => format!("{}/bd-calls.log", self.suite_run(n)),
        }
    }

    pub fn bd_timing_dir(&self) -> String {
        format!("/tmp/bd-timing-{}", self.instance)
    }

    pub fn suite_request(
        &self,
        mode: Mode,
        n: usize,
        suite: &str,
        testdb: Option<&TestDb>,
    ) -> ExecRequest {
        let mut env = Vec::new();
        if mode == Mode::Parallel {
            env.push(kv("HOME", self.suite_home(n)));
        }
        env.extend(self.user_env());
        env.push(kv("SPIRA_IN_TESTENV", "1"));
        env.extend(self.artifact_env());
        env.extend(TestDb::env(testdb));
        env.push(kv("TMUX", ""));
        env.push(kv("SPIRA_PATH", ""));
        env.push(kv("SPIRA_BD_LOG", self.bd_log(mode, n, suite)));
        match mode {
            Mode::Parallel => {
                env.push(kv("SPIRA_INSTANCE", format!("{}-{n}", self.instance)));
                env.push(kv("SPIRA_RUN", self.suite_run(n)));
            }
            Mode::Serial => env.push(kv("SPIRA_RUN", self.batch_run())),
        }
        env.push(kv("SPIRA_TESTDB_DATA", self.testdb_data()));
        env.push(kv(
            "SPIRA_BD_TIMING_LOG",
            format!("{}/{suite}.log", self.bd_timing_dir()),
        ));
        let path = format!("{WORKSPACE}/spira/{suite}");
        self.as_user(&["bash", &path], env)
    }

    /// The private HOME a parallel suite gets, created before it starts. The bd meter
    /// (bdmeter.rs) makes it and links itself in as the suite's `bd`, so `bd_ms` is measured;
    /// an artifact set without the meter falls back to the bare directory, unmetered.
    pub fn make_home(&self, n: usize) {
        let home = self.suite_home(n);
        let meter = format!("{}/bd-meter", self.artifacts);
        let installed = self
            .rt
            .exec(&ExecRequest::new(&self.name, &[&meter, "--install", &home]).user(SPIRA_USER))
            .ok();
        if !installed {
            let dir = format!("{home}/.config/systemd/user");
            let _ = self
                .rt
                .exec(&ExecRequest::new(&self.name, &["mkdir", "-p", &dir]).user(SPIRA_USER));
        }
    }

    /// Contents of a file inside the container, or empty.
    pub fn read_file(&self, path: &str) -> String {
        let out = self
            .rt
            .exec(&self.as_user(&["cat", path], vec![kv("XDG_RUNTIME_DIR", USER_RUNTIME)]));
        if out.ok() {
            out.output
        } else {
            String::new()
        }
    }

    /// A single failed or empty inspect is transient; only a definitive `false`, or
    /// `liveness_retries` non-true answers in a row, is death. A failed lookup says so.
    pub fn liveness(&self) -> Liveness {
        let mut n = 0;
        loop {
            match self.rt.inspect(&self.name, "{{.State.Running}}").as_deref() {
                Some("true") => return Liveness::Alive,
                Some("false") => break,
                _ => {
                    n += 1;
                    if n >= self.liveness_retries {
                        break;
                    }
                    std::thread::sleep(self.liveness_sleep);
                }
            }
        }
        let exit = self
            .rt
            .inspect(&self.name, "{{.State.ExitCode}}")
            .unwrap_or_else(|| "inspect-failed".into());
        let oom = self
            .rt
            .inspect(&self.name, "{{.State.OOMKilled}}")
            .unwrap_or_else(|| "inspect-failed".into());
        Liveness::Dead {
            detail: format!("ExitCode={exit} OOMKilled={oom}"),
        }
    }

    /// Peak memory of the container's cgroup in MiB, if readable.
    pub fn cgroup_peak_mib(&self) -> Option<u64> {
        let cg = self.rt.inspect(&self.name, "{{.State.CgroupPath}}")?;
        let p = format!("/sys/fs/cgroup/{}/memory.peak", cg.trim_start_matches('/'));
        let bytes: u64 = std::fs::read_to_string(p).ok()?.trim().parse().ok()?;
        (bytes > 0).then_some(bytes / 1_048_576)
    }

    pub fn down(&self) -> ExecOutcome {
        self.rt.testenv(&[
            "down".into(),
            "--name".into(),
            self.name.clone(),
            "--volumes".into(),
        ])
    }
}

#[cfg(test)]
pub mod fake {
    //! A scripted container runtime: records every call; answers exec by the first rule
    //! whose predicate matches the request.
    use super::*;
    use std::sync::Mutex;

    type Answer = std::sync::Arc<dyn Fn(&ExecRequest) -> ExecOutcome + Send + Sync>;
    type Rule = (Box<dyn Fn(&ExecRequest) -> bool + Send + Sync>, Answer);

    #[derive(Default)]
    pub struct FakeRuntime {
        pub execs: Mutex<Vec<ExecRequest>>,
        pub testenv_calls: Mutex<Vec<Vec<String>>>,
        pub purged: Mutex<Vec<String>>,
        rules: Mutex<Vec<Rule>>,
        pub running: Mutex<Vec<Option<String>>>,
        pub testenv_rc: Mutex<std::collections::HashMap<String, i32>>,
        pub containers: Mutex<Vec<String>>,
        pub inspect_extra: Mutex<std::collections::HashMap<String, String>>,
    }

    impl FakeRuntime {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn on(
            &self,
            pred: impl Fn(&ExecRequest) -> bool + Send + Sync + 'static,
            ans: impl Fn(&ExecRequest) -> ExecOutcome + Send + Sync + 'static,
        ) {
            self.rules
                .lock()
                .unwrap()
                .push((Box::new(pred), std::sync::Arc::new(ans)));
        }
        /// Answer a suite script (`bash /workspace/spira/<suite>`) with rc and output.
        pub fn suite(&self, suite: &str, rc: i32, output: &str) {
            let path = format!("/workspace/spira/{suite}");
            let out = output.to_string();
            self.on(
                move |r| r.argv.get(1).map(String::as_str) == Some(path.as_str()),
                move |_| ExecOutcome {
                    rc,
                    output: out.clone(),
                },
            );
        }
        /// Successive answers to `{{.State.Running}}`; when exhausted, "true".
        pub fn running_answers(&self, answers: &[Option<&str>]) {
            *self.running.lock().unwrap() = answers.iter().map(|a| a.map(str::to_string)).collect();
        }
        pub fn exec_argv(&self) -> Vec<Vec<String>> {
            self.execs
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.argv.clone())
                .collect()
        }
        pub fn suite_execs(&self) -> Vec<ExecRequest> {
            self.execs
                .lock()
                .unwrap()
                .iter()
                .filter(|r| {
                    r.argv
                        .get(1)
                        .is_some_and(|a| a.starts_with("/workspace/spira/test-"))
                })
                .cloned()
                .collect()
        }
    }

    impl ContainerRuntime for FakeRuntime {
        fn exec(&self, req: &ExecRequest) -> ExecOutcome {
            self.execs.lock().unwrap().push(req.clone());
            // The answer runs outside the lock, so concurrent execs really overlap.
            let ans = {
                let rules = self.rules.lock().unwrap();
                rules.iter().find(|(p, _)| p(req)).map(|(_, a)| a.clone())
            };
            let out = ans.map(|a| a(req)).unwrap_or_default();
            if let Some(p) = &req.output {
                let _ = std::fs::write(p, &out.output);
            }
            out
        }
        fn inspect(&self, name: &str, format: &str) -> Option<String> {
            if format == "{{.State.Running}}" {
                let mut r = self.running.lock().unwrap();
                if r.is_empty() {
                    return Some("true".into());
                }
                return r.remove(0);
            }
            self.inspect_extra
                .lock()
                .unwrap()
                .get(&format!("{name} {format}"))
                .cloned()
        }
        fn exists(&self, name: &str) -> bool {
            self.containers.lock().unwrap().iter().any(|c| c == name)
        }
        fn names_with_prefix(&self, prefix: &str) -> Vec<String> {
            self.containers
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with(prefix))
                .cloned()
                .collect()
        }
        fn purge(&self, name: &str) {
            self.purged.lock().unwrap().push(name.into());
            self.containers.lock().unwrap().retain(|c| c != name);
        }
        fn testenv(&self, args: &[String]) -> ExecOutcome {
            self.testenv_calls.lock().unwrap().push(args.to_vec());
            let sub = args.first().cloned().unwrap_or_default();
            let rc = self
                .testenv_rc
                .lock()
                .unwrap()
                .get(&sub)
                .copied()
                .unwrap_or(0);
            if sub == "down" && rc == 0 {
                if let Some(i) = args.iter().position(|a| a == "--name") {
                    let n = args[i + 1].clone();
                    self.containers.lock().unwrap().retain(|c| *c != n);
                }
            }
            if sub == "up" && rc == 0 {
                if let Some(i) = args.iter().position(|a| a == "--name") {
                    self.containers.lock().unwrap().push(args[i + 1].clone());
                }
            }
            ExecOutcome {
                rc,
                output: if sub == "tag" {
                    "tag123\n".into()
                } else {
                    String::new()
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeRuntime;
    use super::*;

    fn session(rt: &FakeRuntime) -> Session<'_> {
        let mut s = Session::new(rt, "abc123", "aeon");
        s.liveness_sleep = Duration::from_millis(1);
        s
    }

    #[test]
    fn a_suite_home_is_made_by_the_bd_meter_and_falls_back_to_mkdir_without_it() {
        let rt = FakeRuntime::new();
        let s = session(&rt);
        s.make_home(3);
        let argv = rt.exec_argv();
        assert_eq!(argv.len(), 1, "one exec when the meter is in the artifact set");
        assert_eq!(
            argv[0],
            vec![
                format!("{}/bd-meter", s.artifacts),
                "--install".into(),
                s.suite_home(3)
            ]
        );
        let rt = FakeRuntime::new();
        rt.on(
            |r| r.argv[0].ends_with("/bd-meter"),
            |_| ExecOutcome { rc: 127, output: "not found".into() },
        );
        let s = session(&rt);
        s.make_home(1);
        let argv = rt.exec_argv();
        assert_eq!(argv.len(), 2);
        assert_eq!(
            argv[1],
            vec!["mkdir".to_string(), "-p".into(), format!("{}/.config/systemd/user", s.suite_home(1))]
        );
    }

    #[test]
    fn testdb_template_runs_the_artifact_under_test_as_the_suite_user() {
        let rt = FakeRuntime::new();
        let s = session(&rt);
        let r = s.testdb_template_request();
        assert_eq!(r.argv[0], format!("{}/testenv", s.artifacts));
        assert_eq!(r.argv[1..], ["testdb", "template", "--bd", "bd", "--dolt", "dolt"]);
        assert_eq!(r.user.as_deref(), Some(SPIRA_USER));
        assert_eq!(r.env_value("SPIRA_ARTIFACTS"), Some("/workspace/target/aeon"));
    }

    #[test]
    fn install_runs_configure_suspends_then_install_with_artifacts() {
        let rt = FakeRuntime::new();
        let s = session(&rt);
        s.install(&|_| {}).unwrap();
        let argv = rt.exec_argv();
        assert_eq!(argv[0], vec!["bash", "/workspace/spira/configure.sh"]);
        assert_eq!(
            argv[1][..4],
            ["bash", "/workspace/spira/ctrl.sh", "suspend", "spira-loom"]
        );
        assert_eq!(argv[2][3], "spira-cockpit");
        assert_eq!(
            argv[3],
            vec!["bash", "/workspace/systemd/install.sh", "abc123"]
        );
        let execs = rt.execs.lock().unwrap();
        for r in execs.iter() {
            assert_eq!(r.user.as_deref(), Some("spirauser"));
            assert_eq!(
                r.env_value("SPIRA_ARTIFACTS"),
                Some("/workspace/target/aeon")
            );
            assert_eq!(r.container, "spira-batch-abc123");
        }
        assert_eq!(
            execs[0].env_value("CONFIGURE_PROD"),
            Some("/workspace/spira")
        );
        assert_eq!(execs[0].env_value("CONFIGURE_DOLT_DATA"), Some(""));
        assert_eq!(execs[3].env_value("SPIRA_INSTALL_FORCE"), Some("1"));
        assert_eq!(
            execs[3].env_value("SPIRA_RUN"),
            Some("/tmp/spira-batch-abc123")
        );
        assert_eq!(
            execs[3].env_value("SPIRA_TESTDB_DATA"),
            Some("/tmp/spira-batch-abc123/testdb")
        );
    }

    #[test]
    fn a_failed_step_is_an_install_fault_and_stops() {
        let rt = FakeRuntime::new();
        rt.on(
            |r| r.argv.get(2).map(String::as_str) == Some("suspend"),
            |_| ExecOutcome {
                rc: 1,
                output: String::new(),
            },
        );
        let e = session(&rt).install(&|_| {}).unwrap_err();
        assert_eq!(e.rc(), 3);
        assert!(e.message().contains("spira-loom"));
        assert_eq!(rt.exec_argv().len(), 2);
    }

    #[test]
    fn up_then_probe_and_their_faults() {
        let rt = FakeRuntime::new();
        session(&rt).up(Path::new("/wt")).unwrap();
        let calls = rt.testenv_calls.lock().unwrap().clone();
        assert_eq!(
            calls[0],
            vec!["up", "--name", "spira-batch-abc123", "--checkout", "/wt"]
        );
        assert_eq!(calls[1], vec!["probe", "--name", "spira-batch-abc123"]);
        let rt = FakeRuntime::new();
        rt.testenv_rc.lock().unwrap().insert("probe".into(), 1);
        assert_eq!(session(&rt).up(Path::new("/wt")).unwrap_err().rc(), 2);
    }

    #[test]
    fn requirements_are_checked_once_each_and_testenv_is_always_met() {
        let rt = FakeRuntime::new();
        rt.on(
            |r| r.argv.last().map(String::as_str) == Some("dolt"),
            |_| ExecOutcome {
                rc: 1,
                output: String::new(),
            },
        );
        let unmet = session(&rt).unmet_requirements(["jq", "dolt", "jq", "testenv"]);
        assert_eq!(unmet.into_iter().collect::<Vec<_>>(), vec!["dolt"]);
        assert_eq!(rt.exec_argv().len(), 2);
    }

    #[test]
    fn baseline_parses_or_falls_back() {
        let rt = FakeRuntime::new();
        rt.on(|r| r.argv.get(1).map(String::as_str) == Some("-c") && r.argv[2].contains("testdb_up"), |_| ExecOutcome {
            rc: 0,
            output: "noise\nTESTDB_NAME=batch_baseline\nTESTDB_DIR=/d\nTESTDB_BASELINE=/b\nTESTDB_BD=bd\nTESTDB_BIN=\nTESTDB_MODE=file\n".into(),
        });
        let (db, _) = session(&rt).baseline();
        let db = db.unwrap();
        assert_eq!(db.mode, "file");
        let env = TestDb::env(Some(&db));
        assert!(env.contains(&("TESTDB_SHARED".into(), "1".into())));
        assert!(TestDb::parse("TESTDB_NAME=x\n").is_none());
        assert_eq!(
            TestDb::env(None),
            vec![
                ("TESTDB_SHARED".to_string(), "0".to_string()),
                ("TESTDB_NAME".into(), "".into()),
                ("TESTDB_DIR".into(), "".into())
            ]
        );
    }

    #[test]
    fn parallel_suites_get_private_home_instance_and_run() {
        let rt = FakeRuntime::new();
        let s = session(&rt);
        let r = s.suite_request(Mode::Parallel, 3, "test-a.sh", None);
        assert_eq!(r.argv, vec!["bash", "/workspace/spira/test-a.sh"]);
        assert_eq!(
            r.env[0],
            ("HOME".into(), "/tmp/spira-batch-abc123-3/home".into())
        );
        assert_eq!(r.env_value("SPIRA_INSTANCE"), Some("abc123-3"));
        assert_eq!(r.env_value("SPIRA_RUN"), Some("/tmp/spira-batch-abc123-3"));
        assert_eq!(
            r.env_value("SPIRA_BD_LOG"),
            Some("/tmp/spira-batch-abc123-3/bd-calls.log")
        );
        assert_eq!(r.env_value("SPIRA_IN_TESTENV"), Some("1"));
        assert_eq!(r.env_value("TMUX"), Some(""));
        assert_eq!(
            r.env_value("SPIRA_TEST_PLAN_BIN"),
            Some("/workspace/target/aeon/test-plan") // path-ok: the container-side path suites get
        );
        assert_eq!(r.env_value("SPIRA_ARTIFACTS_ROOT"), Some("/workspace"));
        let r = s.suite_request(Mode::Serial, 1, "test-a.sh", None);
        assert_eq!(r.env_value("HOME"), None);
        assert_eq!(r.env_value("SPIRA_INSTANCE"), None);
        assert_eq!(r.env_value("SPIRA_RUN"), Some("/tmp/spira-batch-abc123"));
        assert_eq!(
            r.env_value("SPIRA_BD_LOG"),
            Some("/tmp/spira-batch-abc123/bd/test-a.sh.log")
        );
    }

    #[test]
    fn liveness_retries_empties_but_believes_a_definitive_false() {
        let rt = FakeRuntime::new();
        rt.running_answers(&[None, Some("true")]);
        assert_eq!(session(&rt).liveness(), Liveness::Alive);
        let rt = FakeRuntime::new();
        rt.running_answers(&[Some("false")]);
        assert_eq!(
            session(&rt).liveness(),
            Liveness::Dead {
                detail: "ExitCode=inspect-failed OOMKilled=inspect-failed".into()
            }
        );
        let rt = FakeRuntime::new();
        rt.running_answers(&[None, None, None]);
        assert!(matches!(session(&rt).liveness(), Liveness::Dead { .. }));
    }

    #[test]
    fn account_fault_shape() {
        assert!(is_user_account_fault(
            "Error: unable to find user spirauser: no matching entries in passwd file"
        ));
        assert!(!is_user_account_fault("not ok 1 - passwd file unable"));
    }
}
