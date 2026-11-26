//! Unit tests: MANIFEST, build, verify, rendering, the atomic `current` swap, rollback when a
//! unit fails to come up, the hotfix supersede rule, and prune. Nothing here touches the
//! host's systemd or runs cargo: `FakeGit`, `FakeCargo` and `FakeSystemctl` stand in.

use crate::activate;
use crate::artifacts;
use crate::config::{Config, Env, Flags, Registered};
use crate::config_delta;
use crate::fsutil;
use crate::git::Git;
use crate::install::{self, InstallOpts, Unpack};
use crate::spira_env;
use runtime::units::{ScriptedUnits, UnitHandle, UnitProps, UnitSpec, Units};
use crate::manifest::{Entry, Manifest};
use crate::systemctl::{Systemctl, UnitState};
use crate::units;
use crate::verify::{self};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use testkit::TempDir;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// The activation of a box with no broker, which is every activation here that is not about
/// what it publishes.
fn no_broker() -> Result<spira_pub::Publisher, spira_pub::PubError> {
    Err(spira_pub::PubError::Config("this test has no broker".into()))
}

/// A scratch root whose read-only releases are made writable before `TempDir` removes it.
struct Sandbox(TempDir);
impl Drop for Sandbox {
    fn drop(&mut self) {
        fsutil::make_writable(&self.0);
    }
}
impl Sandbox {
    fn new() -> Sandbox {
        Sandbox(TempDir::new("release-test"))
    }
    fn p(&self) -> &Path {
        &self.0
    }
}

fn exe(path: &Path, body: &str) {
    // testkit::write_exe, never fs::write + set_permissions (sp-os3of).
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    testkit::write_exe(path, body);
}

fn file(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

const SVC: &str = "[Service]\nType=simple\nExecStart=@SPIRA_TOOL_BIN@ --serve\nStandardOutput=append:@SPIRA_RUN@/tool.log\n";
const JOB: &str = "[Service]\nType=oneshot\nExecStart=@SPIRA_PROD@/job.sh\n";
const JOB_TIMER: &str = "[Timer]\nOnCalendar=hourly\nUnit=spira-job.service\n";
const SHARED: &str = "[Service]\nType=oneshot\nExecStart=@SPIRA_PROD_ROOT@/shared.sh start\n";
const WATCH: &str = "[Service]\nType=simple\nExecStart=@SPIRA_PROD@/watchd.sh exec %i\n";

/// A git that "archives" a small workspace: one binary crate `tool`, unit templates, and the
/// scripts they run. `extra` adds files; `ancestors` answers is-ancestor.
#[derive(Default)]
struct FakeGit {
    extra: Vec<(String, String, bool)>,
    /// Add a `spira-config` binary crate to the workspace, for a release that validates config.
    validator: bool,
    /// Add a `unit-ensure` binary crate to the workspace, for a release that installs its own units.
    unit_ensure: bool,
    /// A config delta that only the release built from this sha carries.
    delta_for: Option<(String, String)>,
    /// Files only the release built from the given sha carries: `(sha, path, body)`.
    extra_only: Vec<(String, String, String)>,
    ancestors: RefCell<BTreeSet<(String, String)>>,
}

impl FakeGit {
}

impl Git for FakeGit {
    fn resolve(&self, _repo: &Path, rev: &str) -> Result<String, String> {
        if crate::is_sha(rev) {
            Ok(rev.into())
        } else {
            Err(format!("{rev} is not a commit"))
        }
    }
    fn tree(&self, _repo: &Path, sha: &str) -> Result<String, String> {
        Ok(format!("tree-of-{sha}"))
    }
    fn archive(&self, _repo: &Path, sha: &str, into: &Path) -> Result<(), String> {
        let members = match (self.validator, self.unit_ensure) {
            (true, _) => "[\"tool\", \"spira-config\"]",
            (false, true) => "[\"tool\", \"unit-ensure\"]",
            (false, false) => "[\"tool\"]",
        };
        if self.unit_ensure {
            file(&into.join("unit-ensure/Cargo.toml"), "[package]\nname = \"unit-ensure\"\nversion = \"0.0.0\"\n");
            file(&into.join("unit-ensure/src/main.rs"), "fn main() {}\n");
        }
        file(&into.join("Cargo.toml"), &format!("[workspace]\nmembers = {members}\n"));
        if self.validator {
            file(&into.join("spira-config/Cargo.toml"), "[package]\nname = \"spira-config\"\nversion = \"0.0.0\"\n");
            file(&into.join("spira-config/src/main.rs"), "fn main() {}\n");
        }
        file(&into.join("tool/Cargo.toml"), "[package]\nname = \"tool\"\nversion = \"0.0.0\"\n");
        file(&into.join("tool/src/main.rs"), "fn main() {}\n");
        file(&into.join("systemd/spira-tool.service"), SVC);
        file(&into.join("systemd/spira-job.service"), JOB);
        file(&into.join("systemd/spira-job.timer"), JOB_TIMER);
        file(&into.join("systemd/shared.service"), SHARED);
        file(&into.join("systemd/spira-watch@.service"), WATCH);
        exe(&into.join("spira/job.sh"), &format!("#!/bin/sh\n# {sha}\n"));
        exe(&into.join("spira/watchd.sh"), "#!/bin/sh\n");
        exe(&into.join("shared.sh"), "#!/bin/sh\n");
        std::os::unix::fs::symlink("CLAUDE.md", into.join("AGENTS.md")).unwrap();
        file(&into.join("CLAUDE.md"), "hi\n");
        if let Some((only, body)) = &self.delta_for {
            if only == sha {
                file(&into.join("spira/config-delta.toml"), body);
            }
        }
        for (only, p, body) in &self.extra_only {
            if only == sha {
                file(&into.join(p), body);
            }
        }
        for (p, body, x) in &self.extra {
            if *x {
                exe(&into.join(p), body)
            } else {
                file(&into.join(p), body)
            }
        }
        Ok(())
    }
    fn is_ancestor(&self, _repo: &Path, ancestor: &str, of: &str) -> Result<bool, String> {
        Ok(ancestor == of || self.ancestors.borrow().contains(&(ancestor.into(), of.into())))
    }
}
struct World {
    _sb: Sandbox,
    cfg: Config,
    _git: FakeGit,
}

impl World {
    fn with_git(git: FakeGit) -> World {
        World::with_git_and_reg(git, Registered::default())
    }
    /// [`World::with_git`] with extra registered config (sp-xtdqi-2) — for a case that needs
    /// a host key `with_git` never sets (e.g. `SPIRA_SCCACHE_DAV_ADDR`) without touching
    /// every other test's fixed config. `reg.instance` is always overridden to `"prod"`
    /// regardless of what the caller passes — every other test's unit names depend on it.
    fn with_git_and_reg(git: FakeGit, reg: Registered) -> World {
        let sb = Sandbox::new();
        let mut env = Env::new();
        env.insert("HOME".into(), sb.p().join("home").display().to_string());
        env.insert("SPIRA_UNIT_DIR".into(), sb.p().join("units").display().to_string());
        env.insert("SPIRA_TOML".into(), sb.p().join("cfg.toml").display().to_string());
        let reg = Registered { instance: "prod".into(), ..reg };
        let flags = Flags { releases: Some(sb.p().join("rel")), run: Some(sb.p().join("run")), keep: Some(2) };
        let cfg = Config::resolve_with(&flags, &env, None, &reg).unwrap();
        fs::create_dir_all(sb.p().join("units")).unwrap();
        World { _sb: sb, cfg, _git: git }
    }
}

/// systemd, as far as activation can tell: each unit's state, and what restarting it does.
/// A restarted unit whose installed ExecStart names `poison` fails to come up.
struct FakeSystemctl {
    unit_dir: PathBuf,
    states: RefCell<BTreeMap<String, UnitState>>,
    restarts: RefCell<Vec<String>>,
    /// `start_no_block` calls, in order; it fails for a unit named in `start_fails`.
    starts: RefCell<Vec<String>>,
    order: RefCell<Vec<String>>,
    start_fails: BTreeSet<String>,
    kills: RefCell<Vec<String>>,
    resets: RefCell<Vec<String>>,
    reloads: RefCell<usize>,
    /// The first daemon-reload fails; the undo's own succeeds.
    reload_fails_once: Cell<bool>,
    poison: Option<String>,
    /// Unit names `list_active` must never return, mirroring a `systemd-run` transient
    /// unit (no file of its own).
    transient: BTreeSet<String>,
    /// When set, `restart` fails for this unit name instead of updating its state.
    restart_fails: BTreeSet<String>,
    /// `disable_now` calls, in order — sp-xtdqi-2's retire path.
    disabled: RefCell<Vec<String>>,
    /// When set, `disable_now` fails for this unit name.
    disable_fails: BTreeSet<String>,
    /// Run at every daemon-reload — after `current` flips, before dropped keys are removed.
    probe: Option<Box<dyn Fn() -> String>>,
    probes: RefCell<Vec<String>>,
    /// Run at every restart, before it takes effect.
    on_restart: Option<Box<dyn Fn() -> String>>,
    at_restart: RefCell<Vec<String>>,
    /// Units that go inactive once `state` has been asked about them this many more times.
    exits_after: RefCell<BTreeMap<String, usize>>,
    cache_server_stops: RefCell<usize>,
    /// Where a unit's process executes from; a restart moves it to `restart_lands` unless pinned.
    exes: RefCell<BTreeMap<String, PathBuf>>,
    restart_lands: Option<PathBuf>,
    pinned: BTreeSet<String>,
    /// Restarts of a unit that leave it `activating`, as a slow boot does, before one that comes up.
    slow_starts: RefCell<BTreeMap<String, usize>>,
    start_budgets: BTreeMap<String, std::time::Duration>,
}

impl FakeSystemctl {
    fn new(unit_dir: PathBuf) -> FakeSystemctl {
        let mut s = BTreeMap::new();
        let st = |a: &str, k: &str| UnitState { active: a.into(), result: "success".into(), kind: k.into() };
        s.insert("spira-tool-prod.service".into(), st("active", "simple"));
        s.insert("spira-watch-pool-prod.service".into(), st("active", "simple"));
        s.insert("spira-job-prod.service".into(), st("active", "oneshot"));
        s.insert("shared.service".into(), st("inactive", "oneshot"));
        FakeSystemctl {
            unit_dir,
            states: RefCell::new(s),
            restarts: RefCell::new(vec![]),
            starts: RefCell::new(vec![]),
            order: RefCell::new(vec![]),
            start_fails: BTreeSet::new(),
            kills: RefCell::new(vec![]),
            resets: RefCell::new(vec![]),
            reloads: RefCell::new(0),
            reload_fails_once: Cell::new(false),
            poison: None,
            transient: BTreeSet::new(),
            restart_fails: BTreeSet::new(),
            disabled: RefCell::new(vec![]),
            disable_fails: BTreeSet::new(),
            probe: None,
            probes: RefCell::new(vec![]),
            on_restart: None,
            at_restart: RefCell::new(vec![]),
            exits_after: RefCell::new(BTreeMap::new()),
            cache_server_stops: RefCell::new(0),
            exes: RefCell::new(BTreeMap::new()),
            restart_lands: None,
            pinned: BTreeSet::new(),
            slow_starts: RefCell::new(BTreeMap::new()),
            start_budgets: BTreeMap::new(),
        }
    }
}

impl Systemctl for FakeSystemctl {
    fn daemon_reload(&self) -> Result<(), String> {
        *self.reloads.borrow_mut() += 1;
        if self.reload_fails_once.replace(false) {
            return Err("simulated daemon-reload failure".into());
        }
        if let Some(p) = &self.probe {
            self.probes.borrow_mut().push(p());
        }
        Ok(())
    }
    fn state(&self, unit: &str) -> Result<UnitState, String> {
        if let Some(n) = self.exits_after.borrow_mut().get_mut(unit) {
            if *n == 0 {
                self.states.borrow_mut().entry(unit.into()).or_default().active = "inactive".into();
            } else {
                *n -= 1;
            }
        }
        Ok(self.states.borrow().get(unit).cloned().unwrap_or_else(|| UnitState { active: "inactive".into(), result: "success".into(), kind: "simple".into() }))
    }
    fn start_budget(&self, unit: &str) -> Option<std::time::Duration> {
        self.start_budgets.get(unit).copied()
    }
    fn executables(&self, unit: &str) -> Vec<PathBuf> {
        self.exes.borrow().get(unit).cloned().into_iter().collect()
    }
    fn cat(&self, _unit: &str) -> Result<String, String> {
        Err("cat: not modelled by this fake — activate/rollback never call it".into())
    }
    fn disable_now(&self, unit: &str) -> Result<(), String> {
        self.disabled.borrow_mut().push(unit.into());
        if self.disable_fails.contains(unit) {
            return Err(format!("{unit}: simulated disable failure"));
        }
        let mut s = self.states.borrow_mut();
        let e = s.entry(unit.into()).or_default();
        e.active = "inactive".into();
        e.result = "success".into();
        Ok(())
    }
    fn reset_failed(&self, unit: &str) -> Result<(), String> {
        self.resets.borrow_mut().push(unit.into());
        Ok(())
    }
    fn stop_compiler_cache_server(&self) {
        *self.cache_server_stops.borrow_mut() += 1;
    }
    fn kill(&self, unit: &str) -> Result<(), String> {
        self.kills.borrow_mut().push(unit.into());
        Ok(())
    }
    fn start_no_block(&self, unit: &str) -> Result<(), String> {
        self.starts.borrow_mut().push(unit.into());
        self.order.borrow_mut().push(format!("start {unit}"));
        if self.start_fails.contains(unit) {
            return Err(format!("{unit}: simulated start failure"));
        }
        Ok(())
    }
    fn restart(&self, unit: &str) -> Result<(), String> {
        if unit == "dolt-beads.service" {
            return Err(format!("{unit}: RefuseManualStop=yes denies a restart job"));
        }
        self.restarts.borrow_mut().push(unit.into());
        self.order.borrow_mut().push(format!("restart {unit}"));
        if let (Some(to), false) = (&self.restart_lands, self.pinned.contains(unit)) {
            self.exes.borrow_mut().insert(unit.into(), to.clone());
        }
        if let Some(p) = &self.on_restart {
            self.at_restart.borrow_mut().push(p());
        }
        if self.restart_fails.contains(unit) {
            return Err(format!("{unit}: simulated restart failure"));
        }
        let text = fs::read_to_string(self.unit_dir.join(unit)).unwrap_or_default();
        for l in text.lines() {
            if let Some(dep) = l.strip_prefix("Requires=") {
                if !self.unit_dir.join(dep).exists() {
                    return Err(format!("Unit {dep} not found"));
                }
            }
        }
        let bad = self.poison.as_ref().map(|p| units::exec_lines(&text).iter().any(|l| l.contains(p.as_str()))).unwrap_or(false);
        let slow = self.slow_starts.borrow_mut().get_mut(unit).is_some_and(|n| std::mem::replace(n, n.saturating_sub(1)) > 0);
        let mut s = self.states.borrow_mut();
        let e = s.entry(unit.into()).or_default();
        if slow {
            e.active = "activating".into();
            e.result = "success".into();
        } else if bad {
            e.active = "failed".into();
            e.result = "exit-code".into();
        } else {
            e.active = "active".into();
            e.result = "success".into();
        }
        Ok(())
    }
    fn list_failed(&self, glob: &str) -> Result<Vec<String>, String> {
        let (pre, suf) = glob.split_once('*').unwrap_or((glob, ""));
        Ok(self.states.borrow().iter().filter(|(n, st)| st.active == "failed" && n.starts_with(pre) && n.ends_with(suf)).map(|(n, _)| n.clone()).collect())
    }
    fn list_active(&self, glob: &str) -> Result<Vec<String>, String> {
        // The only glob shapes install-tarball ever passes: "<prefix>*<suffix>".
        let (pre, suf) = glob.split_once('*').unwrap_or((glob, ""));
        Ok(self
            .states
            .borrow()
            .iter()
            .filter(|(name, st)| st.active == "active" && name.starts_with(pre) && name.ends_with(suf) && !self.transient.contains(name.as_str()))
            .map(|(name, _)| name.clone())
            .collect())
    }
}

// ---------------------------------------------------------------- MANIFEST

#[test_id(tzuugjt)]
#[test]
fn manifest_round_trips_and_keeps_the_self_test_line_shape() {
    let mut m = Manifest { commit: A.into(), built: Some("2026-09-29T00:00:00Z".into()), repo: Some("spira".into()), entries: BTreeMap::new() };
    m.entries.insert("bin/tool".into(), Entry::File("0".repeat(64)));
    m.entries.insert("a dir/with space.txt".into(), Entry::File("1".repeat(64)));
    m.entries.insert("AGENTS.md".into(), Entry::Link("CLAUDE.md".into()));
    let text = m.render();
    assert!(text.starts_with(&format!("commit {A}\n")));
    // spira/self-test.sh reads `bin/<name> <sha256>`; conf.sh reads `repo <name>`.
    assert!(text.contains(&format!("bin/tool {}\n", "0".repeat(64))));
    assert!(text.contains("repo spira\n"));
    assert_eq!(Manifest::parse(&text).unwrap(), m);
}

#[test_id(tqci9by)]
#[test]
fn manifest_without_a_commit_or_with_garbage_does_not_parse() {
    assert!(Manifest::parse("bin/x 00\n").is_err());
    assert!(Manifest::parse(&format!("commit {A}\nbin/x nothex\n")).is_err());
    assert!(Manifest::parse("commit short\n").is_err());
}

// ---------------------------------------------------------------- build
// ---------------------------------------------------------------- verify

/// sp-vrn3v: `pre_activate_env` names the release **under verification**, plus the box's own
/// tool tail appended after the system directories, with the spira-env bin ahead of them —
/// the same shape `host_values` renders into `Environment=PATH=` (sp-31gtu,
/// sp-c7b85), reused here for a process this crate spawns directly instead of a unit.
#[test_id(tu69lzx)]
#[test]
fn pre_activate_env_builds_the_release_own_path_and_release_var() {
    let mut env = Env::new();
    env.insert("SPIRA_UNIT_DIR".into(), "units".into());
    env.insert("HOME".into(), "h".into());
    let reg = Registered { releases: "e".into(), releases_keep: "10".into(), path: "h/.local/bin:h/.cargo/bin".into(), ..Default::default() };
    let cfg = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    let rel = PathBuf::from("e").join(A);
    let envs = verify::pre_activate_env(&cfg, &rel).unwrap();
    assert_eq!(envs[0], (spira_config::RELEASE_ENV.to_string(), rel.display().to_string()));
    assert_eq!(envs[1], ("SPIRA_HOME".to_string(), format!("{}/spira", rel.display())));
    assert_eq!(envs[2], ("PATH".to_string(), format!("{}/bin:{}/spira:h/.local/lib/spira-env/bin:/usr/local/bin:/usr/bin:/bin:h/.local/bin:h/.cargo/bin", rel.display(), rel.display())));

    // No tail configured: PATH ends at the system directories.
    let reg = Registered { releases: "e".into(), releases_keep: "10".into(), ..Default::default() };
    let cfg = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    let envs = verify::pre_activate_env(&cfg, &rel).unwrap();
    assert_eq!(envs[2].1, format!("{}/bin:{}/spira:h/.local/lib/spira-env/bin:/usr/local/bin:/usr/bin:/bin", rel.display(), rel.display()));
}

/// The same refusal `host_values` gives a tail inside a release or a checkout (sp-c7b85)
/// applies here too: `pre_activate_env` must not hand pre-activate a `PATH` that could
/// resolve back into a release or a checked-out tree instead of the one under verification.
#[test_id(tvxy6c6)]
#[test]
fn pre_activate_env_refuses_a_tail_inside_a_release_or_checkout() {
    let mut env = Env::new();
    env.insert("SPIRA_UNIT_DIR".into(), "units".into());
    let reg = Registered { releases: "e".into(), releases_keep: "10".into(), path: "x/spira-releases/def/bin".into(), ..Default::default() };
    let cfg = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    let rel = PathBuf::from("e").join(A);
    let e = verify::pre_activate_env(&cfg, &rel).unwrap_err();
    assert!(e.contains("spira-releases"), "{e}");
}

/// The acceptance in the bead's own words: "verifying release B from a shell whose PATH
/// points at release A uses B's binaries." Two releases are built side by side, each with
/// its own `bin/tool`; the *caller's* real `PATH` is pointed at A's `bin/` before verifying
/// B, and the probe script (same as above) still finds B's own `tool` and names B as
/// `SPIRA_RELEASE` — proving the child's env is built from the release passed to `verify`,
/// not inherited. Its `PATH` edit goes through `testkit::env`.

// ---------------------------------------------------------------- render

#[test_id(tv3ox10)]
#[test]
fn installed_units_map_back_to_their_templates() {
    let t: BTreeSet<String> = ["spira-tool.service", "spira-job.timer", "shared.service", "spira-watch@.service", "spira-notify.service"].iter().map(|s| s.to_string()).collect();
    let m = |n: &str| units::template_for(n, &t, "prod").map(|m| (m.template, m.watcher));
    assert_eq!(m("spira-tool-prod.service"), Some(("spira-tool.service".into(), None)));
    assert_eq!(m("spira-job-prod.timer"), Some(("spira-job.timer".into(), None)));
    assert_eq!(m("shared.service"), Some(("shared.service".into(), None)));
    assert_eq!(m("spira-watch-pool-prod.service"), Some(("spira-watch@.service".into(), Some("pool".into()))));
    assert_eq!(m("spira-notify-prod.service"), Some(("spira-notify.service".into(), None)));
    assert_eq!(m("spira-tool-test.service"), None, "another instance's unit is not ours");
    assert_eq!(m("local-overrides.service"), None);
    assert_eq!(m("spira-suites-prod.service"), None);
}

#[test_id(tbqx5gk)]
#[test]
fn render_names_the_release_and_refuses_what_it_cannot_fill() {
    let rel = Path::new("r/spira-releases").join(A);
    let mut host = BTreeMap::new();
    host.insert("SPIRA_RUN".to_string(), "run/x".to_string());
    host.insert("SPIRA_DB".to_string(), String::new());
    let out = units::render("spira-tool.service", SVC, &rel, &host, None, "prod").unwrap();
    assert!(out.contains(&format!("ExecStart=r/spira-releases/{A}/bin/tool --serve")), "{out}");
    assert!(out.contains("append:run/x/tool.log"));
    assert!(out.ends_with("--serve\nStandardOutput=append:run/x/tool.log\n"));
    let t = units::render("spira-job.timer", JOB_TIMER, &rel, &host, None, "prod").unwrap();
    assert!(t.contains("Unit=spira-job-prod.service"), "{t}");
    let wt = units::render("spira-watch@.service", WATCH, &rel, &host, Some("pool"), "prod").unwrap();
    assert!(wt.contains("watchd.sh exec pool"));
    let sup = units::render("x.service", "ExecStart=@SPIRA_SUPERVISE_BIN@ @SPIRA_HOME@/c.sh\n", &rel, &host, None, "prod").unwrap();
    // path-ok: a unit rendered against a release names that release's own binary.
    assert!(sup.contains(&format!("{A}/bin/spira-supervise {}/{A}/spira/c.sh", "r/spira-releases")), "{sup}");
    let e = units::render("y.service", "ExecStart=@NOPE@\n", &rel, &host, None, "prod").unwrap_err();
    assert!(e.contains("nothing fills: NOPE"), "{e}");
    let e = units::render("z.service", "Environment=SPIRA_DB=@SPIRA_DB@\n", &rel, &host, None, "prod").unwrap_err();
    assert!(e.contains("uses SPIRA_DB but no value"), "{e}");
}

#[test_id(t04kdj8)]
#[test]
fn gate_open_is_true_without_a_gate_and_checks_the_host_key_when_there_is_one() {
    let mut host = BTreeMap::new();
    assert!(units::gate_open("spira-tool.service", &host), "no gate at all");
    assert!(!units::gate_open("sccache-dav.service", &host), "gated, key absent");
    host.insert("SPIRA_SCCACHE_DAV_ADDR".to_string(), "  ".to_string());
    assert!(!units::gate_open("sccache-dav.service", &host), "gated, key blank");
    host.insert("SPIRA_SCCACHE_DAV_ADDR".to_string(), "192.168.1.56:9431".to_string());
    assert!(units::gate_open("sccache-dav.service", &host));
    assert_eq!(units::gate_key("sccache-dav.service"), Some(units::SCCACHE_DAV_ADDR_KEY));
    assert_eq!(units::gate_key("spira-tool.service"), None);
}

// ---------------------------------------------------------------- activate

// ---------------------------------------------------------------- rollback

/// History `[A, B]`, current `B`, the installed unit B's text: rolling back to `A` rewrites and
/// restarts `spira-tool-prod.service`.
fn world_rolling_back_to_a() -> World {
    let w = world_activating_a_changed_unit();
    fs::create_dir_all(w.cfg.releases.join(B)).unwrap();
    std::os::unix::fs::symlink(B, w.cfg.releases.join("current")).unwrap();
    let state = w.cfg.state_dir().unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("history"), format!("{A} landed\n{B} landed\n")).unwrap();
    w
}

