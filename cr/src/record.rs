//! One review's record: `<run>/cr/<NNN>/review.json`. `allocate` hands out the number.

use crate::units::Unit;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Scheduled,
    OnDemand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Staged,
    Open,
    Published,
    Closed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Staged => "staged",
            State::Open => "open",
            State::Published => "published",
            State::Closed => "closed",
        }
    }
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Scheduled => "scheduled",
            Kind::OnDemand => "on-demand",
        }
    }
}

/// Every time field is milliseconds since the epoch, whoever writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub number: u32,
    pub kind: Kind,
    pub state: State,
    pub source_repo: String,
    pub source_sha: String,
    pub branch: String,
    pub pr: Option<u64>,
    #[serde(default)]
    pub pr_url: Option<String>,
    pub unit: Unit,
    pub sealed_sha256: Option<String>,
    pub staged_at: u64,
    pub opened_at: Option<u64>,
    pub published_at: Option<u64>,
    pub closed_at: Option<u64>,
}

pub fn root(run: &Path) -> PathBuf {
    run.join("cr")
}

pub fn dir(run: &Path, number: u32) -> PathBuf {
    root(run).join(format!("{number:03}"))
}

pub fn parse_number(text: &str) -> Result<u32, String> {
    match text.parse::<u32>() {
        Ok(n) if text.len() == 3 && n > 0 => Ok(n),
        _ => Err(format!("review number {text:?} is not three digits, as in 001")),
    }
}

/// Claims the next free number with an exclusive `create_dir`, so two opens never share one.
pub fn allocate(run: &Path) -> Result<u32, String> {
    let base = root(run);
    std::fs::create_dir_all(&base).map_err(|e| format!("creating {}: {e}", base.display()))?;
    let taken = std::fs::read_dir(&base)
        .map_err(|e| format!("reading {}: {e}", base.display()))?
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    for n in (taken + 1)..=999 {
        match std::fs::create_dir(dir(run, n)) {
            Ok(()) => return Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("creating {}: {e}", dir(run, n).display())),
        }
    }
    Err("review numbers 001 to 999 are all taken".to_string())
}

pub fn save(run: &Path, r: &Review) -> Result<(), String> {
    let d = dir(run, r.number);
    let text = serde_json::to_string_pretty(r).map_err(|e| e.to_string())?;
    write_atomic(&d.join("review.json"), text.as_bytes())
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("renaming {}: {e}", tmp.display()))
}

pub fn load(run: &Path, number: u32) -> Result<Review, String> {
    let path = dir(run, number).join("review.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("review {number:03}: reading {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("review {number:03}: {}: {e}", path.display()))
}

/// Every review, by number. A claimed number whose record was never written is skipped.
pub fn all(run: &Path) -> Result<Vec<Review>, String> {
    let base = root(run);
    let entries = match std::fs::read_dir(&base) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", base.display())),
    };
    let mut numbers: Vec<u32> = entries.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect();
    numbers.sort_unstable();
    let mut out = Vec::new();
    for n in numbers {
        if dir(run, n).join("review.json").exists() {
            out.push(load(run, n)?);
        }
    }
    Ok(out)
}

pub fn set_state(run: &Path, number: u32, state: State, at: u64) -> Result<Review, String> {
    let mut r = load(run, number)?;
    r.state = state;
    match state {
        State::Staged => {}
        State::Open => r.opened_at = Some(at),
        State::Published => r.published_at = Some(at),
        State::Closed => r.closed_at = Some(at),
    }
    save(run, &r)?;
    Ok(r)
}
