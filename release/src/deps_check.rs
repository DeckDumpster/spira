//! The dependency half of `release verify`: every runtime-tier program in the release's
//! `deps.toml` resolves to an executable on the release's own PATH. Reads files only; no
//! program is started, so a loaded host cannot refuse a green release here.

use spira_config::deps;
use std::path::Path;

const EXIT: &str = "re-run install, or set spira.path";

pub fn problems(rel: &Path, path: &str) -> Vec<String> {
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
        let Some(found) = deps::which(path, &d.name) else {
            out.push(format!("deps: {}: not on PATH; {EXIT}", d.name));
            continue;
        };
        if d.version_min.is_empty() {
            continue;
        }
        match store_version(&found) {
            Some(v) if at_least(&v, &d.version_min) => {}
            Some(v) => out.push(format!("deps: {}: version {v} is below required {}; {EXIT}", d.name, d.version_min)),
            None => out.push(format!("deps: {}: {} records no version, required {}; {EXIT}", d.name, found.display(), d.version_min)),
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
    numbers(got) >= numbers(min)
}
