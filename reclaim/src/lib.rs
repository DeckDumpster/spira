//! Lossless build-cache reclamation. Everything removed here is rebuildable.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const TMP_GRACE_SECS: u64 = 3600;

pub fn wild(pat: &str, text: &str) -> bool {
    match pat.split_once('*') {
        None => pat == text,
        Some((head, rest)) => {
            let Some(tail) = text.strip_prefix(head) else { return false };
            (0..=tail.len()).filter(|i| tail.is_char_boundary(*i)).any(|i| wild(rest, &tail[i..]))
        }
    }
}

pub fn glob(pattern: &str) -> Vec<PathBuf> {
    let mut cur = vec![PathBuf::from(if pattern.starts_with('/') { "/" } else { "." })];
    for comp in pattern.split('/').filter(|c| !c.is_empty()) {
        let mut next = Vec::new();
        for base in &cur {
            if !comp.contains('*') {
                let p = base.join(comp);
                if fs::symlink_metadata(&p).is_ok() {
                    next.push(p);
                }
                continue;
            }
            let Ok(rd) = fs::read_dir(base) else { continue };
            let mut names: Vec<_> = rd.flatten().map(|e| e.file_name()).collect();
            names.sort();
            next.extend(names.into_iter().filter(|n| wild(comp, &n.to_string_lossy())).map(|n| base.join(n)));
        }
        cur = next;
    }
    cur
}

pub struct Usage {
    pub bytes: u64,
    pub newest: u64,
}

pub fn usage(p: &Path) -> Usage {
    let mut u = Usage { bytes: 0, newest: 0 };
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&d) else { continue };
        u.bytes += md.blocks() * 512;
        u.newest = u.newest.max(md.mtime().max(0) as u64);
        if md.is_dir() {
            if let Ok(rd) = fs::read_dir(&d) {
                stack.extend(rd.flatten().map(|e| e.path()));
            }
        }
    }
    u
}

pub fn protected(c: &Path) -> &Path {
    match (c.file_name().and_then(|n| n.to_str()), c.parent()) {
        (Some("target"), Some(parent)) => parent,
        _ => c,
    }
}

/// Paths a live process stands in: cwd, open files, and the CARGO_TARGET_DIR it was handed.
pub fn held_paths(proc_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(proc_root) else { return out };
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let d = e.path();
        out.extend(fs::read_link(d.join("cwd")));
        if let Ok(fds) = fs::read_dir(d.join("fd")) {
            out.extend(fds.flatten().filter_map(|f| fs::read_link(f.path()).ok()));
        }
        if let Ok(env) = fs::read(d.join("environ")) {
            for kv in env.split(|b| *b == 0) {
                if let Some(v) = kv.strip_prefix(b"CARGO_TARGET_DIR=") {
                    out.push(PathBuf::from(String::from_utf8_lossy(v).into_owned()));
                }
            }
        }
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Reclaim,
    Skip(String),
}

pub struct Candidate {
    pub path: PathBuf,
    pub bytes: u64,
    pub age: u64,
    pub verdict: Verdict,
}

pub fn judge(c: &Path, now: u64, idle_secs: u64, held: &[PathBuf], lease_dir: Option<&Path>) -> Candidate {
    let u = usage(c);
    let age = now.saturating_sub(u.newest);
    let prot = protected(c);
    let verdict = if fs::symlink_metadata(c).map(|m| m.file_type().is_symlink()).unwrap_or(true) {
        Verdict::Skip("symlink".into())
    } else if held.iter().any(|h| h.starts_with(prot)) {
        Verdict::Skip("held by a live process".into())
    } else if lease_dir.zip(prot.file_name()).is_some_and(|(d, n)| d.join(format!("{}.lease", n.to_string_lossy())).exists()) {
        Verdict::Skip("leased".into())
    } else if age < idle_secs {
        Verdict::Skip(format!("touched {age}s ago"))
    } else {
        Verdict::Reclaim
    };
    Candidate { path: c.to_path_buf(), bytes: u.bytes, age, verdict }
}

