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
use crate::manifest::{Entry, Manifest};
use crate::systemctl::{Systemctl, UnitState};
use crate::units;
use crate::verify::{self};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use testkit::TempDir;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
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
    kills: RefCell<Vec<String>>,
    resets: RefCell<Vec<String>>,
    reloads: RefCell<usize>,
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
            kills: RefCell::new(vec![]),
            resets: RefCell::new(vec![]),
            reloads: RefCell::new(0),
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
        }
    }
}

impl Systemctl for FakeSystemctl {
    fn daemon_reload(&self) -> Result<(), String> {
        *self.reloads.borrow_mut() += 1;
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
    fn restart(&self, unit: &str) -> Result<(), String> {
        if unit == "dolt-beads.service" {
            return Err(format!("{unit}: RefuseManualStop=yes denies a restart job"));
        }
        self.restarts.borrow_mut().push(unit.into());
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
        let mut s = self.states.borrow_mut();
        let e = s.entry(unit.into()).or_default();
        if bad {
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

#[test]
fn manifest_without_a_commit_or_with_garbage_does_not_parse() {
    assert!(Manifest::parse("bin/x 00\n").is_err());
    assert!(Manifest::parse(&format!("commit {A}\nbin/x nothex\n")).is_err());
    assert!(Manifest::parse("commit short\n").is_err());
}

// ---------------------------------------------------------------- build
// ---------------------------------------------------------------- verify

/// sp-vrn3v: `pre_activate_env` names the release **under verification**, plus the box's own
/// tool tail appended after the system directories, exactly as `release_path_with_tail`
/// documents — the same shape `host_values` renders into `Environment=PATH=` (sp-31gtu,
/// sp-c7b85), reused here for a process this crate spawns directly instead of a unit.
#[test]
fn pre_activate_env_builds_the_release_own_path_and_release_var() {
    let mut env = Env::new();
    env.insert("SPIRA_UNIT_DIR".into(), "/units".into());
    let reg = Registered { releases: "/e".into(), releases_keep: "10".into(), path: "/h/.local/bin:/h/.cargo/bin".into(), ..Default::default() };
    let cfg = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    let rel = PathBuf::from("/e").join(A);
    let envs = verify::pre_activate_env(&cfg, &rel).unwrap();
    assert_eq!(envs[0], (spira_config::RELEASE_ENV.to_string(), rel.display().to_string()));
    assert_eq!(envs[1], ("PATH".to_string(), format!("{}/bin:{}/spira:/usr/local/bin:/usr/bin:/bin:/h/.local/bin:/h/.cargo/bin", rel.display(), rel.display())));

    // No tail configured: PATH ends at the system directories, same as `release_path`.
    let reg = Registered { releases: "/e".into(), releases_keep: "10".into(), ..Default::default() };
    let cfg = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    let envs = verify::pre_activate_env(&cfg, &rel).unwrap();
    assert_eq!(envs[1].1, format!("{}/bin:{}/spira:/usr/local/bin:/usr/bin:/bin", rel.display(), rel.display()));
}

/// The same refusal `host_values` gives a tail inside a release or a checkout (sp-c7b85)
/// applies here too: `pre_activate_env` must not hand pre-activate a `PATH` that could
/// resolve back into a release or a checked-out tree instead of the one under verification.
#[test]
fn pre_activate_env_refuses_a_tail_inside_a_release_or_checkout() {
    let mut env = Env::new();
    env.insert("SPIRA_UNIT_DIR".into(), "/units".into());
    let reg = Registered { releases: "/e".into(), releases_keep: "10".into(), path: "/x/spira-releases/def/bin".into(), ..Default::default() };
    let cfg = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    let rel = PathBuf::from("/e").join(A);
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

#[test]
fn render_names_the_release_and_refuses_what_it_cannot_fill() {
    let rel = Path::new("/r/spira-releases").join(A);
    let mut host = BTreeMap::new();
    host.insert("SPIRA_RUN".to_string(), "/run/x".to_string());
    host.insert("SPIRA_DB".to_string(), String::new());
    let out = units::render("spira-tool.service", SVC, &rel, &host, None, "prod").unwrap();
    assert!(out.contains(&format!("ExecStart=/r/spira-releases/{A}/bin/tool --serve")), "{out}");
    assert!(out.contains("append:/run/x/tool.log"));
    assert!(out.ends_with("--serve\nStandardOutput=append:/run/x/tool.log\n"));
    let t = units::render("spira-job.timer", JOB_TIMER, &rel, &host, None, "prod").unwrap();
    assert!(t.contains("Unit=spira-job-prod.service"), "{t}");
    let wt = units::render("spira-watch@.service", WATCH, &rel, &host, Some("pool"), "prod").unwrap();
    assert!(wt.contains("watchd.sh exec pool"));
    let sup = units::render("x.service", "ExecStart=@SPIRA_SUPERVISE_BIN@ @SPIRA_HOME@/c.sh\n", &rel, &host, None, "prod").unwrap();
    // path-ok: a unit rendered against a release names that release's own binary.
    assert!(sup.contains(&format!("{A}/bin/spira-supervise {}/{A}/spira/c.sh", "/r/spira-releases")), "{sup}");
    let e = units::render("y.service", "ExecStart=@NOPE@\n", &rel, &host, None, "prod").unwrap_err();
    assert!(e.contains("nothing fills: NOPE"), "{e}");
    let e = units::render("z.service", "Environment=SPIRA_DB=@SPIRA_DB@\n", &rel, &host, None, "prod").unwrap_err();
    assert!(e.contains("uses SPIRA_DB but no value"), "{e}");
}

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

// ---------------------------------------------------------------- hotfix

// ---------------------------------------------------------------- prune

// ---------------------------------------------------------------- config

#[test]
fn config_resolves_roots_from_flags_then_config_and_refuses_when_nothing_does() {
    let mut env = Env::new();
    env.insert("HOME".into(), "/h".into());
    assert!(Config::resolve_with(&Flags::default(), &env, None, &Registered::default()).unwrap_err().contains("no releases directory"));
    // `reg.releases` is already fully resolved here (as `cfg("SPIRA_RELEASES")` would hand
    // it — the `workspaces`-derived default is `spira_config`'s own job, not this crate's).
    let reg = Registered { releases: "/w/spira-releases".into(), run: "/r".into(), releases_keep: "7".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert_eq!((c.releases.clone(), c.run.clone(), c.keep), (PathBuf::from("/w/spira-releases"), Some(PathBuf::from("/r")), 7));
    assert_eq!(c.unit_dir, PathBuf::from("/h/.config/systemd/user"));
    let reg2 = Registered { releases: "/e".into(), ..reg.clone() };
    assert_eq!(Config::resolve_with(&Flags::default(), &env, None, &reg2).unwrap().releases, PathBuf::from("/e"));
    let f = Flags { releases: Some("/f".into()), ..Default::default() };
    assert_eq!(Config::resolve_with(&f, &env, None, &reg2).unwrap().releases, PathBuf::from("/f"));
}

#[test]
fn host_values_carries_the_configured_path_tail_and_refuses_a_bad_one() {
    let mut env = Env::new();
    env.insert("HOME".into(), "/h".into());
    // Nothing configured: the tail is empty, not an error.
    let reg = Registered { releases: "/e".into(), releases_keep: "10".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert_eq!(c.host_values().unwrap().get("SPIRA_PATH_TAIL").map(String::as_str), Some(""));
    // The configured key.
    let reg = Registered { releases: "/e".into(), releases_keep: "10".into(), path: "/h/.local/bin:/h/.cargo/bin".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert_eq!(c.host_values().unwrap().get("SPIRA_PATH_TAIL").map(String::as_str), Some(":/h/.local/bin:/h/.cargo/bin"));
    // A tail entry inside a release is refused, naming it.
    let bad = Registered { releases: "/e".into(), releases_keep: "10".into(), path: "/x/spira-releases/def/bin".into(), ..Default::default() };
    let c = Config::resolve_with(&Flags::default(), &env, None, &bad).unwrap();
    let e = c.host_values().unwrap_err();
    assert!(e.contains("spira-releases"), "{e}");
}

#[test]
fn rfc3339_is_correct() {
    assert_eq!(fsutil::rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(fsutil::rfc3339(1_790_640_000), "2026-09-29T00:00:00Z");
}

#[test]
fn parse_rfc3339_round_trips_through_rfc3339() {
    for secs in [0u64, 1, 59, 60, 3599, 3600, 86_399, 86_400, 1_790_640_000, 4_102_444_799, 253_402_300_799] {
        let s = fsutil::rfc3339(secs);
        assert_eq!(fsutil::parse_rfc3339(&s), Some(secs), "round-trip of {s:?}");
    }
}

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

#[test]
fn release_name_from_tarball_accepts_the_naming_convention_and_refuses_the_rest() {
    assert_eq!(install::release_name_from_tarball(Path::new("spira-20260101T000000Z.tar.gz")).unwrap(), "spira-20260101T000000Z");
    assert_eq!(install::release_name_from_tarball(Path::new("/tmp/x/spira-1.tgz")).unwrap(), "spira-1");
    assert!(install::release_name_from_tarball(Path::new("spira-1.zip")).is_err());
    assert!(install::release_name_from_tarball(Path::new("release.tar.gz")).is_err());
    assert!(install::release_name_from_tarball(Path::new("spira-.tar.gz")).is_err());
}

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


// ---------------------------------------------------------------- stage

#[test]
fn stage_rune_names_the_canary_persona_with_one_concurrent_slot() {
    let f = crate::stage::rune_content();
    assert!(f.contains("RUNE_NAME=canary"));
    assert!(f.contains("RUNE_MAX_CONCURRENT=1"));
    assert!(f.contains("SPIRA_SCOPE_LABEL"));
}

#[test]
fn stage_repo_map_line_has_six_columns_named_for_the_repo_and_an_empty_gate() {
    let line = crate::stage::repo_map_line(Path::new("/tmp/x/repo"));
    let cols: Vec<&str> = line.trim_end().split('|').map(str::trim).collect();
    assert_eq!(cols.len(), 6, "{line:?}");
    assert_eq!(cols[0], "repo");
    assert_eq!(cols[1], "/tmp/x/repo");
    assert_eq!(cols[2], "push");
    assert_eq!(cols[3], "origin/main");
    assert_eq!(cols[4], "");
    assert_eq!(cols[5], "");
}

#[test]
fn stage_up_refuses_an_existing_root_and_down_refuses_a_non_stage_directory() {
    let sb = Sandbox::new();
    let existing = sb.p().join("already-here");
    fs::create_dir_all(&existing).unwrap();
    let so = crate::stage::StageOpts { root: Some(existing.clone()), harness_spira: sb.p().join("nope"), bd_embedded: sb.p().join("nope"), testdb_baseline: None, scope_label: String::new() };
    let err = crate::stage::up(&so).unwrap_err();
    assert!(err.contains("already exists"), "{err}");

    let not_a_stage = sb.p().join("plain-dir");
    fs::create_dir_all(&not_a_stage).unwrap();
    let err2 = crate::stage::down(&not_a_stage).unwrap_err();
    assert!(err2.contains("does not look like a stage"), "{err2}");
}

// ---------------------------------------------------------------- config delta

fn delta_git(delta: &str) -> FakeGit {
    FakeGit { validator: true, delta_for: Some((B.to_string(), delta.to_string())), ..Default::default() }
}

const DELTA_BOTH: &str = "removed = [\"spira.old_key\"]\n[spira]\nnew_key = 300\n";

#[test]
fn a_rename_mixing_a_persona_and_a_config_key_is_refused() {
    let e = config_delta::Delta::parse("[renamed]\n\"persona.a\" = \"spira.b\"\n").unwrap_err();
    assert!(e.contains("a rename is persona"), "{e}");
}

// ---------------------------------------------------------------- activation installs new units

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

#[test]
fn a_documentation_line_names_current_so_two_releases_render_one_unit() {
    let text = "[Unit]\nDocumentation=file://@SPIRA_HOME@/README.md\n[Service]\nExecStart=@SPIRA_RELEASE@/bin/x\n";
    let host = BTreeMap::new();
    let a = units::render("x.service", text, Path::new("/r/spira-releases/aaa"), &host, None, "prod").unwrap();
    let b = units::render("x.service", text, Path::new("/r/spira-releases/bbb"), &host, None, "prod").unwrap();
    assert!(a.contains("Documentation=file:///r/spira-releases/current/spira/README.md\n"), "{a}");
    assert!(a.contains("ExecStart=/r/spira-releases/aaa/bin/x"), "Exec lines still name the sha: {a}");
    assert_eq!(units::without_documentation(&a).replace("aaa", "R"), units::without_documentation(&b).replace("bbb", "R"));
    let stale = a.replace("current", "0ldsha");
    assert_eq!(units::without_documentation(&stale), units::without_documentation(&a), "a Documentation-only difference is not a change");
}

// ---------------------------------------------------------------- restart budget

#[test]
fn a_delta_naming_a_placeholder_path_is_refused_and_the_shipped_one_is_clean() {
    let bad = "[spira]\nlc_snapshot = \"/placeholder/run/bead-snapshot.json\"\n";
    let err = config_delta::Delta::parse(bad).unwrap_err();
    assert!(err.contains("placeholder path"), "{err}");
    let ok = config_delta::Delta::parse("[spira]\nlc_snapshot = \"/srv/run/bead-snapshot.json\"\ngroom_gate_label = \"placeholder-gate\"\n");
    assert!(ok.is_ok(), "{ok:?}");
    let shipped = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../", "spira/config-delta.toml")).unwrap();
    let d = config_delta::Delta::parse(&shipped).unwrap();
    assert!(!d.added.contains_key("spira.lc_snapshot"), "the snapshot path derives from the install's run dir");
}
const TC: &str = "1.99.0 abc";

#[test]
fn the_manifest_round_trips_and_the_key_names_tree_and_toolchain() {
    let m = artifacts::Manifest { key: "k".into(), tree: "t".into(), toolchain: TC.into(), tarball_sha256: "s".into(), tarball_bytes: 7, files: vec![("a".into(), "h1".into()), ("b".into(), "h2".into())] };
    assert_eq!(artifacts::Manifest::parse(&m.render()).unwrap(), m);
    assert_eq!(artifacts::key("t1", "deadbeef 1.99.0"), "55ff5d54bdd1a52763970e49c935c72df12820f8369f1e5d6ca4f4b1c2f56561", "the round VM's bash computes this same value");
    assert_ne!(artifacts::key("t1", TC), artifacts::key("t2", TC));
    assert_ne!(artifacts::key("t1", TC), artifacts::key("t1", "1.100.0 def"));
    assert_eq!(artifacts::toolchain_id("rustc 1.99.0 (abc 2026)\nbinary: rustc\ncommit-hash: deadbeef\nrelease: 1.99.0\nhost: x\n"), "deadbeef 1.99.0");
}

#[test]
fn a_delta_value_of_the_wrong_type_is_refused_at_load_naming_the_key_and_file() {
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
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
#[test]
fn the_shipped_delta_carries_every_required_key_with_every_task_in_act_on_systemd() {
    let tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let d = config_delta::load(&tree).unwrap().expect("the tree ships a delta");
    let nothing_known = TempDir::new("no-active-release");
    let done = config_delta::complete(&[toml::map::Map::new()], &tree, Some(nothing_known.path()), Some(d.clone()));
    assert!(done.is_ok(), "{}", done.unwrap_err());
    assert_eq!(d.added.get("spira.units"), Some(&toml::Value::String("systemd".into())));
    let modes = d.added.get("spira.task_modes").and_then(|v| v.as_str()).expect("task_modes is declared");
    let pairs: Vec<&str> = modes.split_whitespace().collect();
    assert!(pairs.len() >= 82, "every task is named: {}", pairs.len());
    assert!(pairs.iter().all(|p| p.ends_with("=act")), "every task runs in act: {modes}");
}

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

#[test]
fn completion_adds_a_key_the_active_registry_declares_and_the_config_lacks_at_its_default() {
    let tmp = TempDir::new("complete");
    let new = tree_declaring(&tmp.path().join("new"), &["SPIRA_ORPHAN_SECS"]);
    let active = tree_declaring(&tmp.path().join("old"), &["SPIRA_ORPHAN_SECS"]);
    let cfg: toml::Table = "[spira]\nid_prefix = \"sp\"\n".parse().unwrap();
    let d = config_delta::complete(&[cfg], &new, Some(&active), None).unwrap().expect("a delta");
    assert_eq!(d.added.get("spira.orphan_secs"), Some(&toml::Value::Integer(600)));
}

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
    let active = tree(&tmp.path().join("old"), "heartbeat=act watch.spill_key=act");
    let new = tree(&tmp.path().join("new"), "heartbeat=act watch.spill_key=act watch.throughput=act");
    let live: toml::Table = "[spira]\ntask_modes = \"heartbeat=act watch.spill_key=observe\"\n".parse().unwrap();
    let d = config_delta::complete(std::slice::from_ref(&live), &new, Some(&active), None).unwrap().expect("a delta that completes task_modes");
    let (_, post, undo) = config_delta::apply(std::slice::from_ref(&live), &d, &[], true).unwrap();
    assert_eq!(
        post[0]["spira"]["task_modes"].as_str(),
        Some("heartbeat=act watch.spill_key=observe watch.throughput=act"),
        "the new task takes the release default; the live override stands"
    );
    let (_, back, _) = config_delta::apply(&post, &undo, &[], false).unwrap();
    assert_eq!(back[0], live, "a rollback puts the live value back as it was");
    assert!(config_delta::complete(&post, &new, Some(&active), None).unwrap().is_none(), "a complete value is left alone");
}

fn shipped_unit(name: &str) -> String {
    std::fs::read_to_string(format!("{}/../systemd/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn a_template_that_restarts_when_idle_gives_a_reason_and_a_bare_boolean_gives_none() {
    let t = |v: &str| format!("[Unit]\nX-Spira-RestartWhenIdle={v}\n");
    assert_eq!(units::restart_when_idle(&t("it ends its own pass when idle")).as_deref(), Some("it ends its own pass when idle"));
    assert_eq!(units::restart_when_idle(&t("true")), None);
    assert_eq!(units::restart_when_idle("[Unit]\n"), None);
}

#[test]
fn the_unit_run_watcher_restarts_itself_when_idle() {
    assert!(units::restart_when_idle(&shipped_unit("spira-unit-run-dbus.service")).is_some());
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
}

impl FakeReads {
    fn serving() -> FakeReads {
        FakeReads { refuses: None, asked: RefCell::new(vec![]) }
    }
}

impl activate::Reads for FakeReads {
    fn lc(&self, _rel: &Path, args: &[&str]) -> Result<String, String> {
        let call = args.join(" ");
        self.asked.borrow_mut().push(call.clone());
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
    let ctx = activate::Ctx { cfg: &w.cfg, reads, sc, git: &w._git, repo: None, landed_ref: "main".into(), settle: Duration::from_millis(10), drain: Duration::from_millis(10) };
    activate::activate(&ctx, A, None)
}

#[test]
fn the_activation_is_recorded_before_the_first_restart_so_a_restart_that_kills_the_activator_loses_nothing() {
    let w = world_activating_a_changed_unit();
    let state = w.cfg.state_dir().unwrap();
    let seen = {
        let state = state.clone();
        move || {
            let machine = crate::machine::load(&state, A).unwrap().and_then(|r| r.state()).map(|s| s.as_str().to_string());
            format!("{machine:?} {}", fs::read_to_string(state.join("history")).unwrap_or_default().trim())
        }
    };
    let mut sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.on_restart = Some(Box::new(seen));
    activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.restarts.borrow(), vec!["spira-tool-prod.service".to_string()]);
    assert_eq!(*sc.at_restart.borrow(), vec![format!("Some(\"activated\") {A} landed")]);
}

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
    }
    let rel = w.cfg.releases.join(A);
    let m = Manifest { commit: A.into(), built: None, repo: None, entries: Manifest::scan(&rel).unwrap() };
    fs::write(rel.join("MANIFEST"), m.render()).unwrap();
    std::os::unix::fs::symlink(&prev, w.cfg.releases.join("current")).unwrap();
    let old = CACHE_UNIT.replace("@SPIRA_RELEASE@", &w.cfg.releases.join(&prev).display().to_string()).replace("@SPIRA_PROD@", &format!("{}/spira", w.cfg.releases.join(&prev).display()));
    file(&w.cfg.unit_dir.join("sccache-dav.service"), &old);
    w
}


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

#[test]
fn an_activation_that_changes_the_cache_backends_binary_restarts_it_and_stops_the_sccache_server() {
    let w = world_activating_over_a_running_cache(true);
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    sc.states.borrow_mut().insert("sccache-dav.service".into(), UnitState { active: "active".into(), result: "success".into(), kind: "simple".into() });
    let out = activate_with(&w, &sc).unwrap();
    assert_eq!(*sc.restarts.borrow(), vec!["sccache-dav.service".to_string()]);
    assert!(out.left_running.is_empty(), "{:?}", out.left_running);
    assert_eq!(*sc.cache_server_stops.borrow(), 1);
}

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

#[test]
fn an_activation_reads_the_lifecycle_and_the_first_beads_content_through_the_new_release() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let reads = FakeReads::serving();
    activate_reading(&w, &sc, &reads).unwrap();
    assert_eq!(*reads.asked.borrow(), vec!["list --state READY".to_string(), "content show sp-first".to_string()]);
}

#[test]
fn a_release_whose_content_read_is_refused_is_rolled_back_and_the_error_names_the_read() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let reads = FakeReads { refuses: Some("content show"), asked: RefCell::new(vec![]) };
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

#[test]
fn a_release_that_drops_a_task_the_host_task_modes_names_activates_with_the_entry_gone() {
    let w = world_dropping_a_task("removed_tasks = [\"repo-events\"]\n");
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    activate_with(&w, &sc).unwrap();
    let cfg = fs::read_to_string(w.cfg.toml_spec().unwrap()).unwrap();
    assert!(cfg.contains("task_modes = \"heartbeat=act\"") && !cfg.contains("repo-events"), "{cfg}");
    assert_eq!(sc.state("spira-tool-prod.service").unwrap().active, "active");
}

#[test]
fn a_release_that_drops_a_task_without_declaring_it_removed_is_refused_naming_the_entry() {
    let w = world_dropping_a_task("");
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let e = activate_with(&w, &sc).unwrap_err();
    assert!(e.contains("repo-events") && e.contains("removed_tasks"), "{e}");
    assert!(fs::read_to_string(w.cfg.toml_spec().unwrap()).unwrap().contains("repo-events=act"), "the host value is untouched");
    assert!(!w.cfg.releases.join("current").exists(), "current did not move");
}

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

#[test]
fn a_release_whose_lifecycle_read_is_refused_is_rolled_back() {
    let w = world_activating_a_changed_unit();
    let sc = FakeSystemctl::new(w.cfg.unit_dir.clone());
    let reads = FakeReads { refuses: Some("list"), asked: RefCell::new(vec![]) };
    let e = activate_reading(&w, &sc, &reads).unwrap_err();
    assert!(e.contains("lifecycle read refused") && e.contains("rolled back"), "{e}");
}

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
