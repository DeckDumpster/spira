//! Unit tests: MANIFEST, build, verify, rendering, the atomic `current` swap, rollback when a
//! unit fails to come up, the hotfix supersede rule, and prune. Nothing here touches the
//! host's systemd or runs cargo: `FakeGit`, `FakeCargo` and `FakeSystemctl` stand in.

use crate::activate::{self, Ctx};
use crate::build::{self, BuildOpts, Cargo};
use crate::config::{Config, Env, Flags, Registered};
use crate::config_delta;
use crate::fsutil;
use crate::git::Git;
use crate::install::{self, InstallOpts, Unpack};
use crate::manifest::{Entry, Manifest};
use crate::systemctl::{Systemctl, UnitState};
use crate::units;
use crate::verify::{self, VerifyOpts};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use testkit::TempDir;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

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
    ancestors: RefCell<BTreeSet<(String, String)>>,
}

impl FakeGit {
    fn contains(&self, ancestor: &str, of: &str) {
        self.ancestors.borrow_mut().insert((ancestor.into(), of.into()));
    }
}

impl Git for FakeGit {
    fn resolve(&self, _repo: &Path, rev: &str) -> Result<String, String> {
        if crate::is_sha(rev) {
            Ok(rev.into())
        } else {
            Err(format!("{rev} is not a commit"))
        }
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

/// A cargo that "builds" every name in `bins` into `<target>/release`.
struct FakeCargo {
    bins: Vec<&'static str>,
    /// A binary's own body, in place of the default.
    bodies: Vec<(&'static str, String)>,
}
impl Cargo for FakeCargo {
    fn build(&self, tree: &Path, target: &Path) -> Result<(), String> {
        for b in &self.bins {
            let body = self.bodies.iter().find(|(n, _)| n == b).map(|(_, body)| body.clone()).unwrap_or_else(|| format!("#!/bin/sh\n# built from {}\n", tree.display()));
            exe(&target.join("release").join(b), &body);
        }
        Ok(())
    }
}

struct World {
    sb: Sandbox,
    cfg: Config,
    git: FakeGit,
}

impl World {
    fn new() -> World {
        World::with_git(FakeGit::default())
    }
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
        env.insert("SPIRA_UNIT_DIR".into(), sb.p().join("units").display().to_string());
        env.insert("SPIRA_TOML".into(), sb.p().join("cfg.toml").display().to_string());
        let reg = Registered { instance: "prod".into(), ..reg };
        let flags = Flags { releases: Some(sb.p().join("rel")), run: Some(sb.p().join("run")), keep: Some(2) };
        let cfg = Config::resolve_with(&flags, &env, None, &reg).unwrap();
        fs::create_dir_all(sb.p().join("units")).unwrap();
        World { sb, cfg, git }
    }
    fn build(&self, sha: &str) -> Result<build::Built, String> {
        self.build_with(sha, &FakeCargo { bins: vec!["tool"], bodies: vec![] }, vec![])
    }
    fn build_with(&self, sha: &str, cargo: &FakeCargo, system_dirs: Vec<PathBuf>) -> Result<build::Built, String> {
        let o = BuildOpts { repo: self.sb.p(), commit: sha, target_dir: Some(self.sb.p().join("target")), system_dirs, bin_dir: None };
        build::build(&self.cfg, &self.git, cargo, &o)
    }
    fn rel(&self, sha: &str) -> PathBuf {
        self.cfg.releases.join(sha)
    }
    fn units(&self) -> PathBuf {
        self.cfg.unit_dir.clone()
    }
    fn unit(&self, name: &str) -> String {
        fs::read_to_string(self.units().join(name)).unwrap()
    }
    fn current(&self) -> Option<String> {
        activate::current(&self.cfg)
    }
    /// Install the units as `install.sh` would have, rendered against release `sha`.
    fn install_units(&self, sha: &str) {
        let rel = self.rel(sha);
        let host = self.cfg.host_values().unwrap();
        for (inst, tpl, w) in [
            ("spira-tool-prod.service", "spira-tool.service", None),
            ("spira-job-prod.service", "spira-job.service", None),
            ("spira-job-prod.timer", "spira-job.timer", None),
            ("shared.service", "shared.service", None),
            ("spira-watch-pool-prod.service", "spira-watch@.service", Some("pool")),
        ] {
            let text = fs::read_to_string(rel.join("systemd").join(tpl)).unwrap();
            let out = units::render(tpl, &text, &rel, &host, w, "prod").unwrap();
            fs::write(self.units().join(inst), out).unwrap();
        }
    }
}

/// systemd, as far as activation can tell: each unit's state, and what restarting it does.
/// A restarted unit whose installed ExecStart names `poison` fails to come up.
struct FakeSystemctl {
    unit_dir: PathBuf,
    states: RefCell<BTreeMap<String, UnitState>>,
    restarts: RefCell<Vec<String>>,
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
    /// Units that go inactive once `state` has been asked about them this many more times.
    exits_after: RefCell<BTreeMap<String, usize>>,
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
            reloads: RefCell::new(0),
            poison: None,
            transient: BTreeSet::new(),
            restart_fails: BTreeSet::new(),
            disabled: RefCell::new(vec![]),
            disable_fails: BTreeSet::new(),
            probe: None,
            probes: RefCell::new(vec![]),
            exits_after: RefCell::new(BTreeMap::new()),
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
    fn restart(&self, unit: &str) -> Result<(), String> {
        self.restarts.borrow_mut().push(unit.into());
        if self.restart_fails.contains(unit) {
            return Err(format!("{unit}: simulated restart failure"));
        }
        let text = fs::read_to_string(self.unit_dir.join(unit)).unwrap_or_default();
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

fn ctx<'a>(w: &'a World, sc: &'a FakeSystemctl) -> Ctx<'a> {
    Ctx { cfg: &w.cfg, sc, git: &w.git, repo: Some(w.sb.p().to_path_buf()), landed_ref: "local/main".into(), settle: Duration::ZERO, drain: Duration::ZERO }
}

fn no_pre() -> VerifyOpts {
    VerifyOpts { pre_activate: false, system_dirs: vec![] }
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

#[test]
fn manifest_check_names_every_difference() {
    let w = World::new();
    let rel = w.build(A).unwrap().dir;
    fsutil::make_writable(&rel);
    let m = Manifest::load(&rel).unwrap();
    assert_eq!(m.check(&rel).unwrap(), Vec::<String>::new());
    fs::write(rel.join("CLAUDE.md"), "edited\n").unwrap();
    fs::remove_file(rel.join("shared.sh")).unwrap();
    fs::write(rel.join("stray"), "x").unwrap();
    fs::remove_file(rel.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink("elsewhere", rel.join("AGENTS.md")).unwrap();
    let p = m.check(&rel).unwrap().join("\n");
    assert!(p.contains("CLAUDE.md: sha256"), "{p}");
    assert!(p.contains("shared.sh: listed in MANIFEST but missing"), "{p}");
    assert!(p.contains("stray: present but not in MANIFEST"), "{p}");
    assert!(p.contains("AGENTS.md: points at elsewhere"), "{p}");
}

// ---------------------------------------------------------------- build

#[test]
fn build_makes_a_read_only_release_named_by_its_commit_whose_manifest_verifies() {
    let w = World::new();
    let b = w.build(A).unwrap();
    assert_eq!(b, build::Built { sha: A.into(), dir: w.rel(A), fresh: true });
    assert!(fsutil::is_executable(&w.rel(A).join("bin/tool")));
    let m = Manifest::load(&w.rel(A)).unwrap();
    assert_eq!(m.commit, A);
    assert!(m.built.is_some());
    assert!(matches!(m.entries.get("bin/tool"), Some(Entry::File(_))));
    assert_eq!(m.entries.get("AGENTS.md"), Some(&Entry::Link("CLAUDE.md".into())));
    assert_eq!(fsutil::writable_paths(&w.rel(A)).unwrap(), Vec::<String>::new());
    assert_eq!(verify::verify(&w.cfg, A, &no_pre()).unwrap(), Vec::<String>::new());
    // No stage left behind.
    let left: Vec<String> = fs::read_dir(&w.cfg.releases).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert_eq!(left, vec![A.to_string()]);
}

#[test]
fn build_runs_conf_gen_so_the_release_carries_the_generated_config() {
    let gen = "#!/bin/bash\nd=$(dirname \"$0\")\necho keys > \"$d/conf.d.keys.generated.sh\"\n";
    let w = World::with_git(FakeGit { extra: vec![("spira/conf-gen.sh".into(), gen.into(), true)], ..Default::default() });
    w.build(A).unwrap();
    assert_eq!(fs::read_to_string(w.rel(A).join("spira/conf.d.keys.generated.sh")).unwrap(), "keys\n");
    assert!(matches!(Manifest::load(&w.rel(A)).unwrap().entries.get("spira/conf.d.keys.generated.sh"), Some(Entry::File(_))));
}

#[test]
fn build_refuses_when_conf_gen_fails_and_leaves_no_release() {
    let gen = "#!/bin/bash\necho broken registry >&2\nexit 3\n";
    let w = World::with_git(FakeGit { extra: vec![("spira/conf-gen.sh".into(), gen.into(), true)], ..Default::default() });
    let e = w.build(A).unwrap_err();
    assert!(e.contains("conf-gen.sh failed") && e.contains("broken registry"), "{e}");
    assert!(fs::symlink_metadata(w.rel(A)).is_err());
}

#[test]
fn build_of_an_existing_release_does_not_rebuild_it() {
    let w = World::new();
    w.build(A).unwrap();
    let again = w.build_with(A, &FakeCargo { bins: vec![], bodies: vec![] }, vec![]).unwrap();
    assert!(!again.fresh);
}

/// A cargo that must never run: `--bin-dir` takes the tested binaries as they are.
struct NoCargo;
impl Cargo for NoCargo {
    fn build(&self, _: &Path, _: &Path) -> Result<(), String> {
        panic!("--bin-dir must not run cargo");
    }
}

#[test]
fn build_with_a_bin_dir_ships_those_binaries_without_cargo_and_still_refuses_a_partial_set() {
    let w = World::new();
    let tested = w.sb.p().join("round/target/release");
    exe(&tested.join("tool"), "#!/bin/sh\n# the round's tested build\n");
    exe(&tested.join("stray"), "#!/bin/sh\n");
    let o = BuildOpts { repo: w.sb.p(), commit: A, target_dir: None, system_dirs: vec![], bin_dir: Some(tested.clone()) };
    let b = build::build(&w.cfg, &w.git, &NoCargo, &o).unwrap();
    assert!(b.fresh);
    assert_eq!(fs::read_to_string(w.rel(A).join("bin/tool")).unwrap(), "#!/bin/sh\n# the round's tested build\n");
    assert!(!w.rel(A).join("bin/stray").exists(), "only declared [[bin]] targets are shipped");
    assert_eq!(verify::verify(&w.cfg, A, &no_pre()).unwrap(), Vec::<String>::new());
    // A tested build missing a declared binary is refused, and nothing is named by the sha.
    let w = World::new();
    let empty = w.sb.p().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let o = BuildOpts { repo: w.sb.p(), commit: A, target_dir: None, system_dirs: vec![], bin_dir: Some(empty) };
    let e = build::build(&w.cfg, &w.git, &NoCargo, &o).unwrap_err();
    assert!(e.contains("declares tool but the build"), "{e}");
    assert!(!w.rel(A).exists());
}

/// sp-6onps-compat (a P0): the one path that matters — `release build --bin-dir`, what
/// `queue land-local` and every real deploy actually takes — must carry every declared
/// compat name, exactly like spira/build-tarball.sh's own build does. Before this fix it
/// did not: c26c3a7d8 shipped with none of them.
#[test]
fn build_with_a_bin_dir_also_carries_every_declared_compat_name() {
    let w = World::with_git(FakeGit {
        extra: vec![(
            "spira/deps.toml".into(),
            "[[compat]]\nname = \"tool\"\nalias = \"tool.sh\"\n\n[[compat]]\nname = \"missing\"\nalias = \"missing.sh\"\n".into(),
            false,
        )],
        ..Default::default()
    });
    let tested = w.sb.p().join("round/target/release");
    exe(&tested.join("tool"), "#!/bin/sh\n# the round's tested build\n");
    let o = BuildOpts { repo: w.sb.p(), commit: A, target_dir: None, system_dirs: vec![], bin_dir: Some(tested) };
    build::build(&w.cfg, &w.git, &NoCargo, &o).unwrap();

    // bin/tool.sh resolves to the real binary — a bare-name PATH lookup for either name
    // reaches the identical file.
    let bin_alias = w.rel(A).join("bin/tool.sh");
    assert!(bin_alias.is_symlink(), "bin/tool.sh must exist");
    assert_eq!(fs::read_link(&bin_alias).unwrap(), Path::new("tool"));
    assert_eq!(fs::read_to_string(&bin_alias).unwrap(), "#!/bin/sh\n# the round's tested build\n");

    // spira/tool.sh resolves the same way, for a caller still spelling
    // "$SPIRA_HOME/tool.sh" rather than relying on PATH.
    let spira_alias = w.rel(A).join("spira/tool.sh");
    assert!(spira_alias.is_symlink(), "spira/tool.sh must exist");
    assert_eq!(fs::read_link(&spira_alias).unwrap(), Path::new("../bin/tool"));
    assert_eq!(fs::read_to_string(&spira_alias).unwrap(), "#!/bin/sh\n# the round's tested build\n");

    // "missing" was never built (its declared binary doesn't exist) — skipped, not refused.
    assert!(!w.rel(A).join("bin/missing.sh").exists());
    assert!(!w.rel(A).join("spira/missing.sh").exists());

    // The manifest records both symlinks (Manifest::scan's existing Node::Link handling),
    // and verify() still reports a clean release.
    let m = Manifest::load(&w.rel(A)).unwrap();
    assert_eq!(m.entries.get("bin/tool.sh"), Some(&Entry::Link("tool".into())));
    assert_eq!(m.entries.get("spira/tool.sh"), Some(&Entry::Link("../bin/tool".into())));
    assert_eq!(verify::verify(&w.cfg, A, &no_pre()).unwrap(), Vec::<String>::new());
}

#[test]
fn build_refuses_a_partial_binary_set_and_leaves_nothing_behind() {
    let w = World::new();
    let e = w.build_with(A, &FakeCargo { bins: vec![], bodies: vec![] }, vec![]).unwrap_err();
    assert!(e.contains("declares tool but the build"), "{e}");
    assert_eq!(fs::read_dir(&w.cfg.releases).unwrap().count(), 0);
}

#[test]
fn build_refuses_a_release_that_shadows_a_system_command() {
    let w = World::with_git(FakeGit { extra: vec![("spira/git".into(), "#!/bin/sh\n".into(), true)], ..Default::default() });
    let sys = w.sb.p().join("usr-bin");
    exe(&sys.join("git"), "");
    exe(&sys.join("tool"), "");
    let e = w.build_with(A, &FakeCargo { bins: vec!["tool"], bodies: vec![] }, vec![sys.clone()]).unwrap_err();
    assert!(e.contains("bin/tool shadows"), "{e}");
    assert!(e.contains("spira/git shadows"), "{e}");
    assert!(!w.rel(A).exists());
    // A non-executable file of the same name is not invokable, so not a clash.
    let w2 = World::with_git(FakeGit { extra: vec![("spira/git".into(), "data\n".into(), false)], ..Default::default() });
    w2.build_with(A, &FakeCargo { bins: vec!["tool"], bodies: vec![] }, vec![w2.sb.p().join("none")]).unwrap();
}

#[test]
fn build_refuses_a_tree_that_tracks_a_manifest_header_name_or_bin() {
    let w = World::with_git(FakeGit { extra: vec![("commit".into(), "x".into(), false)], ..Default::default() });
    assert!(w.build(A).unwrap_err().contains("header key"));
    let w = World::with_git(FakeGit { extra: vec![("bin/x".into(), "x".into(), false)], ..Default::default() });
    assert!(w.build(A).unwrap_err().contains("tracks bin/"));
}

/// A FakeGit whose workspace also declares `work` — the one binary a model may run.
fn git_with_work() -> FakeGit {
    FakeGit {
        extra: vec![
            ("Cargo.toml".into(), "[workspace]\nmembers = [\"tool\", \"work\"]\n".into(), false),
            ("work/Cargo.toml".into(), "[package]\nname = \"work\"\nversion = \"0.0.0\"\n".into(), false),
            ("work/src/main.rs".into(), "fn main() {}\n".into(), false),
        ],
        ..Default::default()
    }
}

/// sp-zf4q3: the release carries model-bin/ holding only `work`, linked to ../bin/work, and
/// recorded in MANIFEST; a release that ships no `work` gets no model-bin/ at all.
#[test]
fn build_links_model_bin_holding_only_work() {
    let mb = spira_config::release_env::MODEL_BIN_DIR;
    let w = World::with_git(git_with_work());
    w.build_with(A, &FakeCargo { bins: vec!["tool", "work"], bodies: vec![] }, vec![]).unwrap();
    let link = w.rel(A).join(mb).join("work");
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("../bin/work"));
    assert!(fsutil::is_executable(&link));
    let names: Vec<String> = fs::read_dir(w.rel(A).join(mb)).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert_eq!(names, vec!["work".to_string()], "model-bin holds work and nothing else");
    let m = Manifest::load(&w.rel(A)).unwrap();
    assert_eq!(m.entries.get(&format!("{mb}/work")), Some(&Entry::Link("../bin/work".into())));
    assert_eq!(verify::verify(&w.cfg, A, &no_pre()).unwrap(), Vec::<String>::new());

    let w = World::new();
    w.build(A).unwrap();
    assert!(!w.rel(A).join(mb).exists(), "no work, no model-bin");
}

#[test]
fn build_refuses_a_tree_that_tracks_model_bin() {
    let w = World::with_git(FakeGit { extra: vec![("model-bin/x".into(), "x".into(), false)], ..Default::default() });
    assert!(w.build(A).unwrap_err().contains("tracks model-bin/"));
}

/// sp-zf4q3: verify reports a release that ships `work` but no model-bin/work, and any
/// model-bin entry that is not a model binary.
#[test]
fn model_bin_problems_names_a_missing_work_and_any_stray_entry() {
    let d = testkit::TempDir::new("model-bin-problems");
    exe(&d.join("bin/work"), "#!/bin/sh\n");
    let p = spira_config::release_env::model_bin_problems(&d).join("\n");
    assert!(p.contains("model-bin/work: missing"), "{p}");
    fs::create_dir_all(d.join("model-bin")).unwrap();
    std::os::unix::fs::symlink("../bin/work", d.join("model-bin/work")).unwrap();
    assert_eq!(spira_config::release_env::model_bin_problems(&d), Vec::<String>::new());
    std::os::unix::fs::symlink("../bin/bdq", d.join("model-bin/bdq")).unwrap();
    let p = spira_config::release_env::model_bin_problems(&d).join("\n");
    assert!(p.contains("model-bin/bdq: not a model binary"), "{p}");
}

// ---------------------------------------------------------------- verify

#[test]
fn verify_catches_tampering_writability_and_a_unit_naming_a_missing_binary() {
    let w = World::with_git(FakeGit {
        extra: vec![("systemd/spira-gone.service".into(), "[Service]\nExecStart=@SPIRA_GONE_BIN@\nExecStartPre=-@SPIRA_PROD@/nope.sh\n".into(), false)],
        ..Default::default()
    });
    w.build(A).unwrap();
    let p = verify::verify(&w.cfg, A, &no_pre()).unwrap().join("\n");
    assert!(p.contains("spira-gone.service: runs") && p.contains("bin/gone"), "{p}");
    assert!(p.contains("nope.sh"), "{p}");

    let w = World::new();
    w.build(A).unwrap();
    fs::set_permissions(w.rel(A).join("shared.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let p = verify::verify(&w.cfg, A, &no_pre()).unwrap().join("\n");
    assert!(p.contains("writable"), "{p}");
    assert!(verify::verify(&w.cfg, "nope", &no_pre()).is_err());
}

#[test]
fn verify_runs_the_releases_own_pre_activate_and_refuses_without_one() {
    let w = World::new();
    w.build(A).unwrap();
    let with = VerifyOpts { pre_activate: true, system_dirs: vec![] };
    let p = verify::verify(&w.cfg, A, &with).unwrap().join("\n");
    assert!(p.contains("pre-activate.sh is missing"), "{p}");

    let ok = World::with_git(FakeGit { extra: vec![("spira/pre-activate.sh".into(), "#!/bin/sh\nexit 0\n".into(), true)], ..Default::default() });
    ok.build(A).unwrap();
    assert_eq!(verify::verify(&ok.cfg, A, &with).unwrap(), Vec::<String>::new());

    let bad = World::with_git(FakeGit { extra: vec![("spira/pre-activate.sh".into(), "#!/bin/sh\necho 'FAIL deps: dolt' >&2\nexit 1\n".into(), true)], ..Default::default() });
    bad.build(A).unwrap();
    let p = verify::verify(&bad.cfg, A, &with).unwrap().join("\n");
    assert!(p.contains("pre-activate failed") && p.contains("FAIL deps: dolt"), "{p}");
}

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

/// sp-vrn3v's acceptance, end to end through a real child process: pre-activate's `deps`
/// check resolves release-tier binaries with `command -v`, so the child must see
/// `SPIRA_RELEASE` and `PATH` naming the release under verification — never whatever the
/// caller inherited. The probe script fails loudly if either one is wrong.
const PATH_PROBE: &str = "#!/bin/sh\nset -eu\n[ \"$SPIRA_RELEASE\" = \"$1\" ] || { echo \"FAIL wrong SPIRA_RELEASE: $SPIRA_RELEASE\" >&2; exit 1; }\ncase \"$PATH\" in \"$1\"/bin:\"$1\"/spira:*) : ;; *) echo \"FAIL wrong PATH: $PATH\" >&2; exit 1;; esac\nt=\"$(command -v tool)\"\n[ \"$t\" = \"$1/bin/tool\" ] || { echo \"FAIL command -v tool resolved to $t\" >&2; exit 1; }\nexit 0\n";

#[test]
fn verify_pre_activate_runs_with_the_release_under_verifications_own_env() {
    let w = World::with_git(FakeGit { extra: vec![("spira/pre-activate.sh".into(), PATH_PROBE.into(), true)], ..Default::default() });
    w.build(A).unwrap();
    let with = VerifyOpts { pre_activate: true, system_dirs: vec![] };
    assert_eq!(verify::verify(&w.cfg, A, &with).unwrap(), Vec::<String>::new());
}

/// The acceptance in the bead's own words: "verifying release B from a shell whose PATH
/// points at release A uses B's binaries." Two releases are built side by side, each with
/// its own `bin/tool`; the *caller's* real `PATH` is pointed at A's `bin/` before verifying
/// B, and the probe script (same as above) still finds B's own `tool` and names B as
/// `SPIRA_RELEASE` — proving the child's env is built from the release passed to `verify`,
/// not inherited. Its `PATH` edit goes through `testkit::env`.

#[test]
fn verify_uses_the_release_under_verification_even_when_the_callers_shell_path_points_at_another_release() {
    let w = World::with_git(FakeGit { extra: vec![("spira/pre-activate.sh".into(), PATH_PROBE.into(), true)], ..Default::default() });
    w.build(A).unwrap();
    w.build(B).unwrap();

    let a_bin = w.rel(A).join("bin");
    let _env = testkit::env(&[("PATH", a_bin.to_str())]);

    let with = VerifyOpts { pre_activate: true, system_dirs: vec![] };
    assert_eq!(verify::verify(&w.cfg, B, &with).unwrap(), Vec::<String>::new());
}

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

/// Every service template this tree ships, rendered against a release, carries that release's
/// launcher PATH, set outright (sp-31gtu), with the box's own tool-directory tail appended
/// after the system directories (sp-c7b85): the acceptance "rendered units carry the release
/// PATH" (both beads), checked against the real templates rather than a fixture. `tail` is
/// `SPIRA_PATH_TAIL` exactly as `host_values` would set it — empty (sp-31gtu's shape) or a
/// `:`-prefixed tail (sp-c7b85's fix).
fn every_shipped_service_carries_path(tail: &str, want_tail: &str) {
    let rel = Path::new("/r/spira-releases").join(A);
    let r = rel.display().to_string();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd");
    let mut host = BTreeMap::new();
    for k in ["SPIRA_RUN", "SPIRA_DB", "SPIRA_DOLT_DATA", "SPIRA_TESTDB_DATA", "SPIRA_TESTDB_PORT", "SPIRA_SNAP_STALE_S", "SPIRA_WATCHTOWER_START_TIMEOUT_S", "DOLT", "SPIRA_INSTANCE", "SPIRA_SCCACHE_DAV_ADDR", "SPIRA_REPO_MAP", "SPIRA_LC_PASSWORD_FILE", "SPIRA_TOML"] {
        host.insert(k.to_string(), format!("/host/{k}"));
    }
    host.insert("SPIRA_PATH_TAIL".to_string(), tail.to_string());
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.ends_with(".service") {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).unwrap();
        let watcher = name.contains('@').then_some("w");
        let out = units::render(&name, &text, &rel, &host, watcher, "prod").unwrap_or_else(|e| panic!("{name}: {e}"));
        if name == "spira-landing-pass.service" {
            assert!(out.lines().any(|l| l == "Environment=SPIRA_REPO_MAP=/host/SPIRA_REPO_MAP"), "{name} must carry the configured repository map");
        }
        let path_lines: Vec<&str> = out.lines().filter(|l| l.starts_with("Environment=PATH=")).collect();
        assert_eq!(
            path_lines,
            [format!("Environment=PATH={r}/bin:{r}/spira:/usr/local/bin:/usr/bin:/bin{want_tail}").as_str()],
            "{name}"
        );
        assert!(out.contains(&format!("\nEnvironment=SPIRA_RELEASE={r}\n")), "{name}");
        for l in out.lines().filter(|l| l.starts_with("ExecStart=")) {
            let prog = l["ExecStart=".len()..].split_whitespace().next().unwrap_or("");
            assert!(
                prog.starts_with(&r) || prog.starts_with("/host/DOLT") || prog == "/bin/bash",
                "{name}: ExecStart runs {prog}, not under the release"
            );
        }
        n += 1;
    }
    assert!(n >= 30, "read the shipped templates ({n})");
}

#[test]
fn every_shipped_service_renders_the_release_path_set_outright() {
    every_shipped_service_carries_path("", "");
}

#[test]
fn every_shipped_service_renders_the_configured_path_tail_after_the_system_dirs() {
    every_shipped_service_carries_path(":/h/.local/bin:/h/.cargo/bin", ":/h/.local/bin:/h/.cargo/bin");
}

/// Every shipped service renders against the REAL `host_values` (not a hand-listed key set), so
/// a placeholder no host value fills is caught the moment a template adds one (sp-qf5x9).
#[test]
fn every_shipped_service_renders_against_real_host_values() {
    let mut env = Env::new();
    env.insert("HOME".into(), "/h".into());
    env.insert("SPIRA_TOML".into(), "/host/cfg.toml".into());
    env.insert("DOLT".into(), "/host/DOLT".into());
    let reg = Registered {
        releases: "/e".into(),
        releases_keep: "10".into(),
        run: "/host/SPIRA_RUN".into(),
        db: "/host/SPIRA_DB".into(),
        dolt_data: "/host/SPIRA_DOLT_DATA".into(),
        testdb_data: "/host/SPIRA_TESTDB_DATA".into(),
        testdb_port: "3308".into(),
        snap_stale_s: "60".into(),
        watchtower_start_timeout_s: "360".into(),
        instance: "/host/SPIRA_INSTANCE".into(),
        repo_map: "/host/SPIRA_REPO_MAP".into(),
        sccache_dav_addr: "/host/SPIRA_SCCACHE_DAV_ADDR".into(),
        ..Default::default()
    };
    let c = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    let host = c.host_values().unwrap();
    let rel = Path::new("/r/spira-releases").join(A);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.ends_with(".service") {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).unwrap();
        let watcher = name.contains('@').then_some("w");
        units::render(&name, &text, &rel, &host, watcher, "prod").unwrap_or_else(|e| panic!("{name}: {e}"));
        n += 1;
    }
    assert!(n >= 30, "read the shipped templates ({n})");
}

// ---------------------------------------------------------------- activate

#[test]
fn activate_swaps_current_rewrites_units_and_restarts_only_changed_long_running_services() {
    let w = World::new();
    w.build(A).unwrap();
    w.build(B).unwrap();
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, A, None).unwrap();
    assert_eq!(w.current().as_deref(), Some(A));
    w.install_units(A);
    sc.restarts.borrow_mut().clear();

    let s = activate::activate(&c, B, None).unwrap();
    assert_eq!(w.current().as_deref(), Some(B));
    assert!(fs::symlink_metadata(w.cfg.releases.join("current")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_link(w.cfg.releases.join("current")).unwrap(), PathBuf::from(B), "current names the sha, relative");
    // Units name the release by sha, never `current`.
    assert!(w.unit("spira-tool-prod.service").contains(&format!("{B}/bin/tool --serve")));
    assert!(!w.unit("spira-tool-prod.service").contains("current"));
    assert!(w.unit("spira-watch-pool-prod.service").contains(&format!("{B}/spira/watchd.sh exec pool")));
    // The timer's rendering does not depend on the release, so it is untouched.
    assert!(!s.rewritten.contains(&"spira-job-prod.timer".to_string()));
    assert_eq!(s.restarted, vec!["spira-tool-prod.service".to_string(), "spira-watch-pool-prod.service".to_string()]);
    // An active oneshot finishes from the old release; an inactive one waits for its timer.
    assert_eq!(s.deferred, vec!["shared.service".to_string(), "spira-job-prod.service".to_string()]);
    assert_eq!(*sc.restarts.borrow(), s.restarted);
    assert!(*sc.reloads.borrow() >= 1);
    let st = w.cfg.state_dir().unwrap();
    assert_eq!(activate::read_history(&st).unwrap().iter().map(|e| e.sha.as_str()).collect::<Vec<_>>(), vec![A, B]);

    // Activating what is already active changes nothing and restarts nothing.
    sc.restarts.borrow_mut().clear();
    let again = activate::activate(&c, B, None).unwrap();
    assert!(again.rewritten.is_empty() && sc.restarts.borrow().is_empty());
    assert_eq!(activate::read_history(&st).unwrap().len(), 2);
}

#[test]
fn activate_rolls_back_when_a_unit_fails_to_come_up() {
    let w = World::new();
    w.build(A).unwrap();
    w.build(B).unwrap();
    let mut sc = FakeSystemctl::new(w.units());
    activate::activate(&ctx(&w, &sc), A, None).unwrap();
    w.install_units(A);
    let before_tool = w.unit("spira-tool-prod.service");
    let before_watch = w.unit("spira-watch-pool-prod.service");

    sc.poison = Some(format!("{B}/bin/tool"));
    let e = activate::activate(&ctx(&w, &sc), B, None).unwrap_err();
    assert!(e.contains("spira-tool-prod.service: did not come up (ActiveState=failed, Result=exit-code)"), "{e}");
    assert!(e.contains(&format!("rolled back to {A}")), "{e}");
    assert!(!e.contains("UNDO INCOMPLETE"), "{e}");
    assert_eq!(w.current().as_deref(), Some(A));
    assert_eq!(w.unit("spira-tool-prod.service"), before_tool);
    assert_eq!(w.unit("spira-watch-pool-prod.service"), before_watch);
    // Both restarted units were restarted again, onto A, and came up.
    let r = sc.restarts.borrow().clone();
    assert_eq!(r.iter().filter(|u| *u == "spira-tool-prod.service").count(), 2, "{r:?}");
    assert_eq!(sc.state("spira-tool-prod.service").unwrap().active, "active");
    // The failed activation is not on the rollback stack.
    let st = w.cfg.state_dir().unwrap();
    assert_eq!(activate::read_history(&st).unwrap().iter().map(|e| e.sha.as_str()).collect::<Vec<_>>(), vec![A]);
}

#[test]
fn a_failed_first_activation_leaves_no_current() {
    let w = World::new();
    w.build(A).unwrap();
    w.build(B).unwrap();
    // Units installed against A by hand, but current never set.
    w.install_units(A);
    let mut sc = FakeSystemctl::new(w.units());
    sc.poison = Some(format!("{B}/bin/tool"));
    activate::activate(&ctx(&w, &sc), B, None).unwrap_err();
    assert_eq!(w.current(), None);
}

#[test]
fn activate_refuses_a_tampered_release_or_an_unrenderable_unit_and_changes_nothing() {
    let w = World::new();
    w.build(A).unwrap();
    w.build(B).unwrap();
    let sc = FakeSystemctl::new(w.units());
    activate::activate(&ctx(&w, &sc), A, None).unwrap();
    w.install_units(A);
    fsutil::make_writable(&w.rel(B));
    fs::write(w.rel(B).join("CLAUDE.md"), "tampered").unwrap();
    let e = activate::activate(&ctx(&w, &sc), B, None).unwrap_err();
    assert!(e.contains("does not match its MANIFEST"), "{e}");
    assert_eq!(w.current().as_deref(), Some(A));

    let w = World::with_git(FakeGit { extra: vec![("systemd/spira-db.service".into(), "[Service]\nEnvironment=SPIRA_DB=@SPIRA_DB@\n".into(), false)], ..Default::default() });
    w.build(A).unwrap();
    fs::write(w.units().join("spira-db-prod.service"), "old\n").unwrap();
    let sc = FakeSystemctl::new(w.units());
    let e = activate::activate(&ctx(&w, &sc), A, None).unwrap_err();
    assert!(e.contains("uses SPIRA_DB but no value") && e.contains("nothing changed"), "{e}");
    assert_eq!(w.current(), None);
    assert_eq!(w.unit("spira-db-prod.service"), "old\n");
}

fn sccache_dav_template() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd/sccache-dav.service")).unwrap()
}

/// THE POSITIVE CONTROL (sp-xtdqi-2): sp-xtdqi correctly declined to INSTALL
/// `sccache-dav.service` on a box that never set `SPIRA_SCCACHE_DAV_ADDR` — but the
/// production box this landed on already had a hand-rendered copy from before the gate
/// existed, and `activate`'s own render loop re-renders whatever is ALREADY on disk, not
/// what the manifest would choose today. That refused the WHOLE activation over one
/// unfillable placeholder. Uses the real shipped template (never a synthetic rewrite), so a
/// future edit to it is covered here too.
#[test]
fn activate_retires_an_already_installed_unit_whose_gate_key_is_unset_rather_than_refusing() {
    let w = World::with_git(FakeGit { extra: vec![("systemd/sccache-dav.service".into(), sccache_dav_template(), false)], ..Default::default() });
    w.build(A).unwrap();
    // A hand-rendered copy already on disk — its exact content does not matter; what
    // matters is that activate neither refuses nor tries to fill its placeholder.
    fs::write(w.units().join("sccache-dav.service"), "[Service]\nEnvironment=SCCACHE_DAV_ADDR=192.168.1.56:9431\n").unwrap();
    let sc = FakeSystemctl::new(w.units());
    let s = activate::activate(&ctx(&w, &sc), A, None).unwrap();
    assert_eq!(w.current().as_deref(), Some(A), "the key being unset must not block activation");
    assert_eq!(s.retired, vec!["sccache-dav.service".to_string()]);
    assert!(s.rewritten.is_empty(), "retired, not rewritten — never rendered at all");
    assert!(!w.units().join("sccache-dav.service").exists(), "removed, not left stale");
    assert_eq!(sc.disabled.borrow().as_slice(), ["sccache-dav.service".to_string()]);
}

/// A box with no sccache-dav.service installed at all (the common case — nothing ever
/// rendered it) activates exactly as before: nothing to retire, nothing refused.
#[test]
fn activate_is_unaffected_by_a_gated_template_that_was_never_installed() {
    let w = World::with_git(FakeGit { extra: vec![("systemd/sccache-dav.service".into(), sccache_dav_template(), false)], ..Default::default() });
    w.build(A).unwrap();
    let sc = FakeSystemctl::new(w.units());
    let s = activate::activate(&ctx(&w, &sc), A, None).unwrap();
    assert_eq!(w.current().as_deref(), Some(A));
    assert!(s.retired.is_empty());
    assert!(!w.units().join("sccache-dav.service").exists());
}

/// THE SECOND HALF of sp-xtdqi-2, updated for the one-source-of-config rule (per Ryan
/// 2026-10-05): `SPIRA_SCCACHE_DAV_ADDR` no longer has an environment-bootstrap path of its
/// own — once the config file declares it, `host_values` carries it through exactly like every
/// other registered key.
#[test]
fn activate_renders_the_address_once_the_gate_key_is_configured() {
    let w = World::with_git_and_reg(
        FakeGit { extra: vec![("systemd/sccache-dav.service".into(), sccache_dav_template(), false)], ..Default::default() },
        Registered { sccache_dav_addr: "192.168.1.56:9431".into(), ..Default::default() },
    );
    w.build(A).unwrap();
    fs::write(w.units().join("sccache-dav.service"), "stale\n").unwrap();
    let sc = FakeSystemctl::new(w.units());
    let s = activate::activate(&ctx(&w, &sc), A, None).unwrap();
    assert_eq!(w.current().as_deref(), Some(A));
    assert!(s.retired.is_empty(), "the gate is open — nothing to retire");
    assert!(s.rewritten.contains(&"sccache-dav.service".to_string()));
    assert!(w.unit("sccache-dav.service").contains("Environment=SCCACHE_DAV_ADDR=192.168.1.56:9431"), "{}", w.unit("sccache-dav.service"));
    assert!(sc.disabled.borrow().is_empty());
}

// ---------------------------------------------------------------- rollback

#[test]
fn rollback_activates_the_previous_release_and_pops_it() {
    let w = World::new();
    for s in [A, B] {
        w.build(s).unwrap();
    }
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, A, None).unwrap();
    w.install_units(A);
    let a_tool = w.unit("spira-tool-prod.service");
    activate::activate(&c, B, None).unwrap();
    assert_eq!(activate::rollback(&c).unwrap(), A);
    assert_eq!(w.current().as_deref(), Some(A));
    assert_eq!(w.unit("spira-tool-prod.service"), a_tool);
    let e = activate::rollback(&c).unwrap_err();
    assert!(e.contains("only release ever activated"), "{e}");
}

#[test]
fn rollback_refuses_when_the_record_and_current_disagree() {
    let w = World::new();
    for s in [A, B] {
        w.build(s).unwrap();
    }
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, A, None).unwrap();
    activate::activate(&c, B, None).unwrap();
    fsutil::atomic_symlink(&w.cfg.releases.join("current"), A).unwrap();
    let e = activate::rollback(&c).unwrap_err();
    assert!(e.contains("disagree"), "{e}");
}

// ---------------------------------------------------------------- hotfix

#[test]
fn a_hotfix_is_recorded_and_shown_as_running_unlanded() {
    let w = World::new();
    for s in [A, B] {
        w.build(s).unwrap();
    }
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, A, None).unwrap();
    assert!(activate::activate(&c, B, Some("  ")).unwrap_err().contains("needs a reason"));
    activate::activate(&c, B, Some("stop the\nworld fix")).unwrap();
    let st = activate::status(&w.cfg).unwrap();
    assert!(st.contains(&format!("RUNNING UNLANDED {B}: stop the world fix")), "{st}");
    assert!(st.contains(&format!("current {B}")) && st.contains(&format!("previous {A}")), "{st}");
}

#[test]
fn a_landing_supersedes_a_hotfix_only_when_the_landed_ref_and_the_new_commit_contain_it() {
    let w = World::new();
    for s in [A, B, C] {
        w.build(s).unwrap();
    }
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, A, Some("hot")).unwrap();
    let state = w.cfg.state_dir().unwrap();

