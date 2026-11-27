//! The release's pinned tool environment: `flake.nix`'s `spira-env`, linked at
//! `~/.local/lib/spira-env`, through which every unit's PATH resolves its tools.

use crate::fsutil;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub use spira_config::flake_digest;

pub trait Nix {
    /// Builds `target` and links the result at `out`; `out` is untouched when the build fails.
    fn build(&self, target: &str, out: &Path) -> Result<(), String>;
    fn add_root(&self, root: &Path, store_path: &Path) -> Result<(), String>;
}

/// Runs `nix` or `nix-store` with these arguments.
pub type Runner<'a> = &'a dyn Fn(&str, &[&str]) -> Result<(), String>;

/// [`Nix`] over a command runner.
pub struct Argv<'a>(pub Runner<'a>);

impl Nix for Argv<'_> {
    fn build(&self, target: &str, out: &Path) -> Result<(), String> {
        (self.0)("nix", &["--extra-experimental-features", "nix-command flakes", "build", target, "-o", &out.display().to_string()])
    }
    fn add_root(&self, root: &Path, store_path: &Path) -> Result<(), String> {
        (self.0)("nix-store", &["--add-root", &root.display().to_string(), "--indirect", "--realise", &store_path.display().to_string()])
    }
}

/// nix from `$SPIRA_NIX_PROFILE_BIN`: a unit's PATH has none.
pub struct RealNix;

impl Nix for RealNix {
    fn build(&self, target: &str, out: &Path) -> Result<(), String> {
        Argv(&run_from_profile).build(target, out)
    }
    fn add_root(&self, root: &Path, store_path: &Path) -> Result<(), String> {
        Argv(&run_from_profile).add_root(root, store_path)
    }
}

fn run_from_profile(prog: &str, args: &[&str]) -> Result<(), String> {
    let bin = spira_config::process::nix_profile_program(prog)?;
    // batch-job: nix build of the release's tool env
    let out = Command::new(&bin).args(args).output().map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    verdict(&bin, &out)
}

/// A failed run carries nix's own words: they are all that says why the env did not build.
pub(crate) fn verdict(bin: &Path, out: &Output) -> Result<(), String> {
    if out.status.success() {
        return Ok(());
    }
    Err(format!("{} failed ({}):\n{}", bin.display(), out.status, String::from_utf8_lossy(&out.stderr).trim_end()))
}

/// What a build did to the env link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    /// The store path the link held before; `None` when there was no env.
    pub before: Option<PathBuf>,
    pub after: PathBuf,
    /// The source stamp as it was before the build; `None` when there was none.
    pub stamp_before: Option<String>,
}

impl Built {
    /// The env this build displaced; `None` when there was none or the build left it linked.
    pub fn replaced(&self) -> Option<&Path> {
        self.before.as_deref().filter(|before| *before != self.after)
    }
}

pub fn has_flake(release_root: &Path) -> bool {
    release_root.join("flake.nix").is_file() && release_root.join("flake.lock").is_file()
}

/// The flake reference for the release's `spira-env`. The root is resolved through its symlinks:
/// nix copies a symlinked `path:` root into the store as a symlink, and a relative target (the
/// `current` link to `spira-<name>`) then resolves against `/nix/store`, which pure evaluation refuses.
pub fn target(release_root: &Path) -> String {
    let resolved = std::fs::canonicalize(release_root).unwrap_or_else(|_| release_root.to_path_buf());
    format!("path:{}#spira-env", resolved.display())
}

/// Builds `release_root`'s env into `home`'s env link, roots the env it replaced and stamps the
/// release's flake digest beside it. On `Err` the link is as it was. An unchanged lock rebuilds to the same store path.
pub fn build(nix: &dyn Nix, release_root: &Path, home: &Path) -> Result<Built, String> {
    let out = home.join(spira_config::SPIRA_ENV_REL);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let before = std::fs::canonicalize(&out).ok();
    let digest = flake_digest(release_root)?;
    let stamp_before = std::fs::read_to_string(home.join(spira_config::SPIRA_ENV_SOURCE_REL)).ok();
    nix.build(&target(release_root), &out)?;
    let after = std::fs::canonicalize(&out).map_err(|e| format!("the build left no env at {}: {e}", out.display()))?;
    let built = Built { before, after, stamp_before };
    if let Some(old) = built.replaced() {
        let prev = home.join(spira_config::SPIRA_ENV_PREV_REL);
        if let Err(e) = nix.add_root(&prev, old) {
            return Err(format!("cannot root the previous spira-env {}: {e}{}", old.display(), put_back(home, &built, &digest)));
        }
    }
    if let Err(e) = write_stamp(home, &digest) {
        return Err(format!("{e}{}", put_back(home, &built, &digest)));
    }
    Ok(built)
}

