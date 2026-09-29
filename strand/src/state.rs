//! Episode state: `$SPIRA_RUN/strands.json` (DESIGN.md §3.3). One entry per
//! `<partition>:<kind>:<id>`, carrying when the strand was first seen and when it was acted
//! on or escalated. Recomputed every pass against what is stranded now, so a cleared strand
//! starts a fresh episode rather than inheriting a suppression from the last one.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::model::{AgedRow, PartRow, StateEntry};

pub type State = BTreeMap<String, StateEntry>;

pub fn key(part: &str, kind: &str, id: &str) -> String {
    format!("{part}:{kind}:{id}")
}

/// Unreadable or malformed state reads as empty — an episode is an observation that can be
/// recomputed, and a corrupt file must not wedge the detector.
pub fn load(path: &Path) -> State {
    let Ok(text) = fs::read_to_string(path) else {
        return State::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return State::new();
    };
    let serde_json::Value::Object(map) = v else {
        return State::new();
    };
    map.into_iter()
        .filter_map(|(k, v)| {
            let e = StateEntry {
                first: v.get("first").and_then(as_i64).unwrap_or(0),
                acted: v.get("acted").and_then(as_i64).unwrap_or(0),
                escalated: v.get("escalated").and_then(as_i64).unwrap_or(0),
            };
            v.is_object().then_some((k, e))
        })
        .collect()
}

fn as_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

/// Age every row against `st`, and return the state pruned to exactly these rows (new rows
/// start an episode at `now`).
pub fn apply(st: &State, rows: &[PartRow], now: i64) -> (Vec<AgedRow>, State) {
    let mut keep = State::new();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let k = key(&r.part, &r.row.kind, &r.row.id);
        let e = st
            .get(&k)
            .copied()
            .unwrap_or(StateEntry { first: now, acted: 0, escalated: 0 });
        keep.insert(k, e);
        out.push(AgedRow {
            part: r.part.clone(),
            row: r.row.clone(),
            age: now - e.first,
            acted: e.acted,
            escalated: e.escalated,
        });
    }
    (out, keep)
}

#[derive(Clone, Copy)]
pub enum Mark {
    Acted,
    Escalated,
}

pub fn mark(st: &mut State, part: &str, kind: &str, id: &str, what: Mark, now: i64) {
    let e = st
        .entry(key(part, kind, id))
        .or_insert(StateEntry { first: now, acted: 0, escalated: 0 });
    match what {
        Mark::Acted => e.acted = now,
        Mark::Escalated => e.escalated = now,
    }
}

/// Atomic replace through a unique temp file in the same directory — a fixed temp name
/// shared by concurrent writers could promote the other writer's half-written file.
pub fn save(path: &Path, st: &State) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let body = serde_json::to_string(st).map_err(std::io::Error::other)?;
    let mut n = 0u32;
    let tmp: PathBuf = loop {
        let cand = dir.join(format!(".strands-{}-{}.tmp", std::process::id(), n));
        match OpenOptions::new().write(true).create_new(true).open(&cand) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(body.as_bytes()).and_then(|_| f.sync_all()) {
                    let _ = fs::remove_file(&cand);
                    return Err(e);
                }
                break cand;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
            Err(e) => return Err(e),
        }
    };
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// An exclusive, non-blocking `flock` on `<state>.lock` — the same lock the bash version
/// took on fd 9, so the two can never both run `check` against one state file during
/// cutover. Released when the returned file is dropped.
pub fn try_lock(path: &Path) -> std::io::Result<Option<File>> {
    let f = OpenOptions::new().write(true).create(true).truncate(false).open(path)?;
    // SAFETY: flock on an fd we own.
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(Some(f))
    } else {
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Disposition, Row};

    fn pr(part: &str, kind: &str, id: &str) -> PartRow {
        PartRow { part: part.into(), row: Row::new(kind, id, Disposition::Escalate, "d".into(), "a".into()) }
    }

    #[test]
    fn ages_prunes_and_keys_by_partition() {
        let mut st = State::new();
        st.insert(key("spira,plan", "starved", "-"), StateEntry { first: 100, acted: 0, escalated: 150 });
        st.insert(key("spira,plan", "stuck", "E"), StateEntry { first: 10, acted: 0, escalated: 0 });
        let rows = vec![pr("spira,plan", "starved", "-"), pr("spira,incident", "starved", "-")];
        let (aged, keep) = apply(&st, &rows, 1000);
        assert_eq!((aged[0].age, aged[0].escalated), (900, 150));
        // Another partition's identical row is its own episode.
        assert_eq!((aged[1].age, aged[1].escalated), (0, 0));
        assert!(!keep.contains_key(&key("spira,plan", "stuck", "E")), "cleared strand pruned");
        assert_eq!(keep.len(), 2);
    }

    #[test]
    fn save_load_mark_and_lock() {
        let dir = testkit::TempDir::new("strand-state");
        let p = dir.join("strands.json");
        let mut st = State::new();
        mark(&mut st, "p", "ghost", "g", Mark::Acted, 42);
        save(&p, &st).unwrap();
        let back = load(&p);
        assert_eq!(back.get("p:ghost:g").unwrap(), &StateEntry { first: 42, acted: 42, escalated: 0 });
        // The on-disk shape the cockpit and auron read.
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["p:ghost:g"]["acted"], 42);
        fs::write(&p, "not json").unwrap();
        assert!(load(&p).is_empty());
        let lock = dir.join("strands.json.lock");
        let held = try_lock(&lock).unwrap();
        assert!(held.is_some());
        assert!(try_lock(&lock).unwrap().is_none(), "second locker declines");
        drop(held);
        assert!(try_lock(&lock).unwrap().is_some());
        fs::remove_dir_all(&dir).unwrap();
    }
}