    // B does not contain the hotfix: refused, nothing changed.
    let e = activate::activate(&c, B, None).unwrap_err();
    assert!(e.contains(&format!("RUNNING UNLANDED {A}")) && e.contains(&format!("{B} does not contain it")), "{e}");
    assert_eq!(w.current().as_deref(), Some(A));

    // C contains it, but local/main does not (yet): still refused.
    w.git.contains(A, C);
    let e = activate::activate(&c, C, None).unwrap_err();
    assert!(e.contains("local/main does not contain it"), "{e}");
    assert!(activate::read_hotfix(&state).unwrap().is_some());

    // Once it has landed: superseded, and the record is gone.
    w.git.contains(A, "local/main");
    activate::activate(&c, C, None).unwrap();
    assert_eq!(w.current().as_deref(), Some(C));
    assert_eq!(activate::read_hotfix(&state).unwrap(), None);
    assert!(!activate::status(&w.cfg).unwrap().contains("UNLANDED"));
}

#[test]
fn the_hotfix_rule_fails_closed_without_a_repository() {
    let w = World::new();
    for s in [A, B] {
        w.build(s).unwrap();
    }
    let sc = FakeSystemctl::new(w.units());
    let mut c = ctx(&w, &sc);
    activate::activate(&c, A, Some("hot")).unwrap();
    c.repo = None;
    assert!(activate::activate(&c, B, None).unwrap_err().contains("no --repo"));
    // A second hotfix needs no ancestry: it replaces the first.
    activate::activate(&c, B, Some("hotter")).unwrap();
    assert_eq!(activate::read_hotfix(&w.cfg.state_dir().unwrap()).unwrap().unwrap().sha, B);
}

