//! The dependency half of `release verify`: every runtime-tier program in the release's
//! `deps.toml` resolves to an executable on the release's own PATH, except that a
//! `provider = "spira-env"` row of a flake release resolves in the env that flake builds. Reads
//! files only; no program is started, so a loaded host cannot refuse a green release here.

use spira_config::deps;
use std::path::Path;

const EXIT: &str = "re-run install, or set spira.path";
const FLAKE_EXIT: &str = "fix the release's flake.nix or flake.lock";

/// Where a release's `spira-env` rows resolve.
#[derive(Debug, Clone, Copy)]
pub enum SpiraEnv<'a> {
    /// No flake: activation keeps the installed env, which is on `path`.
    Installed,
    /// The `bin` of the env the release's flake built.
    Built(&'a Path),
    /// The flake did not build: no env can answer for these rows.
    Unbuilt,
}

pub fn problems(rel: &Path, path: &str, env: SpiraEnv) -> Vec<String> {
    let file = rel.join("spira/deps.toml");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => return vec![format!("deps: {}: unreadable: {e}", file.display())],
    };
    let manifest = match deps::parse(&text) {
        Ok(m) => m,
        Err(e) => return vec![format!("deps: {}: unparseable: {}", file.display(), e.replace('\n', " "))],
    };
    let mut out = Vec::new();
    for d in manifest.deps.iter().filter(|d| d.tier == "runtime") {
        let (searched, missing, exit) = match env {
            SpiraEnv::Built(bin) if d.provider == "spira-env" => (bin.display().to_string(), "not in the release's spira-env", FLAKE_EXIT),
            SpiraEnv::Unbuilt if d.provider == "spira-env" => continue,
            _ => (path.to_string(), "not on PATH", EXIT),
        };
        let Some(found) = deps::which(&searched, &d.name) else {
            out.push(format!("deps: {}: {missing}; {exit}", d.name));
            continue;
        };
        if d.version_min.is_empty() {
            continue;
        }
        match store_version(&found) {
            Some(v) if at_least(&v, &d.version_min) => {}
            Some(v) => out.push(format!("deps: {}: version {v} is below required {}; {exit}", d.name, d.version_min)),
            None => out.push(format!("deps: {}: {} records no version, required {}; {exit}", d.name, found.display(), d.version_min)),
        }
    }
    out
}

fn store_version(exe: &Path) -> Option<String> {
    let real = std::fs::canonicalize(exe).ok()?;
    let mut parts = real.components();
    parts.find(|c| c.as_os_str() == "store")?;
    let dir = parts.next()?.as_os_str().to_str()?.to_string();
    let b = dir.as_bytes();
    let at = (0..b.len().saturating_sub(1)).find(|&i| b[i] == b'-' && b[i + 1].is_ascii_digit())?;
    Some(dir[at + 1..].to_string())
}

fn numbers(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

fn at_least(got: &str, min: &str) -> bool {
    let (mut got, mut min) = (numbers(got), numbers(min));
    let n = got.len().max(min.len());
    got.resize(n, 0);
    min.resize(n, 0);
    got >= min
}
