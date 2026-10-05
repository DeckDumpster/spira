//! `release build <commit>` (DESIGN.md "build").

use crate::config::Config;
use crate::fsutil;
use crate::git::Git;
use crate::manifest::{self, Manifest};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Compiles the workspace in `tree` into the target directory `target`. A trait so the rest of
/// build is tested without cargo.
pub trait Cargo {
    fn build(&self, tree: &Path, target: &Path) -> Result<(), String>;
}

pub struct RealCargo;

impl Cargo for RealCargo {
    fn build(&self, tree: &Path, target: &Path) -> Result<(), String> {
        // Through the box's compilation cache (sp-z61hj; spira-config/DESIGN-build-cache.md):
        // absent sccache refuses. The target is a `--target-dir` argument, never the
        // CARGO_TARGET_DIR variable, which sccache hashes into every key — two releases built
        // that way would share no dependency.
        let wrapper = spira_config::build::wrapper_from_env()?;
        if wrapper == spira_config::build::Wrapper::Off {
            eprintln!("release: {}", wrapper.describe());
        }
        // THE SHARED STORE (sp-xtdqi): resolved in-process from this binary's own
        // environment and the operator's own config, exactly like `wrapper_from_env` itself — never a bare
        // `~/.cargo/config.toml` dependency, which only ever reached a build that happened to
        // run under a shell that had sourced it.
        let store = spira_config::build::Store::from_env();
        let st = Command::new("cargo")
            .args(["build", "--release", "--workspace", "--locked", "--target-dir"])
            .arg(target)
            .args(spira_config::build::one_shot("release"))
            .envs(wrapper.env(store.as_ref()))
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_INCREMENTAL")
            .current_dir(tree)
            .stdout(std::io::stderr())
            .status()
            .map_err(|e| format!("cannot run cargo: {e}"))?;
        if !st.success() {
            return Err(format!("cargo build --release --workspace --locked failed ({st})"));
        }
        Ok(())
    }
}

pub struct BuildOpts<'a> {
    pub repo: &'a Path,
    pub commit: &'a str,
    /// Where cargo's target directory lives; `None` means `$SPIRA_RUN/release/target`, else a
    /// throw-away directory beside the stage.
    pub target_dir: Option<PathBuf>,
    /// The directories whose commands a release may not shadow ([`crate::SYSTEM_DIRS`]).
    pub system_dirs: Vec<PathBuf>,
    /// `--bin-dir`: take the binaries from this directory (a round's own tested build of this
    /// commit) instead of running cargo (law-deploy-the-tested-artifacts). The caller vouches
    /// that it holds `<commit>`'s build; the same every-declared-binary rule applies.
    pub bin_dir: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Built {
    pub sha: String,
    pub dir: PathBuf,
    /// False when `spira-releases/<sha>` already existed and nothing was built.
    pub fresh: bool,
}

/// Removes a directory (read-only or not) when dropped, unless disarmed.
struct Cleanup(Option<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fsutil::remove_tree(&p);
        }
    }
}