#[test]
fn rollback_off_a_hotfix_clears_it_and_back_onto_one_restores_it() {
    let w = World::new();
    for s in [A, B, C] {
        w.build(s).unwrap();
    }
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    let state = w.cfg.state_dir().unwrap();
    activate::activate(&c, A, None).unwrap();
    activate::activate(&c, B, Some("hot")).unwrap();
    assert_eq!(activate::rollback(&c).unwrap(), A);
    assert_eq!(activate::read_hotfix(&state).unwrap(), None);

    activate::activate(&c, B, Some("hot")).unwrap();
    w.git.contains(B, C);
    w.git.contains(B, "local/main");
    activate::activate(&c, C, None).unwrap();
    assert_eq!(activate::read_hotfix(&state).unwrap(), None);
    assert_eq!(activate::rollback(&c).unwrap(), B);
    let h = activate::read_hotfix(&state).unwrap().unwrap();
    assert_eq!((h.sha.as_str(), h.reason.as_str()), (B, "hot"));
}

// ---------------------------------------------------------------- prune

#[test]
fn prune_keeps_the_newest_n_and_everything_rollback_or_a_hotfix_needs() {
    let w = World::new(); // keep = 2
    let shas: Vec<String> = (0..5).map(|i| format!("{i}").repeat(40)).collect();
    for s in &shas {
        w.build(s).unwrap();
    }
    // Stamp distinct build times: shas[4] newest.
    for (i, s) in shas.iter().enumerate() {
        let rel = w.rel(s);
        fsutil::make_writable(&rel);
        let mut m = Manifest::load(&rel).unwrap();
        m.built = Some(format!("2026-09-2{i}T00:00:00Z"));
        fs::write(rel.join("MANIFEST"), m.render()).unwrap();
        fsutil::set_readonly(&rel).unwrap();
    }
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, &shas[0], None).unwrap(); // becomes the rollback target
    activate::activate(&c, &shas[1], Some("hot")).unwrap(); // current, and a hotfix
    fs::create_dir_all(w.cfg.releases.join(".stage-x-999999999")).unwrap();
    fs::create_dir_all(w.cfg.releases.join(format!(".stage-x-{}", std::process::id()))).unwrap();

    let p = crate::prune::prune(&w.cfg).unwrap();
    assert!(p.failed.is_empty(), "{:?}", p.failed);
    let mut kept = p.kept.clone();
    kept.sort();
    assert_eq!(kept, vec![shas[0].clone(), shas[1].clone(), shas[3].clone(), shas[4].clone()]);
    assert!(p.removed.contains(&shas[2]) && !w.rel(&shas[2]).exists());
    assert!(p.removed.contains(&".stage-x-999999999".to_string()));
    assert!(w.cfg.releases.join(format!(".stage-x-{}", std::process::id())).exists(), "a live builder's stage is left alone");
}

