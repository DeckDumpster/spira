//! The container tier: one batch container, its install, the shared test-DB baseline and
//! the per-suite exec environment (DESIGN.md §4.2). Everything goes through
//! [`ContainerRuntime`], so the orchestration is tested against a fake.

use crate::plan;
use crate::record::Mode;
use crate::container::RC_QUEUE;
use crate::runtime::{ContainerRuntime, ExecOutcome, ExecRequest, RC_DEADLINE};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

// Baked into the image; must agree with the Containerfile (and container.rs).
/// Units suspended (ctrl.sh) before install, each with the reason it cannot run in the
/// container. Loom and the cockpit collector need a live Dolt data directory, and the
/// container is configured with none (`CONFIGURE_DOLT_DATA=`); the image's rustc is not the
/// reason. The queue-watch watcher execs `/workspace/bin/queue-watch`, which the
/// container never has (artifacts live under `target/<profile>`), so it exits 127 on every
/// start: a CPUQuota on its unit used to keep it `active` long enough to pass install's
/// is-active check; with quotas retired (sp-b4oct) it is seen in `activating` (auto-restart)
/// and install refuses, so it is suspended like the other Rust-backed units.
pub const SUSPENDED_UNITS: [(&str, &str); 3] = [
    ("spira-loom", "container is configured without a Dolt data directory, so it has no backing store"),
    ("spira-cockpit", "container is configured without a Dolt data directory, so it has no backing store"),
    (
        "spira-watch-queue-watch",
        "renders /workspace/bin/queue-watch, which the container does not have",
    ),
];

pub const SPIRA_USER: &str = "spirauser";
pub const USER_RUNTIME: &str = "/run/user/1001";
pub const CONTAINER_CARGO: &str = "/var/spira/cargo";
pub const CONTAINER_CARGO_TARGET: &str = "/var/spira/cargo/target";
pub const WORKSPACE: &str = "/workspace";
/// Where every test database lives inside the container (sp-t26yx): the container's `/tmp`
/// is a tmpfs (`podman run --systemd=true`), so a throwaway database's fsyncs never reach the
/// host disk. On the overlay (`/var/tmp`) a `bd init`'s CREATE DATABASE queued behind the
/// host's writeback and overran bd's 10 s read timeout (`[mysql] i/o timeout`).
pub const CONTAINER_TESTDB_ROOT: &str = "/tmp/spira-testdb";

/// What the one setup exec established ([`Session::setup`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Setup {
    /// Requirement tokens `command -v` did not find.
    pub unmet: BTreeSet<String>,
    /// The testdb template step's rc and output.
    pub template: ExecOutcome,
    pub template_ms: u64,
    /// Seconds each setup phase ran inside the container, in order.
    pub phase_secs: Vec<(&'static str, f64)>,
    /// The exec's wall on the host; the difference is podman's own cost.
    pub wall_secs: f64,
}

/// A setup that did not complete, with the VERDICT reason word it maps to (`stage`,
/// `install`, or `deadline` for [`Fault::Deadline`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupFault {
    pub fault: Fault,
    pub reason: &'static str,
}

/// The template and every server fixture live under [`CONTAINER_TESTDB_ROOT`], and
/// `testenv testdb` refuses a root that is not a tmpfs rather than silently fsync to disk.
pub fn testdb_root_env() -> Vec<(String, String)> {
    vec![
        kv("TESTDB_ROOT", CONTAINER_TESTDB_ROOT),
        kv("TESTDB_REQUIRE_TMPFS", "1"),
    ]
}

fn static_phase(p: &str) -> &'static str {
    match p {
        "requirements" => "requirements",
        "testdb" => "testdb",
        _ => "install",
    }
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The image's own PATH (Containerfile: `PATH=/usr/local/cargo/bin:$PATH` over Debian's
/// default). A launched process's PATH is the staged release's `bin/` and `spira/`, then this
/// — set outright, never appended to (sp-isom7).
pub const IMAGE_PATH: &str =
    "/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Stage the tree under test as a release layout (DESIGN.md §5, sp-isom7): `$1` is the
/// release root (made afresh), `$2` the build's artifact directory, `$3` the tree; the rest are
/// the binaries `bin/` must hold. Every top-level entry of the tree is linked in beside `bin/`
/// (a release is `git archive` of its commit plus `bin/`); `target/` is not part of a release.
/// A binary the build did not produce is a refusal naming it, never a partial `bin/`.
pub const STAGE_SCRIPT: &str = r#"set -eu
r="$1"; a="$2"; w="$3"; shift 3
rm -rf "$r"
mkdir -p "$r/bin"
for f in "$w"/*; do
    n="${f##*/}"
    case "$n" in bin|target) continue ;; esac
    ln -s "$f" "$r/$n"
done
for b in "$@"; do
    [ -x "$a/$b" ] || { echo "testenv: stage: $b was not built into $a" >&2; exit 1; }
    ln -s "$a/$b" "$r/bin/$b"
done
"#;

pub const BASELINE_SCRIPT: &str = ". /workspace/spira/testdb.sh && testdb_up batch_baseline && printf \"TESTDB_NAME=%s\\nTESTDB_DIR=%s\\nTESTDB_BASELINE=%s\\nTESTDB_BD=%s\\nTESTDB_BIN=%s\\nTESTDB_MODE=%s\\n\" \"$TESTDB_NAME\" \"$TESTDB_DIR\" \"${TESTDB_BASELINE:-}\" \"$TESTDB_BD\" \"${TESTDB_BIN:-}\" \"${TESTDB_MODE:-}\"";

/// A harness fault in the container tier, with the exit status it maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// rc 2: the container did not come up, or probe failed.
    Up(String),
    /// rc 3: configure / suspend / install failed.
    Install(String),
    /// rc 2: a setup phase was still running at the trial's setup cutoff (DESIGN.md D9).
    /// The phase word is the VERDICT reason's suffix (`deadline-<phase>`).
    Deadline(&'static str),
    /// rc 2: no container slot came free inside the queue bound; the trial judged nothing.
    Queue,
    /// rc 2: the container image is not built; a bounded trial never builds it.
    ImageNotReady,
}