fn rollback_with(w: &World, sc: &FakeSystemctl) -> Result<String, String> {
    let reads = FakeReads::serving();
    let nix = FakeNix::default();
    let ctx = activate::Ctx { cfg: &w.cfg, reads: &reads, sc, nix: &nix, git: &w._git, repo: None, landed_ref: "main".into(), unit_ensure: &activate::ensure_new_units, settle: Duration::from_millis(10), drain: Duration::from_millis(10), steady: Duration::from_secs(60), warm_wait: Duration::from_secs(60), warm_pause: Duration::from_millis(1), report: &|_| {}, gc: None, publisher: &no_broker };
    activate::rollback(&ctx)
}

fn current_of(w: &World) -> String {
    fs::read_link(w.cfg.releases.join("current")).unwrap().to_string_lossy().to_string()
}

#[test_id(tnnh3mo)]
#[test]
fn a_rollback_whose_unit_is_slow_to_start_stays_on_the_target_and_names_what_is_down() {
    let w = world_rolling_back_to_a();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.slow_starts.borrow_mut().insert("spira-tool-prod.service".into(), 2);
    let e = rollback_with(&w, &sc).unwrap_err();
    assert_eq!(current_of(&w), A, "{e}");
    assert!(e.starts_with(&format!("rolled back to {A}; 1 unit(s) not up")), "{e}");
    assert!(e.contains("spira-tool-prod.service") && e.contains("activating") && e.contains("systemctl --user start spira-tool-prod.service"), "{e}");
    assert!(!e.contains("UNDO INCOMPLETE"), "{e}");
    assert_eq!(activate::read_history(&w.cfg.state_dir().unwrap()).unwrap().iter().map(|h| h.sha.as_str()).collect::<Vec<_>>(), vec![A]);
    assert_eq!(sc.restarts.borrow().len(), 2, "restarted once more under the target, and no more");
}

#[test_id(tmrwgrx)]
#[test]
fn a_rollback_whose_unit_cannot_be_restarted_at_all_still_stays_on_the_target_and_names_it() {
    let w = world_rolling_back_to_a();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    let e = rollback_with(&w, &sc).unwrap_err();
    assert_eq!(current_of(&w), A, "{e}");
    assert!(e.contains("spira-tool-prod.service (restart failed") && e.contains("systemctl --user start spira-tool-prod.service"), "{e}");
    assert!(!e.contains("UNDO INCOMPLETE"), "{e}");
}

#[test_id(th1m1lr)]
#[test]
fn a_rollback_whose_slow_unit_comes_up_on_the_second_restart_succeeds() {
    let w = world_rolling_back_to_a();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.slow_starts.borrow_mut().insert("spira-tool-prod.service".into(), 1);
    assert_eq!(rollback_with(&w, &sc), Ok(A.to_string()));
    assert_eq!(current_of(&w), A);
}

// ---------------------------------------------------------------- broker config

const BROKER: &str = "spira-broker-prod.service";
const BROKER_UNIT: &str = "[Service]\nType=notify\nExecStart=@SPIRA_ENV_PATH@/mosquitto -c @SPIRA_RUN@/mosquitto.conf\n";
const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

/// Releases `A`, `B` and `C` ship one broker unit; `B`'s `mosquitto.conf` differs from `A`'s and
/// `C`'s matches `B`'s. `current` is `A`, with its unit installed and its config in the run dir.
fn world_with_a_broker_on_a(socket: &str) -> World {
    let w = World::with_git_and_reg(FakeGit::default(), Registered { broker_socket: socket.into(), ..Default::default() });
    for (sha, note) in [(A, "first"), (B, "second"), (C, "second")] {
        let rel = w.cfg.releases.join(sha);
        file(&rel.join("systemd/spira-broker.service"), BROKER_UNIT);
        file(&rel.join("systemd/mosquitto.conf"), &format!("# {note}\nlistener 0 @SPIRA_BROKER_SOCKET@\n"));
        let m = Manifest { commit: sha.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
        fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    }
    let run = w.cfg.run.clone().unwrap();
    let home = w.cfg.home().unwrap().display().to_string();
    let unit = BROKER_UNIT.replace("@SPIRA_ENV_PATH@", &spira_config::spira_env_bin(&home)).replace("@SPIRA_RUN@", &run.display().to_string());
    file(&w.cfg.unit_dir.join(BROKER), &unit);
    file(&run.join("mosquitto.conf"), &broker_conf_of(&w, "first"));
    std::os::unix::fs::symlink(A, w.cfg.releases.join("current")).unwrap();
    let state = w.cfg.state_dir().unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("history"), format!("{A} landed\n")).unwrap();
    w
}

fn broker_conf_of(w: &World, note: &str) -> String {
    format!("# {note}\nlistener 0 {}\n", w.cfg.run.as_ref().unwrap().join("sock/b.sock").display())
}

fn placed_broker_conf(w: &World) -> String {
    fs::read_to_string(w.cfg.run.as_ref().unwrap().join("mosquitto.conf")).unwrap()
}

fn broker_running(w: &World) -> FakeSystemctl {
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.states.borrow_mut().insert(BROKER.into(), UnitState { active: "active".into(), result: "success".into(), kind: "notify".into() });
    sc
}

fn activate_sha(w: &World, sc: &FakeSystemctl, sha: &str) -> Result<activate::Switched, String> {
    let reads = FakeReads::serving();
    let nix = FakeNix::default();
    let ctx = activate::Ctx { cfg: &w.cfg, reads: &reads, sc, nix: &nix, git: &w._git, repo: None, landed_ref: "main".into(), unit_ensure: &activate::ensure_new_units, settle: Duration::from_millis(10), drain: Duration::from_millis(10), steady: Duration::from_secs(60), warm_wait: Duration::from_secs(60), warm_pause: Duration::from_millis(1), report: &|_| {}, gc: None, publisher: &no_broker };
    activate::activate(&ctx, sha, None)
}

fn restarts_of_the_broker(sc: &FakeSystemctl) -> usize {
    sc.restarts.borrow().iter().filter(|u| *u == BROKER).count()
}

#[test_id(tp5ewbe)]
#[test]
fn activate_and_rollback_each_place_their_releases_broker_config_and_restart_the_broker() {
    let w = world_with_a_broker_on_a("sock/b.sock");
    let sc = broker_running(&w);
    let seen = {
        let conf = w.cfg.run.clone().unwrap().join("mosquitto.conf");
        move || fs::read_to_string(&conf).unwrap()
    };
    let sc = FakeSystemctl { on_restart: Some(Box::new(seen)), ..sc };

    let out = activate_sha(&w, &sc, B).unwrap();
    assert_eq!(out.restarted, [BROKER], "no restart");
    assert_eq!(placed_broker_conf(&w), broker_conf_of(&w, "second"));
    assert!(w.cfg.run.as_ref().unwrap().join("sock").is_dir(), "the socket's directory is made");

    rollback_with(&w, &sc).unwrap();
    assert_eq!(placed_broker_conf(&w), broker_conf_of(&w, "first"));
    assert_eq!(restarts_of_the_broker(&sc), 2);
    assert_eq!(*sc.at_restart.borrow(), [broker_conf_of(&w, "second"), broker_conf_of(&w, "first")], "each restart finds its release's config in place");
}

#[test_id(tixqeuj)]
#[test]
fn the_broker_is_restarted_though_the_new_releases_unit_ensure_writes_the_same_config() {
    let w = world_with_a_broker_on_a("sock/b.sock");
    let sc = broker_running(&w);
    // What the release's unit-ensure does to this file: the same renderer, the same values.
    let unit_ensure = |cfg: &Config, sha: &str| {
        let socket = cfg.host_values()?["SPIRA_BROKER_SOCKET"].clone();
        crate::broker::place_conf(&cfg.releases.join(sha).join("systemd"), &socket, cfg.run.as_ref().unwrap()).map(|_| ())
    };
    let reads = FakeReads::serving();
    let nix = FakeNix::default();
    let ctx = activate::Ctx { cfg: &w.cfg, reads: &reads, sc: &sc, nix: &nix, git: &w._git, repo: None, landed_ref: "main".into(), unit_ensure: &unit_ensure, settle: Duration::from_millis(10), drain: Duration::from_millis(10), steady: Duration::from_secs(60), warm_wait: Duration::from_secs(60), warm_pause: Duration::from_millis(1), report: &|_| {}, gc: None, publisher: &no_broker };
    assert_eq!(activate::activate(&ctx, B, None).unwrap().restarted, [BROKER]);
}

#[test_id(ty8gcer)]
#[test]
fn a_release_whose_broker_config_renders_the_same_leaves_the_broker_running() {
    let w = world_with_a_broker_on_a("sock/b.sock");
    let sc = broker_running(&w);
    activate_sha(&w, &sc, B).unwrap();
    let out = activate_sha(&w, &sc, C).unwrap();
    assert_eq!(out.left_running, [BROKER]);
    assert!(out.restarted.is_empty(), "{:?}", out.restarted);
    assert_eq!(restarts_of_the_broker(&sc), 1);
}

#[test_id(tko63ff)]
#[test]
fn an_undone_activation_restores_the_broker_config_before_restarting_the_broker_on_it() {
    let w = world_with_a_broker_on_a("sock/b.sock");
    let conf = w.cfg.run.clone().unwrap().join("mosquitto.conf");
    let mut sc = broker_running(&w);
    sc.restart_fails.insert(BROKER.into());
    sc.on_restart = Some(Box::new(move || fs::read_to_string(&conf).unwrap()));
    let e = activate_sha(&w, &sc, B).unwrap_err();
    assert_eq!(current_of(&w), A, "{e}");
    assert_eq!(placed_broker_conf(&w), broker_conf_of(&w, "first"), "{e}");
    assert_eq!(*sc.at_restart.borrow(), [broker_conf_of(&w, "second"), broker_conf_of(&w, "first")], "{e}");
}

#[test_id(tl3vtys)]
#[test]
fn a_broker_config_that_cannot_be_placed_undoes_the_activation_naming_the_key() {
    let w = world_with_a_broker_on_a("");
    let sc = broker_running(&w);
    let e = activate_sha(&w, &sc, B).unwrap_err();
    assert!(e.contains("SPIRA_BROKER_SOCKET") && e.contains("rolled back"), "{e}");
    assert_eq!(current_of(&w), A, "{e}");
    assert!(sc.restarts.borrow().is_empty(), "{e}");
}

#[test_id(tlgbn0x)]
#[test]
fn a_registry_that_refuses_the_socket_key_resolves_and_the_refused_placing_says_why() {
    let w = world_with_a_broker_on_a("");
    let reg = Registered { instance: "prod".into(), broker_socket_refusal: "fixture registry refusal".into(), ..Default::default() };
    let mut env = Env::new();
    env.insert("HOME".into(), w.cfg.home().unwrap().display().to_string());
    env.insert("SPIRA_UNIT_DIR".into(), w.cfg.unit_dir.display().to_string());
    env.insert("SPIRA_TOML".into(), w.cfg.toml_spec().unwrap());
    let flags = Flags { releases: Some(w.cfg.releases.clone()), run: w.cfg.run.clone(), keep: Some(2) };
    let w = World { cfg: Config::resolve_with(&flags, &env, None, &reg).unwrap(), ..w };
    let e = activate_sha(&w, &broker_running(&w), B).unwrap_err();
    assert!(e.contains("SPIRA_BROKER_SOCKET is empty") && e.contains("fixture registry refusal"), "{e}");
}

#[test_id(t99geib)]
#[test]
fn a_box_without_the_broker_unit_gets_no_broker_config_from_activation() {
    let w = world_with_a_broker_on_a("");
    fs::remove_file(w.cfg.unit_dir.join(BROKER)).unwrap();
    activate_sha(&w, &broker_running(&w), B).unwrap();
    assert!(placed_broker_conf(&w).starts_with("# first\n"));
}

// ---------------------------------------------------------------- release/active

/// [`activate_sha`] publishing to the broker at `socket`.
#[cfg(feature = "integration")]
fn activate_publishing(w: &World, sc: &FakeSystemctl, sha: &str, socket: &Path) -> Result<activate::Switched, String> {
    let reads = FakeReads::serving();
    let nix = FakeNix::default();
    let publisher = || {
        let clock = std::sync::Arc::new(runtime::ports::VirtualClock::new(0));
        spira_pub::Publisher::with("release", spira_pub::Options::new(socket, clock, Box::new(runtime::ports::SeededRng::new(1))))
    };
    let ctx = activate::Ctx { cfg: &w.cfg, reads: &reads, sc, nix: &nix, git: &w._git, repo: None, landed_ref: "main".into(), unit_ensure: &activate::ensure_new_units, settle: Duration::from_millis(10), drain: Duration::from_millis(10), steady: Duration::from_secs(60), warm_wait: Duration::from_secs(60), warm_pause: Duration::from_millis(1), report: &|_| {}, gc: None, publisher: &publisher };
    activate::activate(&ctx, sha, None)
}

#[cfg(feature = "integration")]
#[test_id(tv9496a)]
#[test]
fn a_completed_activation_publishes_release_active_with_its_sha() {
    let broker = testkit::broker::Broker::up();
    let w = world_with_a_broker_on_a("sock/b.sock");
    let out = activate_publishing(&w, &broker_running(&w), B, broker.socket()).unwrap();
    let held = broker.retained("release/active");
    let said: serde_json::Value = serde_json::from_str(&held[0].1).unwrap();
    assert_eq!((said["sha"].as_str(), &said["restarted"]), (Some(B), &serde_json::json!(out.restarted)), "{said}");
    assert!(said["at"].as_u64().is_some_and(|at| at > 0), "{said}");
}

#[cfg(feature = "integration")]
#[test_id(tfgt1xj)]
#[test]
fn a_failed_activation_publishes_nothing() {
    let broker = testkit::broker::Broker::up();
    let w = world_with_a_broker_on_a("");
    activate_publishing(&w, &broker_running(&w), B, broker.socket()).unwrap_err();
    assert!(broker.retained("release/active").is_empty());
}

#[cfg(feature = "integration")]
#[test_id(tngsm9n)]
#[test]
fn with_the_broker_stopped_an_activation_succeeds_as_before() {
    let mut broker = testkit::broker::Broker::up();
    broker.stop();
    let w = world_with_a_broker_on_a("sock/b.sock");
    let out = activate_publishing(&w, &broker_running(&w), B, broker.socket()).unwrap();
    assert_eq!((out.restarted, current_of(&w)), (vec![BROKER.to_string()], B.to_string()));
}

