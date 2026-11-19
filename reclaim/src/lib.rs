//! Lossless reclamation of idle tmp directories and the shared compilation cache. Everything removed here is rebuildable.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const TMP_GRACE_SECS: u64 = 3600;

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

pub fn judge(c: &Path, now: u64, idle_secs: u64, held: &[PathBuf]) -> Candidate {
    let u = usage(c);
    let age = now.saturating_sub(u.newest);
    let verdict = if fs::symlink_metadata(c).map(|m| m.file_type().is_symlink()).unwrap_or(true) {
        Verdict::Skip("symlink".into())
    } else if held.iter().any(|h| h.starts_with(c)) {
        Verdict::Skip("held by a live process".into())
    } else if age < idle_secs {
        Verdict::Skip(format!("touched {age}s ago"))
    } else {
        Verdict::Reclaim
    };
    Candidate { path: c.to_path_buf(), bytes: u.bytes, age, verdict }
}

/// The uid this process runs as: the owner of its own `/proc` entry.
pub fn own_uid() -> u32 {
    fs::metadata(Path::new(system_paths::PROC).join("self")).map_or(u32::MAX, |m| m.uid())
}

fn holds_socket_or_fifo(p: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&d) else { continue };
        let ft = md.file_type();
        if ft.is_socket() || ft.is_fifo() {
            return true;
        }
        if ft.is_dir() {
            match fs::read_dir(&d) {
                Ok(rd) => stack.extend(rd.flatten().map(|e| e.path())),
                Err(_) => return true,
            }
        }
    }
    false
}

/// The top-level directories of `root` owned by `uid`, oldest first. A socket or fifo inside keeps
/// a directory: `/proc/<pid>/fd` names a unix socket by inode, so its holder cannot be seen.
pub fn tmp_plan(root: &Path, uid: u32, now: u64, idle_secs: u64, held: &[PathBuf]) -> Vec<Candidate> {
    let Ok(rd) = fs::read_dir(root) else { return vec![] };
    let mut v: Vec<Candidate> = rd
        .flatten()
        .filter_map(|e| {
            let md = fs::symlink_metadata(e.path()).ok()?;
            (md.uid() == uid && (md.is_dir() || md.file_type().is_symlink())).then(|| e.path())
        })
        .map(|p| {
            let mut c = judge(&p, now, idle_secs, held);
            let unjudged_by_age = matches!(&c.verdict, Verdict::Reclaim) || matches!(&c.verdict, Verdict::Skip(w) if w.starts_with("touched"));
            if unjudged_by_age && holds_socket_or_fifo(&p) {
                c.verdict = Verdict::Skip("holds a socket or fifo".into());
            }
            c
        })
        .collect();
    v.sort_by_key(|c| std::cmp::Reverse(c.age));
    v
}

/// Every file under `root` as `(mtime, bytes, path)`, oldest first, leaving out a `.tmp` file
/// younger than an hour (a write still in progress).
pub fn cache_files(root: &Path, now: u64) -> Vec<(u64, u64, PathBuf)> {
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
    files.sort();
    files
}

/// The files to remove, oldest first, until the total of `files` (oldest first) is at or under
/// `cap_bytes`. Pure.
pub fn trim_selection<P: Clone>(files: &[(u64, u64, P)], cap_bytes: u64) -> Vec<P> {
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    let mut out = vec![];
    for (_, sz, p) in files {
        if total <= cap_bytes {
            break;
        }
        total -= sz;
        out.push(p.clone());
    }
    out
}

#[cfg(test)]
mod tests;