#[test]
fn prune_keeps_a_release_a_worktree_git_hook_still_names() {
    let w = World::new(); // keep = 2
    let shas: Vec<String> = (0..4).map(|i| format!("{i}").repeat(40)).collect();
    for s in &shas {
        w.build(s).unwrap();
    }
    for (i, s) in shas.iter().enumerate() {
        let rel = w.rel(s);
        fsutil::make_writable(&rel);
        let mut m = Manifest::load(&rel).unwrap();
        m.built = Some(format!("2026-09-2{i}T00:00:00Z"));
        fs::write(rel.join("MANIFEST"), m.render()).unwrap();
        fsutil::set_readonly(&rel).unwrap();
    }
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, &shas[3], None).unwrap();
    let run = w.cfg.run.clone().unwrap();
    let wt = run.join("worktree/a");
    let gitdir = run.join("gitdirs/a");
    fs::create_dir_all(gitdir.join("hooks")).unwrap();
    fs::create_dir_all(&wt).unwrap();
    fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    fs::write(gitdir.join("hooks/pre-commit"), format!("bash \"{}/{}/spira/hooks/pre-commit\"\n", w.cfg.releases.display(), shas[0])).unwrap();

    let p = crate::prune::prune(&w.cfg).unwrap();
    assert!(p.kept.contains(&shas[0]) && w.rel(&shas[0]).exists(), "{p:?}");
    assert!(p.removed.contains(&shas[1]), "{p:?}");
}

