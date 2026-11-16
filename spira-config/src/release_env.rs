//! Where a process finds its own release's tools and builds a child's `PATH`
//! (`law-a-binary-resolves-the-config-it-reads`, `law-no-fallbacks`): from `$SPIRA_RELEASE`,
//! which every launcher sets, and nothing else. Unset is a refusal naming the key — no
//! inherited `PATH` tier, no search upward from `current_exe()`.

use std::path::{Path, PathBuf};

/// The release directory holding ONLY the binaries a model may run (sp-zf4q3): a sibling of
/// `bin/`, and the one release directory the model's restricted PATH names
/// (`dwarf::restrict`). `bin/` itself also holds ~40 tools that call `bd` themselves (bdq,
/// mail, spira-claim, bead, sentinel, queue, ...), so naming it gave the model a route to
/// the database. Every entry is a relative symlink `<name> -> ../bin/<name>`, made by the
/// release builders (`release::build`, `spira/build-tarball.sh`, testenv's stage script)
/// and checked by `release verify` and acceptance phase A ([`model_bin_problems`]).
pub const MODEL_BIN_DIR: &str = "model-bin";

/// What `MODEL_BIN_DIR` holds: every binary a model may run, and nothing else.
pub const MODEL_BINS: &[&str] = &["work", "testenv"];

/// The one exit from the dwarf's refusal to start a restricted session in a release with no
/// `model-bin/`: set it to a directory you have CONSIDERED and that holds only what a model
/// may run, and the dwarf uses that directory as the model's release PATH entry instead.
/// It is an explicit act, never a fallback: unset, a missing `model-bin/` refuses.
pub const MODEL_BIN_OVERRIDE_ENV: &str = "SPIRA_MODEL_BIN_CONSIDERED";

/// Every way `<release>/model-bin` departs from its contract, for a release whose `bin/`
/// holds at least one of [`MODEL_BINS`]: each such binary must be reachable and executable
/// through `model-bin/<name>`, and `model-bin/` must hold nothing outside [`MODEL_BINS`].
/// Empty for a release that ships none of them (a repository other than the harness).
pub fn model_bin_problems(release: &Path) -> Vec<String> {
    let bin = release.join("bin");
    let wanted: Vec<&str> = MODEL_BINS.iter().copied().filter(|b| std::fs::symlink_metadata(bin.join(b)).is_ok()).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let dir = release.join(MODEL_BIN_DIR);
    let mut problems = Vec::new();
    for b in &wanted {
        let p = dir.join(b);
        let exe = std::fs::metadata(&p).map(|m| m.is_file() && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0).unwrap_or(false);
        if !exe {
            problems.push(format!("{MODEL_BIN_DIR}/{b}: missing or not executable (bin/{b} is shipped, so the model's PATH needs it)"));
        }
    }
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut extra: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| !MODEL_BINS.contains(&n.as_str())).collect();
        extra.sort();
        for n in extra {
            problems.push(format!("{MODEL_BIN_DIR}/{n}: not a model binary — {MODEL_BIN_DIR}/ holds only {}", MODEL_BINS.join(", ")));
        }
    }
    problems
}

/// Links `<release>/model-bin/<name> -> ../bin/<name>` for every [`MODEL_BINS`] entry
/// present in `<release>/bin`. Creates nothing when none is (a non-harness repository).
/// Returns the names linked. Idempotent, and safe to race: a link another caller made first
/// (every suite of a testenv batch may call this on the one staged release, sp-jq4wq) is
/// accepted when it already points at `../bin/<name>`.
pub fn link_model_bin(release: &Path) -> Result<Vec<String>, String> {
    let bin = release.join("bin");
    let present: Vec<&str> = MODEL_BINS.iter().copied().filter(|b| bin.join(b).is_file()).collect();
    if present.is_empty() {
        return Ok(Vec::new());
    }
    let dir = release.join(MODEL_BIN_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for b in &present {
        let link = dir.join(b);
        let target = format!("../bin/{b}");
        if std::fs::symlink_metadata(&link).is_err() {
            match std::os::unix::fs::symlink(&target, &link) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && std::fs::read_link(&link).is_ok_and(|t| t == Path::new(&target)) => {}
                Err(e) => return Err(format!("cannot symlink {MODEL_BIN_DIR}/{b} -> {target}: {e}")),
            }
        }
    }
    Ok(present.iter().map(|s| s.to_string()).collect())
}