// ---------------------------------------------------------------- hotfix

const STANDING_HOTFIX: &str = "sha cccccccccccccccccccccccccccccccccccccccc\nreason r\nat t\n";

fn world_activating_over_a_landed_hotfix() -> World {
    let w = world_activating_a_changed_unit();
    let h = "c".repeat(40);
    w._git.ancestors.borrow_mut().extend([(h.clone(), A.to_string()), (h, "main".to_string())]);
    let state = w.cfg.state_dir().unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("hotfix"), STANDING_HOTFIX).unwrap();
    w
}

fn activate_as_the_batcher(w: &World, sc: &FakeSystemctl) -> Result<activate::Switched, String> {
    let reads = FakeReads::serving();
    let nix = FakeNix::default();
    let ctx = activate::Ctx { cfg: &w.cfg, reads: &reads, sc, nix: &nix, git: &w._git, repo: Some(w.cfg.releases.clone()), landed_ref: "main".into(), unit_ensure: &activate::ensure_new_units, settle: Duration::from_millis(10), drain: Duration::from_millis(10), steady: Duration::from_secs(60), warm_wait: Duration::from_secs(60), warm_pause: Duration::from_millis(5), report: &|_| {}, gc: None, publisher: &no_broker };
    activate::activate(&ctx, A, None)
}

#[test_id(t2gtgqd)]
#[test]
fn a_superseded_hotfix_is_cleared_before_the_first_restart() {
    let w = world_activating_over_a_landed_hotfix();
    let path = w.cfg.state_dir().unwrap().join("hotfix");
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.on_restart = Some(Box::new(move || path.exists().to_string()));
    activate_as_the_batcher(&w, &sc).unwrap();
    assert_eq!(*sc.at_restart.borrow(), vec!["false".to_string()]);
    assert!(!w.cfg.state_dir().unwrap().join("hotfix").exists());
}

#[test_id(t6hl11m)]
#[test]
fn a_failed_restart_puts_the_superseded_hotfix_record_back() {
    let w = world_activating_over_a_landed_hotfix();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    activate_as_the_batcher(&w, &sc).unwrap_err();
    assert_eq!(fs::read_to_string(w.cfg.state_dir().unwrap().join("hotfix")).unwrap(), STANDING_HOTFIX);
}

// ---------------------------------------------------------------- prune

// ---------------------------------------------------------------- config

