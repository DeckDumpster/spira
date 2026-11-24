//! Which transcript is whose, and what it says: turns and growth read forward from a cursor.

use std::io::{BufRead, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::files::{read_kv, write_by_rename};

const KEEP: usize = 21;

pub fn slug(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn digest16(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

/// The hook's own session, in order: the sibling of its transcript_path named by session_id, the
/// transcript_path, the project directory's session file. Newest-by-mtime only when the hook
/// named no session at all.
pub fn resolve_line(projects: &Path, sid: &str, hp: &str, cwd: &str) -> String {
    let mut order: Vec<PathBuf> = Vec::new();
    if !sid.is_empty() && !hp.is_empty() {
        order.push(Path::new(hp).parent().unwrap_or(Path::new("")).join(format!("{sid}.jsonl")));
    }
    if !hp.is_empty() {
        order.push(hp.into());
    }
    if !sid.is_empty() {
        order.push(projects.join(slug(cwd)).join(format!("{sid}.jsonl")));
    }
    if let Some(p) = order.iter().find(|p| p.exists()) {
        return p.display().to_string();
    }
    if !sid.is_empty() || !hp.is_empty() {
        return String::new();
    }
    let newest = std::fs::read_dir(projects.join(slug(cwd)))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| mtime(&p).map(|m| (m, p)))
        .max_by_key(|(m, _)| *m);
    newest.map(|(_, p)| p.display().to_string()).unwrap_or_default()
}

#[derive(Debug, Default, PartialEq)]
pub struct Scan {
    pub turns: i64,
    /// Context carried at each of the last 21 turns, oldest first.
    pub tail: Vec<i64>,
    pub scanned: i64,
    pub broken: bool,
}

struct Cursor {
    off: u64,
    turns: i64,
    last: String,
    tail: Vec<i64>,
}

fn read_cursor(path: &Path, tp: &str, head: &str, ino: u64, size: u64) -> Option<Cursor> {
    let c = read_kv(path, false).ok()?;
    let off: u64 = c.get("off")?.trim().parse().ok()?;
    let ok = c.get("path").is_some_and(|p| p == tp) && c.get("head").is_some_and(|h| h == head) && c.get("ino")?.trim().parse::<u64>().ok()? == ino && off <= size;
    if !ok {
        return None;
    }
    let tail = c.get("tail").map(String::as_str).unwrap_or("").split(',').filter(|x| !x.is_empty()).map(|x| x.trim().parse()).collect::<Result<Vec<i64>, _>>().ok()?;
    Some(Cursor { off, turns: c.get("turns")?.trim().parse().ok()?, last: c.get("last").cloned().unwrap_or_default(), tail })
}

fn as_count(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        _ => 0,
    }
}

/// Turns and the tail of per-turn context, reading only what was appended since the cursor in
/// `<run>/ctx-meter/`. The cursor is keyed on path, inode and a digest of the opening bytes, and
/// is dropped for a full read when any of them disagree: it may make this faster, never different.
pub fn scan(run: &Path, tp: &str) -> Scan {
    let broken = |turns, tail, scanned| Scan { turns, tail, scanned, broken: true };
    let Ok(meta) = std::fs::metadata(tp) else { return broken(0, vec![], 0) };
    let Ok(mut fh) = std::fs::File::open(tp) else { return broken(0, vec![], 0) };
    let mut first = Vec::new();
    if fh.by_ref().take(256).read_to_end(&mut first).is_err() {
        return broken(0, vec![], 0);
    }
    let head = digest16(&first);
    let cur_path = run.join("ctx-meter").join(format!("{}.cursor", digest16(tp.as_bytes())));
    let c = read_cursor(&cur_path, tp, &head, meta.ino(), meta.len()).unwrap_or(Cursor { off: 0, turns: 0, last: String::new(), tail: vec![] });
    let (start, mut off, mut turns, mut last, mut tail) = (c.off, c.off, c.turns, c.last, c.tail);
    if fh.seek(SeekFrom::Start(off)).is_err() {
        return broken(turns, tail, 0);
    }
    let mut rd = std::io::BufReader::new(fh);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match rd.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Err(_) => return broken(turns, tail, (off - start) as i64),
            Ok(_) => {}
        }
        if !buf.ends_with(b"\n") {
            break;
        }
        off += buf.len() as u64;
        if !buf.windows(7).any(|w| w == b"\"usage\"") {
            continue;
        }
        let Ok(o) = serde_json::from_slice::<Value>(&buf) else { continue };
        let Some(m) = o.get("message").filter(|m| m.is_object()) else { continue };
        let usage = m.get("usage").filter(|u| u.as_object().is_some_and(|o| !o.is_empty()));
        let mid = m.get("id").and_then(Value::as_str).filter(|s| !s.is_empty());
        let (Some(u), Some(mid)) = (usage, mid) else { continue };
        if mid == last {
            continue;
        }
        last = mid.to_string();
        turns += 1;
        tail.push(as_count(u.get("input_tokens")) + as_count(u.get("cache_creation_input_tokens")) + as_count(u.get("cache_read_input_tokens")));
        if tail.len() > KEEP {
            tail.remove(0);
        }
    }
    if off != start || start == 0 {
        let t: Vec<String> = tail.iter().map(i64::to_string).collect();
        write_by_rename(&cur_path, &format!("path={tp}\nino={}\nhead={head}\noff={off}\nturns={turns}\nlast={last}\ntail={}\n", meta.ino(), t.join(",")));
    }
    Scan { turns, tail, scanned: (off - start) as i64, broken: false }
}