impl Fault {
    pub fn rc(&self) -> i32 {
        match self {
            Fault::Up(_) | Fault::Deadline(_) | Fault::Queue | Fault::ImageNotReady => 2,
            Fault::Install(_) => 3,
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Fault::Up(m) | Fault::Install(m) => m,
            Fault::Deadline(p) => p,
            Fault::Queue => "queue",
            Fault::ImageNotReady => "container image not built",
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

/// The test database each suite's `testdb_up` builds (DESIGN-testdb.md §2.4, sp-34ru2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fixtures {
    /// The server template built: every suite gets a private server fixture from it, so a
    /// `bd` call never opens the store (the embedded per-call open is what cost 61 % of wall).
    Server,
    /// Fallback when the template did not build: copies of the embedded baseline.
    Shared(TestDb),
    /// Neither: each suite initialises its own embedded store.
    PerSuite,
}

impl Fixtures {
    /// The suite environment that selects this tier in `testdb.sh`'s `testdb_up`. A server
    /// fixture is made by `testenv testdb up`, found by name on the suite's PATH — the staged
    /// release's `bin/` — however the suite repoints `SPIRA_REPO`/`SPIRA_HOME`.
    pub fn env(&self) -> Vec<(String, String)> {
        match self {
            Fixtures::Server => {
                let mut env = TestDb::env(None);
                env.push(("SPIRA_TESTDB_MODE".into(), "server".into()));
                env.extend(testdb_root_env());
                env
            }
            Fixtures::Shared(t) => TestDb::env(Some(t)),
            Fixtures::PerSuite => TestDb::env(None),
        }
    }

    /// The word the batch log uses.
    pub fn describe(&self) -> String {
        match self {
            Fixtures::Server => "server (a private sql-server per suite)".into(),
            Fixtures::Shared(t) => format!(
                "shared baseline (mode: {}, name: {})",
                if t.mode.is_empty() { "?" } else { &t.mode },
                t.name
            ),
            Fixtures::PerSuite => "per-suite databases".into(),
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
    /// The tree under test staged as a release inside the container (`/tmp/spira-release-
    /// <instance>`): every exec's PATH is built from it, and nothing else (sp-isom7).
    pub release: String,
    /// The binaries the staged release's `bin/` holds: the workspace's binary targets.
    pub bins: Vec<String>,
    pub liveness_retries: u32,
    pub liveness_sleep: Duration,
    /// The trial's setup cutoff (`--deadline`, DESIGN.md D9): every setup exec and
    /// `testenv container` call carries it. None = unbounded, as before.
    pub setup_deadline: Option<Instant>,
    /// Seconds `up` may wait for a container slot before it is a queue fault, not a boot one.
    pub queue_bound: Option<u64>,
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
            release: format!("/tmp/spira-release-{instance}"),
            bins: Vec::new(),
            liveness_retries: 3,
            liveness_sleep: Duration::from_secs(3),
            setup_deadline: None,
            queue_bound: None,
        }
    }

    /// `/tmp/spira-batch-<instance>`: the batch's SPIRA_RUN inside the container.
    pub fn batch_run(&self) -> String {
        format!("/tmp/spira-batch-{}", self.instance)
    }

    pub fn testdb_data(&self) -> String {
        format!("{}/testdb", self.batch_run())
    }

    /// The launcher contract every exec that runs harness code carries (DESIGN.md §5): the
    /// staged release, and a PATH set outright from it — the same rule production's launchers
    /// follow, against a different release.
    pub fn release_env(&self) -> Vec<(String, String)> {
        vec![
            kv("SPIRA_RELEASE", &self.release),
            kv("PATH", self.release_path()),
        ]
    }

    /// `<release>/bin:<release>/spira:<image PATH>`.
    pub fn release_path(&self) -> String {
        format!("{r}/bin:{r}/spira:{IMAGE_PATH}", r = self.release)
    }

    /// A program in the staged release, by the path an exec names it.
    fn in_release(&self, rel: &str) -> String {
        format!("{}/{rel}", self.release)
    }

    /// Stage the release layout ([`STAGE_SCRIPT`]); before configure, and whether or not the
    /// install runs, since every suite's PATH names it.
    pub fn stage_request(&self) -> ExecRequest {
        let mut argv: Vec<&str> = vec![
            "bash",
            "-c",
            STAGE_SCRIPT,
            "_",
            &self.release,
            &self.artifacts,
            WORKSPACE,
        ];
        argv.extend(self.bins.iter().map(String::as_str));
        self.setup_as_user(&argv, vec![kv("XDG_RUNTIME_DIR", USER_RUNTIME)])
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
            // The container has no sccache (sp-z61hj): a suite that drives a Spira build tool
            // in here builds uncached on purpose, never refused for the host's cache.
            kv(spira_config::build::CACHE_ENV, "off"),
        ]
    }

    fn as_user(&self, argv: &[&str], env: Vec<(String, String)>) -> ExecRequest {
        ExecRequest::new(&self.name, argv)
            .user(SPIRA_USER)
            .env(&env)
    }

    /// A setup exec: [`Session::as_user`] bounded by the setup cutoff (never a suite's).
    fn setup_as_user(&self, argv: &[&str], env: Vec<(String, String)>) -> ExecRequest {
        let mut r = self.as_user(argv, env);
        r.deadline = self.setup_deadline;
        r
    }

    /// `testenv container up --name <n> --checkout <worktree>`, then `probe`.
    pub fn up(&self, checkout: &Path) -> Result<(), Fault> {
        let mut args: Vec<String> = vec![
            "up".into(),
            "--name".into(),
            self.name.clone(),
            "--checkout".into(),
            checkout.display().to_string(),
        ];
        if let Some(b) = self.queue_bound {
            args.extend(["--queue-timeout".into(), b.to_string()]);
        }
        if self.setup_deadline.is_some() {
            args.push("--no-build".into());
        }
        let up = self.rt.testenv(&args, self.setup_deadline);
        if up.rc == RC_DEADLINE {
            return Err(Fault::Deadline("up"));
        }
        if up.rc == RC_QUEUE {
            return Err(Fault::Queue);
        }
        if up.rc == crate::container::RC_IMAGE_NOT_READY {
            return Err(Fault::ImageNotReady);
        }
        if !up.ok() {
            return Err(Fault::Up(format!(
                "container {} did not come up",
                self.name
            )));
        }
        self.probe()
    }

    /// `testenv container probe`: user systemd reachable in the container.
    pub fn probe(&self) -> Result<(), Fault> {
        let probe = self.rt.testenv(
            &["probe".into(), "--name".into(), self.name.clone()],
            self.setup_deadline,
        );
        if probe.rc == RC_DEADLINE {
            return Err(Fault::Deadline("up"));
        }
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
            kv("CONFIGURE_PROD", self.in_release("spira")),
            kv("CONFIGURE_MAX_AEONS", "1"),
            kv("CONFIGURE_MAX_LIVE_AEONS", "1"),
            kv("CONFIGURE_LOOM_ADDR", "127.0.0.1:7300"),
            kv("CONFIGURE_DOLT_DATA", ""),
        ]);
        env.extend(self.release_env());
        let configure = self.in_release("spira/configure.sh");
        self.setup_as_user(&["bash", &configure], env)
    }

    pub fn suspend_request(&self, unit: &str, reason: &str) -> ExecRequest {
        let mut env = vec![
            kv("XDG_RUNTIME_DIR", USER_RUNTIME),
            kv("SPIRA_RUN", self.batch_run()),
            // sp-bp249 follow-up: an explicit SPIRA_RUN now also needs a real config
            // registry in-process (the containment check), so ctrl can no longer infer its
            // home from its own exe's location alone — `bin/ctrl` here is a symlink into
            // `self.artifacts`, outside the staged release tree entirely, so `current_exe`'s
            // ancestor-walk finds no `spira/lib.sh` nearby and comes back empty. Named
            // explicitly, matching the staged layout ([`Self::in_release`]), so this setup
            // step never depends on that inference succeeding.
            kv("SPIRA_HOME", self.in_release("spira")),
        ];
        env.extend(self.release_env());
        // ctrl.sh is the `ctrl` binary now (sp-6onps): a native executable under bin/, not
        // a bash script under spira/. Launched through bash -c 'exec "$0" "$@"' rather than
        // a bare exec of the ELF directly — every other setup step (stage, configure,
        // install) runs this way, through bash, and this one seam is not the place to find
        // out whether a direct podman-exec of a freshly built binary needs something a
        // shell launch already provides for the others. Args travel as bash's own "$0"/"$@",
        // never interpolated into the script text.
        let ctrl = self.in_release("bin/ctrl");
        self.setup_as_user(
            &[
                "bash",
                "-c",
                "exec \"$0\" \"$@\"",
                &ctrl,
                "suspend",
                unit,
                "--reason",
                reason,
                "--owner",
                "sp-fud1",
            ],
            env,
        )
    }

    pub fn install_request(&self) -> ExecRequest {
        let mut env = self.user_env();
        env.extend([
            kv("SPIRA_PROD", self.in_release("spira")),
            kv("SPIRA_INSTALL_FORCE", "1"),
            kv("SPIRA_RUN", self.batch_run()),
            kv("SPIRA_TESTDB_DATA", self.testdb_data()),
            // sp-e5v53-4: a watcher daemon (pr-notify, inbox-keeper, publish-backlog — every
            // spira-watch-* unit) reaches outside this container — GitHub, mail, the forge —
            // which a fixture, by design, cannot; units-install reads this to warn about one
            // that never reaches active rather than fault a run no watcher is what is tested.
            kv("SPIRA_IN_TESTENV", "1"),
        ]);
        env.extend(self.release_env());
        // sp-31dm0: systemd/install.sh is retired; the per-instance unit installer is the
        // compiled units-install binary in the release's own bin/ now, run directly (no
        // `bash` wrapper).
        let install = self.in_release("bin/units-install");
        self.setup_as_user(&[&install, &self.instance], env)
    }

    /// The setup's steps, in order (sp-t26yx, DESIGN.md §11.4): stage; unless `install` is
    /// false, configure, suspend the Rust-backed units the container cannot run
    /// ([`SUSPENDED_UNITS`]) and install; one `command -v` per distinct requirement token
    /// (`testenv` is testenv's own and never checked); the testdb template. Each step is
    /// exactly the request that used to be its own `podman exec`.
    pub fn setup_steps<'t>(
        &self,
        install: bool,
        tokens: impl IntoIterator<Item = &'t str>,
    ) -> Vec<plan::Step> {
        let step = |name: String, phase: &str, req: ExecRequest, must: bool| plan::Step {
            name,
            phase: phase.into(),
            argv: req.argv,
            env: req.env,
            must,
        };
        let mut steps = vec![step("stage".into(), "install", self.stage_request(), true)];
        if install {
            steps.push(step("configure".into(), "install", self.configure_request(), true));
            for (unit, reason) in SUSPENDED_UNITS {
                steps.push(step(
                    format!("suspend:{unit}"),
                    "install",
                    self.suspend_request(unit, reason),
                    true,
                ));
            }
            steps.push(step("install".into(), "install", self.install_request(), true));
        }
        let mut seen = BTreeSet::new();
        for t in tokens {
            if t.is_empty() || t == "testenv" || !seen.insert(t.to_string()) {
                continue;
            }
            steps.push(step(
                format!("requires:{t}"),
                "requirements",
                self.requirement_request(t),
                false,
            ));
        }
        steps.push(step(
            "template".into(),
            "testdb",
            self.testdb_template_request(),
            false,
        ));
        steps
    }

    /// `command -v <token>` as the suite user, on the staged release's PATH.
    pub fn requirement_request(&self, token: &str) -> ExecRequest {
        let mut env = vec![kv("XDG_RUNTIME_DIR", USER_RUNTIME)];
        env.extend(self.release_env());
        self.setup_as_user(
            &["bash", "-c", "command -v \"$1\" >/dev/null 2>&1", "_", token],
            env,
        )
    }

    /// Run the whole setup ([`Session::setup_steps`]) in ONE `podman exec` of `runner` (the
    /// container's path to [`plan::RUNNER_REL`]), bounded by the setup cutoff, and read it
    /// back. A failing stage/configure/suspend/install step is the same harness fault its own
    /// exec was; a requirement that fails is unmet (a SKIP-REQ), never a fault; the template
    /// is reported, not judged. An exec killed at the cutoff is `Fault::Deadline` naming the
    /// phase of the step it was in — never "unmet", never a template failure.
    pub fn setup<'t>(
        &self,
        runner: &str,
        install: bool,
        tokens: impl IntoIterator<Item = &'t str>,
        log: &dyn Fn(&str),
    ) -> Result<Setup, SetupFault> {
        let plan = plan::Plan {
            nonce: plan::nonce(),
            steps: self.setup_steps(install, tokens),
        };
        let json = serde_json::to_string(&plan).unwrap_or_default();
        let mut req = ExecRequest::new(&self.name, &[runner, plan::PLAN_ARG, &json]).user(SPIRA_USER);
        req.deadline = self.setup_deadline;
        let t0 = Instant::now();
        let out = self.rt.exec(&req);
        let wall = t0.elapsed().as_secs_f64();
        let parsed = plan::parse(&plan, &out.output);
        let fault = |fault: Fault, reason: &'static str| SetupFault { fault, reason };
        for r in &parsed.results {
            let must = plan.steps.iter().any(|s| s.name == r.name && s.must);
            if !must || r.rc == 0 {
                continue;
            }
            if r.rc == RC_DEADLINE {
                break;
            }
            return Err(match r.name.as_str() {
                "stage" => {
                    log(&format!("stage rc={} output tail:\n{}", r.rc, tail(&r.output, 40)));
                    fault(
                        Fault::Install(format!(
                            "staging the release layout at {} failed — harness fault",
                            self.release
                        )),
                        "stage",
                    )
                }
                "configure" => {
                    log(&format!("configure rc={} output tail:\n{}", r.rc, tail(&r.output, 40)));
                    fault(Fault::Install("configure failed — harness fault".into()), "install")
                }
                "install" => {
                    log(&format!("install rc={} output tail:\n{}", r.rc, tail(&r.output, 40)));
                    fault(Fault::Install("install failed — harness fault".into()), "install")
                }
                other => {
                    // The other three branches dump their step's own output tail before
                    // faulting; this one (every "suspend:<unit>" step) did not, which is
                    // why sp-6onps's "ctrl suspend failed" fault carried no evidence at
                    // all — this was the one branch that left an operator guessing.
                    log(&format!("{other} rc={} output tail:\n{}", r.rc, tail(&r.output, 40)));
                    fault(
                        Fault::Install(format!(
                            "ctrl suspend failed for {} — harness fault",
                            other.strip_prefix("suspend:").unwrap_or(other)
                        )),
                        "install",
                    )
                }
            });
        }
        if out.rc == RC_DEADLINE {
            // The step it was in: the one that began and never ended, else the first with no
            // result (the exec was cut before the runner said anything).
            let name = parsed.unfinished.clone().or_else(|| {
                plan.steps
                    .iter()
                    .find(|s| !parsed.results.iter().any(|r| r.name == s.name))
                    .map(|s| s.name.clone())
            });
            let phase = name
                .and_then(|n| plan.steps.iter().find(|s| s.name == n))
                .map(|s| s.phase.as_str())
                .unwrap_or("install");
            return Err(fault(Fault::Deadline(static_phase(phase)), "deadline"));
        }
        if parsed.results.len() != plan.steps.len() {
            log(&format!(
                "setup runner rc={} returned {} of {} steps — output tail:\n{}",
                out.rc,
                parsed.results.len(),
                plan.steps.len(),
                out.tail(20)
            ));
            return Err(fault(
                Fault::Install(format!(
                    "the setup runner {runner} did not complete in {} — harness fault",
                    self.name
                )),
                "stage",
            ));
        }
        let mut phase_secs: Vec<(&'static str, f64)> = Vec::new();
        for r in &parsed.results {
            let p = static_phase(&r.phase);
            let secs = r.ms as f64 / 1000.0;
            match phase_secs.iter_mut().find(|(n, _)| *n == p) {
                Some((_, s)) => *s += secs,
                None => phase_secs.push((p, secs)),
            }
        }
        let inside: f64 = phase_secs.iter().map(|(_, s)| s).sum();
        log(&format!(
            "staged the tree under test as a release at {} ({} binaries); PATH={}",
            self.release,
            self.bins.len(),
            self.release_path()
        ));
        if install {
            log(&format!(
                "configured, suspended the Rust-backed units ({}) and installed instance {} inside {}",
                SUSPENDED_UNITS.map(|(u, _)| u).join(", "),
                self.instance,
                self.name
            ));
        }
        log(&format!(
            "setup in one exec: {wall:.1}s wall, {inside:.1}s in the container ({}) — podman exec overhead {:.1}s",
            parsed
                .results
                .iter()
                .map(|r| format!("{} {:.1}s", r.name, r.ms as f64 / 1000.0))
                .collect::<Vec<_>>()
                .join(", "),
            (wall - inside).max(0.0)
        ));
        let unmet = parsed
            .results
            .iter()
            .filter(|r| r.phase == "requirements" && r.rc != 0)
            .filter_map(|r| r.name.strip_prefix("requires:").map(str::to_string))
            .collect();
        let template = parsed
            .results
            .iter()
            .find(|r| r.name == "template")
            .map(|r| ExecOutcome {
                rc: r.rc,
                output: r.output.clone(),
            })
            .unwrap_or_default();
        let template_ms = parsed
            .results
            .iter()
            .find(|r| r.name == "template")
            .map(|r| r.ms)
            .unwrap_or(0);
        Ok(Setup {
            unmet,
            template,
            template_ms,
            phase_secs,
            wall_secs: wall,
        })
    }

    pub fn baseline_request(&self) -> ExecRequest {
        let mut env = vec![
            kv("XDG_RUNTIME_DIR", USER_RUNTIME),
            kv("SPIRA_TESTDB_DATA", self.testdb_data()),
        ];
        env.extend(self.release_env());
        self.setup_as_user(&["bash", "-c", BASELINE_SCRIPT], env)
    }

    /// `testenv testdb template` with the artifact under test: the server-mode template is
    /// built once here, so no server-mode suite waits on it (DESIGN-testdb.md §2.1).
    pub fn testdb_template_request(&self) -> ExecRequest {
        let mut env = vec![kv("XDG_RUNTIME_DIR", USER_RUNTIME)];
        env.extend(self.release_env());
        env.extend(testdb_root_env());
        let exe = self.in_release("bin/testenv");
        self.setup_as_user(
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
        fixtures: &Fixtures,
    ) -> ExecRequest {
        let mut env = Vec::new();
        if mode == Mode::Parallel {
            env.push(kv("HOME", self.suite_home(n)));
        }
        env.extend(self.user_env());
        env.push(kv("SPIRA_IN_TESTENV", "1"));
        env.extend(self.release_env());
        env.extend(fixtures.env());
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
        let meter = self.in_release("bin/bd-meter");
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
    ///
    /// The detail names the container (sp-2zu0t: a harness-fault line that only says
    /// `ExitCode=… OOMKilled=…` cannot be correlated to any container after the fact — the
    /// container is already removed by the time anyone reads the gate's output) and, when
    /// every attempt came back as a failed/empty lookup rather than a definitive `false`,
    /// says so explicitly instead of asserting `ExitCode`/`OOMKilled` values that were never
    /// actually read: under podman control-plane contention (DESIGN.md §11.4) `podman
    /// inspect` itself can time out or error, which is evidence the *lookup* is unreliable,
    /// not evidence the container died.
    pub fn liveness(&self) -> Liveness {
        let mut n = 0;
        let mut confirmed_dead = false;
        loop {
            match self.rt.inspect(&self.name, "{{.State.Running}}").as_deref() {
                Some("true") => return Liveness::Alive,
                Some("false") => {
                    confirmed_dead = true;
                    break;
                }
                _ => {
                    n += 1;
                    if n >= self.liveness_retries {
                        break;
                    }
                    std::thread::sleep(self.liveness_sleep);
                }
            }
        }
        let name = &self.name;
        if !confirmed_dead {
            // Every inspect attempt failed or was empty: the container's actual state was
            // never read. ExitCode/OOMKilled would be fabricated confidence, so the detail
            // says exactly what happened instead of printing stale-looking fault fields.
            return Liveness::Dead {
                detail: format!(
                    "container={name} state unknown — {n} consecutive `podman inspect` \
                     lookups failed or returned no answer (never saw Running=false)"
                ),
            };
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
            detail: format!("container={name} ExitCode={exit} OOMKilled={oom}"),
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
        self.rt.testenv(
            &[
                "down".into(),
                "--name".into(),
                self.name.clone(),
                "--volumes".into(),
            ],
            None,
        )
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
        /// `testenv container` subcommand → how long it takes (for the setup-cutoff tests).
        pub testenv_delay: Mutex<std::collections::HashMap<String, Duration>>,
        /// Plan execs seen ([`crate::plan`]): what really crosses podman, once per setup.
        pub plans: Mutex<usize>,
        /// Answer plan execs with this instead of running their steps (a broken runner).
        pub plan_answer: Mutex<Option<ExecOutcome>>,
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

    impl FakeRuntime {
        /// One exec, recorded and answered by the first matching rule.
        pub fn exec_one(&self, req: &ExecRequest) -> ExecOutcome {
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

        /// A plan exec (`<runner> plan <json>`, [`crate::plan`]) is answered the way the real
        /// runner answers it: each step becomes the exec it used to be (recorded and answered
        /// by the rules), framed; a step answered with `RC_DEADLINE` is the one the cutoff
        /// killed (begun, never ended), and a failing `must` step ends the plan.
        fn exec_plan(&self, req: &ExecRequest) -> Option<ExecOutcome> {
            if req.argv.get(1).map(String::as_str) != Some(crate::plan::PLAN_ARG)
                || !req.argv[0].ends_with(crate::plan::RUNNER_REL)
            {
                return None;
            }
            let plan: crate::plan::Plan = serde_json::from_str(req.argv.get(2)?).ok()?;
            *self.plans.lock().unwrap() += 1;
            if let Some(a) = self.plan_answer.lock().unwrap().clone() {
                return Some(a);
            }
            let mut out = String::new();
            for step in &plan.steps {
                out.push_str(&crate::plan::frame_begin(&plan.nonce, &step.name));
                let mut sr = ExecRequest::new(&req.container, &[]);
                sr.argv = step.argv.clone();
                sr.env = step.env.clone();
                sr.user = req.user.clone();
                sr.deadline = req.deadline;
                let o = self.exec_one(&sr);
                if o.rc == RC_DEADLINE {
                    return Some(ExecOutcome { rc: RC_DEADLINE, output: out });
                }
                out.push_str(&crate::plan::frame_end(&plan.nonce, &step.name, o.rc, 0, &o.output));
                if step.must && o.rc != 0 {
                    return Some(ExecOutcome { rc: o.rc, output: out });
                }
            }
            Some(ExecOutcome { rc: 0, output: out })
        }
    }

    impl ContainerRuntime for FakeRuntime {
        fn exec(&self, req: &ExecRequest) -> ExecOutcome {
            if let Some(o) = self.exec_plan(req) {
                return o;
            }
            self.exec_one(req)
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
        fn testenv(&self, args: &[String], deadline: Option<Instant>) -> ExecOutcome {
            self.testenv_calls.lock().unwrap().push(args.to_vec());
            let sub0 = args.first().cloned().unwrap_or_default();
            if let Some(d) = self.testenv_delay.lock().unwrap().get(&sub0).copied() {
                // a slow call: past the deadline it is "killed" like the real one
                let end = Instant::now() + d;
                while Instant::now() < end {
                    if deadline.is_some_and(|dl| Instant::now() >= dl) {
                        return ExecOutcome {
                            rc: RC_DEADLINE,
                            output: String::new(),
                        };
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
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

    #[test]
    fn suspension_reasons_do_not_blame_the_image_rustc() {
        for (unit, reason) in SUSPENDED_UNITS {
            assert!(!reason.contains("rustc") && !reason.contains("lockfile"), "{unit}: {reason}");
        }
    }

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
                "/tmp/spira-release-abc123/bin/bd-meter".to_string(),
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
        assert_eq!(r.argv[0], "/tmp/spira-release-abc123/bin/testenv");
        assert_eq!(r.argv[1..], ["testdb", "template", "--bd", "bd", "--dolt", "dolt"]);
        assert_eq!(r.user.as_deref(), Some(SPIRA_USER));
        assert_eq!(r.env_value("SPIRA_RELEASE"), Some("/tmp/spira-release-abc123"));
    }

    const RUNNER: &str = "/workspace/target/.testenv-runner/testenv";

    fn run_setup(s: &Session, install: bool, tokens: &[&str]) -> Result<Setup, SetupFault> {
        s.setup(RUNNER, install, tokens.iter().copied(), &|_| {})
    }

    #[test]
    fn install_runs_configure_suspends_then_install_from_the_staged_release() {
        let rt = FakeRuntime::new();
        let s = session(&rt);
        run_setup(&s, true, &[]).unwrap();
        // stage first, then configure, three suspends, install, then the template
        let mut argv = rt.exec_argv();
        assert_eq!(argv.remove(0)[..3], ["bash", "-c", STAGE_SCRIPT]);
        assert_eq!(argv.len(), 6);
        assert_eq!(argv[5][1..3], ["testdb", "template"]);
        rt.execs.lock().unwrap().remove(0);
        assert_eq!(argv[0], vec!["bash", "/tmp/spira-release-abc123/spira/configure.sh"]);
        assert_eq!(
            argv[1][..6],
            ["bash", "-c", "exec \"$0\" \"$@\"", "/tmp/spira-release-abc123/bin/ctrl", "suspend", "spira-loom"]
        );
        assert_eq!(argv[2][5], "spira-cockpit");
        assert_eq!(argv[3][5], "spira-watch-queue-watch");
        assert_eq!(
            argv[4],
            vec!["/tmp/spira-release-abc123/bin/units-install", "abc123"]
        );
        let execs = rt.execs.lock().unwrap();
        for r in execs.iter().take(5) {
            assert_eq!(r.user.as_deref(), Some("spirauser"));
            assert_eq!(r.env_value("SPIRA_RELEASE"), Some("/tmp/spira-release-abc123"));
            assert_eq!(
                r.env_value("PATH"),
                Some("/tmp/spira-release-abc123/bin:/tmp/spira-release-abc123/spira:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
                "PATH is set outright from the staged release"
            );
            assert_eq!(r.env_value("SPIRA_ARTIFACTS"), None);
            assert_eq!(r.container, "spira-batch-abc123");
        }
        assert_eq!(
            execs[0].env_value("CONFIGURE_PROD"),
            Some("/tmp/spira-release-abc123/spira")
        );
        assert_eq!(
            execs[4].env_value("SPIRA_PROD"),
            Some("/tmp/spira-release-abc123/spira"),
            "units render against the staged release's root, which has bin/"
        );
        assert_eq!(execs[0].env_value("CONFIGURE_DOLT_DATA"), Some(""));
        assert_eq!(execs[4].env_value("SPIRA_INSTALL_FORCE"), Some("1"));
        assert_eq!(
            execs[4].env_value("SPIRA_RUN"),
            Some("/tmp/spira-batch-abc123")
        );
        assert_eq!(
            execs[4].env_value("SPIRA_TESTDB_DATA"),
            Some("/tmp/spira-batch-abc123/testdb")
        );
    }

    #[test]
    fn staging_links_the_tree_and_the_workspace_binaries_as_a_release() {
        let rt = FakeRuntime::new();
        let mut s = session(&rt);
        s.bins = vec!["spira-config".into(), "testenv".into()];
        run_setup(&s, false, &[]).unwrap();
        let argv = rt.exec_argv();
        assert_eq!(argv.len(), 2, "stage, then the template (no install)");
        assert_eq!(argv[0][..3], ["bash", "-c", STAGE_SCRIPT]);
        assert_eq!(
            argv[0][3..],
            ["_", "/tmp/spira-release-abc123", "/workspace/target/aeon", "/workspace", "spira-config", "testenv"]
        );
        let rt = FakeRuntime::new();
        rt.on(|r| r.argv.get(2).map(String::as_str) == Some(STAGE_SCRIPT), |_| ExecOutcome {
            rc: 1,
            output: "testenv: stage: testenv was not built into /workspace/target/aeon".into(),
        });
        let e = run_setup(&session(&rt), true, &[]).unwrap_err();
        assert_eq!(e.fault.rc(), 3, "a release that cannot be staged is a harness fault");
        assert_eq!(e.reason, "stage");
        assert_eq!(rt.exec_argv().len(), 1, "nothing runs after a failed stage");
    }

    #[test]
    fn the_stage_script_builds_a_release_layout_from_a_tree_and_a_build() {
        // The script itself, run on the host against a scratch tree.
        let d = testkit::TempDir::new("testenv-stage");
        let (w, a, r) = (d.join("w"), d.join("a"), d.join("rel"));
        for p in [w.join("spira"), w.join("systemd"), w.join("target/aeon"), a.clone()] {
            std::fs::create_dir_all(&p).unwrap();
        }
        std::fs::write(w.join("spira/conf.sh"), "").unwrap();
        for b in ["testenv", "spira-lint"] {
            testkit::write_exe(&a.join(b), "#!/bin/sh\n");
        }
        let run = |bins: &[&str]| {
            std::process::Command::new("bash")
                .args(["-c", STAGE_SCRIPT, "_"])
                .arg(&r)
                .arg(&a)
                .arg(&w)
                .args(bins)
                .output()
                .unwrap()
        };
        assert!(run(&["testenv", "spira-lint"]).status.success());
        assert!(r.join("spira/conf.sh").is_file());
        assert!(r.join("systemd").is_dir());
        assert!(!r.join("target").exists(), "target/ is not part of a release");
        assert!(r.join("bin/testenv").is_file());
        assert!(r.join("bin/spira-lint").is_file());
        let o = run(&["testenv", "work"]);
        assert!(!o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("work was not built"));
    }

    #[test]
    fn a_failed_step_is_an_install_fault_and_stops() {
        let rt = FakeRuntime::new();
        rt.on(
            |r| r.argv.get(4).map(String::as_str) == Some("suspend"),
            |_| ExecOutcome {
                rc: 1,
                output: String::new(),
            },
        );
        let e = run_setup(&session(&rt), true, &["jq"]).unwrap_err();
        assert_eq!(e.fault.rc(), 3);
        assert_eq!(e.reason, "install");
        assert!(e.fault.message().contains("spira-loom"));
        assert_eq!(rt.exec_argv().len(), 3, "stage, configure, the failed suspend — then nothing");
    }

    #[test]
    fn the_whole_setup_is_one_exec_as_the_suite_user_under_the_cutoff() {
        let rt = FakeRuntime::new();
        let mut s = session(&rt);
        let cut = Instant::now() + Duration::from_secs(60);
        s.setup_deadline = Some(cut);
        let got = run_setup(&s, true, &["jq", "dolt"]).unwrap();
        assert_eq!(*rt.plans.lock().unwrap(), 1, "one podman exec for every setup step");
        assert_eq!(rt.exec_argv().len(), 9, "stage, configure, 3 suspends, install, 2 checks, template");
        for r in rt.execs.lock().unwrap().iter() {
            assert_eq!(r.user.as_deref(), Some(SPIRA_USER));
            assert_eq!(r.deadline, Some(cut));
        }
        assert!(got.unmet.is_empty());
        assert_eq!(
            got.phase_secs.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            vec!["install", "requirements", "testdb"]
        );
    }

    #[test]
    fn a_setup_cut_during_install_is_a_deadline_fault_named_install() {
        let rt = FakeRuntime::new();
        rt.on(
            |r| r.argv.first().is_some_and(|a| a.ends_with("/bin/units-install")),
            |_| ExecOutcome { rc: RC_DEADLINE, output: String::new() },
        );
        let e = run_setup(&session(&rt), true, &["jq"]).unwrap_err();
        assert_eq!(e.fault, Fault::Deadline("install"));
    }

    #[test]
    fn a_template_cut_at_the_cutoff_is_a_deadline_fault_not_a_fallback() {
        let rt = FakeRuntime::new();
        rt.on(
            |r| r.argv.get(1).map(String::as_str) == Some("testdb"),
            |_| ExecOutcome { rc: RC_DEADLINE, output: String::new() },
        );
        let e = run_setup(&session(&rt), true, &[]).unwrap_err();
        assert_eq!(e.fault, Fault::Deadline("testdb"));
    }

    #[test]
    fn a_failed_template_is_reported_not_a_fault() {
        let rt = FakeRuntime::new();
        rt.on(
            |r| r.argv.get(1).map(String::as_str) == Some("testdb"),
            |_| ExecOutcome { rc: 1, output: "bd init (server) failed".into() },
        );
        let got = run_setup(&session(&rt), true, &[]).unwrap();
        assert_eq!(got.template.rc, 1);
        assert!(got.template.output.contains("bd init"));
    }

    #[test]
    fn a_runner_that_says_nothing_is_a_harness_fault_never_a_pass() {
        let rt = FakeRuntime::new();
        *rt.plan_answer.lock().unwrap() = Some(ExecOutcome {
            rc: 127,
            output: "crun: executable file not found".into(),
        });
        let e = run_setup(&session(&rt), true, &[]).unwrap_err();
        assert_eq!((e.fault.rc(), e.reason), (3, "stage"));
        // and one cut before it said anything is a deadline in its first phase
        *rt.plan_answer.lock().unwrap() = Some(ExecOutcome { rc: RC_DEADLINE, output: String::new() });
        let e = run_setup(&session(&rt), true, &[]).unwrap_err();
        assert_eq!(e.fault, Fault::Deadline("install"));
    }

    #[test]
    fn the_template_and_server_fixtures_live_on_the_container_tmpfs() {
        let rt = FakeRuntime::new();
        let s = session(&rt);
        let r = s.testdb_template_request();
        assert_eq!(r.env_value("TESTDB_ROOT"), Some(CONTAINER_TESTDB_ROOT));
        assert_eq!(r.env_value("TESTDB_REQUIRE_TMPFS"), Some("1"));
        assert!(CONTAINER_TESTDB_ROOT.starts_with("/tmp/"));
        let env = Fixtures::Server.env();
        assert!(env.contains(&("TESTDB_ROOT".into(), CONTAINER_TESTDB_ROOT.into())));
        assert!(env.contains(&("TESTDB_REQUIRE_TMPFS".into(), "1".into())));
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
        let got = run_setup(&session(&rt), false, &["jq", "dolt", "jq", "testenv"]).unwrap();
        assert_eq!(got.unmet.into_iter().collect::<Vec<_>>(), vec!["dolt"]);
        let checks = rt
            .exec_argv()
            .into_iter()
            .filter(|a| a.get(2).is_some_and(|c| c.starts_with("command -v")))
            .count();
        assert_eq!(checks, 2, "once per distinct token; testenv is never checked");
    }

    #[test]
    fn setup_execs_carry_the_cutoff_and_suite_execs_never_do() {
        let rt = FakeRuntime::new();
        let mut s = session(&rt);
        let cut = Instant::now() + Duration::from_secs(60);
        s.setup_deadline = Some(cut);
        for r in [
            s.stage_request(),
            s.configure_request(),
            s.suspend_request("u", "why"),
            s.install_request(),
            s.baseline_request(),
            s.testdb_template_request(),
        ] {
            assert_eq!(r.deadline, Some(cut), "{:?}", r.argv);
        }
        let fx = Fixtures::PerSuite;
        let suite = s.suite_request(Mode::Parallel, 1, "test-a.sh", &fx);
        assert_eq!(suite.deadline, None, "a suite is bounded by the suite phase, not setup");
    }

    #[test]
    fn a_requirement_check_killed_at_the_cutoff_is_a_deadline_fault_not_unmet() {
        let rt = FakeRuntime::new();
        rt.on(
            |r| r.argv.last().map(String::as_str) == Some("jq"),
            |_| ExecOutcome {
                rc: RC_DEADLINE,
                output: String::new(),
            },
        );
        assert_eq!(
            run_setup(&session(&rt), true, &["jq"]).map(|_| ()),
            Err(SetupFault {
                fault: Fault::Deadline("requirements"),
                reason: "deadline"
            })
        );
    }

    #[test]
    fn a_container_up_past_the_cutoff_is_a_deadline_fault_named_up() {
        let rt = FakeRuntime::new();
        rt.testenv_delay
            .lock()
            .unwrap()
            .insert("up".into(), Duration::from_secs(5));
        let mut s = session(&rt);
        s.setup_deadline = Some(Instant::now() + Duration::from_millis(50));
        assert_eq!(s.up(Path::new("/wt")), Err(Fault::Deadline("up")));
        assert_eq!(Fault::Deadline("up").rc(), 2);
    }

    #[test]
    fn a_bounded_up_never_builds_and_an_unbuilt_image_is_its_own_fault() {
        let rt = FakeRuntime::new();
        let mut s = session(&rt);
        s.setup_deadline = Some(Instant::now() + Duration::from_secs(60));
        let _ = s.up(Path::new("/wt"));
        let calls = rt.testenv_calls.lock().unwrap().clone();
        assert!(calls[0].contains(&"--no-build".to_string()), "{calls:?}");
        assert_eq!(Fault::ImageNotReady.rc(), 2);
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
    fn a_server_tier_sends_every_suite_to_a_private_server() {
        let rt = FakeRuntime::new();
        let s = session(&rt);
        let r = s.suite_request(Mode::Parallel, 2, "test-a.sh", &Fixtures::Server);
        assert_eq!(r.env_value("SPIRA_TESTDB_MODE"), Some("server"));
        assert_eq!(
            r.env_value("TESTDB_SHARED"),
            Some("0"),
            "no suite takes the embedded shared-baseline branch"
        );
        assert_eq!(r.env_value("TESTDB_BASELINE"), None);
        assert_eq!(
            r.env_value("TESTDB_TESTENV"),
            None,
            "testenv is found by name on the suite's PATH, however it repoints SPIRA_REPO"
        );
        // The fallback tiers never set the mode: testdb_up keeps its embedded default.
        let t = TestDb::parse("TESTDB_NAME=b\nTESTDB_BASELINE=/b\nTESTDB_MODE=embedded\n").unwrap();
        for f in [Fixtures::Shared(t), Fixtures::PerSuite] {
            let r = s.suite_request(Mode::Parallel, 2, "test-a.sh", &f);
            assert_eq!(r.env_value("SPIRA_TESTDB_MODE"), None, "{f:?}");
            assert_eq!(r.env_value("TESTDB_TESTENV"), None, "{f:?}");
        }
        let r = s.suite_request(
            Mode::Parallel,
            2,
            "test-a.sh",
            &Fixtures::Shared(TestDb::parse("TESTDB_NAME=b\nTESTDB_BASELINE=/b\n").unwrap()),
        );
        assert_eq!(r.env_value("TESTDB_SHARED"), Some("1"));
    }

    #[test]
    fn parallel_suites_get_private_home_instance_and_run() {
        let rt = FakeRuntime::new();
        let s = session(&rt);
        let r = s.suite_request(Mode::Parallel, 3, "test-a.sh", &Fixtures::PerSuite);
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
        assert_eq!(r.env_value("SPIRA_TEST_PLAN_BIN"), None);
        assert_eq!(r.env_value("SPIRA_ARTIFACTS"), None);
        assert_eq!(r.env_value("SPIRA_ARTIFACTS_ROOT"), None);
        assert_eq!(r.env_value("SPIRA_RELEASE"), Some("/tmp/spira-release-abc123"));
        assert!(r
            .env_value("PATH")
            .unwrap()
            .starts_with("/tmp/spira-release-abc123/bin:/tmp/spira-release-abc123/spira:"));
        let r = s.suite_request(Mode::Serial, 1, "test-a.sh", &Fixtures::PerSuite);
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
                detail: "container=spira-batch-abc123 ExitCode=inspect-failed OOMKilled=inspect-failed"
                    .into()
            }
        );
        let rt = FakeRuntime::new();
        rt.running_answers(&[None, None, None]);
        assert!(matches!(session(&rt).liveness(), Liveness::Dead { .. }));
    }

    /// sp-2zu0t: a `harness-fault` line that only says `ExitCode=… OOMKilled=…` cannot be
    /// correlated to any container after the fact (the container is already removed by the
    /// time anyone reads the gate's output), so the detail must name the container. And a
    /// run of failed/empty inspects — the observed shape of podman control-plane contention,
    /// DESIGN.md §11.4 — must say the lookup never got a definitive answer, never assert an
    /// `ExitCode`/`OOMKilled` that was never actually read.
    #[test]
    fn liveness_detail_names_the_container_and_distinguishes_unreachable_from_confirmed_dead() {
        let rt = FakeRuntime::new();
        rt.running_answers(&[Some("false")]);
        let Liveness::Dead { detail } = session(&rt).liveness() else {
            panic!("expected Dead");
        };
        assert!(
            detail.contains("container=spira-batch-abc123"),
            "confirmed-dead detail must name the container: {detail}"
        );
        assert!(
            detail.contains("ExitCode=") && detail.contains("OOMKilled="),
            "confirmed-dead detail must carry the real exit fields: {detail}"
        );

        let rt = FakeRuntime::new();
        rt.running_answers(&[None, None, None]);
        let Liveness::Dead { detail } = session(&rt).liveness() else {
            panic!("expected Dead");
        };
        assert!(
            detail.contains("container=spira-batch-abc123"),
            "unreachable detail must name the container too: {detail}"
        );
        assert!(
            !detail.contains("ExitCode="),
            "an inspect that never answered must not assert an ExitCode it never read: {detail}"
        );
        assert!(
            detail.contains("never saw Running=false"),
            "the unreachable case must say the state was never confirmed: {detail}"
        );
    }

    #[test]
    fn account_fault_shape() {
        assert!(is_user_account_fault(
            "Error: unable to find user spirauser: no matching entries in passwd file"
        ));
        assert!(!is_user_account_fault("not ok 1 - passwd file unable"));
    }
}