#[test_id(t8dh09w)]
#[test]
fn config_resolves_roots_from_flags_then_config_and_refuses_when_nothing_does() {
    let mut env = Env::new();
    env.insert("HOME".into(), "h".into());
    assert!(Config::resolve_with(&Flags::default(), &env, None, &Registered::default()).unwrap_err().contains("no releases directory"));
    // `reg.releases` is already fully resolved here (as `cfg("SPIRA_RELEASES")` would hand
    // it — the `workspaces`-derived default is `spira_config`'s own job, not this crate's).
    let reg = Registered { releases: "w/spira-releases".into(), run: "r".into(), releases_keep: "7".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert_eq!((c.releases.clone(), c.run.clone(), c.keep), (PathBuf::from("w/spira-releases"), Some(PathBuf::from("r")), 7));
    assert_eq!(c.unit_dir, PathBuf::from("h/.config/systemd/user"));
    let reg2 = Registered { releases: "e".into(), ..reg.clone() };
    assert_eq!(Config::resolve_with(&Flags::default(), &env, None, &reg2).unwrap().releases, PathBuf::from("e"));
    let f = Flags { releases: Some("f".into()), ..Default::default() };
    assert_eq!(Config::resolve_with(&f, &env, None, &reg2).unwrap().releases, PathBuf::from("f"));
}

#[test_id(tpesolv)]
#[test]
fn host_values_carries_the_configured_path_tail_and_refuses_a_bad_one() {
    let mut env = Env::new();
    env.insert("HOME".into(), "h".into());
    // Nothing configured: the tail is empty, not an error.
    let reg = Registered { releases: "e".into(), releases_keep: "10".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert_eq!(c.host_values().unwrap().get("SPIRA_PATH_TAIL").map(String::as_str), Some(""));
    // The configured key.
    let reg = Registered { releases: "e".into(), releases_keep: "10".into(), path: "h/.local/bin:h/.cargo/bin".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert_eq!(c.host_values().unwrap().get("SPIRA_PATH_TAIL").map(String::as_str), Some(":h/.local/bin:h/.cargo/bin"));
    // A tail entry inside a release is refused, naming it.
    let bad = Registered { releases: "e".into(), releases_keep: "10".into(), path: "x/spira-releases/def/bin".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &bad).unwrap();
    let e = c.host_values().unwrap_err();
    assert!(e.contains("spira-releases"), "{e}");
}

#[test_id(tbtirv8)]
#[test]
fn host_values_carries_the_cache_store_root_the_sccache_dav_unit_renders() {
    // The unit template reads @SPIRA_SCCACHE_DAV_ROOT@; activation refused every release
    // ("placeholders nothing fills") while this map lacked it.
    let mut env = Env::new();
    env.insert("HOME".into(), "h".into());
    let reg = Registered { releases: "e".into(), run: "h/run".into(), releases_keep: "10".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert_eq!(c.host_values().unwrap().get("SPIRA_SCCACHE_DAV_ROOT").map(String::as_str), Some("h/run/sccache-dav"), "empty means the run directory's store");
    let reg = Registered { releases: "e".into(), run: "h/run".into(), releases_keep: "10".into(), sccache_dav_root: "mnt/other/store".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert_eq!(c.host_values().unwrap().get("SPIRA_SCCACHE_DAV_ROOT").map(String::as_str), Some("mnt/other/store"));
    let unit = std::fs::read_to_string(spira_config::resolve::checkout_root_for_process().unwrap().join("systemd/sccache-dav.service")).unwrap();
    let rendered = units::render("sccache-dav.service", &unit, std::path::Path::new("e/0123"), &c.host_values().unwrap(), None, "");
    assert!(!rendered.as_ref().err().is_some_and(|e| e.contains("SPIRA_SCCACHE_DAV_ROOT")), "{rendered:?}");
}

#[test_id(t6bnbe0)]
#[test]
fn rfc3339_is_correct() {
    assert_eq!(fsutil::rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(fsutil::rfc3339(1_790_640_000), "2026-09-29T00:00:00Z");
}

#[test_id(tyltge5)]
#[test]
fn parse_rfc3339_round_trips_through_rfc3339() {
    for secs in [0u64, 1, 59, 60, 3599, 3600, 86_399, 86_400, 1_790_640_000, 4_102_444_799, 253_402_300_799] {
        let s = fsutil::rfc3339(secs);
        assert_eq!(fsutil::parse_rfc3339(&s), Some(secs), "round-trip of {s:?}");
    }
}

#[test_id(thmpqcy)]
#[test]
fn parse_rfc3339_refuses_anything_it_did_not_write() {
    for bad in ["", "not a date", "2026-09-29T00:00:00", "2026-09-29 00:00:00Z", "2026-13-01T00:00:00Z", "2026-09-29T24:00:00Z", "2026-09-29T00:00:00Zx"] {
        assert_eq!(fsutil::parse_rfc3339(bad), None, "{bad:?} must not parse");
    }
}

// ---------------------------------------------------------------- hotfix alert threshold

// ---------------------------------------------------------------- install-tarball

/// An `Unpack` that ignores the tarball's actual bytes (tests never build a real
/// `.tar.gz`) and materialises `into/<name>/` with the given files, mirroring what
/// `spira/build-tarball.sh`'s tarball contains: `git archive` output plus `bin/`.
struct FakeUnpack {
    name: &'static str,
    files: Vec<(&'static str, &'static str, bool)>,
    fails: bool,
}

impl Unpack for FakeUnpack {
    fn extract(&self, _tarball: &Path, into: &Path) -> Result<(), String> {
        if self.fails {
            return Err("simulated extract failure".into());
        }
        let root = into.join(self.name);
        for (p, body, x) in &self.files {
            if *x {
                exe(&root.join(p), body)
            } else {
                file(&root.join(p), body)
            }
        }
        Ok(())
    }
}

fn tarball_bins() -> Vec<(&'static str, &'static str, bool)> {
    vec![("bin/loom", "#!/bin/sh\n", true), ("bin/panel", "#!/bin/sh\n", true), ("MANIFEST", "commit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\ntimestamp 20260101T000000Z\nrepo spira\n", false)]
}

/// A releases-dir World with no build/verify machinery involved — install-tarball never
/// touches `cfg.run`'s history/hotfix state, so `run` is left unset here on purpose.
fn install_world() -> (Sandbox, Config) {
    let sb = Sandbox::new();
    let mut env = Env::new();
    env.insert("SPIRA_UNIT_DIR".into(), sb.p().join("units").display().to_string());
    let flags = Flags { releases: Some(sb.p().join("rel")), run: None, keep: Some(2) };
    let reg = Registered { instance: "prod".into(), ..Default::default() };
    let cfg = Config::resolve_with(&flags, &env, None, &reg).unwrap();
    fs::create_dir_all(sb.p().join("units")).unwrap();
    (sb, cfg)
}

fn touch_tarball(sb: &Sandbox, name: &str) -> PathBuf {
    let tb = sb.p().join(format!("{name}.tar.gz"));
    file(&tb, "");
    tb
}

#[test_id(tuu1v1x)]
#[test]
fn release_name_from_tarball_accepts_the_naming_convention_and_refuses_the_rest() {
    assert_eq!(install::release_name_from_tarball(Path::new("spira-20260101T000000Z.tar.gz")).unwrap(), "spira-20260101T000000Z");
    assert_eq!(install::release_name_from_tarball(Path::new("tmp/x/spira-1.tgz")).unwrap(), "spira-1");
    assert!(install::release_name_from_tarball(Path::new("spira-1.zip")).is_err());
    assert!(install::release_name_from_tarball(Path::new("release.tar.gz")).is_err());
    assert!(install::release_name_from_tarball(Path::new("spira-.tar.gz")).is_err());
}

#[test_id(tj8w0nb)]
#[test]
fn install_tarball_is_idempotent_when_the_release_already_exists() {
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    fs::create_dir_all(cfg.releases.join("spira-20260101T000000Z")).unwrap();
    let tb = touch_tarball(&sb, "spira-20260101T000000Z");
    // Extraction must never be attempted for an already-present release (releases are
    // immutable); this Unpack always errors, so a call would fail the install.
    let un = FakeUnpack { name: "spira-20260101T000000Z", files: vec![], fails: true };

    let r = install::install(&cfg, &sc, &un, &tb, &InstallOpts { dry_run: false, settle: Duration::ZERO, skip_restart: false }).unwrap();
    assert!(!r.fresh);
    assert_eq!(fs::read_link(cfg.releases.join("current")).unwrap().to_string_lossy(), "spira-20260101T000000Z");
}

// covers: UC-instance-lifecycle-15
#[test_id(thc2rp1)]
#[test]
fn install_tarball_makes_the_just_unpacked_release_live_over_a_stale_one_and_leaves_it_read_only() {
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    let stale = cfg.releases.join("spira-20200101T000000Z");
    file(&stale.join("bin/loom"), "stale-marker");
    std::os::unix::fs::symlink("spira-20200101T000000Z", cfg.releases.join("current")).unwrap();
    assert_eq!(fs::read_to_string(cfg.releases.join("current/bin/loom")).unwrap(), "stale-marker");

    let tb = touch_tarball(&sb, "spira-20260925T000000Z");
    let un = FakeUnpack { name: "spira-20260925T000000Z", files: vec![("bin/loom", "sp-x.fixed", true)], fails: false };
    let r = install::install(&cfg, &sc, &un, &tb, &InstallOpts { dry_run: false, settle: Duration::ZERO, skip_restart: true }).unwrap();

    assert!(r.fresh);
    assert_eq!(fs::read_to_string(cfg.releases.join("current/bin/loom")).unwrap(), "sp-x.fixed");
    let meta = fs::metadata(cfg.releases.join("spira-20260925T000000Z/bin/loom")).unwrap();
    assert!(meta.permissions().readonly(), "an installed release is a read-only tree");
}

#[test_id(tic9jq2)]
#[test]
fn install_tarball_refuses_when_the_archive_does_not_produce_the_promised_directory() {
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    let tb = touch_tarball(&sb, "spira-20260101T000000Z");
    // The archive unpacks to a DIFFERENT top-level name than the tarball promised.
    let un = FakeUnpack { name: "spira-99999999T999999Z", files: vec![("MANIFEST", "commit a\n", false)], fails: false };

    let err = install::install(&cfg, &sc, &un, &tb, &InstallOpts { dry_run: false, settle: Duration::ZERO, skip_restart: false }).unwrap_err();
    assert!(err.contains("did not unpack to the expected directory"), "{err}");
    assert!(!cfg.releases.join("current").exists());
    assert!(!cfg.releases.join("spira-20260101T000000Z").exists());
}

#[test_id(tgsfrx4)]
#[test]
fn install_tarball_refuses_a_missing_file_or_a_bad_name_before_touching_anything() {
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    let un = FakeUnpack { name: "x", files: vec![], fails: true };

    let missing = sb.p().join("spira-1.tar.gz");
    let e1 = install::install(&cfg, &sc, &un, &missing, &InstallOpts::default()).unwrap_err();
    assert!(e1.contains("not found"), "{e1}");

    let bad_name = touch_tarball(&sb, "notspira");
    let e2 = install::install(&cfg, &sc, &un, &bad_name, &InstallOpts::default()).unwrap_err();
    assert!(e2.contains("must be named"), "{e2}");
    assert!(!cfg.releases.exists() || fs::read_dir(&cfg.releases).unwrap().next().is_none());
}

#[test_id(t8vcacx)]
#[test]
fn install_tarball_dry_run_reports_intent_and_changes_nothing() {
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    let tb = touch_tarball(&sb, "spira-20260101T000000Z");
    let un = FakeUnpack { name: "spira-20260101T000000Z", files: tarball_bins(), fails: true };

    let r = install::install(&cfg, &sc, &un, &tb, &InstallOpts { dry_run: true, settle: Duration::ZERO, skip_restart: false }).unwrap();
    assert_eq!(r.name, "spira-20260101T000000Z");
    assert!(!cfg.releases.join("spira-20260101T000000Z").exists());
    assert!(!cfg.releases.join("current").exists());
    assert_eq!(*sc.reloads.borrow(), 0);
    assert!(sc.restarts.borrow().is_empty());
}


// ---------------------------------------------------------------- config delta

fn delta_git(delta: &str) -> FakeGit {
    FakeGit { validator: true, delta_for: Some((B.to_string(), delta.to_string())), ..Default::default() }
}

const DELTA_BOTH: &str = "removed = [\"spira.old_key\"]\n[spira]\nnew_key = 300\n";

#[test_id(tln20ij)]
#[test]
fn a_rename_mixing_a_persona_and_a_config_key_is_refused() {
    let e = config_delta::Delta::parse("[renamed]\n\"persona.a\" = \"spira.b\"\n").unwrap_err();
    assert!(e.contains("a rename is persona"), "{e}");
}

// ---------------------------------------------------------------- activation installs new units

#[test_id(tx0ef8p)]
#[test]
fn config_for_a_tree_has_its_delta_applied_and_a_key_without_an_entry_stays_absent() {
    let w = World::with_git(delta_git(DELTA_BOTH));
    let spec = w.cfg.toml_spec().unwrap();
    file(Path::new(&spec), "[spira]\nid_prefix = \"sp\"\nold_key = 1\n");
    let tree = w.cfg.releases.join("head-tree");
    file(&tree.join(config_delta::DELTA_PATH), "[spira]\nnew_key = 7\n");
    let (dir, staged) = config_delta::staged_for_tree(&spec, &tree).unwrap().expect("the tree has a delta");
    let text = fs::read_to_string(&staged).unwrap();
    assert!(text.contains("new_key = 7") && !text.contains("other_key"), "{text}");
    let _ = fs::remove_dir_all(dir);
    let bare = w.cfg.releases.join("bare-tree");
    fs::create_dir_all(&bare).unwrap();
    assert!(config_delta::staged_for_tree(&spec, &bare).unwrap().is_none(), "no delta, nothing staged");
}

#[test_id(tfxagql)]
#[test]
fn a_key_the_delta_removes_still_resolves_under_the_registry_that_requires_it() {
    let w = World::with_git(delta_git(""));
    let spec = w.cfg.toml_spec().unwrap();
    file(Path::new(&spec), "[spira]\nid_prefix = \"sp\"\nunit_test_slow_secs = 9\n");
    let tree = w.cfg.releases.join("head-tree");
    file(&tree.join(config_delta::DELTA_PATH), "removed = [\"spira.unit_test_slow_secs\"]\n");
    let (dir, staged) = config_delta::staged_for_tree(&spec, &tree).unwrap().expect("the tree has a delta");
    let text = fs::read_to_string(&staged).unwrap();
    let predecessor = tree_declaring(&w.cfg.releases.join("old"), &["SPIRA_UNIT_TEST_SLOW_SECS"]);
    fs::create_dir_all(predecessor.join("spira/conf.toml.d")).unwrap();
    let reg = spira_config::registry::Registry::from_dirs(&predecessor.join("spira")).unwrap();
    let (doc, _) = spira_config::validate_with(&text, &reg).unwrap();
    assert!(doc.spira.unwrap().0.contains_key("unit_test_slow_secs"), "the predecessor's registry requires it, so the resolver must still see it: {text}");
    let _ = fs::remove_dir_all(dir);
}

#[test_id(t8pzamw)]
#[test]
fn activation_holds_a_retired_key_the_release_being_replaced_still_declares() {
    let w = World::with_git(delta_git(""));
    file(Path::new(&w.cfg.toml_spec().unwrap()), "[spira]\nid_prefix = \"sp\"\nround_vm_disk_need_gib = 0\n");
    let rel = w.cfg.releases.join("new");
    fs::create_dir_all(rel.join("bin")).unwrap();
    testkit::write_exe(rel.join("bin/spira-config"), "#!/bin/sh\nexit 0\n");
    tree_declaring(&rel, &["SPIRA_OTHER_SECS"]);
    let old = tree_declaring(&w.cfg.releases.join("old"), &["SPIRA_ROUND_VM_DISK_NEED_GIB"]);
    std::os::unix::fs::symlink(&old, w.cfg.releases.join("current")).unwrap();
    let state = w.cfg.state_dir().unwrap();
    fs::create_dir_all(&state).unwrap();
    config_delta::save_retired(&state, &["spira.round_vm_disk_need_gib".to_string()]).unwrap();
    let txn = config_delta::prepare(&w.cfg, &rel, &config_delta::Delta::default(), true).unwrap();
    let scratch = w.cfg.releases.join("scratch");
    fs::create_dir_all(&scratch).unwrap();
    let staged = fs::read_to_string(txn.stage(&scratch).unwrap()).unwrap();
    assert!(staged.contains("round_vm_disk_need_gib = 0"), "the replaced release's registry still requires it: {staged}");
    assert_eq!(txn.deferred, vec!["spira.round_vm_disk_need_gib".to_string()], "held for the activation after this one");

    fs::remove_file(w.cfg.releases.join("current")).unwrap();
    let old = tree_declaring(&w.cfg.releases.join("old2"), &["SPIRA_OTHER_SECS"]);
    std::os::unix::fs::symlink(&old, w.cfg.releases.join("current")).unwrap();
    let txn = config_delta::prepare(&w.cfg, &rel, &config_delta::Delta::default(), true).unwrap();
    let staged = fs::read_to_string(txn.stage(&scratch).unwrap()).unwrap();
    assert!(!staged.contains("round_vm_disk_need_gib"), "control: a registry that no longer declares it lets the retired key go: {staged}");
    assert!(txn.deferred.is_empty());
}

#[test_id(tn5ziug)]
#[test]
fn systemd_timespans_parse_into_a_start_budget() {
    use crate::systemctl::parse_timespan;
    assert_eq!(parse_timespan("5s"), Some(Duration::from_secs(5)));
    assert_eq!(parse_timespan("15min"), Some(Duration::from_secs(900)));
    assert_eq!(parse_timespan("1min 30s"), Some(Duration::from_secs(90)));
    assert_eq!(parse_timespan("100ms"), Some(Duration::from_millis(100)));
    assert_eq!(parse_timespan("infinity"), None);
    assert_eq!(crate::systemctl::budget_from_show("RestartUSec=5s\nTimeoutStartUSec=15min\n"), Some(Duration::from_secs(905)));
}

#[test_id(tme1593)]
#[test]
fn a_documentation_line_names_current_so_two_releases_render_one_unit() {
    let text = "[Unit]\nDocumentation=file://@SPIRA_HOME@/README.md\n[Service]\nExecStart=@SPIRA_RELEASE@/bin/x\n";
    let host = BTreeMap::new();
    let a = units::render("x.service", text, Path::new("r/spira-releases/aaa"), &host, None, "prod").unwrap();
    let b = units::render("x.service", text, Path::new("r/spira-releases/bbb"), &host, None, "prod").unwrap();
    assert!(a.contains("Documentation=file://r/spira-releases/current/spira/README.md\n"), "{a}");
    assert!(a.contains("ExecStart=r/spira-releases/aaa/bin/x"), "Exec lines still name the sha: {a}");
    assert_eq!(units::without_documentation(&a).replace("aaa", "R"), units::without_documentation(&b).replace("bbb", "R"));
    let stale = a.replace("current", "0ldsha");
    assert_eq!(units::without_documentation(&stale), units::without_documentation(&a), "a Documentation-only difference is not a change");
}

// ---------------------------------------------------------------- restart budget

#[test_id(them4wr)]
#[test]
fn a_delta_naming_a_placeholder_path_is_refused_and_the_shipped_one_is_clean() {
    let bad = "[spira]\nbatch_ledger = \"/placeholder/run/batch-timing.tsv\"\n";
    let err = config_delta::Delta::parse(bad).unwrap_err();
    assert!(err.contains("placeholder path"), "{err}");
    let ok = config_delta::Delta::parse("[spira]\nbatch_ledger = \"/srv/run/batch-timing.tsv\"\ngroom_gate_label = \"placeholder-gate\"\n");
    assert!(ok.is_ok(), "{ok:?}");
    let shipped = std::fs::read_to_string(spira_config::resolve::locate_home_for_process().unwrap().join("config-delta.toml")).unwrap();
    let d = config_delta::Delta::parse(&shipped).unwrap();
    assert!(!d.added.contains_key("spira.batch_ledger"), "the ledger path derives from the install's run dir");
}
#[test_id(tjs59o8)]
#[test]
fn the_shipped_delta_declares_the_deleted_watch_tasks_removed() {
    let d = config_delta::load(&spira_config::resolve::checkout_root_for_process().unwrap()).unwrap().expect("the tree ships a delta");
    for t in ["watch.perf_happy", "watch.spill_key", "watch.statute_audit", "sweep.repo_contract"] {
        assert!(d.removed_tasks.iter().any(|r| r == t), "{t} missing from removed_tasks");
    }
}

#[test_id(tv75ar0)]
#[test]
fn a_release_whose_two_changes_each_retire_a_key_in_their_own_fragment_loads_both() {
    let tmp = TempDir::new("delta-fragments");
    let tree = tmp.path().to_path_buf();
    file(&tree.join(config_delta::DELTA_PATH), "[spira]\nk = 1\n");
    file(&tree.join("spira/config-delta.d/sp-a.toml"), "removed = [\"spira.x\"]\nremoved_tasks = [\"t.a\"]\n");
    file(&tree.join("spira/config-delta.d/sp-b.toml"), "removed = [\"spira.y\"]\nremoved_tasks = [\"t.b\"]\n");
    let d = config_delta::load(&tree).unwrap().expect("the tree ships a delta");
    assert_eq!(d.removed, ["spira.x", "spira.y"]);
    assert_eq!(d.removed_tasks, ["t.a", "t.b"]);
    file(&tree.join("spira/config-delta.d/sp-c.toml"), "[spira]\nk = 2\n");
    let d = config_delta::load(&tree).unwrap().expect("the tree ships a delta");
    assert_eq!(d.added["spira.k"].as_integer(), Some(2), "a fragment's value replaces the base's");
    file(&tree.join("spira/config-delta.d/sp-d.toml"), "[spira]\nk = 3\n");
    let e = config_delta::load(&tree).unwrap_err();
    assert!(e.contains("sp-c.toml") && e.contains("sp-d.toml"), "{e}");
}

const TC: &str = "1.99.0 abc";

#[test_id(tdp7d65)]
#[test]
fn the_manifest_round_trips_and_the_key_names_tree_and_toolchain() {
    let m = artifacts::Manifest { key: "k".into(), tree: "t".into(), toolchain: TC.into(), tarball_sha256: "s".into(), tarball_bytes: 7, files: vec![("a".into(), "h1".into()), ("b".into(), "h2".into())] };
    assert_eq!(artifacts::Manifest::parse(&m.render()).unwrap(), m);
    assert_eq!(artifacts::key("t1", "deadbeef 1.99.0"), "55ff5d54bdd1a52763970e49c935c72df12820f8369f1e5d6ca4f4b1c2f56561", "the round VM's bash computes this same value");
    assert_ne!(artifacts::key("t1", TC), artifacts::key("t2", TC));
    assert_ne!(artifacts::key("t1", TC), artifacts::key("t1", "1.100.0 def"));
    assert_eq!(artifacts::toolchain_id("rustc 1.99.0 (abc 2026)\nbinary: rustc\ncommit-hash: deadbeef\nrelease: 1.99.0\nhost: x\n"), "deadbeef 1.99.0");
}

#[test_id(talreuc)]
#[test]
fn a_delta_value_of_the_wrong_type_is_refused_at_load_naming_the_key_and_file() {
    let shipped = spira_config::resolve::locate_home_for_process().unwrap();
    let tmp = TempDir::new("delta-type");
    let rel = tmp.path().to_path_buf();
    let spira = rel.join("spira");
    fs::create_dir_all(&spira).unwrap();
    for d in ["conf.d", "conf.toml.d", "testlib"] {
        let _ = std::os::unix::fs::symlink(shipped.join(d), spira.join(d));
    }
    fs::write(spira.join("config-delta.toml"), "[spira]\nlocal_backlog_ref_max_age = 3600\n").unwrap();
    assert!(config_delta::load(&rel).unwrap().is_some(), "control: the integer form loads");
    fs::write(spira.join("config-delta.toml"), "[spira]\nlocal_backlog_ref_max_age = \"3600\"\n").unwrap();
    let e = config_delta::load(&rel).unwrap_err();
    assert!(e.contains("config-delta.toml") && e.contains("spira.local_backlog_ref_max_age") && e.contains("u32"), "{e}");
}

/// The scheduler consolidation's production delta: activation carries every key a task pins.
/// A box whose config and running release know none of this tree's keys still converges, so
/// every REQUIRED key is declared (`complete` refuses one that is not), the values pass the
/// release's own validation, every task runs in act (ruling D4) and the units run on systemd.
#[test_id(ttui1g8)]
#[test]
fn the_shipped_delta_carries_every_required_key_with_every_task_in_act_on_systemd() {
    let tree = spira_config::resolve::checkout_root_for_process().unwrap();
    let d = config_delta::load(&tree).unwrap().expect("the tree ships a delta");
    let nothing_known = TempDir::new("no-active-release");
    let done = config_delta::complete(&[toml::map::Map::new()], &tree, Some(nothing_known.path()), Some(d.clone()));
    let done = done.unwrap_or_else(|e| panic!("{e}")).expect("a completed delta");
    assert!(!d.added.contains_key("spira.task_modes"), "a retirement never rewrites a task_modes line; the registry default carries it");
    assert_eq!(d.added.get("spira.units"), Some(&toml::Value::String("systemd".into())));
    let modes = done.added.get("spira.task_modes").and_then(|v| v.as_str()).expect("task_modes completes from the registry default");
    let pairs: Vec<&str> = modes.split_whitespace().collect();
    assert!(pairs.len() >= 79, "every task is named: {}", pairs.len());
    assert!(pairs.iter().all(|p| p.ends_with("=act")), "every task runs in act: {modes}");
}

#[test_id(txbvjzk)]
#[test]
fn an_undo_record_in_the_old_added_shape_still_loads_for_a_rollback() {
    let tmp = TempDir::new("legacy-undo");
    let p = config_delta::undo_path(tmp.path(), "abc");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, "removed = [\"spira.k\"]\n[added]\n\"spira.old\" = 5\n").unwrap();
    let d = config_delta::load_undo(tmp.path(), "abc").unwrap().unwrap();
    assert_eq!(d.removed, vec!["spira.k".to_string()]);
    assert_eq!(d.added.get("spira.old"), Some(&toml::Value::Integer(5)));
}

#[test_id(t11wawa)]
#[test]
fn a_delta_with_a_nested_table_round_trips_through_render() {
    let d = config_delta::Delta::parse("[persona.x]\nmodel = \"m\"\n[spira]\nk = 1\n").unwrap();
    assert!(d.added.contains_key("persona.x.model") && d.added.contains_key("spira.k"));
    assert_eq!(config_delta::Delta::parse(&d.render()).unwrap(), d);
}

fn tree_declaring(root: &Path, names: &[&str]) -> PathBuf {
    let dir = root.join("spira/conf.d");
    for n in names {
        file(&dir.join(n), &format!("TYPE=u32\nDOC={n}\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${{{n}:=600}}\"\nSPIRA_CONF_DEFAULT_EOF\n"));
    }
    root.to_path_buf()
}

#[test_id(t6lgheb)]
#[test]
fn completion_adds_a_key_the_active_registry_declares_and_the_config_lacks_at_its_default() {
    let tmp = TempDir::new("complete");
    let new = tree_declaring(&tmp.path().join("new"), &["SPIRA_ORPHAN_SECS"]);
    let active = tree_declaring(&tmp.path().join("old"), &["SPIRA_ORPHAN_SECS"]);
    let cfg: toml::Table = "[spira]\nid_prefix = \"sp\"\n".parse().unwrap();
    let d = config_delta::complete(&[cfg], &new, Some(&active), None).unwrap().expect("a delta");
    assert_eq!(d.added.get("spira.orphan_secs"), Some(&toml::Value::Integer(600)));
}

#[test_id(tuijdjm)]
#[test]
fn completion_leaves_a_key_the_config_already_sets() {
    let tmp = TempDir::new("complete");
    let new = tree_declaring(&tmp.path().join("new"), &["SPIRA_ORPHAN_SECS", "SPIRA_OTHER_SECS"]);
    let cfg: toml::Table = "[spira]\norphan_secs = 5\n".parse().unwrap();
    let d = config_delta::complete(&[cfg], &new, Some(&new), None).unwrap().expect("a delta");
    assert!(!d.added.contains_key("spira.orphan_secs"));
    assert_eq!(d.added.get("spira.other_secs"), Some(&toml::Value::Integer(600)));
}

/// sp-rdldh2: a release that registers a new scheduler task ships its mode in the
/// `SPIRA_TASK_MODES` default, but a live `spira.task_modes` written before the task existed
/// wins over the default, and the new runtime refuses to start without a mode for every task.
/// Completion adds the default's mode for each task the live value lacks and keeps the live
/// modes for the rest.
#[test_id(tb9zrf1)]
#[test]
fn completion_gives_a_task_the_live_task_modes_lacks_its_release_default_mode() {
    let tmp = TempDir::new("complete-task-modes");
    let tree = |root: &Path, default: &str| {
        file(
            &root.join("spira/conf.d/SPIRA_TASK_MODES"),
            &format!("TYPE=string\nDOC=modes\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${{SPIRA_TASK_MODES:={default}}}\"\nSPIRA_CONF_DEFAULT_EOF\n"),
        );
        root.to_path_buf()
    };
    let active = tree(&tmp.path().join("old"), "heartbeat=act watch.ami_stale=act");
    let new = tree(&tmp.path().join("new"), "heartbeat=act watch.ami_stale=act watch.throughput=act");
    let live: toml::Table = "[spira]\ntask_modes = \"heartbeat=act watch.ami_stale=observe\"\n".parse().unwrap();
    let d = config_delta::complete(std::slice::from_ref(&live), &new, Some(&active), None).unwrap().expect("a delta that completes task_modes");
    let (_, post, undo) = config_delta::apply(std::slice::from_ref(&live), &d, &[], true).unwrap();
    assert_eq!(
        post[0]["spira"]["task_modes"].as_str(),
        Some("heartbeat=act watch.ami_stale=observe watch.throughput=act"),
        "the new task takes the release default; the live override stands"
    );
    let (_, back, _) = config_delta::apply(&post, &undo, &[], false).unwrap();
    assert_eq!(back[0], live, "a rollback puts the live value back as it was");
    assert!(config_delta::complete(&post, &new, Some(&active), None).unwrap().is_none(), "a complete value is left alone");
}

fn shipped_unit(name: &str) -> String {
    std::fs::read_to_string(spira_config::resolve::checkout_root_for_process().unwrap().join("systemd").join(name)).unwrap()
}

#[test_id(tdfh3b9)]
#[test]
fn a_template_that_restarts_when_idle_gives_a_reason_and_a_bare_boolean_gives_none() {
    let t = |v: &str| format!("[Unit]\nX-Spira-RestartWhenIdle={v}\n");
    assert_eq!(units::restart_when_idle(&t("it ends its own pass when idle")).as_deref(), Some("it ends its own pass when idle"));
    assert_eq!(units::restart_when_idle(&t("true")), None);
    assert_eq!(units::restart_when_idle("[Unit]\n"), None);
}

#[test_id(tclwmck)]
#[test]
fn the_unit_run_watcher_restarts_itself_when_idle() {
    let unit = shipped_unit("spira-unit-run-dbus.service");
    assert_eq!(units::restart_when_idle(&unit).as_deref(), Some("unit-run watch exits once its rows are stored and its release is not current"));
    assert!(unit.lines().any(|l| l.trim() == "RestartSec=1"));
    assert_eq!(units::never_restart(&shipped_unit("spira-unit-run-dbus.service")), None);
}

const OLD_UNIT: &str = "[Service]\nType=simple\nExecStart=/bin/true old\n";
const NEW_UNIT: &str = "[Service]\nType=simple\nExecStart=/bin/true new\n";

/// A release `A` whose only template differs from the installed unit, so activating it restarts
/// that unit. Returns the world with `current` unset.
fn world_activating_a_changed_unit() -> World {
    let w = World::with_git(FakeGit::default());
    let rel = w.cfg.releases.join(A);
    file(&rel.join("systemd/spira-tool.service"), NEW_UNIT);
    let entries = Manifest::scan(&rel).unwrap();
    let m = Manifest { commit: A.into(), built: None, repo: None, entries };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    file(&w.cfg.unit_dir.join("spira-tool-prod.service"), OLD_UNIT);
    w
}

/// The new release's `spira-lc`: one READY bead, and a content read that is refused when
/// `refuses` names it (the argv's words, joined).
struct FakeReads {
    refuses: Option<&'static str>,
    asked: RefCell<Vec<String>>,
    /// How many `dwell` reads answer "deadline" before the server is warm.
    cold_dwells: Cell<u32>,
}

impl FakeReads {
    fn serving() -> FakeReads {
        FakeReads { refuses: None, asked: RefCell::new(vec![]), cold_dwells: Cell::new(0) }
    }
}

impl activate::Reads for FakeReads {
    fn lc(&self, _rel: &Path, args: &[&str]) -> Result<String, String> {
        let call = args.join(" ");
        self.asked.borrow_mut().push(call.clone());
        if call.starts_with("dwell") && self.cold_dwells.get() > 0 {
            self.cold_dwells.set(self.cold_dwells.get() - 1);
            return Err("dwell (exit status: 2): spira-lc: cannot tell: deadline".into());
        }
        if self.refuses.is_some_and(|r| call.starts_with(r)) {
            return Err(format!("{call} (exit status: 2): spira-lc content: cannot tell: the beads database spira does not have the layout the content reads query (missing events.issue_id, ...)"));
        }
        Ok(if call.starts_with("list") { r#"[{"bead_id":"sp-first"},{"bead_id":"sp-second"}]"#.into() } else { "[]".into() })
    }
}

fn activate_with(w: &World, sc: &FakeSystemctl) -> Result<activate::Switched, String> {
    activate_reading(w, sc, &FakeReads::serving())
}

fn activate_reading(w: &World, sc: &FakeSystemctl, reads: &FakeReads) -> Result<activate::Switched, String> {
    activate_reading_within(w, sc, reads, Duration::from_secs(60), Duration::from_secs(60))
}

fn activate_reading_within(w: &World, sc: &FakeSystemctl, reads: &FakeReads, steady: Duration, warm_wait: Duration) -> Result<activate::Switched, String> {
    activate_reporting(w, sc, reads, steady, warm_wait, &|_| {})
}

fn activate_reporting(w: &World, sc: &FakeSystemctl, reads: &FakeReads, steady: Duration, warm_wait: Duration, report: &dyn Fn(&str)) -> Result<activate::Switched, String> {
    let nix = FakeNix::default();
    let ctx = activate::Ctx { cfg: &w.cfg, reads, sc, nix: &nix, git: &w._git, repo: None, landed_ref: "main".into(), unit_ensure: &activate::ensure_new_units, settle: Duration::from_millis(10), drain: Duration::from_millis(10), steady, warm_wait, warm_pause: Duration::from_millis(5), report, gc: None, publisher: &no_broker };
    activate::activate(&ctx, A, None)
}

fn recorded_state(w: &World) -> Option<String> {
    crate::machine::load(&w.cfg.state_dir().unwrap(), A).unwrap().and_then(|r| r.state()).map(|s| s.as_str().to_string())
}

fn published(w: &World) {
    crate::machine::enter(&w.cfg.state_dir().unwrap(), A, crate::machine::ReleaseState::Published, "activation started").unwrap();
}

fn activated_entries(w: &World) -> usize {
    crate::machine::load(&w.cfg.state_dir().unwrap(), A).unwrap().map_or(0, |r| r.events.iter().filter(|e| e.to == crate::machine::ReleaseState::Activated).count())
}

#[test_id(t9cp7f6)]
#[test]
fn the_activation_is_recorded_only_after_the_restarted_units_are_checked_up() {
    let w = world_activating_a_changed_unit();
    published(&w);
    let state = w.cfg.state_dir().unwrap();
    let seen = {
        let state = state.clone();
        move || format!("{:?} {}", crate::machine::load(&state, A).unwrap().and_then(|r| r.state()).map(|s| s.as_str().to_string()), fs::read_to_string(state.join("history")).unwrap_or_default().trim())
    };
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.on_restart = Some(Box::new(seen));
    activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.at_restart.borrow(), vec!["Some(\"published\") ".to_string()]);
    assert_eq!(recorded_state(&w).as_deref(), Some("activated"));
    assert!(fs::read_to_string(state.join("history")).unwrap().contains(A));
}

#[test_id(tftory4)]
#[test]
fn a_scheduler_that_keeps_going_back_to_activating_is_rolled_back_and_never_recorded_activated() {
    let w = world_activating_a_changed_unit();
    published(&w);
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.slow_starts.borrow_mut().insert("spira-tool-prod.service".into(), usize::MAX);
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("spira-tool-prod.service") && e.contains("did not come up") && e.contains("rolled back"), "{e}");
    assert!(!w.cfg.releases.join("current").exists(), "current is put back");
    assert_eq!(fs::read_to_string(w.cfg.unit_dir.join("spira-tool-prod.service")).unwrap(), OLD_UNIT);
    assert_eq!(recorded_state(&w).as_deref(), Some("rolled_back"));
    assert_eq!(activated_entries(&w), 0);
    assert!(!fs::read_to_string(w.cfg.state_dir().unwrap().join("history")).unwrap_or_default().contains(A));
}

#[test_id(tv8kat3)]
#[test]
fn an_activator_stopped_right_after_the_last_restart_leaves_no_activated_entry() {
    let w = world_activating_a_changed_unit();
    published(&w);
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.on_restart = Some(Box::new(|| panic!("the activator is killed with the unit it restarted")));
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| activate_with(&w, &sc)));
    assert!(died.is_err());
    assert_eq!(activated_entries(&w), 0);
    assert_eq!(recorded_state(&w).as_deref(), Some("published"));
}

#[test_id(t8sfnym)]
#[test]
fn an_activation_whose_record_cannot_be_written_after_the_units_are_up_is_undone_and_names_it() {
    let w = world_activating_a_changed_unit();
    published(&w);
    let path = w.cfg.state_dir().unwrap().join("machine").join(format!("{A}.json"));
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.on_restart = Some(Box::new(move || {
        if path.is_file() {
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
        }
        String::new()
    }));
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("cannot record the activation of") && e.contains("rolled back"), "{e}");
    assert!(!w.cfg.releases.join("current").exists(), "current is put back");
    assert!(!fs::read_to_string(w.cfg.state_dir().unwrap().join("history")).unwrap_or_default().contains(A), "history is put back");
}

#[test_id(t88ns1j)]
#[test]
fn an_activation_whose_restart_fails_is_undone_in_the_record_as_in_the_link() {
    let w = world_activating_a_changed_unit();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("rolled back"), "{e}");
    let state = w.cfg.state_dir().unwrap();
    let record = crate::machine::load(&state, A).unwrap().unwrap();
    assert_eq!(record.state().map(|s| s.as_str()), Some("rolled_back"));
    assert!(!fs::read_to_string(state.join("history")).unwrap_or_default().contains(A));
}

const CACHE_UNIT: &str = "[Service]\nType=simple\nEnvironment=PATH=@SPIRA_RELEASE@/bin:/usr/bin\nExecStart=@SPIRA_PROD@/sccache-dav.sh\n";

/// Release `A` over a previous release `P` (current), each with its own `sccache-dav` binary
/// and launcher script; the installed unit names `P`. Returns the world.
fn world_activating_over_a_running_cache(binary_changes: bool) -> World {
    let w = World::with_git_and_reg(FakeGit::default(), Registered { sccache_dav_addr: "192.0.2.7:9431".into(), ..Default::default() });
    let prev = "p".repeat(40);
    for (name, bin) in [(prev.as_str(), "backend v1"), (A, if binary_changes { "backend v2" } else { "backend v1" })] {
        let rel = w.cfg.releases.join(name);
        file(&rel.join("systemd/sccache-dav.service"), CACHE_UNIT);
        file(&rel.join("bin/sccache-dav"), bin);
        file(&rel.join("spira/sccache-dav.sh"), "#!/bin/sh\nexec sccache-dav\n");
        file(&rel.join("sccache-dav/Cargo.toml"), "[dependencies]\ndep = { path = \"../dep\" }\n\n[dev-dependencies]\ndevdep = { path = \"../devdep\" }\n");
        file(&rel.join("dep/Cargo.toml"), "[package]\nname = \"dep\"\n");
        file(&rel.join("dep/src/lib.rs"), "pub fn f() {}\n");
        file(&rel.join("devdep/src/lib.rs"), "pub fn g() {}\n");
        file(&rel.join("Cargo.lock"), "# lock\n");
    }
    let rel = w.cfg.releases.join(A);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    std::os::unix::fs::symlink(&prev, w.cfg.releases.join("current")).unwrap();
    let old = CACHE_UNIT.replace("@SPIRA_RELEASE@", &w.cfg.releases.join(&prev).display().to_string()).replace("@SPIRA_PROD@", &format!("{}/spira", w.cfg.releases.join(&prev).display()));
    file(&w.cfg.unit_dir.join("sccache-dav.service"), &old);
    w
}


#[test_id(tifkkpp)]
#[test]
fn an_activation_that_changes_neither_the_cache_backends_unit_nor_its_binary_leaves_it_and_the_sccache_server_alone() {
    let w = world_activating_over_a_running_cache(false);
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.states.borrow_mut().insert("sccache-dav.service".into(), UnitState { active: "active".into(), result: "success".into(), kind: "simple".into() });
    let out = activate_with(&w, &sc).unwrap();
    assert_eq!(out.retired, Vec::<String>::new(), "the unit must be live for this to show anything");
    assert!(!out.restarted.iter().any(|u| u == "sccache-dav.service"), "{:?}", out.restarted);
    assert_eq!(out.left_running, vec!["sccache-dav.service".to_string()], "the activation names what it left running");
    assert_eq!(*sc.cache_server_stops.borrow(), 0);
}

#[test_id(trn0ehd)]
#[test]
fn an_activation_that_changes_the_cache_backends_binary_restarts_it_and_stops_the_sccache_server() {
    let w = world_activating_over_a_running_cache(true);
    change_new_release_file(&w, "Cargo.lock", "# lock v2\n");
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.states.borrow_mut().insert("sccache-dav.service".into(), UnitState { active: "active".into(), result: "success".into(), kind: "simple".into() });
    let out = activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.restarts.borrow(), vec!["sccache-dav.service".to_string()]);
    assert!(out.left_running.is_empty(), "{:?}", out.left_running);
    assert_eq!(*sc.cache_server_stops.borrow(), 1);
}