#[test]
fn prune_after_activate_keeps_current_and_previous_and_removes_the_rest() {
    let w = World::new(); // keep = 2
    let shas: Vec<String> = (0..6).map(|i| format!("{i}").repeat(40)).collect();
    for (i, s) in shas.iter().enumerate() {
        w.build(s).unwrap();
        let rel = w.rel(s);
        fsutil::make_writable(&rel);
        let mut m = Manifest::load(&rel).unwrap();
        m.built = Some(format!("2026-09-2{i}T00:00:00Z"));
        fs::write(rel.join("MANIFEST"), m.render()).unwrap();
        fsutil::set_readonly(&rel).unwrap();
    }
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    // current + previous are old, so only they survive besides the newest two.
    crate::prune::activate_and_prune(&c, &shas[0], None).unwrap();
    let (_, p) = crate::prune::activate_and_prune(&c, &shas[5], None).unwrap();
    assert!(p.failed.is_empty(), "{:?}", p.failed);
    for i in [0, 4, 5] {
        assert!(w.rel(&shas[i]).exists(), "{i} kept");
    }
    for i in [1, 2, 3] {
        assert!(!w.rel(&shas[i]).exists(), "{i} removed");
    }
}

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

#[test]
fn hotfix_alert_hours_defaults_to_four_and_is_configurable() {
    // SPIRA_HOTFIX_ALERT_HOURS is not a registered key (no spira/conf.d/ entry) — it still
    // reads the raw environment/document, so this test is unchanged by the one-source-of-
    // config migration apart from supplying `reg.releases` directly.
    let mut base = Env::new();
    base.insert("HOME".into(), "/h".into());
    let reg = Registered { releases: "/e".into(), releases_keep: "10".into(), ..Default::default() };

    let cfg = Config::resolve_with(&Flags::default(), &base, None, &reg).unwrap();
    assert_eq!(cfg.hotfix_alert_hours().unwrap(), 4);

    let mut env = base.clone();
    env.insert("SPIRA_HOTFIX_ALERT_HOURS".into(), "1".into());
    let cfg = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert_eq!(cfg.hotfix_alert_hours().unwrap(), 1);

    let toml: spira_config::SpiraToml = spira_config::validate("[spira]\nhotfix_alert_hours = 9\n").unwrap();
    let cfg = Config::resolve_with(&Flags::default(), &base, Some(toml), &reg).unwrap();
    assert_eq!(cfg.hotfix_alert_hours().unwrap(), 9);

    let mut env = base;
    env.insert("SPIRA_HOTFIX_ALERT_HOURS".into(), "nope".into());
    let cfg = Config::resolve_with(&Flags::default(), &env, None, &reg).unwrap();
    assert!(cfg.hotfix_alert_hours().unwrap_err().contains("SPIRA_HOTFIX_ALERT_HOURS"));
}