/// Reclaimable candidates, oldest first.
pub fn plan(patterns: &[String], now: u64, idle_secs: u64, held: &[PathBuf], lease_dir: Option<&Path>) -> Vec<Candidate> {
    let mut v: Vec<Candidate> = patterns.iter().flat_map(|p| glob(p)).filter(|p| p.is_dir()).map(|p| judge(&p, now, idle_secs, held, lease_dir)).collect();
    v.sort_by(|a, b| b.age.cmp(&a.age));
    v
}

pub fn trim_cache(root: &Path, cap_bytes: u64, now: u64, dry: bool) -> (u64, u64) {
    let mut files: Vec<(u64, u64, PathBuf)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(e.path());
            } else if md.is_file() {
                let m = md.mtime().max(0) as u64;
                let fresh_tmp = e.file_name().to_string_lossy().ends_with(".tmp") && now.saturating_sub(m) < TMP_GRACE_SECS;
                if !fresh_tmp {
                    files.push((m, md.blocks() * 512, e.path()));
                }
            }
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    let (mut n, mut freed) = (0, 0);
    for (_, sz, p) in files {
        if total <= cap_bytes {
            break;
        }
        if dry || fs::remove_file(&p).is_ok() {
            total -= sz;
            freed += sz;
            n += 1;
        }
    }
    (n, freed)
}

pub struct Config {
    pub patterns: Vec<String>,
    pub require: Vec<String>,
    pub lease_dir: Option<PathBuf>,
    pub idle_secs: u64,
    pub min_free_mib: u64,
    pub target_free_mib: u64,
    pub cache_dir: Option<PathBuf>,
    pub cache_cap_bytes: u64,
    pub dry: bool,
}

/// Err is the failed positive control: nothing has been removed.
pub fn run(cfg: &Config, now: u64, held: &[PathBuf], free_mib: &dyn Fn() -> Option<u64>, say: &mut dyn FnMut(String)) -> Result<u64, String> {
    for r in &cfg.require {
        if glob(r).is_empty() {
            return Err(format!("positive control failed: {r} matches nothing, so an empty sweep proves nothing"));
        }
    }
    let mut freed = 0u64;
    let free = free_mib().ok_or("cannot read free space")?;
    if free >= cfg.min_free_mib {
        say(format!("free {free} MiB >= trigger {} MiB: no target directory is touched", cfg.min_free_mib));
    } else {
        let cands = plan(&cfg.patterns, now, cfg.idle_secs, held, cfg.lease_dir.as_deref());
        say(format!("free {free} MiB < trigger {} MiB: {} candidate(s)", cfg.min_free_mib, cands.len()));
        for c in cands {
            if let Verdict::Skip(why) = &c.verdict {
                say(format!("kept {} ({why})", c.path.display()));
                continue;
            }
            if free_mib().is_some_and(|f| f >= cfg.target_free_mib) {
                say(format!("kept {} (target free space reached)", c.path.display()));
                continue;
            }
            if cfg.dry || fs::remove_dir_all(&c.path).is_ok() {
                freed += c.bytes;
                say(format!("reclaimed {} {} MiB age {}s{}", c.path.display(), c.bytes >> 20, c.age, if cfg.dry { " (dry)" } else { "" }));
            } else {
                say(format!("failed to remove {}", c.path.display()));
            }
        }
    }
    if let Some(dir) = &cfg.cache_dir {
        let (n, b) = trim_cache(dir, cfg.cache_cap_bytes, now, cfg.dry);
        freed += b;
        say(format!("cache {}: trimmed {n} file(s), {} MiB, cap {} MiB", dir.display(), b >> 20, cfg.cache_cap_bytes >> 20));
    }
    Ok(freed)
}

#[cfg(test)]
mod tests;