#[test_id(tbkuk4i)]
#[test]
fn an_activation_that_changes_the_cache_backends_unit_restarts_it_and_stops_the_sccache_server() {
    let w = world_activating_over_a_running_cache(false);
    let changed = format!("{CACHE_UNIT}Environment=SCCACHE_DAV_EXTRA=1\n");
    file(&w.cfg.releases.join(A).join("systemd/sccache-dav.service"), &changed);
    let rel = w.cfg.releases.join(A);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.states.borrow_mut().insert("sccache-dav.service".into(), UnitState { active: "active".into(), result: "success".into(), kind: "simple".into() });
    activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.restarts.borrow(), vec!["sccache-dav.service".to_string()]);
    assert_eq!(*sc.cache_server_stops.borrow(), 1);
}

fn change_new_release_file(w: &World, path: &str, content: &str) {
    let rel = w.cfg.releases.join(A);
    file(&rel.join(path), content);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
}

fn activate_over_running_cache(w: &World) -> (activate::Switched, FakeSystemctl) {
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.states.borrow_mut().insert("sccache-dav.service".into(), UnitState { active: "active".into(), result: "success".into(), kind: "simple".into() });
    let out = activate_with(w, &sc).unwrap();
    (out, sc)
}

#[test_id(thc0s02)]
#[test]
fn a_rebuild_that_changes_the_cache_backends_bytes_but_not_its_source_leaves_it_running() {
    let w = world_activating_over_a_running_cache(true);
    let (out, sc) = activate_over_running_cache(&w);
    assert!(!out.restarted.iter().any(|u| u == "sccache-dav.service"), "{:?}", out.restarted);
    assert_eq!(out.left_running, vec!["sccache-dav.service".to_string()]);
    assert_eq!(*sc.cache_server_stops.borrow(), 0);
}

#[test_id(to8jlx4)]
#[test]
fn a_changed_binary_and_a_changed_dependency_source_restarts_the_cache_backend() {
    let w = world_activating_over_a_running_cache(true);
    change_new_release_file(&w, "dep/src/lib.rs", "pub fn f() { 1; }\n");
    let (_, sc) = activate_over_running_cache(&w);
    assert_eq!(*sc.restarts.borrow(), vec!["sccache-dav.service".to_string()]);
    assert_eq!(*sc.cache_server_stops.borrow(), 1);
}

#[test_id(tjvpvc4)]
#[test]
fn a_changed_binary_and_a_changed_dev_dependency_source_leaves_the_cache_backend_running() {
    let w = world_activating_over_a_running_cache(true);
    change_new_release_file(&w, "devdep/src/lib.rs", "pub fn g() { 1; }\n");
    let (out, sc) = activate_over_running_cache(&w);
    assert!(!out.restarted.iter().any(|u| u == "sccache-dav.service"), "{:?}", out.restarted);
    assert_eq!(*sc.cache_server_stops.borrow(), 0);
}

#[test_id(tbww3d2)]
#[test]
fn a_changed_launcher_restarts_the_cache_backend_though_its_binary_is_identical() {
    let w = world_activating_over_a_running_cache(false);
    change_new_release_file(&w, "spira/sccache-dav.sh", "#!/bin/sh\nexec sccache-dav --x\n");
    let (_, sc) = activate_over_running_cache(&w);
    assert_eq!(*sc.restarts.borrow(), vec!["sccache-dav.service".to_string()]);
}

#[test_id(twn6z1h)]
#[test]
fn a_changed_binary_with_no_source_tree_to_compare_restarts_the_cache_backend() {
    let w = world_activating_over_a_running_cache(true);
    fs::remove_file(w.cfg.releases.join(A).join("Cargo.lock")).unwrap();
    change_new_release_file(&w, "dep/src/lib.rs", "pub fn f() {}\n");
    let (_, sc) = activate_over_running_cache(&w);
    assert_eq!(*sc.restarts.borrow(), vec!["sccache-dav.service".to_string()]);
}

#[test_id(t3nj26q)]
#[test]
fn a_changed_binary_and_a_changed_transitive_dependency_restarts_the_cache_backend() {
    let w = world_activating_over_a_running_cache(true);
    let prev = w.cfg.releases.join("p".repeat(40));
    for rel in [prev, w.cfg.releases.join(A)] {
        file(&rel.join("dep/Cargo.toml"), "[dependencies]\ndeep = { path = \"../deep\" }\n");
        file(&rel.join("deep/src/lib.rs"), "pub fn d() {}\n");
        file(&rel.join("deep/Cargo.toml"), "[package]\nname = \"deep\"\n");
    }
    change_new_release_file(&w, "deep/src/lib.rs", "pub fn d() { 1; }\n");
    let (_, sc) = activate_over_running_cache(&w);
    assert_eq!(*sc.restarts.borrow(), vec!["sccache-dav.service".to_string()]);
}

#[test_id(tuf12nn)]
#[test]
fn a_changed_binary_and_a_changed_toolchain_pin_restarts_the_cache_backend() {
    let w = world_activating_over_a_running_cache(true);
    change_new_release_file(&w, "rust-toolchain.toml", "[toolchain]\nchannel = \"1.99\"\n");
    let (_, sc) = activate_over_running_cache(&w);
    assert_eq!(*sc.restarts.borrow(), vec!["sccache-dav.service".to_string()]);
}

const LC_UNIT: &str = "[Service]\nType=simple\nExecStart=@SPIRA_PROD_ROOT@/bin/spira-lc serve\n";

/// Release `A` whose `spira-lc` differs from the previous release `P`'s, with `current`
/// already on `A` and the installed unit already naming `A`: nothing in the unit or the
/// links says the running server is old. Returns the world and where the old server runs.
fn world_with_an_lc_serve_left_on_the_previous_release() -> (World, PathBuf) {
    let w = World::with_git(FakeGit::default());
    let prev = "p".repeat(40);
    for (name, bin) in [(prev.as_str(), "spira-lc v1"), (A, "spira-lc v2 with holds")] {
        file(&w.cfg.releases.join(name).join("systemd/lc-serve.service"), LC_UNIT);
        exe(&w.cfg.releases.join(name).join("bin/spira-lc"), bin);
    }
    let rel = w.cfg.releases.join(A);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    std::os::unix::fs::symlink(A, w.cfg.releases.join("current")).unwrap();
    file(&w.cfg.unit_dir.join("lc-serve.service"), &LC_UNIT.replace("@SPIRA_PROD_ROOT@", &rel.display().to_string()));
    let old = w.cfg.releases.join(&prev).join("bin/spira-lc");
    (w, old)
}

fn lc_serve_running(sc: &FakeSystemctl, from: &Path) {
    sc.states.borrow_mut().insert("lc-serve.service".into(), UnitState { active: "active".into(), result: "success".into(), kind: "simple".into() });
    sc.exes.borrow_mut().insert("lc-serve.service".into(), from.to_path_buf());
}

#[test_id(tiz99kk)]
#[test]
fn an_activation_restarts_an_lc_serve_still_executing_the_superseded_releases_binary() {
    let (w, old_bin) = world_with_an_lc_serve_left_on_the_previous_release();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_lands = Some(w.cfg.releases.join(A).join("bin/spira-lc"));
    lc_serve_running(&sc, &old_bin);
    let out = activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.restarts.borrow(), vec!["lc-serve.service".to_string()]);
    assert!(out.restarted.contains(&"lc-serve.service".to_string()), "{:?}", out.restarted);
    assert!(sc.executables("lc-serve.service").iter().all(|e| e.starts_with(w.cfg.releases.join(A))));
}

#[test_id(tfe6ako)]
#[test]
fn an_activation_that_cannot_move_lc_serve_off_the_superseded_binary_is_rolled_back() {
    let (w, old_bin) = world_with_an_lc_serve_left_on_the_previous_release();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_lands = Some(w.cfg.releases.join(A).join("bin/spira-lc"));
    sc.pinned.insert("lc-serve.service".into());
    lc_serve_running(&sc, &old_bin);
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("lc-serve.service") && e.contains("rolled back"), "{e}");
}

#[test_id(tl1tuqy)]
#[test]
fn a_running_process_is_found_executing_the_binary_it_was_started_from() {
    let seen = crate::systemctl::exes_of(&[std::process::id(), u32::MAX]);
    assert_eq!(seen, vec![fs::canonicalize(std::env::current_exe().unwrap()).unwrap()], "a pid that is gone is left out");
}

#[test_id(takr9p4)]
#[test]
fn an_activation_names_a_stale_lc_serve_that_declares_it_is_never_restarted() {
    let (w, old_bin) = world_with_an_lc_serve_left_on_the_previous_release();
    let text = format!("{LC_UNIT}X-Spira-NoRestart=the operator's panes\n");
    for name in [A.to_string(), "p".repeat(40)] {
        file(&w.cfg.releases.join(name).join("systemd/lc-serve.service"), &text);
    }
    let rel = w.cfg.releases.join(A);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    file(&w.cfg.unit_dir.join("lc-serve.service"), &text.replace("@SPIRA_PROD_ROOT@", &rel.display().to_string()));
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    lc_serve_running(&sc, &old_bin);
    let out = activate_with(&w, &sc).unwrap();
    assert!(sc.restarts.borrow().is_empty());
    assert!(out.stale.iter().any(|s| s.starts_with("lc-serve.service") && s.contains("spira-lc")), "{:?}", out.stale);
}

#[test_id(t4j3dzl)]
#[test]
fn an_activation_leaves_a_service_alone_whose_old_release_binary_has_the_same_bytes() {
    let (w, old_bin) = world_with_an_lc_serve_left_on_the_previous_release();
    exe(&old_bin, "spira-lc v2 with holds");
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    lc_serve_running(&sc, &old_bin);
    activate_with(&w, &sc).unwrap();
    assert!(sc.restarts.borrow().is_empty(), "{:?}", sc.restarts.borrow());
}

#[test_id(te1wve6)]
#[test]
fn an_activation_reads_the_lifecycle_and_the_first_beads_content_through_the_new_release() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let reads = FakeReads::serving();
    activate_reading(&w, &sc, &reads).unwrap();
    assert_eq!(*reads.asked.borrow(), vec!["list --state READY".to_string(), "content show sp-first".to_string(), "dwell".to_string(), "list --state READY".to_string()]);
}

#[test_id(t1r5ccu)]
#[test]
fn an_activation_waits_for_a_cold_store_server_to_answer_a_dwell_and_a_list_before_it_is_done() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let reads = FakeReads::serving();
    reads.cold_dwells.set(3);
    activate_reading(&w, &sc, &reads).unwrap();
    let asked = reads.asked.borrow();
    assert_eq!(asked.iter().filter(|a| a.starts_with("dwell")).count(), 4, "{asked:?}");
    assert_eq!(asked.last().map(String::as_str), Some("list --state READY"), "the list read follows the warm dwell: {asked:?}");
}

#[test_id(t8fsycc)]
#[test]
fn an_activation_whose_store_server_stays_cold_is_done_and_not_rolled_back() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let reads = FakeReads::serving();
    reads.cold_dwells.set(u32::MAX);
    activate_reading_within(&w, &sc, &reads, Duration::from_millis(500), Duration::from_millis(200)).unwrap();
}

fn store_restart_report(reads: &FakeReads, warm_wait: Duration) -> Vec<String> {
    let (w, old_bin) = world_with_an_lc_serve_left_on_the_previous_release();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_lands = Some(w.cfg.releases.join(A).join("bin/spira-lc"));
    lc_serve_running(&sc, &old_bin);
    let said = RefCell::new(Vec::new());
    activate_reporting(&w, &sc, reads, Duration::from_millis(500), warm_wait, &|l| said.borrow_mut().push(l.to_string())).unwrap();
    said.into_inner()
}

#[test_id(tee48hu)]
#[test]
fn an_activation_that_restarts_the_store_states_how_long_it_took_to_answer() {
    let reads = FakeReads::serving();
    reads.cold_dwells.set(2);
    let said = store_restart_report(&reads, Duration::from_secs(60));
    let ms: u128 = said.iter().find_map(|l| l.strip_prefix("release: the store answered ")?.split(' ').next()?.parse().ok()).unwrap_or_else(|| panic!("no store answered line: {said:?}"));
    assert!(ms >= 10, "two refused asks 5 ms apart: {ms} ms");
    assert!(said.iter().any(|l| l.ends_with("(bound 60 s)")), "{said:?}");
}

#[test_id(tlietp8)]
#[test]
fn an_activation_whose_restarted_store_never_answers_warns_and_still_is_not_rolled_back() {
    let reads = FakeReads::serving();
    reads.cold_dwells.set(u32::MAX);
    let said = store_restart_report(&reads, Duration::from_millis(200));
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("release: WARN: the store did not answer inside ") && said[0].ends_with(" s of its restart"), "{said:?}");
}

#[test_id(tf6wqfl)]
#[test]
fn an_activation_that_leaves_the_store_running_states_nothing_about_it() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let said = RefCell::new(Vec::new());
    activate_reporting(&w, &sc, &FakeReads::serving(), Duration::from_secs(60), Duration::from_secs(60), &|l| said.borrow_mut().push(l.to_string())).unwrap();
    assert!(said.into_inner().is_empty());
}

#[test_id(t6va949)]
#[test]
fn a_release_whose_content_read_is_refused_is_rolled_back_and_the_error_names_the_read() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let reads = FakeReads { refuses: Some("content show"), ..FakeReads::serving() };
    let e = activate_reading(&w, &sc, &reads).unwrap_err();
    assert!(e.contains("content read refused") && e.contains("missing events.issue_id") && e.contains("rolled back"), "{e}");
    let state = w.cfg.state_dir().unwrap();
    assert_eq!(crate::machine::load(&state, A).unwrap().unwrap().state().map(|s| s.as_str()), Some("rolled_back"));
    assert_eq!(fs::read_to_string(w.cfg.unit_dir.join("spira-tool-prod.service")).unwrap(), OLD_UNIT, "the unit file is put back");
    assert!(!w.cfg.releases.join("current").exists(), "current is put back");
}

