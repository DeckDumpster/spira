//! The two records the status line refresh and the scheduler share, under one directory the
//! caller names (`--run`, in the user runtime directory): `sessions/<sid>.json`, written by the
//! refresh, and `status/<sid>.json`, written only by the scheduler's writers under [`lock`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::files::write_by_rename;
use crate::{archivist, archivist_segment, growth, transcript, Config};

/// Seconds a status file may go untouched before the refresh prints plain client figures.
pub const STALE_SECS: i64 = 60;

static LOCK: Mutex<()> = Mutex::new(());

/// Held by every writer of a status file, so two writers never interleave a read and a replace.
pub fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn valid_session(sid: &str) -> bool {
    !sid.is_empty() && sid.len() <= 128 && sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn sessions_dir(run: &Path) -> PathBuf {
    run.join("sessions")
}

pub fn status_dir(run: &Path) -> PathBuf {
    run.join("status")
}

pub fn samples_path(run: &Path) -> PathBuf {
    run.join("limits.samples")
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub session: String,
    pub transcript: String,
    pub cwd: String,
    pub len: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub warn: i64,
    pub high: i64,
    pub limit: i64,
    pub turns: i64,
    pub growth: i64,
    pub ctx: Option<i64>,
    pub broken: bool,
    pub archivist: String,
    pub archivist_warn: String,
    pub inflight: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrote {
    Rewrote,
    Touched,
}

pub fn read_record(path: &Path) -> Option<Record> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub fn read_status(path: &Path) -> Option<Status> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Seconds since `path` was last written or touched; `None` when it cannot be stat'd.
pub fn age(path: &Path, now: i64) -> Option<f64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(now as f64 - m.as_secs_f64())
}

/// Sets the mtime through a read-only open, which raises no watched event.
pub fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

fn put(path: &Path, body: &str) -> Wrote {
    if std::fs::read(path).is_ok_and(|old| old == body.as_bytes()) {
        touch(path);
        return Wrote::Touched;
    }
    write_by_rename(path, body);
    Wrote::Rewrote
}

pub fn put_record(path: &Path, r: &Record) -> Wrote {
    put(path, &serde_json::to_string(r).unwrap_or_default())
}

pub fn put_status(path: &Path, s: &Status) -> Wrote {
    put(path, &serde_json::to_string(s).unwrap_or_default())
}

/// The collector's in-flight segment while its snapshot is fresh, else empty (rendered `?inflight`).
pub fn inflight_now(run: &Path, now: i64) -> String {
    let snap = run.join("cockpit.env");
    if age(&snap, now).is_none_or(|a| a > STALE_SECS as f64) {
        return String::new();
    }
    let Ok(bytes) = std::fs::read(&snap) else { return String::new() };
    for line in String::from_utf8_lossy(&bytes).lines() {
        if let Some(v) = line.strip_prefix("SP_INFLIGHT_SEG=") {
            return if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') { &v[1..v.len() - 1] } else { v }.to_string();
        }
    }
    String::new()
}

/// What the status line says about the session `rec` names, measured from its transcript and
/// the archivist's state; `inflight` is carried in, not measured here.
pub fn compute(cfg: &Config, rec: &Record, inflight: String) -> Status {
    let tp = transcript::resolve_line(&cfg.projects, &rec.session, &rec.transcript, &rec.cwd);
    let s = if tp.is_empty() { transcript::Scan::default() } else { transcript::scan(&cfg.run, &tp) };
    let arc = archivist(&cfg.run, &rec.session, s.turns);
    Status {
        warn: cfg.warn,
        high: cfg.high,
        limit: cfg.limit,
        turns: s.turns,
        growth: growth(&s.tail),
        ctx: s.tail.last().copied(),
        broken: s.broken,
        archivist: archivist_segment(&arc, 0, &rec.session, cfg),
        archivist_warn: archivist_segment(&arc, i64::MAX, &rec.session, cfg),
        inflight,
    }
}

/// Recomputes session `sid`'s status from its record and replaces the file only if it differs;
/// `None` when it has no record. The caller holds [`lock`].
pub fn refresh_session(cfg: &Config, now: i64, sid: &str) -> Option<Wrote> {
    if !valid_session(sid) {
        return None;
    }
    let rec = read_record(&sessions_dir(&cfg.status).join(format!("{sid}.json")))?;
    let path = status_dir(&cfg.status).join(format!("{sid}.json"));
    let inflight = read_status(&path).map(|s| s.inflight).unwrap_or_else(|| inflight_now(&cfg.run, now));
    Some(put_status(&path, &compute(cfg, &rec, inflight)))
}

/// Every session that has a record.
pub fn sessions(status_root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(sessions_dir(status_root)).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).filter_map(|n| n.strip_suffix(".json").map(String::from)).filter(|s| valid_session(s)).collect();
    out.sort();
    out
}

/// Sets every status file's in-flight segment to `seg`: a file already showing it is touched,
/// which is what keeps it fresh; any other is replaced. Answers `(rewritten, touched)`.
pub fn set_inflight(status_root: &Path, seg: &str) -> (usize, usize) {
    let _guard = lock();
    let (mut rewrote, mut touched) = (0, 0);
    for e in std::fs::read_dir(status_dir(status_root)).into_iter().flatten().flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(mut st) = read_status(&path) else { continue };
        st.inflight = seg.to_string();
        match put_status(&path, &st) {
            Wrote::Rewrote => rewrote += 1,
            Wrote::Touched => touched += 1,
        }
    }
    (rewrote, touched)
}

/// Removes records not touched for `idle` seconds, and the status of any session without a record.
pub fn prune(status_root: &Path, now: i64, idle: i64) {
    let _guard = lock();
    for e in std::fs::read_dir(sessions_dir(status_root)).into_iter().flatten().flatten() {
        if age(&e.path(), now).is_some_and(|a| a > idle as f64) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    for e in std::fs::read_dir(status_dir(status_root)).into_iter().flatten().flatten() {
        if !sessions_dir(status_root).join(e.file_name()).exists() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
