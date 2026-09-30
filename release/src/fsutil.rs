//! Filesystem helpers: hashing, walking, read-only marking, atomic symlinks.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Lowercase hex sha256 of a file's contents.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    use std::fmt::Write;
    Ok(h.finalize().iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// A regular file (following symlinks) with any execute bit set.
pub fn is_executable(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// What sits at one path inside a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    File,
    Dir,
    Link(String),
}

/// Every entry under `root`, as (path relative to root with `/` separators, node), sorted by
/// path. Symlinks are reported, never followed.
pub fn walk(root: &Path) -> Result<Vec<(String, Node)>, String> {
    let mut out = Vec::new();
    walk_into(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn walk_into(root: &Path, dir: &Path, out: &mut Vec<(String, Node)>) -> Result<(), String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for e in rd {
        let e = e.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let p = e.path();
        let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().to_string();
        let ft = e.file_type().map_err(|e| format!("cannot stat {}: {e}", p.display()))?;
        if ft.is_symlink() {
            let t = fs::read_link(&p).map_err(|e| format!("cannot readlink {}: {e}", p.display()))?;
            out.push((rel, Node::Link(t.to_string_lossy().to_string())));
        } else if ft.is_dir() {
            out.push((rel, Node::Dir));
            walk_into(root, &p, out)?;
        } else {
            out.push((rel, Node::File));
        }
    }
    Ok(())
}

/// Clear every write bit under `root` (and on root itself), deepest first. Symlinks are
/// left alone: their own mode is meaningless on Linux.
pub fn set_readonly(root: &Path) -> Result<(), String> {
    let mut entries = walk(root)?;
    entries.reverse();
    for (rel, node) in &entries {
        if matches!(node, Node::Link(_)) {
            continue;
        }
        chmod(&root.join(rel), |m| m & !0o222)?;
    }
    chmod(root, |m| m & !0o222)
}

/// Give the owner write permission everywhere under `root` (directories top-down), so it
/// can be removed. Best-effort: a path this uid does not own stays as it was.
pub fn make_writable(root: &Path) {
    if fs::symlink_metadata(root).map(|m| m.file_type().is_symlink()).unwrap_or(true) {
        return;
    }
    let _ = chmod(root, |m| m | 0o200);
    if let Ok(entries) = walk(root) {
        for (rel, node) in entries {
            if !matches!(node, Node::Link(_)) {
                let _ = chmod(&root.join(rel), |m| m | 0o200);
            }
        }
    }
}

/// Remove a tree that may be read-only.
pub fn remove_tree(root: &Path) -> Result<(), String> {
    if fs::symlink_metadata(root).is_err() {
        return Ok(());
    }
    make_writable(root);
    fs::remove_dir_all(root).map_err(|e| format!("cannot remove {}: {e}", root.display()))
}

fn chmod(p: &Path, f: impl Fn(u32) -> u32) -> Result<(), String> {
    let m = fs::symlink_metadata(p).map_err(|e| format!("cannot stat {}: {e}", p.display()))?;
    let mode = m.permissions().mode();
    let new = f(mode);
    if new != mode {
        fs::set_permissions(p, fs::Permissions::from_mode(new)).map_err(|e| format!("cannot chmod {}: {e}", p.display()))?;
    }
    Ok(())
}

/// Every path under `root` (root included) that has any write bit set.
pub fn writable_paths(root: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let is_w = |p: &Path| fs::symlink_metadata(p).map(|m| m.permissions().mode() & 0o222 != 0).unwrap_or(false);
    if is_w(root) {
        out.push(".".to_string());
    }
    for (rel, node) in walk(root)? {
        if !matches!(node, Node::Link(_)) && is_w(&root.join(&rel)) {
            out.push(rel);
        }
    }
    Ok(out)
}

/// Point `link` at `target` atomically: a symlink written beside it, renamed over it.
pub fn atomic_symlink(link: &Path, target: &str) -> Result<(), String> {
    let dir = link.parent().unwrap_or(Path::new("."));
    let name = link.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.new.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    fs::rename(&tmp, link).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot swap {}: {e}", link.display())
    })
}

/// Write `text` to `path` atomically (`<path>.new`, then rename), mode 0644.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = PathBuf::from(format!("{}.new", path.display()));
    fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644));
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot install {}: {e}", path.display())
    })
}

/// Seconds since the epoch, now.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Now, as RFC 3339 UTC to the second (`2026-09-29T12:34:56Z`). Sorts as text.
pub fn now_rfc3339() -> String {
    rfc3339(now_secs())
}

/// Seconds since the epoch as RFC 3339 UTC.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Inverse of [`rfc3339`]: seconds since the epoch for exactly the text it emits
/// (`YYYY-MM-DDTHH:MM:SSZ`). `None` for anything else — a hotfix "since" a reader cannot
/// parse must never silently read as "just now" (age 0).
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let y = digits(0..4)?;
    let mo = digits(5..7)?;
    let d = digits(8..10)?;
    let h = digits(11..13)?;
    let mi = digits(14..16)?;
    let se = digits(17..19)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    // days_from_civil, the inverse of civil_from_days above (Howard Hinnant's algorithm).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + se;
    u64::try_from(secs).ok()
}