/// Release `A` registering only `heartbeat`, over a host whose `spira.task_modes` also names
/// `repo-events`; `delta` is the release's config delta.
fn world_dropping_a_task(delta: &str) -> World {
    let w = world_activating_a_changed_unit();
    let rel = w.cfg.releases.join(A);
    file(
        &rel.join("spira/conf.d/SPIRA_TASK_MODES"),
        "TYPE=string\nDOC=modes\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_TASK_MODES:=heartbeat=act}\"\nSPIRA_CONF_DEFAULT_EOF\n",
    );
    exe(&rel.join("bin/spira-config"), "#!/bin/sh\nexit 0\n");
    file(&rel.join("spira/config-delta.toml"), delta);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    file(Path::new(&w.cfg.toml_spec().unwrap()), "[spira]\nid_prefix = \"sp\"\ntask_modes = \"heartbeat=act repo-events=act\"\n");
    w
}

#[test_id(t1h5qc3)]
#[test]
fn a_release_that_drops_a_task_the_host_task_modes_names_activates_with_the_entry_gone() {
    let w = world_dropping_a_task("removed_tasks = [\"repo-events\"]\n");
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    activate_with(&w, &sc).unwrap();
    let cfg = fs::read_to_string(w.cfg.toml_spec().unwrap()).unwrap();
    assert!(cfg.contains("task_modes = \"heartbeat=act\"") && !cfg.contains("repo-events"), "{cfg}");
    assert_eq!(sc.state("spira-tool-prod.service").unwrap().active, "active");
}

#[test_id(tfb92mu)]
#[test]
fn a_release_that_drops_a_task_without_declaring_it_removed_is_refused_naming_the_entry() {
    let w = world_dropping_a_task("");
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("repo-events") && e.contains("removed_tasks"), "{e}");
    assert!(fs::read_to_string(w.cfg.toml_spec().unwrap()).unwrap().contains("repo-events=act"), "the host value is untouched");
    assert!(!w.cfg.releases.join("current").exists(), "current did not move");
}

#[test_id(t7lgjr4)]
#[test]
fn a_release_whose_restarted_scheduler_comes_up_then_exits_is_rolled_back_onto_the_previous_one() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.exits_after.borrow_mut().insert("spira-tool-prod.service".into(), 2);
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("spira-tool-prod.service") && e.contains("did not stay up") && e.contains("rolled back"), "{e}");
    assert_eq!(*sc.restarts.borrow(), vec!["spira-tool-prod.service".to_string(); 2], "restarted again on the previous release");
    assert_eq!(fs::read_to_string(w.cfg.unit_dir.join("spira-tool-prod.service")).unwrap(), OLD_UNIT);
    assert!(!w.cfg.releases.join("current").exists(), "current is put back");
    sc.exits_after.borrow_mut().clear();
    assert_eq!(sc.state("spira-tool-prod.service").unwrap().active, "active", "the previous release's unit is running");
}

#[test_id(tiul5yx)]
#[test]
fn a_release_whose_lifecycle_read_is_refused_is_rolled_back() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let reads = FakeReads { refuses: Some("list"), ..FakeReads::serving() };
    let e = activate_reading(&w, &sc, &reads).unwrap_err();
    assert!(e.contains("lifecycle read refused") && e.contains("rolled back"), "{e}");
}

#[test_id(tdfysfz)]
#[test]
fn the_real_reads_run_the_releases_own_spira_lc_and_carry_a_refusal_in_its_words() {
    let w = World::with_git(FakeGit::default());
    let rel = w.cfg.releases.join(A);
    exe(&rel.join("bin/spira-lc"), "#!/bin/sh\ncase \"$1\" in list) echo '[{\"bead_id\":\"sp-q\"}]';; *) echo 'spira-lc content: cannot tell: layout' >&2; exit 2;; esac\n");
    let reads = activate::RealReads(&w.cfg);
    let listed = activate::Reads::lc(&reads, &rel, &["list", "--state", "READY"]).unwrap();
    assert!(listed.contains("sp-q"), "{listed}");
    let e = activate::Reads::lc(&reads, &rel, &["content", "show", "sp-q"]).unwrap_err();
    assert!(e.contains("content show sp-q") && e.contains("cannot tell: layout"), "{e}");
    let missing = activate::Reads::lc(&reads, &w.cfg.releases.join(B), &["list"]).unwrap_err();
    assert!(!missing.is_empty());
}

fn lc_serve_running_old_binary_under(installed: &str, shipped: &str) -> (activate::Switched, Vec<String>) {
    let (w, old_bin) = world_with_an_lc_serve_left_on_the_previous_release();
    file(&w.cfg.releases.join(A).join("systemd/lc-serve.service"), shipped);
    let rel = w.cfg.releases.join(A);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    file(&w.cfg.unit_dir.join("lc-serve.service"), &installed.replace("@SPIRA_PROD_ROOT@", &rel.display().to_string()));
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_lands = Some(rel.join("bin/spira-lc"));
    lc_serve_running(&sc, &old_bin);
    let out = activate_with(&w, &sc).unwrap();
    let restarts = sc.restarts.borrow().clone();
    (out, restarts)
}

#[test_id(tomyrjb)]
#[test]
fn an_activation_restarts_a_watcher_whose_running_release_predates_the_idle_restart_key() {
    let with_key = format!("{LC_UNIT}X-Spira-RestartWhenIdle=ends its own pass\n");
    let (out, restarts) = lc_serve_running_old_binary_under(LC_UNIT, &with_key);
    assert_eq!(restarts, vec!["lc-serve.service".to_string()]);
    assert!(out.restarted.contains(&"lc-serve.service".to_string()), "{:?}", out.restarted);
    assert!(out.when_idle.is_empty(), "{:?}", out.when_idle);
}

#[test_id(tb3bm85)]
#[test]
fn an_activation_leaves_a_watcher_to_restart_itself_when_its_installed_unit_already_carried_the_key() {
    let with_key = format!("{LC_UNIT}X-Spira-RestartWhenIdle=ends its own pass\n");
    let (out, restarts) = lc_serve_running_old_binary_under(&with_key, &with_key);
    assert!(restarts.is_empty(), "{restarts:?}");
    assert!(out.restarted.is_empty() && out.stale.iter().chain(&out.when_idle).any(|s| s.starts_with("lc-serve.service")), "{out:?}");
}

#[test_id(t8g5kjr)]
#[test]
fn an_activation_restarts_a_watcher_whose_installed_idle_restart_reason_differs_from_the_shipped_one() {
    let old = format!("{LC_UNIT}X-Spira-RestartWhenIdle=ends its own pass\n");
    let new = format!("{LC_UNIT}X-Spira-RestartWhenIdle=ends its own pass once its rows are stored\n");
    let (out, restarts) = lc_serve_running_old_binary_under(&old, &new);
    assert_eq!(restarts, vec!["lc-serve.service".to_string()]);
    assert!(out.restarted.contains(&"lc-serve.service".to_string()) && out.when_idle.is_empty(), "{out:?}");
}

/// `current` on the previous release `P`, whose installed `lc-serve` unit names it; release `A`
/// ships `spira-lc` with `new_bytes`, `P` with the bytes `spira-lc v1`.
fn world_switching_lc_serve_from_the_previous_release(new_bytes: &str) -> World {
    let w = World::with_git(FakeGit::default());
    let prev = "p".repeat(40);
    for (name, bin) in [(prev.as_str(), "spira-lc v1"), (A, new_bytes)] {
        file(&w.cfg.releases.join(name).join("systemd/lc-serve.service"), LC_UNIT);
        exe(&w.cfg.releases.join(name).join("bin/spira-lc"), bin);
    }
    let rel = w.cfg.releases.join(A);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    std::os::unix::fs::symlink(&prev, w.cfg.releases.join("current")).unwrap();
    file(&w.cfg.unit_dir.join("lc-serve.service"), &LC_UNIT.replace("@SPIRA_PROD_ROOT@", &w.cfg.releases.join(&prev).display().to_string()));
    w
}

fn running_lc_serve(w: &World) -> FakeSystemctl {
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.states.borrow_mut().insert("lc-serve.service".into(), UnitState { active: "active".into(), result: "success".into(), kind: "simple".into() });
    sc
}

fn recorded_restarts(w: &World) -> Vec<String> {
    let r = crate::machine::load(&w.cfg.state_dir().unwrap(), A).unwrap().unwrap();
    let e = r.events.iter().rev().find(|e| e.to == crate::machine::ReleaseState::Activated).unwrap();
    e.restarted.clone()
}

#[test_id(tp8z6ba)]
#[test]
fn an_activation_that_changes_spira_lc_records_that_it_restarted_lc_serve() {
    let w = world_switching_lc_serve_from_the_previous_release("spira-lc v2 with holds");
    let sc = running_lc_serve(&w);
    activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.restarts.borrow(), vec!["lc-serve.service".to_string()]);
    assert_eq!(recorded_restarts(&w), vec!["lc-serve.service".to_string()]);
}

#[test_id(ttfr7i4)]
#[test]
fn an_activation_that_leaves_spira_lc_identical_records_no_lc_serve_restart() {
    let w = world_switching_lc_serve_from_the_previous_release("spira-lc v1");
    let sc = running_lc_serve(&w);
    activate_with(&w, &sc).unwrap();
    assert!(sc.restarts.borrow().is_empty());
    assert!(!recorded_restarts(&w).contains(&"lc-serve.service".to_string()));
}

const DRILL_UNIT: &str = "[Unit]\nX-Spira-StartWhenChanged=spira/lc-backup.sh state-store-sqlite/src/durability/\n[Service]\nType=oneshot\nExecStart=@SPIRA_PROD@/lc-backup.sh drill\n";

/// Release `A` over a previous release `P` (current) when `with_previous`, each carrying the
/// listed paths at the content `(script, durability)` gives it, and an unlisted file.
fn world_activating_a_drill_unit(with_previous: bool, previous: (&str, &str), incoming: (&str, &str), unlisted_changes: bool) -> World {
    let w = World::with_git_and_reg(FakeGit::default(), Registered::default());
    let prev = "p".repeat(40);
    for (name, (script, dur), other) in [(prev.as_str(), previous, "v1"), (A, incoming, if unlisted_changes { "v2" } else { "v1" })] {
        let rel = w.cfg.releases.join(name);
        file(&rel.join("systemd/lc-backup-drill.service"), DRILL_UNIT);
        file(&rel.join("spira/lc-backup.sh"), script);
        file(&rel.join("state-store-sqlite/src/durability/ship.rs"), dur);
        file(&rel.join("state-store-sqlite/src/lib.rs"), other);
    }
    let rel = w.cfg.releases.join(A);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    let old_root = w.cfg.releases.join(&prev);
    if with_previous {
        std::os::unix::fs::symlink(&prev, w.cfg.releases.join("current")).unwrap();
    }
    let installed = DRILL_UNIT.replace("@SPIRA_PROD@", &format!("{}/spira", old_root.display()));
    file(&w.cfg.unit_dir.join("lc-backup-drill.service"), &installed);
    w
}

const SAME: (&str, &str) = ("#!/bin/sh\nrun v1\n", "ship v1\n");

#[test_id(tcre2ic)]
#[test]
fn an_activation_that_changes_the_backup_script_starts_the_drill_once_after_restarts_settle() {
    let w = world_activating_a_drill_unit(true, SAME, ("#!/bin/sh\nrun v2\n", SAME.1), false);
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let out = activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.starts.borrow(), vec!["lc-backup-drill.service".to_string()]);
    assert_eq!(out.started.len(), 1, "{:?}", out.started);
    assert!(out.started[0].contains("spira/lc-backup.sh changed"), "{:?}", out.started);
    let order = sc.order.borrow();
    let last_restart = order.iter().rposition(|c| c.starts_with("restart "));
    let start = order.iter().position(|c| c.starts_with("start ")).unwrap();
    assert!(last_restart.is_none_or(|r| r < start), "{order:?}");
}

#[test_id(txo9acr)]
#[test]
fn a_change_only_under_a_listed_directory_starts_the_drill() {
    let w = world_activating_a_drill_unit(true, SAME, (SAME.0, "ship v2\n"), false);
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.starts.borrow(), vec!["lc-backup-drill.service".to_string()]);
}

#[test_id(td6fubo)]
#[test]
fn a_change_only_in_an_unlisted_file_starts_nothing() {
    let w = world_activating_a_drill_unit(true, SAME, SAME, true);
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let out = activate_with(&w, &sc).unwrap();
    assert!(sc.starts.borrow().is_empty() && out.started.is_empty(), "{:?}", sc.starts.borrow());
}

#[test_id(ts8ysbn)]
#[test]
fn an_activation_with_no_previous_release_starts_the_drill() {
    let w = world_activating_a_drill_unit(false, SAME, SAME, false);
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.starts.borrow(), vec!["lc-backup-drill.service".to_string()]);
}

#[test_id(t7osnkj)]
#[test]
fn a_drill_that_cannot_be_started_is_reported_and_the_activation_stands() {
    let w = world_activating_a_drill_unit(true, SAME, ("#!/bin/sh\nrun v2\n", SAME.1), false);
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.start_fails.insert("lc-backup-drill.service".into());
    let out = activate_with(&w, &sc).unwrap();
    assert!(out.started.is_empty());
    assert_eq!(sc.starts.borrow().len(), 1);
    assert_eq!(activate::current(&w.cfg).as_deref(), Some(A), "the release stays active");
}

#[test_id(t76q7h5)]
#[test]
fn start_when_changed_lists_the_paths_of_the_key_and_nothing_else() {
    assert_eq!(units::start_when_changed(DRILL_UNIT), ["spira/lc-backup.sh", "state-store-sqlite/src/durability/"]);
    assert!(units::start_when_changed("[Unit]\nX-Spira-NoDrain=true\n").is_empty());
}

#[test_id(t8dtw9i)]
#[test]
fn the_drill_unit_runs_after_a_fresh_backup_and_names_live_trigger_paths() {
    let root = spira_config::resolve::checkout_root_for_process().unwrap();
    let text = fs::read_to_string(root.join("systemd/lc-backup-drill.service")).unwrap();
    let key = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)).unwrap_or_default().to_string();
    assert!(key("Wants=").split_whitespace().any(|u| u == "lc-backup.service"), "{text}");
    assert!(key("After=").split_whitespace().any(|u| u == "lc-backup.service"), "{text}");
    let paths = units::start_when_changed(&text);
    assert!(!paths.is_empty(), "the key is absent");
    for p in paths {
        assert!(root.join(&p).exists(), "{p} does not exist in the tree");
    }
}

#[test_id(t1h85k8)]
#[test]
fn the_trees_delta_loads_and_replaces_the_backup_age_and_database_defaults() {
    let spira = spira_config::resolve::checkout_root_for_process().unwrap().join("spira");
    let text = spira_config::delta::delta_text(&spira).unwrap().expect("the tree ships a delta");
    let delta = crate::config_delta::Delta::parse(&text).unwrap();
    assert_eq!(delta.replaced.get("spira.lc_backup_max_age").and_then(|v| v.as_integer()), Some(50_400));
    assert_eq!(delta.replaced.get("spira.lc_backup_db").and_then(|v| v.as_str()), Some("spira"));
}

#[test_id(tqqpp8f)]
#[test]
fn the_trees_delta_gives_the_runtime_overrides_backstop_the_registry_default() {
    let root = spira_config::resolve::checkout_root_for_process().unwrap();
    let text = spira_config::delta::delta_text(&root.join("spira")).unwrap().expect("the tree ships a delta");
    let delta = crate::config_delta::Delta::parse(&text).unwrap();
    let reg = spira_config::registry::Registry::load(&root.join("spira")).unwrap();
    let default = reg.by_field("runtime_overrides_secs").unwrap().default_expr().unwrap().trim().parse::<i64>().unwrap();
    assert_eq!(delta.added["spira.runtime_overrides_secs"].as_integer(), Some(default));
}

#[test_id(t9c70ma)]
#[test]
fn activation_writes_the_runtime_overrides_backstop_only_where_the_config_lacks_it() {
    let root = spira_config::resolve::checkout_root_for_process().unwrap();
    let text = spira_config::delta::delta_text(&root.join("spira")).unwrap().unwrap();
    let delta = crate::config_delta::Delta::parse(&text).unwrap();
    let run = |live: &str| {
        let live: toml::Table = live.parse().unwrap();
        config_delta::apply(std::slice::from_ref(&live), &delta, &[], true).unwrap().1[0]["spira"]["runtime_overrides_secs"].as_integer()
    };
    assert_eq!(run("[spira]\nruntime_overrides_secs = 60\n"), Some(60));
    assert_eq!(run("[spira]\n"), Some(3600));
}

#[test_id(t26g6e6)]
#[test]
fn a_release_ships_the_installed_units_its_templates_make_and_no_other() {
    let w = World::with_git(FakeGit::default());
    let rel = w.cfg.releases.join(A);
    file(&rel.join("systemd/spira-runtime.service"), SVC);
    file(&rel.join("systemd/shared.service"), SHARED);
    assert!(units::is_shipped_by(&rel, "prod", "spira-runtime-prod.service"));
    assert!(units::is_shipped_by(&rel, "prod", "shared.service"));
    assert!(!units::is_shipped_by(&rel, "prod", "spira-run-sentinel_2dpass-landing.service"));
    assert!(!units::is_shipped_by(&rel, "other", "spira-runtime-prod.service"));
}

fn verify_symbol_problems(lc: &[u8]) -> Vec<String> {
    let w = World::with_git(FakeGit::default());
    let rel = w.cfg.releases.join(A);
    fs::create_dir_all(rel.join("bin")).unwrap();
    fs::write(rel.join("bin/spira-lc"), lc).unwrap();
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    let o = verify::VerifyOpts { pre_activate: false, system_dirs: vec![], nix: &FakeNix::default() };
    verify::verify(&w.cfg, A, &o).unwrap().into_iter().filter(|p| p.starts_with("bin/spira-lc")).collect()
}

#[test_id(t4nqf84)]
#[test]
fn verify_refuses_a_spira_lc_whose_function_names_are_stripped() {
    let p = verify_symbol_problems(&crate::symbols::tests::elf_image(&[], false));
    assert!(p.iter().any(|p| p.contains("bin/spira-lc")), "{p:?}");
    let p = verify_symbol_problems(&crate::symbols::tests::elf_image(&["sqlite3_step"], false));
    assert!(p.iter().any(|p| p.contains("spira_lc::")), "{p:?}");
}

#[test_id(tnskqvr)]
#[test]
fn verify_accepts_a_spira_lc_that_keeps_its_function_names() {
    let p = verify_symbol_problems(&crate::symbols::tests::elf_image(&["_RNCNvCsiVYX3EDehpB_8spira_lc4mains0_0B3_", "sqlite3_step"], false));
    assert_eq!(p, Vec::<String>::new());
}

// ---------------------------------------------------------------- a failed unit's own last line

fn world_activating_a_logging_unit(log: &Path) -> World {
    let w = world_activating_a_changed_unit();
    let text = format!("{NEW_UNIT}StandardError=append:{}\n", log.display());
    file(&w.cfg.releases.join(A).join("systemd/spira-tool.service"), &text);
    let entries = Manifest::scan(&w.cfg.releases.join(A)).unwrap();
    let m = Manifest { commit: A.into(), built: None, repo: None, entries };
    fs::write(w.cfg.releases.join(A).join("MANIFEST"), m.render()).unwrap();
    w
}

fn append_to(log: PathBuf, line: &'static str) -> Box<dyn Fn() -> String> {
    Box::new(move || {
        use std::io::Write;
        fs::OpenOptions::new().create(true).append(true).open(&log).unwrap().write_all(line.as_bytes()).unwrap();
        String::new()
    })
}