/// The release root `$SPIRA_RELEASE` names — the one declared source of where the running
/// release is. Unset or empty is a refusal naming the key; nothing is searched for.
pub fn release_root(value: Option<&str>) -> Result<PathBuf, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(r) => Ok(PathBuf::from(r.trim_end_matches('/'))),
        None => Err(format!("{} is not set — it names the release this process runs from; there is no fallback", crate::RELEASE_ENV)),
    }
}

/// [`release_root`] of this process's `$SPIRA_RELEASE`.
pub fn release_root_for_process() -> Result<PathBuf, String> {
    release_root(std::env::var(crate::RELEASE_ENV).ok().as_deref())
}

/// The `PATH` a launched process runs under, the same one every unit template renders:
/// `<release>/bin:<release>/spira:<spira-env bin>:<system dirs>[:<tail>]`. Built outright
/// from the declared release, `HOME` (whose pinned tool environment it names) and the
/// configured `SPIRA_PATH` tail — never from an inherited `PATH`.
pub fn launcher_path(release: Option<&str>, home: Option<&str>, tail: &str) -> Result<String, String> {
    let release = release_root(release)?;
    let home = home.filter(|h| !h.is_empty()).ok_or_else(|| "HOME is not set — the child PATH names its spira-env bin; there is no fallback".to_string())?;
    let r = release.display();
    let base = format!("{r}/bin:{r}/spira:{}:{}", crate::spira_env_bin(home), crate::SYSTEM_PATH);
    let tail = tail.trim();
    if tail.is_empty() {
        return Ok(base);
    }
    let refusals = crate::tail_refusals(tail);
    if !refusals.is_empty() {
        return Err(refusals.join("; "));
    }
    Ok(format!("{base}:{tail}"))
}

/// `PATH` and `SPIRA_RELEASE` for a child process: [`launcher_path`] and the release it names.
pub fn child_path_env(release: Option<&str>, home: Option<&str>, tail: &str) -> Result<Vec<(String, String)>, String> {
    let path = launcher_path(release, home, tail)?;
    let release = release_root(release)?;
    Ok(vec![(crate::RELEASE_ENV.to_string(), release.display().to_string()), ("PATH".to_string(), path)])
}

/// `PATH`/`SPIRA_RELEASE` for a child run against a release being BUILT: `stage` is the release it
/// runs from, and its PATH is that release plus the system directories ([`crate::release_path`]).
/// Declared by the builder, so a build needs no installed release and no config.
pub fn stage_child_path_env(stage: &Path) -> Vec<(String, String)> {
    let s = stage.display().to_string();
    vec![(crate::RELEASE_ENV.to_string(), s.clone()), ("PATH".to_string(), crate::release_path(&s))]
}

/// [`child_path_env`] for THIS process: its `$SPIRA_RELEASE`, `$HOME` and configured
/// `SPIRA_PATH` — what every spawn of a release tool or a `conf.sh`-sourcing shell sets,
/// e.g. `cmd.envs(child_path_env_for_process()?)`.
pub fn child_path_env_for_process() -> Result<Vec<(String, String)>, String> {
    let tail = crate::process::cfg("SPIRA_PATH")?;
    child_path_env(std::env::var(crate::RELEASE_ENV).ok().as_deref(), std::env::var("HOME").ok().as_deref(), &tail)
}

/// Sets this process's own `PATH` to [`child_path_env_for_process`]'s, so every child it
/// spawns by bare name resolves from the declared release. Call it first thing in `main`,
/// before any thread starts.
pub fn ensure_release_path() -> Result<(), String> {
    for (k, v) in child_path_env_for_process()? {
        std::env::set_var(k, v);
    }
    Ok(())
}

/// The program to exec for a release-shipped tool: its file in `<release>/bin` or
/// `<release>/spira`, where the release is `$SPIRA_RELEASE`. Unset, or a tool the release does
/// not ship, is a refusal naming the key — never a `PATH` lookup or a bare name.
pub fn resolve_tool(name: &str, release: Option<&str>) -> Result<String, String> {
    let root = release_root(release).map_err(|e| format!("cannot resolve {name}: {e}"))?;
    let runnable = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.is_file() && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0);
    for dir in ["bin", "spira"] {
        let p = root.join(dir).join(name);
        if runnable(&p) {
            return Ok(p.display().to_string());
        }
    }
    Err(format!("{name} is not in {} ({}) bin/ or spira/", crate::RELEASE_ENV, root.display()))
}

/// [`resolve_tool`] for this process: how every exec of a release-shipped tool names its
/// program (`spira-lint`'s `release-tool-exec` refuses a bare name).
pub fn tool(name: &str) -> Result<String, String> {
    resolve_tool(name, std::env::var(crate::RELEASE_ENV).ok().as_deref())
}

#[cfg(test)]
mod tests;