fn write_stamp(home: &Path, digest: &str) -> Result<(), String> {
    fsutil::write_atomic(&home.join(spira_config::SPIRA_ENV_SOURCE_REL), &format!("{digest}\n"))
}

/// Restores the link a failed build moved; "" or why it still names the new env.
fn put_back(home: &Path, built: &Built, digest: &str) -> String {
    let Some(old) = built.replaced() else { return String::new() };
    let Err(e) = restore(home, old) else { return String::new() };
    // The old stamp would describe an env that is no longer linked: stamp the new one, or none.
    if write_stamp(home, digest).is_err() {
        let _ = std::fs::remove_file(home.join(spira_config::SPIRA_ENV_SOURCE_REL));
    }
    format!("; the env link still names {}: {e}", built.after.display())
}

/// Puts the source stamp back to what it was before `built`.
pub fn restore_stamp(home: &Path, built: &Built) -> Result<(), String> {
    let stamp = home.join(spira_config::SPIRA_ENV_SOURCE_REL);
    match &built.stamp_before {
        Some(text) => fsutil::write_atomic(&stamp, text),
        None => match std::fs::remove_file(&stamp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("cannot remove {}: {e}", stamp.display())),
            _ => Ok(()),
        },
    }
}

/// Atomically points the env link back at `before`.
pub fn restore(home: &Path, before: &Path) -> Result<(), String> {
    fsutil::atomic_symlink(&home.join(spira_config::SPIRA_ENV_REL), &before.display().to_string())
}

/// The task name of the garbage-collection unit an install or a switch starts; no scheduler task
/// owns it, so every task ignores its `unit.ran`.
pub const NIX_GC_UNIT_TASK: &str = "nix-gc";

/// What the collection unit needs besides the env: where `nix-collect-garbage` may live, where its
/// output goes, and the key that makes one release start one unit.
#[derive(Debug, Clone)]
pub struct GcPlan {
    pub profile_bin: PathBuf,
    pub log: PathBuf,
    pub key: String,
}

impl GcPlan {
    pub fn for_release(profile_bin: &Path, run_dir: &Path, root: &Path) -> GcPlan {
        GcPlan { profile_bin: profile_bin.to_path_buf(), log: run_dir.join("nix-gc.log"), key: root_key(root) }
    }
}

pub fn gc_plan(profile_bin: Result<String, String>, run_dir: Result<String, String>, root: &Path) -> Result<GcPlan, String> {
    Ok(GcPlan::for_release(Path::new(&profile_bin?), Path::new(&run_dir?), root))
}

pub fn root_key(root: &Path) -> String {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// What a switch needs to start a collection; its [`GcPlan`] is keyed by the release switched to.
pub struct Collector<'a> {
    pub units: &'a dyn runtime::units::Units,
    pub profile_bin: PathBuf,
    pub run_dir: PathBuf,
}

/// Starts `nix-collect-garbage` as a background unit and returns at once. Without `-d` it removes
/// only store paths no GC root reaches, so the current env, the rooted previous env and every
/// path a running process holds survive. `Ok` is the started unit's name, `Err` the warning.
pub fn collect_nix_garbage(units: &dyn runtime::units::Units, gc: &GcPlan, env: &Path) -> Result<String, String> {
    use runtime::units::{UnitProps, UnitSpec};
    let Some(bin) = [gc.profile_bin.clone(), env.join("bin")].into_iter().map(|d| d.join("nix-collect-garbage")).find(|p| fsutil::is_executable(p)) else {
        return Err("nix garbage will not be collected: nix-collect-garbage was found neither in the profile nor in the spira-env".into());
    };
    let spec = UnitSpec { task: NIX_GC_UNIT_TASK.into(), key: gc.key.clone(), argv: vec![bin.display().to_string()], env: vec![], cwd: None };
    let props = UnitProps { cpu_quota_pct: Some(20), nice: Some(10), io_class: Some("idle".into()), runtime_max_ms: Some(1_800_000), output_append: Some(gc.log.clone()), ..UnitProps::default() };
    units.start_with(&spec, &props).map(|h| h.name).map_err(|e| format!("nix garbage collection did not start: {e}"))
}