#[test_id(t9oepx2)]
#[test]
fn a_unit_whose_restart_fails_is_named_with_the_last_line_it_wrote_to_its_own_log() {
    let d = TempDir::new("own-log");
    let log = d.path().join("u.log");
    fs::write(&log, "written before the restart\n").unwrap();
    let w = world_activating_a_logging_unit(&log);
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    sc.on_restart = Some(append_to(log.clone(), "boot: first\nboot: store not answering\n"));
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("simulated restart failure — its own last line: \"boot: store not answering\""), "{e}");
    assert!(!e.contains("written before the restart") && !e.contains("boot: first"), "{e}");
}

#[test_id(tzxnylj)]
#[test]
fn a_unit_that_did_not_come_up_is_named_with_the_last_line_it_wrote_to_its_own_log() {
    let d = TempDir::new("own-log");
    let log = d.path().join("u.log");
    let w = world_activating_a_logging_unit(&log);
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.slow_starts.borrow_mut().insert("spira-tool-prod.service".into(), usize::MAX);
    sc.on_restart = Some(append_to(log, "boot: still waiting\n"));
    let e = activate_reading_within(&w, &sc, &FakeReads::serving(), Duration::from_millis(50), Duration::from_millis(50)).unwrap_err();
    assert!(e.contains("did not come up") && e.contains("its own last line: \"boot: still waiting\""), "{e}");
}

#[test_id(t6yst8l)]
#[test]
fn a_unit_that_did_not_stay_up_is_named_with_the_last_line_it_wrote_to_its_own_log() {
    let d = TempDir::new("own-log");
    let log = d.path().join("u.log");
    let w = world_activating_a_logging_unit(&log);
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.on_restart = Some(append_to(log, "died: out of memory\n"));
    sc.exits_after.borrow_mut().insert("spira-tool-prod.service".into(), 2);
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("did not stay up") && e.contains("its own last line: \"died: out of memory\""), "{e}");
}

#[test_id(t1tblnu)]
#[test]
fn a_failed_unit_whose_log_got_nothing_after_its_restart_says_so() {
    let d = TempDir::new("own-log");
    let log = d.path().join("u.log");
    fs::write(&log, "old line\n").unwrap();
    let w = world_activating_a_logging_unit(&log);
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains(&format!("its own log {} got no line after its restart", log.display())), "{e}");
    assert!(!e.contains("old line"), "{e}");
}

#[test_id(taggpux)]
#[test]
fn a_failed_unit_with_no_append_log_reports_the_line_it_always_did() {
    let w = world_activating_a_changed_unit();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("simulated restart failure") && !e.contains("its own"), "{e}");
}

#[test_id(t0yav2u)]
#[test]
fn an_over_long_last_line_is_cut_to_300_characters() {
    let d = TempDir::new("own-log");
    let log = d.path().join("u.log");
    let w = world_activating_a_logging_unit(&log);
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    let long = log.clone();
    sc.on_restart = Some(Box::new(move || {
        fs::write(&long, format!("{}\n", "x".repeat(900))).unwrap();
        String::new()
    }));
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains(&format!("last line: \"{}\"", "x".repeat(300))) && !e.contains(&"x".repeat(301)), "{e}");
}

#[test_id(t0ph2e0)]
#[test]
fn a_log_truncated_after_the_mark_is_read_from_its_start() {
    let d = TempDir::new("own-log");
    let log = d.path().join("u.log");
    fs::write(&log, "old line that is longer than what the restart writes\n".repeat(4)).unwrap();
    let w = world_activating_a_logging_unit(&log);
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    let rotated = log.clone();
    sc.on_restart = Some(Box::new(move || {
        fs::write(&rotated, "fresh\n").unwrap();
        String::new()
    }));
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("its own last line: \"fresh\""), "{e}");
}

#[test_id(thc4r22)]
#[test]
fn a_unit_with_only_a_standard_output_append_log_is_named_with_its_last_line() {
    let d = TempDir::new("own-log");
    let log = d.path().join("u.log");
    let w = world_activating_a_logging_unit(&log);
    let text = format!("{NEW_UNIT}StandardOutput=append:{}\n", log.display());
    file(&w.cfg.releases.join(A).join("systemd/spira-tool.service"), &text);
    let entries = Manifest::scan(&w.cfg.releases.join(A)).unwrap();
    fs::write(w.cfg.releases.join(A).join("MANIFEST"), Manifest { commit: A.into(), built: None, repo: None, entries }.render()).unwrap();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    sc.on_restart = Some(append_to(log, "out: refused\n"));
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("its own last line: \"out: refused\""), "{e}");
}

/// Release `A` shipping new templates for `a-early`, `lc-serve` and `spira-runtime`, each over an old installed copy.
fn world_activating_the_store_server_among_others() -> World {
    let w = World::with_git(FakeGit::default());
    let rel = w.cfg.releases.join(A);
    for n in ["a-early.service", "lc-serve.service", "spira-runtime.service"] {
        file(&rel.join("systemd").join(n), NEW_UNIT);
    }
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    for n in ["a-early.service", "lc-serve.service", "spira-runtime-prod.service"] {
        file(&w.cfg.unit_dir.join(n), OLD_UNIT);
    }
    w
}

fn run_the_three(sc: &FakeSystemctl) {
    for n in ["a-early.service", "lc-serve.service", "spira-runtime-prod.service"] {
        sc.states.borrow_mut().insert(n.into(), UnitState { active: "active".into(), result: "success".into(), kind: "simple".into() });
    }
}

#[test_id(td9sikc)]
#[test]
fn an_activation_restarts_the_store_server_before_every_other_unit() {
    let w = world_activating_the_store_server_among_others();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    run_the_three(&sc);
    activate_with(&w, &sc).unwrap();
    let restarts = sc.restarts.borrow().clone();
    assert_eq!(restarts.first().map(String::as_str), Some("lc-serve.service"), "{restarts:?}");
    assert!(restarts.contains(&"a-early.service".to_string()) && restarts.contains(&"spira-runtime-prod.service".to_string()), "{restarts:?}");
}

#[test_id(tqr8s7c)]
#[test]
fn a_store_server_restart_that_fails_names_its_start_bound_and_is_rolled_back() {
    let w = world_activating_the_store_server_among_others();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    run_the_three(&sc);
    sc.restart_fails.insert("lc-serve.service".into());
    sc.start_budgets.insert("lc-serve.service".into(), std::time::Duration::from_secs(600));
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("TimeoutStartSec=600s"), "{e}");
    assert!(e.contains("rolled back"), "{e}");
}

#[test_id(tvz55xh)]
#[test]
fn a_store_server_restart_that_fails_with_no_known_bound_gives_the_bare_error() {
    let w = world_activating_the_store_server_among_others();
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    run_the_three(&sc);
    sc.restart_fails.insert("lc-serve.service".into());
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("lc-serve.service: simulated restart failure") && !e.contains("TimeoutStartSec"), "{e}");
}

#[test_id(t0pih7i)]
#[test]
fn the_shipped_store_server_unit_is_notify_with_a_finite_start_bound() {
    let t = shipped_unit("lc-serve.service");
    assert!(t.lines().any(|l| l == "Type=notify") && t.lines().any(|l| l == "NotifyAccess=main"), "{t}");
    let bound = t.lines().find_map(|l| l.strip_prefix("TimeoutStartSec=")).expect("TimeoutStartSec");
    assert!(bound.parse::<u64>().is_ok_and(|n| n > 0), "{bound}");
}

// ---------------------------------------------------------------- verify: runtime dependencies from files

fn dep_row(name: &str, version_min: &str) -> String {
    format!("[[dep]]\nname = \"{name}\"\ntier = \"runtime\"\nversion_min = \"{version_min}\"\nversion_probe = \"{name} --version\"\n")
}

fn deps_problems(rel: &Path, path: &str) -> Vec<String> {
    crate::deps_check::problems(rel, path, crate::deps_check::SpiraEnv::Installed)
}

fn stub(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    testkit::write_exe(path, body);
}

#[test_id(tbmnuzu)]
#[test]
fn verify_names_a_runtime_program_missing_from_the_release_path_as_a_deps_problem() {
    let w = World::with_git(FakeGit::default());
    let rel = w.cfg.releases.join(A);
    file(&rel.join("spira/deps.toml"), &dep_row("spira-no-such-runtime-program", ""));
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    let o = verify::VerifyOpts { pre_activate: true, system_dirs: vec![], nix: &FakeNix::default() };
    let p = verify::verify(&w.cfg, A, &o).unwrap();
    assert!(p.iter().any(|p| p.starts_with("deps: spira-no-such-runtime-program:")), "{p:?}");
    stub(&rel.join("bin/spira-no-such-runtime-program"), "#!/bin/sh\n");
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    let o = verify::VerifyOpts { pre_activate: true, system_dirs: vec![], nix: &FakeNix::default() };
    let p = verify::verify(&w.cfg, A, &o).unwrap();
    assert!(!p.iter().any(|p| p.starts_with("deps:")), "a program on the release's own bin resolves: {p:?}");
    let o = verify::VerifyOpts { pre_activate: false, system_dirs: vec![], nix: &FakeNix::default() };
    let p = verify::verify(&w.cfg, A, &o).unwrap();
    assert!(!p.iter().any(|p| p.starts_with("deps:")), "--no-pre-activate skips the check: {p:?}");
}

const ENV_PROG: &str = "spira-no-such-env-program";

/// Verifies release A, a flake release whose `spira-env` row names [`ENV_PROG`], plus any
/// `more_rows`, with `nix`, over an installed env on verify's PATH that ships `installed`. Asserts that verify
/// left the installed env alone, and returns its `deps:` problems.
fn verify_a_flake_release(nix: FakeNix, installed: &[&str], more_rows: &str) -> Vec<String> {
    let w = World::with_git(FakeGit::default());
    let held = installed_env(&w, "installed-spira-env");
    for prog in installed {
        stub(&held.join("bin").join(prog), "#!/bin/sh\n");
    }
    let linked = fs::read_link(env_link(&w)).unwrap();
    let rel = w.cfg.releases.join(A);
    file(&rel.join("spira/deps.toml"), &format!("{}provider = \"spira-env\"\n{more_rows}", dep_row(ENV_PROG, "")));
    release_gains(&w, A, &FLAKE);
    let nix = FakeNix { store: w._sb.p().join("store"), ..nix };

    let o = verify::VerifyOpts { pre_activate: true, system_dirs: vec![], nix: &nix };
    let p = verify::verify(&w.cfg, A, &o).unwrap();

    assert_eq!(fs::read_link(env_link(&w)).unwrap(), linked, "the installed env link moved");
    assert!(!home_of(&w).join(spira_config::SPIRA_ENV_PREV_REL).exists());
    let log = nix.log.borrow();
    let [NixCall::Build { target, out, .. }] = &log[..] else { panic!("want one build and no root: {log:?}") };
    assert_eq!(target, &spira_env::target(&rel));
    assert_ne!(out, &env_link(&w));
    assert!(fs::symlink_metadata(out).is_err() && !out.parent().unwrap().exists(), "the scratch link is left: {}", out.display());
    p.into_iter().filter(|p| p.starts_with("deps:")).collect()
}

#[test_id(t4962w5)]
#[test]
fn verify_finds_a_spira_env_program_only_the_releases_own_flake_ships() {
    let p = verify_a_flake_release(FakeNix { provides: vec![ENV_PROG], ..FakeNix::default() }, &[], "");
    assert_eq!(p, Vec::<String>::new());
}

#[test_id(ta4x16z)]
#[test]
fn verify_refuses_a_spira_env_program_the_releases_flake_drops_though_the_installed_env_has_it() {
    let p = verify_a_flake_release(FakeNix::default(), &[ENV_PROG], "");
    assert!(p.len() == 1 && p[0].starts_with(&format!("deps: {ENV_PROG}:")), "{p:?}");
}

#[test_id(tv1gevw)]
#[test]
fn verify_reports_a_spira_env_that_does_not_build_as_one_problem_in_nixs_words() {
    let p = verify_a_flake_release(FakeNix { fails: Some("attribute 'spira-env' missing"), ..FakeNix::default() }, &[], "");
    assert_eq!(p, vec!["deps: cannot build the release's spira-env: attribute 'spira-env' missing".to_string()]);
}

#[test_id(ts2irlh)]
#[test]
fn verify_judges_a_flake_releases_other_rows_on_its_path_whether_or_not_the_env_builds() {
    const HOST_PROG: &str = "spira-no-such-host-program";
    let host_row = dep_row(HOST_PROG, "");
    let built = FakeNix { provides: vec![ENV_PROG], ..FakeNix::default() };
    assert_eq!(verify_a_flake_release(built, &[HOST_PROG], &host_row), Vec::<String>::new());
    let broken = FakeNix { fails: Some("boom"), ..FakeNix::default() };
    let p = verify_a_flake_release(broken, &[], &host_row);
    assert!(p.len() == 2 && p[1].starts_with(&format!("deps: {HOST_PROG}: not on PATH")), "{p:?}");
}

#[test_id(tyq957n)]
#[test]
fn a_spira_env_program_below_version_min_in_the_built_env_points_at_the_flake() {
    let d = TempDir::new("deps-built-version");
    let bin = d.path().join("store/abc123-slowtool-1.4.2/bin");
    stub(&bin.join("slowtool"), "#!/bin/sh\n");
    let rel = d.path().join("rel");
    file(&rel.join("spira/deps.toml"), &format!("{}provider = \"spira-env\"\n", dep_row("slowtool", "1.10")));
    let p = crate::deps_check::problems(&rel, "", crate::deps_check::SpiraEnv::Built(&bin));
    assert!(p.len() == 1 && p[0].contains("1.4.2") && p[0].contains("flake.lock") && !p[0].contains("spira.path"), "{p:?}");
}

#[test_id(tx2oupg)]
#[test]
fn verify_without_pre_activate_calls_no_nix_for_a_flake_release() {
    let w = World::with_git(FakeGit::default());
    file(&w.cfg.releases.join(A).join("spira/deps.toml"), &dep_row(ENV_PROG, ""));
    release_gains(&w, A, &FLAKE);
    let nix = FakeNix::default();
    let o = verify::VerifyOpts { pre_activate: false, system_dirs: vec![], nix: &nix };
    let p = verify::verify(&w.cfg, A, &o).unwrap();
    assert!(!p.iter().any(|p| p.starts_with("deps:")), "{p:?}");
    assert_eq!(*nix.log.borrow(), Vec::<NixCall>::new());
}

#[test_id(tfmpvdw)]
#[test]
fn verify_finds_a_spira_env_program_in_the_env_bin_without_a_link_or_a_tail() {
    let w = World::with_git(FakeGit::default());
    let prog = "spira-no-such-env-program";
    let rel = w.cfg.releases.join(A);
    file(&rel.join("spira/deps.toml"), &format!("{}provider = \"spira-env\"\n", dep_row(prog, "")));
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    let home = w.cfg.home().unwrap();
    let in_env = PathBuf::from(spira_config::spira_env_bin(&home.display().to_string())).join(prog);
    stub(&in_env, "#!/bin/sh\n");
    let o = verify::VerifyOpts { pre_activate: true, system_dirs: vec![], nix: &FakeNix::default() };
    let p = verify::verify(&w.cfg, A, &o).unwrap();
    assert!(!p.iter().any(|p| p.starts_with("deps:")), "{p:?}");
    fs::remove_file(&in_env).unwrap();
    let p = verify::verify(&w.cfg, A, &o).unwrap();
    assert!(p.iter().any(|p| p.starts_with(&format!("deps: {prog}:"))), "{p:?}");
}

#[test_id(tqeytds)]
#[test]
fn the_shipped_pre_activate_script_starts_no_version_probe() {
    let script = spira_config::resolve::checkout_root_for_process().unwrap().join("spira/pre-activate.sh");
    let text = fs::read_to_string(&script).unwrap();
    assert!(text.contains("check_store"), "positive control: this is the script that runs");
    assert!(!text.contains("version_probe"), "{}", script.display());
}

#[test_id(tbhmdm1)]
#[test]
fn deps_problems_never_start_the_program_they_resolve() {
    let d = TempDir::new("deps-stub");
    let marker = d.path().join("ran");
    let bin = d.path().join("bin");
    stub(&bin.join("slowtool"), &format!("#!/bin/sh\nsleep 30\ntouch {}\n", marker.display()));
    let rel = d.path().join("rel");
    file(&rel.join("spira/deps.toml"), &dep_row("slowtool", ""));
    assert_eq!(deps_problems(&rel, &bin.display().to_string()), Vec::<String>::new());
    assert!(!marker.exists());
}

#[test_id(th3nb25)]
#[test]
fn deps_problems_judge_version_min_by_the_store_directory_name() {
    let d = TempDir::new("deps-store");
    let store = d.path().join("store/abc123-slowtool-1.4.2");
    stub(&store.join("bin/slowtool"), "#!/bin/sh\n");
    let bin = d.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(store.join("bin/slowtool"), bin.join("slowtool")).unwrap();
    let path = bin.display().to_string();
    let rel = d.path().join("rel");
    file(&rel.join("spira/deps.toml"), &dep_row("slowtool", "1.4"));
    assert_eq!(deps_problems(&rel, &path), Vec::<String>::new());
    file(&rel.join("spira/deps.toml"), &dep_row("slowtool", "1.10"));
    let p = deps_problems(&rel, &path);
    assert!(p.len() == 1 && p[0].starts_with("deps: slowtool:") && p[0].contains("1.4.2") && p[0].contains("spira.path"), "{p:?}");
    let plain = d.path().join("plain");
    stub(&plain.join("slowtool"), "#!/bin/sh\n");
    let p = deps_problems(&rel, &plain.display().to_string());
    assert!(p.len() == 1 && p[0].contains(&plain.join("slowtool").display().to_string()), "no recorded version names the path: {p:?}");
}

#[test_id(t74h76i)]
#[test]
fn deps_problems_refuse_a_dangling_link_and_an_unparseable_manifest() {
    let d = TempDir::new("deps-bad");
    let bin = d.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(d.path().join("gone"), bin.join("slowtool")).unwrap();
    let rel = d.path().join("rel");
    file(&rel.join("spira/deps.toml"), &dep_row("slowtool", ""));
    let p = deps_problems(&rel, &bin.display().to_string());
    assert!(p.len() == 1 && p[0].starts_with("deps: slowtool:"), "{p:?}");
    file(&rel.join("spira/deps.toml"), "[[dep\n");
    assert_eq!(deps_problems(&rel, &bin.display().to_string()).len(), 1);
    assert_eq!(deps_problems(&d.path().join("none"), "").len(), 1, "unreadable is a problem");
}

// ---------------------------------------------------------------- spira-env

/// One nix call, with how the installed unit and `current` stood when it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NixCall {
    Build { target: String, out: PathBuf, unit: String, current: Option<String> },
    AddRoot { root: PathBuf, store_path: PathBuf },
}

/// A nix that "builds" by linking `out` to a directory named for the target's release.
#[derive(Default)]
struct FakeNix {
    store: PathBuf,
    unit: PathBuf,
    current: PathBuf,
    fails: Option<&'static str>,
    root_fails: bool,
    /// Executables each built env ships in its `bin/`.
    provides: Vec<&'static str>,
    /// A failing root also leaves the env link impossible to repoint.
    restore_blocked: Option<PathBuf>,
    log: RefCell<Vec<NixCall>>,
}

impl FakeNix {
    fn watching(w: &World) -> FakeNix {
        FakeNix { store: w._sb.p().join("store"), unit: w.cfg.unit_dir.join("spira-tool-prod.service"), current: w.cfg.releases.join("current"), ..FakeNix::default() }
    }
    fn store_path(&self, release: &str) -> PathBuf {
        self.store.join(format!("{release}-spira-env"))
    }
}