#[test]
fn status_alerts_only_once_a_standing_hotfix_crosses_the_threshold() {
    let w = World::new(); // default threshold: 4h
    w.build(A).unwrap();
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, A, Some("stop the world")).unwrap();
    let state = w.cfg.state_dir().unwrap();

    // Just under the threshold: RUNNING UNLANDED shows, no ALERT.
    let at = fsutil::rfc3339(fsutil::now_secs() - 3 * 3600 - 1800);
    fs::write(state.join("hotfix"), format!("sha {A}\nreason stop the world\nat {at}\n")).unwrap();
    let st = activate::status(&w.cfg).unwrap();
    assert!(st.contains(&format!("RUNNING UNLANDED {A}: stop the world")), "{st}");
    assert!(!st.contains("ALERT"), "{st}");

    // Past the threshold: ALERT joins the RUNNING UNLANDED line, naming the sha and hours.
    let at = fsutil::rfc3339(fsutil::now_secs() - 5 * 3600);
    fs::write(state.join("hotfix"), format!("sha {A}\nreason stop the world\nat {at}\n")).unwrap();
    let st = activate::status(&w.cfg).unwrap();
    assert!(st.contains(&format!("RUNNING UNLANDED {A}")), "{st}");
    assert!(st.contains(&format!("ALERT hotfix {A} standing 5h >= threshold 4h")), "{st}");
}

#[test]
fn status_never_alerts_when_no_hotfix_stands() {
    let w = World::new();
    w.build(A).unwrap();
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, A, None).unwrap();
    let st = activate::status(&w.cfg).unwrap();
    assert!(!st.contains("UNLANDED") && !st.contains("ALERT"), "{st}");
}

#[test]
fn a_configured_threshold_changes_when_status_alerts() {
    let w = World::new();
    w.build(A).unwrap();
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, A, Some("hot")).unwrap();
    let state = w.cfg.state_dir().unwrap();
    let at = fsutil::rfc3339(fsutil::now_secs() - 2 * 3600);
    fs::write(state.join("hotfix"), format!("sha {A}\nreason hot\nat {at}\n")).unwrap();

    // Default threshold (4h): 2h standing does not alert.
    assert!(!activate::status(&w.cfg).unwrap().contains("ALERT"));

    // A 1h threshold, read from the environment override, does.
    let mut env = Env::new();
    env.insert("SPIRA_UNIT_DIR".into(), w.cfg.unit_dir.display().to_string());
    env.insert("SPIRA_HOTFIX_ALERT_HOURS".into(), "1".into());
    let flags = Flags { releases: Some(w.cfg.releases.clone()), run: w.cfg.run.clone(), keep: Some(2) };
    let reg = Registered { instance: "prod".into(), ..Default::default() };
    let low_cfg = Config::resolve_with(&flags, &env, None, &reg).unwrap();
    assert!(activate::status(&low_cfg).unwrap().contains("ALERT hotfix"));
}

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
fn install_tarball_unpacks_read_only_swaps_current_and_restarts_active_services() {
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    let tb = touch_tarball(&sb, "spira-20260101T000000Z");
    let un = FakeUnpack { name: "spira-20260101T000000Z", files: tarball_bins(), fails: false };

    let r = install::install(&cfg, &sc, &un, &tb, &InstallOpts { dry_run: false, settle: Duration::ZERO, skip_restart: false }).unwrap();

    assert_eq!(r.name, "spira-20260101T000000Z");
    assert!(r.fresh);
    let dir = cfg.releases.join(&r.name);
    assert!(dir.join("bin/loom").exists());
    // Read-only: the fsutil::set_readonly sweep cleared every write bit.
    assert_eq!(fs::metadata(dir.join("bin/loom")).unwrap().permissions().mode() & 0o222, 0);
    assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o222, 0);
    assert_eq!(fs::read_link(cfg.releases.join("current")).unwrap().to_string_lossy(), r.name);
    // FakeSystemctl seeds two active spira-*.service units (spira-tool-prod, spira-watch-pool-prod)
    // plus one active oneshot (spira-job-prod, also a .service); install-tarball restarts every
    // active spira-*.service unconditionally (DESIGN.md "install-tarball"), unlike release
    // activate's changed-ExecStart-only rule.
    let mut restarted = r.restarted.clone();
    restarted.sort();
    assert_eq!(restarted, vec!["spira-job-prod.service", "spira-tool-prod.service", "spira-watch-pool-prod.service"]);
    assert_eq!(*sc.reloads.borrow(), 1);
    assert!(r.restart_failed.is_empty());
    fsutil::make_writable(&dir);
}