pub fn build(cfg: &Config, git: &dyn Git, cargo: &dyn Cargo, o: &BuildOpts) -> Result<Built, String> {
    let sha = git.resolve(o.repo, o.commit)?;
    let dir = cfg.releases.join(&sha);
    if fs::symlink_metadata(&dir).is_ok() {
        eprintln!("release: {sha} already built at {} — releases are immutable; not rebuilding", dir.display());
        return Ok(Built { sha, dir, fresh: false });
    }
    fs::create_dir_all(&cfg.releases).map_err(|e| format!("cannot create {}: {e}", cfg.releases.display()))?;
    let pid = std::process::id();
    let stage = cfg.releases.join(format!(".stage-{sha}-{pid}"));
    let _ = fsutil::remove_tree(&stage);
    fs::create_dir(&stage).map_err(|e| format!("cannot create {}: {e}", stage.display()))?;
    let mut stage_guard = Cleanup(Some(stage.clone()));

    git.archive(o.repo, &sha, &stage)?;
    for k in manifest::HEADER_KEYS {
        if fs::symlink_metadata(stage.join(k)).is_ok() {
            return Err(format!("{sha} tracks a top-level file named {k:?}, which MANIFEST reserves as a header key"));
        }
    }
    if fs::symlink_metadata(stage.join("bin")).is_ok() {
        return Err(format!("{sha} tracks bin/, which a release reserves for its binaries"));
    }
    if fs::symlink_metadata(stage.join(spira_config::release_env::MODEL_BIN_DIR)).is_ok() {
        return Err(format!("{sha} tracks {}/, which a release reserves for the model's binaries", spira_config::release_env::MODEL_BIN_DIR));
    }

    // GENERATED CONFIG (sp-5fw50): spira/conf.d.*.generated.sh are gitignored — derived from
    // spira/conf.d/, never committed — so `git archive` does not carry them, and a read-only
    // release cannot regenerate them lazily. spira/build-tarball.sh runs conf-gen.sh against
    // its stage; this path never did, and round 133's release failed pre-activate on it.
    regenerate_config(&stage).map_err(|e| format!("{sha}: {e}"))?;

    // The binaries: a named tested build (--bin-dir), or cargo's own build of the stage.
    let (out, _target_guard) = match &o.bin_dir {
        Some(d) => {
            eprintln!("release: {sha} takes its binaries from {} (no cargo build)", d.display());
            (d.clone(), Cleanup(None))
        }
        None => {
            let (target, guard) = match (&o.target_dir, &cfg.run) {
                (Some(t), _) => (t.clone(), Cleanup(None)),
                (None, Some(run)) => (run.join("release").join("target"), Cleanup(None)),
                (None, None) => {
                    let t = cfg.releases.join(format!(".target-{pid}"));
                    (t.clone(), Cleanup(Some(t)))
                }
            };
            fs::create_dir_all(&target).map_err(|e| format!("cannot create {}: {e}", target.display()))?;
            eprintln!("release: building {sha} (target {})", target.display());
            cargo.build(&stage, &target)?;
            (target.join("release"), guard)
        }
    };

    let bins = crate::workspace::expected_bins(&stage)?;
    if bins.is_empty() {
        return Err(format!("{sha}: the workspace declares no [[bin]] targets"));
    }
    let bin_dir = stage.join("bin");
    fs::create_dir(&bin_dir).map_err(|e| format!("cannot create {}: {e}", bin_dir.display()))?;
    let mut missing = Vec::new();
    for b in &bins {
        let src = out.join(b);
        if !fsutil::is_executable(&src) {
            missing.push(b.clone());
            continue;
        }
        fs::copy(&src, bin_dir.join(b)).map_err(|e| format!("cannot copy {} into bin/: {e}", src.display()))?;
    }
    if !missing.is_empty() {
        return Err(format!("the workspace declares {} but the build at {} did not produce {}", missing.join(", "), out.display(), if missing.len() == 1 { "it" } else { "them" }));
    }

    // COMPAT NAMES (sp-6onps-compat, a P0): spira/deps.toml's [[compat]] table, read the
    // same way spira/build-tarball.sh reads it, so a release built by either path carries
    // every compat name in bin/ and spira/ — this was the one path (`queue land-local` →
    // `release build --bin-dir`) that never did.
    crate::compat::link(&stage, &bin_dir)?;

    // MODEL-BIN (sp-zf4q3): the one release directory the model's restricted PATH names —
    // only `work`, linked to ../bin/work — so no tool in bin/ that calls bd is reachable by
    // name from inside an aeon's session.
    spira_config::release_env::link_model_bin(&stage).map_err(|e| format!("{sha}: {e}"))?;

    let clashes = clashes(&stage, &o.system_dirs);
    if !clashes.is_empty() {
        return Err(format!("{sha} would shadow system commands: {}", clashes.join("; ")));
    }

    let m = Manifest { commit: sha.clone(), built: Some(fsutil::now_rfc3339()), repo: cfg.home_repo(), entries: Manifest::scan(&stage)? };
    fs::write(stage.join(manifest::FILE), m.render()).map_err(|e| format!("cannot write MANIFEST: {e}"))?;
    fsutil::set_readonly(&stage)?;
    fs::rename(&stage, &dir).map_err(|e| format!("cannot move {} to {}: {e}", stage.display(), dir.display()))?;
    stage_guard.0 = None;
    eprintln!("release: built {sha}: {} binaries, {} files", bins.len(), m.entries.len());
    Ok(Built { sha, dir, fresh: true })
}

/// Run `spira/conf-gen.sh` against a staged tree when it has one, refusing on failure. A
/// tree without it (any repo but the harness) needs nothing generated.
pub fn regenerate_config(stage: &Path) -> Result<(), String> {
    let gen = stage.join("spira/conf-gen.sh");
    if !gen.is_file() {
        return Ok(());
    }
    let out = Command::new("bash").envs(spira_config::release_env::child_path_env_for_process()).arg(&gen).current_dir(stage).output().map_err(|e| format!("cannot run {}: {e}", gen.display()))?;
    if !out.status.success() {
        return Err(format!(
            "conf-gen.sh failed against the staged tree ({}) — refusing to build a release with no generated config: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Executables directly in `<rel>/bin` or `<rel>/spira` whose name is also a command in one
/// of `system_dirs` (DESIGN.md "Name clashes"), as `<rel path> shadows <system path>`.
pub fn clashes(rel: &Path, system_dirs: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for sub in ["bin", "spira"] {
        let Ok(rd) = fs::read_dir(rel.join(sub)) else { continue };
        let mut names: Vec<String> = rd.flatten().filter(|e| fsutil::is_executable(&e.path())).map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        for n in names {
            for d in system_dirs {
                let sys = d.join(&n);
                if fs::symlink_metadata(&sys).is_ok() {
                    out.push(format!("{sub}/{n} shadows {}", sys.display()));
                }
            }
        }
    }
    out
}