impl spira_env::Nix for FakeNix {
    fn build(&self, target: &str, out: &Path) -> Result<(), String> {
        self.log.borrow_mut().push(NixCall::Build {
            target: target.into(),
            out: out.into(),
            unit: fs::read_to_string(&self.unit).unwrap_or_default(),
            current: fs::read_link(&self.current).ok().map(|t| t.display().to_string()),
        });
        if let Some(e) = self.fails {
            return Err(e.into());
        }
        let release = target.trim_end_matches("#spira-env").rsplit('/').next().unwrap();
        let dest = self.store_path(release);
        fs::create_dir_all(&dest).unwrap();
        for prog in &self.provides {
            stub(&dest.join("bin").join(prog), "#!/bin/sh\n");
        }
        fsutil::atomic_symlink(out, &dest.display().to_string())
    }
    fn add_root(&self, root: &Path, store_path: &Path) -> Result<(), String> {
        self.log.borrow_mut().push(NixCall::AddRoot { root: root.into(), store_path: store_path.into() });
        if let Some(link) = &self.restore_blocked {
            // A directory where the atomic swap writes its temporary link.
            fs::create_dir_all(link.with_file_name(format!(".spira-env.new.{}", std::process::id()))).unwrap();
        }
        if self.root_fails {
            return Err("no root".into());
        }
        Ok(())
    }
}

fn home_of(w: &World) -> PathBuf {
    w._sb.p().join("home")
}

fn env_link(w: &World) -> PathBuf {
    home_of(w).join(spira_config::SPIRA_ENV_REL)
}

/// Points the env link at `store/<name>`, as an earlier build left it, and returns that path.
fn installed_env(w: &World, name: &str) -> PathBuf {
    let dest = w._sb.p().join("store").join(name);
    fs::create_dir_all(&dest).unwrap();
    fs::create_dir_all(env_link(w).parent().unwrap()).unwrap();
    fsutil::atomic_symlink(&env_link(w), &dest.display().to_string()).unwrap();
    fs::canonicalize(&dest).unwrap()
}

/// Adds `files` to release `sha` and rewrites its MANIFEST to match.
fn release_gains(w: &World, sha: &str, files: &[&str]) {
    let rel = w.cfg.releases.join(sha);
    for f in files {
        file(&rel.join(f), "{}\n");
    }
    let _ = fs::remove_file(rel.join("MANIFEST"));
    let entries = Manifest::scan(&rel).unwrap();
    let m = Manifest { commit: sha.into(), built: None, repo: None, entries };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
}

const FLAKE: [&str; 2] = ["flake.nix", "flake.lock"];

fn world_activating_a_flake_release() -> World {
    let w = world_activating_a_changed_unit();
    release_gains(&w, A, &FLAKE);
    w
}

fn ctx_with_nix<'a>(w: &'a World, sc: &'a FakeSystemctl, nix: &'a FakeNix, reads: &'a FakeReads) -> activate::Ctx<'a> {
    activate::Ctx { cfg: &w.cfg, reads, sc, nix, git: &w._git, repo: None, landed_ref: "main".into(), unit_ensure: &activate::ensure_new_units, settle: Duration::from_millis(10), drain: Duration::from_millis(10), steady: Duration::from_secs(60), warm_wait: Duration::from_secs(60), warm_pause: Duration::from_millis(1), report: &|_| {}, gc: None, publisher: &no_broker }
}

fn activate_with_nix(w: &World, sc: &FakeSystemctl, nix: &FakeNix) -> Result<activate::Switched, String> {
    activate::activate(&ctx_with_nix(w, sc, nix, &FakeReads::serving()), A, None)
}

fn env_target_of(w: &World, sha: &str) -> String {
    format!("path:{}#spira-env", fs::canonicalize(w.cfg.releases.join(sha)).unwrap().display())
}

#[test_id(tkuvner)]
#[test]
fn activation_builds_the_releases_env_once_before_a_unit_is_rewritten_or_current_moves() {
    let w = world_activating_a_flake_release();
    let nix = FakeNix::watching(&w);
    activate_with_nix(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &nix).unwrap();
    let want = NixCall::Build { target: env_target_of(&w, A), out: env_link(&w), unit: OLD_UNIT.into(), current: None };
    assert_eq!(*nix.log.borrow(), vec![want]);
    assert_eq!(fs::read_link(env_link(&w)).unwrap(), nix.store_path(A));
    assert_eq!(current_of(&w), A);
}

#[test_id(tsmvun5)]
#[test]
fn a_failed_env_build_refuses_activation_with_nothing_changed() {
    let w = world_activating_a_flake_release();
    let before = installed_env(&w, "old-env");
    let nix = FakeNix { fails: Some("boom"), ..FakeNix::watching(&w) };
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let e = activate_with_nix(&w, &sc, &nix).unwrap_err();
    assert!(e.contains("cannot build spira-env") && e.contains(A) && e.contains("boom") && e.contains("nothing changed"), "{e}");
    assert!(!w.cfg.releases.join("current").exists());
    assert_eq!(fs::read_to_string(w.cfg.unit_dir.join("spira-tool-prod.service")).unwrap(), OLD_UNIT);
    assert!(activate::read_history(&w.cfg.state_dir().unwrap()).unwrap().is_empty());
    assert_eq!(fs::canonicalize(env_link(&w)).unwrap(), before);
    assert_eq!((*sc.reloads.borrow(), sc.restarts.borrow().len()), (0, 0));
}

#[test_id(tpd80yv)]
#[test]
fn an_activation_undone_at_daemon_reload_puts_the_env_back() {
    let w = world_activating_a_flake_release();
    let before = installed_env(&w, "old-env");
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.reload_fails_once.set(true);
    let e = activate_with_nix(&w, &sc, &FakeNix::watching(&w)).unwrap_err();
    assert!(e.contains("daemon-reload failed") && !e.contains("UNDO INCOMPLETE"), "{e}");
    assert_eq!(fs::canonicalize(env_link(&w)).unwrap(), before);
}

#[test_id(tj60x1v)]
#[test]
fn an_activation_undone_by_a_failed_restart_puts_the_env_back_before_restarting_on_the_old_release() {
    let w = world_activating_a_flake_release();
    let before = installed_env(&w, "old-env");
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    let link = env_link(&w);
    sc.on_restart = Some(Box::new(move || fs::canonicalize(&link).unwrap().display().to_string()));
    let nix = FakeNix::watching(&w);
    activate_with_nix(&w, &sc, &nix).unwrap_err();
    assert_eq!(sc.at_restart.borrow().last(), Some(&before.display().to_string()), "{:?}", sc.at_restart.borrow());
    assert_eq!(fs::canonicalize(env_link(&w)).unwrap(), before);
}

#[test_id(tnd1rv6)]
#[test]
fn a_replaced_env_is_rooted_and_an_unchanged_one_is_not() {
    let w = world_activating_a_flake_release();
    let before = installed_env(&w, "old-env");
    let nix = FakeNix::watching(&w);
    activate_with_nix(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &nix).unwrap();
    let root = NixCall::AddRoot { root: home_of(&w).join(".local/lib/spira-env.prev"), store_path: before };
    assert_eq!(nix.log.borrow()[1..], [root]);

    nix.log.borrow_mut().clear();
    activate_with_nix(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &nix).unwrap();
    assert!(matches!(nix.log.borrow()[..], [NixCall::Build { .. }]), "{:?}", nix.log.borrow());
}

#[test_id(tuvs3zl)]
#[test]
fn an_env_that_cannot_be_rooted_refuses_activation_and_stays_linked() {
    let w = world_activating_a_flake_release();
    let before = installed_env(&w, "old-env");
    let nix = FakeNix { root_fails: true, ..FakeNix::watching(&w) };
    let e = activate_with_nix(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &nix).unwrap_err();
    assert!(e.contains("cannot root the previous spira-env") && e.contains("no root") && e.contains("nothing changed"), "{e}");
    assert!(!w.cfg.releases.join("current").exists());
    assert_eq!(fs::canonicalize(env_link(&w)).unwrap(), before);
}

/// A scripted unit backend that records what `current` read as each unit started.
struct WatchingUnits {
    scripted: ScriptedUnits,
    current: PathBuf,
    current_at_start: std::sync::Mutex<Vec<Option<String>>>,
}

impl WatchingUnits {
    fn new(w: &World) -> WatchingUnits {
        let scripted = ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0)));
        scripted.script(spira_env::NIX_GC_UNIT_TASK, 1_000, runtime::units::Exit::Code(0));
        WatchingUnits { scripted, current: w.cfg.releases.join("current"), current_at_start: Default::default() }
    }
}

impl Units for WatchingUnits {
    fn start(&self, spec: &UnitSpec) -> runtime::Result<UnitHandle> {
        self.scripted.start(spec)
    }
    fn start_with(&self, spec: &UnitSpec, props: &UnitProps) -> runtime::Result<UnitHandle> {
        self.current_at_start.lock().unwrap().push(fs::read_link(&self.current).ok().map(|t| t.display().to_string()));
        self.scripted.start_with(spec, props)
    }
    fn stop(&self, h: &UnitHandle) -> runtime::Result<()> {
        self.scripted.stop(h)
    }
    fn restart(&self, h: &UnitHandle) -> runtime::Result<()> {
        self.scripted.restart(h)
    }
    fn facts(&self, h: &UnitHandle) -> runtime::Result<runtime::units::UnitFacts> {
        self.scripted.facts(h)
    }
}

/// `ctx_with_nix` with a collector over `units`, finding `nix-collect-garbage` in a fake profile.
fn ctx_collecting<'a>(w: &'a World, sc: &'a FakeSystemctl, nix: &'a FakeNix, reads: &'a FakeReads, units: &'a dyn Units) -> activate::Ctx<'a> {
    let profile_bin = w._sb.p().join("profile/bin");
    fs::create_dir_all(&profile_bin).unwrap();
    testkit::write_exe(profile_bin.join("nix-collect-garbage"), "#!/bin/sh\n");
    let gc = spira_env::Collector { units, profile_bin, run_dir: w._sb.p().join("run") };
    activate::Ctx { gc: Some(gc), ..ctx_with_nix(w, sc, nix, reads) }
}

fn activate_collecting(w: &World, sc: &FakeSystemctl, nix: &FakeNix, units: &dyn Units) -> Result<activate::Switched, String> {
    activate::activate(&ctx_collecting(w, sc, nix, &FakeReads::serving(), units), A, None)
}

fn gc_unit_for(sha: &str) -> String {
    UnitHandle::of(spira_env::NIX_GC_UNIT_TASK, sha).unwrap().name
}

#[test_id(tb1b72s)]
#[test]
fn an_activation_that_replaced_the_env_starts_one_idle_collection_once_current_has_moved() {
    let w = world_activating_a_flake_release();
    installed_env(&w, "old-env");
    let units = WatchingUnits::new(&w);
    activate_collecting(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &FakeNix::watching(&w), &units).unwrap();
    let name = gc_unit_for(A);
    assert_eq!(units.scripted.calls(), [format!("start {name}")]);
    assert_eq!(*units.current_at_start.lock().unwrap(), [Some(A.to_string())]);
    let p = units.scripted.props(&name);
    assert_eq!((p.cpu_quota_pct, p.nice, p.io_class.as_deref(), p.runtime_max_ms), (Some(20), Some(10), Some("idle"), Some(1_800_000)));
    assert_eq!(p.output_append, Some(w._sb.p().join("run/nix-gc.log")));
}

#[test_id(t31fcrs)]
#[test]
fn an_activation_that_left_the_env_as_it_was_starts_no_collection() {
    let w = world_activating_a_flake_release();
    let nix = FakeNix::watching(&w);
    let same = nix.store_path(A);
    installed_env(&w, same.file_name().unwrap().to_str().unwrap());
    let units = WatchingUnits::new(&w);
    activate_collecting(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &nix, &units).unwrap();
    assert_eq!(fs::read_link(env_link(&w)).unwrap(), same);
    assert!(units.scripted.calls().is_empty(), "{:?}", units.scripted.calls());
}

#[test_id(tarbjj5)]
#[test]
fn an_activation_undone_after_replacing_the_env_starts_no_collection() {
    let w = world_activating_a_flake_release();
    let before = installed_env(&w, "old-env");
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.reload_fails_once.set(true);
    let units = WatchingUnits::new(&w);
    activate_collecting(&w, &sc, &FakeNix::watching(&w), &units).unwrap_err();
    assert_eq!(fs::canonicalize(env_link(&w)).unwrap(), before);
    assert!(units.scripted.calls().is_empty(), "{:?}", units.scripted.calls());
}

#[test_id(t1awlgt)]
#[test]
fn an_activation_undone_by_a_failed_restart_starts_no_collection() {
    let w = world_activating_a_flake_release();
    let before = installed_env(&w, "old-env");
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.restart_fails.insert("spira-tool-prod.service".into());
    let units = WatchingUnits::new(&w);
    activate_collecting(&w, &sc, &FakeNix::watching(&w), &units).unwrap_err();
    assert_eq!(fs::canonicalize(env_link(&w)).unwrap(), before);
    assert!(units.scripted.calls().is_empty(), "{:?}", units.scripted.calls());
}

#[test_id(t665nic)]
#[test]
fn a_release_with_no_flake_starts_no_collection() {
    let w = world_activating_a_changed_unit();
    installed_env(&w, "old-env");
    let units = WatchingUnits::new(&w);
    activate_collecting(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &FakeNix::watching(&w), &units).unwrap();
    assert!(units.scripted.calls().is_empty(), "{:?}", units.scripted.calls());
}

#[test_id(tr57upk)]
#[test]
fn a_collection_that_cannot_start_leaves_the_activation_succeeding() {
    let w = world_activating_a_flake_release();
    installed_env(&w, "old-env");
    let units = WatchingUnits { scripted: ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0))), ..WatchingUnits::new(&w) };
    activate_collecting(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &FakeNix::watching(&w), &units).unwrap();
    assert_eq!(units.current_at_start.lock().unwrap().len(), 1, "the start was attempted");
    assert!(units.scripted.calls().is_empty());
    assert_eq!(current_of(&w), A);
}

#[test_id(tif9z6r)]
#[test]
fn a_rollback_that_replaced_the_env_collects_under_the_release_it_returns_to() {
    let w = world_rolling_back_to_a();
    release_gains(&w, A, &FLAKE);
    installed_env(&w, "old-env");
    let units = WatchingUnits::new(&w);
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let nix = FakeNix::watching(&w);
    activate::rollback(&ctx_collecting(&w, &sc, &nix, &FakeReads::serving(), &units)).unwrap();
    assert_eq!(units.scripted.calls(), [format!("start {}", gc_unit_for(A))]);
}

#[test_id(trczhfq)]
#[test]
fn rollback_builds_the_env_of_the_release_it_returns_to() {
    let w = world_rolling_back_to_a();
    release_gains(&w, A, &FLAKE);
    let nix = FakeNix::watching(&w);
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    activate::rollback(&ctx_with_nix(&w, &sc, &nix, &FakeReads::serving())).unwrap();
    let want = NixCall::Build { target: env_target_of(&w, A), out: env_link(&w), unit: OLD_UNIT.into(), current: Some(B.into()) };
    assert_eq!(*nix.log.borrow(), vec![want]);
}

#[test_id(trjozvx)]
#[test]
fn a_release_without_a_flake_lock_activates_without_calling_nix() {
    let w = world_activating_a_changed_unit();
    release_gains(&w, A, &["flake.nix"]);
    let nix = FakeNix::watching(&w);
    activate_with_nix(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &nix).unwrap();
    assert!(nix.log.borrow().is_empty());
    assert_eq!(current_of(&w), A);
}

#[test_id(tlfrwew)]
#[test]
fn spira_env_target_names_the_release_directory_not_the_symlink_to_it() {
    let td = TempDir::new("env-target");
    let real = td.path().join("spira-daily");
    fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink("spira-daily", td.path().join("current")).unwrap();
    let want = format!("path:{}#spira-env", fs::canonicalize(&real).unwrap().display());
    assert_eq!(spira_env::target(&td.path().join("current")), want);
}

#[test_id(tdabs9l)]
#[test]
fn an_activation_that_cannot_move_current_puts_the_env_back() {
    let w = world_activating_a_flake_release();
    let before = installed_env(&w, "old-env");
    fs::create_dir_all(w.cfg.releases.join("current/occupied")).unwrap();
    let e = activate_with_nix(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &FakeNix::watching(&w)).unwrap_err();
    assert!(e.contains("cannot swap") && !e.contains("UNDO INCOMPLETE"), "{e}");
    assert_eq!(fs::canonicalize(env_link(&w)).unwrap(), before);
    assert_eq!(fs::read_to_string(w.cfg.unit_dir.join("spira-tool-prod.service")).unwrap(), OLD_UNIT);
}

#[test_id(tdhmlwl)]
#[test]
fn a_refusal_whose_env_link_could_not_be_put_back_does_not_say_nothing_changed() {
    let w = world_activating_a_flake_release();
    installed_env(&w, "old-env");
    let nix = FakeNix { root_fails: true, restore_blocked: Some(env_link(&w)), ..FakeNix::watching(&w) };
    let e = activate_with_nix(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &nix).unwrap_err();
    assert!(e.contains("the env link still names") && !e.contains("nothing changed"), "{e}");
    assert_eq!(fs::read_link(env_link(&w)).unwrap(), nix.store_path(A));
}

#[test_id(t7zr92b)]
#[test]
fn an_activation_that_cannot_write_a_unit_puts_the_env_back() {
    let w = world_activating_a_flake_release();
    let before = installed_env(&w, "old-env");
    fs::create_dir_all(w.cfg.unit_dir.join("spira-tool-prod.service.new")).unwrap();
    let e = activate_with_nix(&w, &FakeSystemctl::new(w.cfg.unit_dir.clone()), &FakeNix::watching(&w)).unwrap_err();
    assert!(e.contains("cannot write") && !e.contains("UNDO INCOMPLETE"), "{e}");
    assert_eq!(fs::canonicalize(env_link(&w)).unwrap(), before);
    assert!(!w.cfg.releases.join("current").exists());
}

#[test_id(tolvf7h)]
#[test]
fn the_nix_commands_build_to_the_link_and_root_the_old_env_indirectly() {
    let calls = RefCell::new(Vec::new());
    let run = |prog: &str, args: &[&str]| {
        calls.borrow_mut().push(format!("{prog} {}", args.join(" ")));
        Ok(())
    };
    let nix = spira_env::Argv(&run);
    spira_env::Nix::build(&nix, "path:r#spira-env", Path::new("h/env")).unwrap();
    spira_env::Nix::add_root(&nix, Path::new("h/env.prev"), Path::new("store/old")).unwrap();
    assert_eq!(*calls.borrow(), [
        "nix --extra-experimental-features nix-command flakes build path:r#spira-env -o h/env",
        "nix-store --add-root h/env.prev --indirect --realise store/old",
    ]);
}

#[test_id(tvkix2p)]
#[test]
fn a_failed_nix_run_reports_what_nix_wrote_to_stderr() {
    use std::os::unix::process::ExitStatusExt;
    let said = |code: i32| std::process::Output { status: std::process::ExitStatus::from_raw(code << 8), stdout: vec![], stderr: b"error: attribute 'spira-env' missing\n".to_vec() };
    assert_eq!(spira_env::verdict(Path::new("p/nix"), &said(0)), Ok(()));
    let e = spira_env::verdict(Path::new("p/nix"), &said(1)).unwrap_err();
    assert!(e.contains("p/nix failed") && e.contains("attribute 'spira-env' missing"), "{e}");
}