#[test]
fn install_tarball_skip_restart_restarts_nothing_and_still_swaps_current() {
    // sp-r15cf: --skip-restart is for a caller that immediately re-renders and restarts
    // what changed (deploy.sh's _render_release_units, the acceptance harness's own
    // install_tarball+install_sh pair) — install-tarball's own restart cannot be what
    // switches a unit onto the new release (every ExecStart is templated with the
    // release's own path, not "current"), so it is pure duplication there, and racing it
    // against the render's restart of the same unit failed one deterministically enough
    // to roll back a release that added nothing wrong (acceptance phase B).
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    let tb = touch_tarball(&sb, "spira-20260101T000000Z");
    let un = FakeUnpack { name: "spira-20260101T000000Z", files: tarball_bins(), fails: false };

    let r = install::install(&cfg, &sc, &un, &tb, &InstallOpts { dry_run: false, settle: Duration::ZERO, skip_restart: true }).unwrap();

    // current still swaps, the release is still unpacked and made read-only — only the
    // restart is skipped.
    assert_eq!(fs::read_link(cfg.releases.join("current")).unwrap().to_string_lossy(), r.name);
    assert!(r.restarted.is_empty());
    assert!(r.restart_failed.is_empty());
    // daemon-reload still runs: a unit whose file has gone should not be left stale.
    assert_eq!(*sc.reloads.borrow(), 1);
    fsutil::make_writable(&cfg.releases.join(&r.name));
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

#[test]
fn install_tarball_never_restarts_a_transient_unit() {
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    sc.states.borrow_mut().insert("spira-landing-prod.service".into(), UnitState { active: "active".into(), result: "success".into(), kind: "simple".into() });
    // Mark it transient (a systemd-run unit with no file of its own — sp-hvtdj).
    let sc = FakeSystemctl { transient: BTreeSet::from(["spira-landing-prod.service".to_string()]), ..sc };
    let tb = touch_tarball(&sb, "spira-20260101T000000Z");
    let un = FakeUnpack { name: "spira-20260101T000000Z", files: tarball_bins(), fails: false };

    let r = install::install(&cfg, &sc, &un, &tb, &InstallOpts { dry_run: false, settle: Duration::ZERO, skip_restart: false }).unwrap();
    assert!(!r.restarted.contains(&"spira-landing-prod.service".to_string()), "{:?}", r.restarted);
}

#[test]
fn install_tarball_reports_a_restart_failure_but_does_not_fail_the_install() {
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    let sc = FakeSystemctl { restart_fails: BTreeSet::from(["spira-tool-prod.service".to_string()]), ..sc };
    let tb = touch_tarball(&sb, "spira-20260101T000000Z");
    let un = FakeUnpack { name: "spira-20260101T000000Z", files: tarball_bins(), fails: false };

    let r = install::install(&cfg, &sc, &un, &tb, &InstallOpts { dry_run: false, settle: Duration::ZERO, skip_restart: false }).unwrap();
    assert_eq!(r.restart_failed.len(), 1);
    assert!(r.restart_failed[0].starts_with("spira-tool-prod.service"), "{:?}", r.restart_failed);
    assert!(r.restarted.contains(&"spira-watch-pool-prod.service".to_string()));
    fsutil::make_writable(&cfg.releases.join(&r.name));
}

#[test]
fn install_tarball_prunes_the_oldest_timestamp_named_releases_beyond_keep_never_current() {
    let (sb, cfg) = install_world();
    let sc = FakeSystemctl::new(cfg.unit_dir.clone());
    fs::create_dir_all(&cfg.releases).unwrap();
    for old in ["spira-20250101T000000Z", "spira-20250201T000000Z", "spira-20250301T000000Z"] {
        fs::create_dir_all(cfg.releases.join(old)).unwrap();
    }
    let tb = touch_tarball(&sb, "spira-20260101T000000Z");
    let un = FakeUnpack { name: "spira-20260101T000000Z", files: tarball_bins(), fails: false };

    // keep=2: the new install plus the newest old one survive; the two oldest are pruned.
    let r = install::install(&cfg, &sc, &un, &tb, &InstallOpts { dry_run: false, settle: Duration::ZERO, skip_restart: false }).unwrap();
    assert!(r.prune_failed.is_empty(), "{:?}", r.prune_failed);
    let mut pruned = r.pruned.clone();
    pruned.sort();
    assert_eq!(pruned, vec!["spira-20250101T000000Z", "spira-20250201T000000Z"]);
    assert!(cfg.releases.join("spira-20250301T000000Z").exists());
    assert!(cfg.releases.join("spira-20260101T000000Z").exists());
    fsutil::make_writable(&cfg.releases.join("spira-20260101T000000Z"));
}


// ---------------------------------------------------------------- stage

#[test]
fn stage_fayth_names_the_canary_persona_with_one_concurrent_slot() {
    let f = crate::stage::fayth_content();
    assert!(f.contains("FAYTH_NAME=canary"));
    assert!(f.contains("FAYTH_MAX_CONCURRENT=1"));
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

/// A `spira-config` standing in for one release's schema: `validate` loads iff every key in
/// `needs` is in the layers `$SPIRA_TOML` names and none of `refuses` is.
fn schema(needs: &[&str], refuses: &[&str]) -> String {
    let mut s = String::from("#!/bin/sh\n[ \"$1\" = validate ] || exit 2\ncfg=$(cat $(echo \"$SPIRA_TOML\" | tr : ' '))\n");
    for k in needs {
        s.push_str(&format!("echo \"$cfg\" | grep -q '{k}' || {{ echo \"config has no {k}\" >&2; exit 1; }}\n"));
    }
    for k in refuses {
        s.push_str(&format!("echo \"$cfg\" | grep -q '{k}' && {{ echo \"spira.{k}: unknown field\" >&2; exit 1; }}\n"));
    }
    s.push_str("exit 0\n");
    s
}

fn delta_git(delta: &str) -> FakeGit {
    FakeGit { validator: true, delta_for: Some((B.to_string(), delta.to_string())), ..Default::default() }
}

fn build_with_schema(w: &World, sha: &str, needs: &[&str], refuses: &[&str]) {
    let cargo = FakeCargo { bins: vec!["tool", "spira-config"], bodies: vec![("spira-config", schema(needs, refuses))] };
    w.build_with(sha, &cargo, vec![]).unwrap();
}

fn loads(w: &World, sha: &str) -> bool {
    std::process::Command::new(w.rel(sha).join("bin/spira-config")).arg("validate").env("SPIRA_TOML", w.cfg.toml_spec().unwrap()).status().unwrap().success()
}

fn cfg_text(w: &World) -> String {
    fs::read_to_string(w.cfg.toml_spec().unwrap()).unwrap()
}

const DELTA_BOTH: &str = "removed = [\"spira.old_key\"]\n[added]\n\"spira.new_key\" = 300\n";

#[test]
fn a_release_that_adds_and_drops_a_key_activates_over_the_other_with_no_hand_edit() {
    let w = World::with_git(delta_git(DELTA_BOTH));
    file(Path::new(&w.cfg.toml_spec().unwrap()), "[spira]\nid_prefix = \"sp\"\nold_key = 1\n");
    build_with_schema(&w, A, &["old_key"], &["new_key"]);
    build_with_schema(&w, B, &["new_key"], &["old_key"]);
    let mut sc = FakeSystemctl::new(w.units());
    let (a, b) = (w.rel(A).join("bin/spira-config"), w.rel(B).join("bin/spira-config"));
    let spec = w.cfg.toml_spec().unwrap();
    sc.probe = Some(Box::new(move || {
        let ok = |p: &Path| std::process::Command::new(p).arg("validate").env("SPIRA_TOML", &spec).status().map(|s| s.success()).unwrap_or(false);
        format!("a={} b={}", ok(&a), ok(&b))
    }));
    let c = ctx(&w, &sc);

    activate::activate(&c, A, None).unwrap();
    assert!(loads(&w, A) && !loads(&w, B), "A on its own config");
    sc.probes.borrow_mut().clear();

    activate::activate(&c, B, None).unwrap();
    assert_eq!(w.current().as_deref(), Some(B));
    assert!(cfg_text(&w).contains("new_key = 300") && !cfg_text(&w).contains("old_key"), "{}", cfg_text(&w));
    assert!(loads(&w, B), "the new release loads the config after the flip");
    assert_eq!(*sc.probes.borrow(), vec!["a=false b=false".to_string()], "at the flip the added key is present and the dropped one not yet removed");

    assert_eq!(activate::rollback(&c).unwrap(), A);
    assert!(cfg_text(&w).contains("old_key = 1") && !cfg_text(&w).contains("new_key"), "{}", cfg_text(&w));
    assert!(loads(&w, A), "the old release loads the config again after rollback");
}

#[test]
fn verify_pre_activate_sees_the_config_with_the_delta_applied_and_leaves_the_files_alone() {
    let probe = "#!/bin/sh\ncat $(echo \"$SPIRA_TOML\" | tr : ' ') | grep -q new_key || { echo 'FAIL units: new_key is not declared' >&2; exit 1; }\nexit 0\n";
    let mut g = delta_git("[added]\n\"spira.new_key\" = 300\n");
    g.extra.push(("spira/pre-activate.sh".into(), probe.into(), true));
    let w = World::with_git(g);
    let before = "[spira]\nid_prefix = \"sp\"\nold_key = 1\n";
    file(Path::new(&w.cfg.toml_spec().unwrap()), before);
    build_with_schema(&w, A, &["old_key"], &[]);
    build_with_schema(&w, B, &["new_key"], &[]);
    let with = VerifyOpts { pre_activate: true, system_dirs: vec![] };
    let p = verify::verify(&w.cfg, B, &with).unwrap();
    assert!(!p.iter().any(|l| l.contains("pre-activate")), "{p:?}");
    assert_eq!(cfg_text(&w), before, "verify applies nothing");
    let sc = FakeSystemctl::new(w.units());
    activate::activate(&ctx(&w, &sc), B, None).unwrap();
    assert!(cfg_text(&w).contains("new_key = 300"));

    let nodelta = World::with_git(FakeGit { extra: vec![("spira/pre-activate.sh".into(), probe.into(), true)], ..Default::default() });
    nodelta.build(A).unwrap();
    let p = verify::verify(&nodelta.cfg, A, &with).unwrap().join("\n");
    assert!(p.contains("new_key is not declared"), "control: without the delta the probe fails: {p}");
}

#[test]
fn config_delta_that_leaves_a_required_key_unset_refuses_naming_it_and_changes_nothing() {
    let w = World::with_git(delta_git("[added]\n\"spira.other\" = 1\n"));
    let before = "[spira]\nid_prefix = \"sp\"\nold_key = 1\n";
    file(Path::new(&w.cfg.toml_spec().unwrap()), before);
    build_with_schema(&w, A, &["old_key"], &[]);
    build_with_schema(&w, B, &["new_key"], &[]);
    let sc = FakeSystemctl::new(w.units());
    let c = ctx(&w, &sc);
    activate::activate(&c, A, None).unwrap();
    let e = activate::activate(&c, B, None).unwrap_err();
    assert!(e.contains("config has no new_key") && e.contains("nothing changed"), "{e}");
    assert_eq!(w.current().as_deref(), Some(A));
    assert_eq!(cfg_text(&w), before);
}

#[test]
fn a_failed_switch_puts_the_config_back() {
    let w = World::with_git(delta_git(DELTA_BOTH));
    let before = "[spira]\nid_prefix = \"sp\"\nold_key = 1\n";
    file(Path::new(&w.cfg.toml_spec().unwrap()), before);
    build_with_schema(&w, A, &["old_key"], &["new_key"]);
    build_with_schema(&w, B, &["new_key"], &["old_key"]);
    let mut sc = FakeSystemctl::new(w.units());
    activate::activate(&ctx(&w, &sc), A, None).unwrap();
    w.install_units(A);
    sc.poison = Some(format!("{B}/bin/tool"));
    activate::activate(&ctx(&w, &sc), B, None).unwrap_err();
    assert_eq!(w.current().as_deref(), Some(A));
    assert_eq!(cfg_text(&w), before);
}

#[test]
fn an_added_key_the_operator_already_set_keeps_the_operators_value() {
    let w = World::with_git(delta_git("[added]\n\"spira.new_key\" = 300\n"));
    file(Path::new(&w.cfg.toml_spec().unwrap()), "[spira]\nid_prefix = \"sp\"\nnew_key = 9\n");
    build_with_schema(&w, B, &["new_key"], &[]);
    let sc = FakeSystemctl::new(w.units());
    activate::activate(&ctx(&w, &sc), B, None).unwrap();
    assert!(cfg_text(&w).contains("new_key = 9"), "{}", cfg_text(&w));
    assert!(!w.cfg.state_dir().unwrap().join("config-undo").join(B).exists(), "nothing changed, so nothing to undo");
}

#[test]
fn an_added_key_held_as_the_empty_string_takes_the_releases_value() {
    let w = World::with_git(delta_git("[added]\n\"spira.new_key\" = \"240\"\n"));
    file(Path::new(&w.cfg.toml_spec().unwrap()), "[spira]\nid_prefix = \"sp\"\nnew_key = \"\"\n");
    build_with_schema(&w, B, &["new_key"], &[]);
    let sc = FakeSystemctl::new(w.units());
    activate::activate(&ctx(&w, &sc), B, None).unwrap();
    assert!(cfg_text(&w).contains("new_key = \"240\""), "{}", cfg_text(&w));
}

#[test]
fn a_malformed_config_delta_refuses_the_activation() {
    for bad in ["[added]\n\"spira.x\" = 1\n[extra]\nk = 1\n", "removed = [\"spira.x\"]\n[added]\n\"spira.x\" = 1\n", "removed = [1]\n", "[added]\n\"spira..x\" = 1\n"] {
        assert!(config_delta::Delta::parse(bad).is_err(), "{bad}");
    }
    let w = World::with_git(delta_git("surprise = 1\n"));
    build_with_schema(&w, B, &[], &[]);
    let sc = FakeSystemctl::new(w.units());
    let e = activate::activate(&ctx(&w, &sc), B, None).unwrap_err();
    assert!(e.contains("config-delta.toml") && e.contains("surprise"), "{e}");
    assert_eq!(w.current(), None);
}

#[test]
fn activate_waits_for_a_running_oneshot_to_exit() {
    let w = World::new();
    w.build(A).unwrap();
    w.build(B).unwrap();
    let sc = FakeSystemctl::new(w.units());
    activate::activate(&ctx(&w, &sc), A, None).unwrap();
    w.install_units(A);
    sc.exits_after.borrow_mut().insert("spira-job-prod.service".into(), 3);
    let mut c = ctx(&w, &sc);
    c.drain = Duration::from_secs(5);
    activate::activate(&c, B, None).unwrap();
    assert_eq!(sc.state("spira-job-prod.service").unwrap().active, "inactive");
    assert!(sc.exits_after.borrow()["spira-job-prod.service"] == 0, "activation polled the oneshot until it exited");
}

// ---------------------------------------------------------------- activation installs new units

fn build_with_unit_ensure(w: &World, sha: &str, body: String) {
    let cargo = FakeCargo { bins: vec!["tool", "unit-ensure"], bodies: vec![("unit-ensure", body)] };
    w.build_with(sha, &cargo, vec![]).unwrap();
}

#[test]
fn activate_runs_the_new_releases_unit_ensure_so_units_it_adds_get_installed() {
    let w = World::with_git(FakeGit { unit_ensure: true, ..Default::default() });
    let marker = w.sb.p().join("ensured");
    build_with_unit_ensure(&w, B, format!("#!/bin/sh\necho \"$SPIRA_HOME $SPIRA_RELEASE\" > {}\n", marker.display()));
    let sc = FakeSystemctl::new(w.units());
    activate::activate(&ctx(&w, &sc), B, None).unwrap();
    let rel = w.rel(B).display().to_string();
    assert_eq!(fs::read_to_string(&marker).unwrap().trim(), format!("{rel}/spira {rel}"), "run from the new release, not the one the caller was on");
}

#[test]
fn a_failing_unit_ensure_is_a_loud_activation_error_and_the_release_stays_active() {
    let w = World::with_git(FakeGit { unit_ensure: true, ..Default::default() });
    build_with_unit_ensure(&w, B, "#!/bin/sh\necho 'cannot render spira-new-prod.timer' >&2\nexit 1\n".into());
    let sc = FakeSystemctl::new(w.units());
    let e = activate::activate(&ctx(&w, &sc), B, None).unwrap_err();
    assert!(e.contains("unit-ensure could not install its new units"), "{e}");
    assert_eq!(w.current().as_deref(), Some(B));
}

#[test]
fn config_for_a_tree_has_its_delta_applied_and_a_key_without_an_entry_stays_absent() {
    let w = World::with_git(delta_git(DELTA_BOTH));
    let spec = w.cfg.toml_spec().unwrap();
    file(Path::new(&spec), "[spira]\nid_prefix = \"sp\"\nold_key = 1\n");
    let tree = w.cfg.releases.join("head-tree");
    file(&tree.join(config_delta::DELTA_PATH), "[added]\n\"spira.new_key\" = 7\n");
    let (dir, staged) = config_delta::staged_for_tree(&spec, &tree).unwrap().expect("the tree has a delta");
    let text = fs::read_to_string(&staged).unwrap();
    assert!(text.contains("new_key = 7") && !text.contains("other_key"), "{text}");
    let _ = fs::remove_dir_all(dir);
    let bare = w.cfg.releases.join("bare-tree");
    fs::create_dir_all(&bare).unwrap();
    assert!(config_delta::staged_for_tree(&spec, &bare).unwrap().is_none(), "no delta, nothing staged");
}

#[test]
fn config_for_resolution_has_the_delta_applied_without_touching_the_files_in_force() {
    let w = World::with_git(delta_git(DELTA_BOTH));
    let before = "[spira]\nid_prefix = \"sp\"\nold_key = 1\n";
    let spec = w.cfg.toml_spec().unwrap();
    file(Path::new(&spec), before);
    build_with_schema(&w, B, &["new_key"], &["old_key"]);
    let (dir, staged) = config_delta::staged_for_resolution(&spec, Some(&w.cfg.releases), B).unwrap().expect("the release has a delta");
    let text = fs::read_to_string(&staged).unwrap();
    assert!(text.contains("new_key = 300") && !text.contains("old_key"), "{text}");
    assert_eq!(cfg_text(&w), before);
    let _ = fs::remove_dir_all(dir);
    assert!(config_delta::staged_for_resolution(&spec, Some(&w.cfg.releases), A).unwrap().is_none(), "no release, no delta, nothing to stage");
}
