//! The drill's proof of a restored copy: fold the audited writes after the snapshot onto the
//! snapshot's own keys, and require the restored store to hold exactly the fold.
//!
//! A txn is audited when the restored store holds its `events/<main>`, and the record's subs
//! must name exactly the keys the run says that txn wrote. Two kinds of write are never
//! audited, so each is excluded by name and counted:
//!
//! - every key under `telemetry/`, which the contract keeps out of the history;
//! - a TTL'd key the fold holds and the restored copy lacks, deleted in a txn with no
//!   `events/` record: the GC controller's expiry. A TTL'd key on both sides must still be
//!   byte-identical.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

use serde_json::Value;
use state_store::{
    carries_ttl, events_key, Backend, LogEntry, Object, Rev, Version, EVENTS_PREFIX,
    REQUESTS_PREFIX, TELEMETRY_PREFIX,
};

use super::restore::{self, Restored};
use super::revlog::{self, Run, REVLOG, SNAPSHOT};
use super::ship::names;
use crate::{Options, SqliteBackend};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exclusions {
    pub telemetry: bool,
    pub ttl_swept: bool,
}

impl Exclusions {
    /// The contract's own: what it never audits.
    pub const CONTRACT: Exclusions = Exclusions {
        telemetry: true,
        ttl_swept: true,
    };
    pub const NONE: Exclusions = Exclusions {
        telemetry: false,
        ttl_swept: false,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// Both sides hold the key, with different bytes or revisions.
    Value { key: String },
    /// The fold holds the key and the restored copy does not.
    Missing { key: String },
    /// The restored copy holds a key no audited write produced.
    Unfolded { key: String },
    /// An `events/<main>` record that does not describe the txn the run holds.
    Event { main: u64, detail: String },
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mismatch::Value { key } => write!(f, "{key}: the restored value is not the fold's"),
            Mismatch::Missing { key } => {
                write!(f, "{key}: the fold holds it and the restored copy does not")
            }
            Mismatch::Unfolded { key } => write!(
                f,
                "{key}: the restored copy holds it and no audited write produced it"
            ),
            Mismatch::Event { main, detail } => write!(f, "{}: {detail}", events_key(*main)),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub snapshot_rev: u64,
    pub through: u64,
    pub compared: usize,
    pub telemetry_excluded: BTreeSet<String>,
    pub ttl_swept: BTreeSet<String>,
    pub mismatches: Vec<Mismatch>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.mismatches.is_empty()
    }
}

const PAGE: usize = 1_000;

fn all_keys(b: &SqliteBackend, at: u64) -> Result<Vec<(String, Version)>, String> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = b
            .gc_range(after.as_deref(), PAGE, at)
            .map_err(|e| e.to_string())?;
        let full = page.len() == PAGE;
        after = page.last().map(|(k, _)| k.clone());
        out.extend(page);
        if !full {
            return Ok(out);
        }
    }
}

fn bookkeeping(key: &str) -> bool {
    key.starts_with(EVENTS_PREFIX) || key.starts_with(REQUESTS_PREFIX)
}

/// What the fold and the restored copy hold of one key.
type Held = (Rev, Rev, Vec<u8>);

fn held(v: &Version) -> Held {
    (v.mod_rev, v.create_rev, v.value.clone())
}

/// The subs `events/<main>` records, as `(key, sub, written)`: `written` is false for a delete.
fn recorded(ev: &Version) -> Result<Vec<(String, u32, bool)>, String> {
    let o = Object::decode(&ev.value).map_err(|e| e.to_string())?;
    let subs = o
        .data
        .get("subs")
        .and_then(Value::as_array)
        .ok_or("records no subs")?;
    subs.iter()
        .map(|s| {
            let key = s
                .get("key")
                .and_then(Value::as_str)
                .ok_or("a sub has no key")?;
            let sub = s
                .get("sub")
                .and_then(Value::as_u64)
                .ok_or("a sub has no number")?;
            let op = s
                .get("op")
                .and_then(Value::as_str)
                .ok_or("a sub has no op")?;
            Ok((key.to_string(), sub as u32, op != "delete"))
        })
        .collect()
}

/// Folds the audited writes of `runs` after `base`'s head onto `base`'s keys, and compares the
/// fold with `restored` at its head.
pub fn fold_compare(
    base: &SqliteBackend,
    runs: &[Run],
    restored: &SqliteBackend,
    ex: Exclusions,
) -> Result<Report, String> {
    let s = base.head().map_err(|e| e.to_string())?;
    let r = restored.head().map_err(|e| e.to_string())?;
    let mut rep = Report {
        snapshot_rev: s,
        through: r,
        ..Report::default()
    };
    let excluded = |k: &str| ex.telemetry && k.starts_with(TELEMETRY_PREFIX);
    let mut fold: BTreeMap<String, Held> = BTreeMap::new();
    let mut expired: BTreeSet<String> = BTreeSet::new();
    for (k, v) in all_keys(base, s)? {
        if bookkeeping(&k) {
            continue;
        }
        if excluded(&k) {
            rep.telemetry_excluded.insert(k);
            continue;
        }
        fold.insert(k, held(&v));
    }
    for run in runs {
        for (main, entries) in revlog::mains(run) {
            if main <= s || main > r {
                continue;
            }
            let writes: Vec<&LogEntry> = entries.iter().filter(|e| !bookkeeping(&e.key)).collect();
            for e in &writes {
                if excluded(&e.key) {
                    rep.telemetry_excluded.insert(e.key.clone());
                }
            }
            let Some(ev) = restored
                .get(&events_key(main), r)
                .map_err(|e| e.to_string())?
            else {
                expired.extend(
                    writes
                        .iter()
                        .filter(|e| e.value.is_none())
                        .map(|e| e.key.clone()),
                );
                continue;
            };
            let subs = match recorded(&ev) {
                Ok(subs) => subs,
                Err(detail) => {
                    rep.mismatches.push(Mismatch::Event { main, detail });
                    writes
                        .iter()
                        .map(|e| (e.key.clone(), e.rev.sub, e.value.is_some()))
                        .collect()
                }
            };
            let ran: BTreeSet<(String, u32, bool)> = writes
                .iter()
                .map(|e| (e.key.clone(), e.rev.sub, e.value.is_some()))
                .collect();
            let said: BTreeSet<(String, u32, bool)> = subs.into_iter().collect();
            if ran != said {
                let detail = format!(
                    "records {:?}, the run holds {:?}",
                    said.difference(&ran).collect::<Vec<_>>(),
                    ran.difference(&said).collect::<Vec<_>>()
                );
                rep.mismatches.push(Mismatch::Event { main, detail });
            }
            for e in writes {
                if excluded(&e.key) {
                    continue;
                }
                match (&e.value, e.create_rev) {
                    (Some(v), Some(c)) => {
                        fold.insert(e.key.clone(), (e.rev, c, v.clone()));
                    }
                    _ => {
                        fold.remove(&e.key);
                    }
                }
            }
        }
    }
    let mut live: BTreeMap<String, Held> = BTreeMap::new();
    for (k, v) in all_keys(restored, r)? {
        if bookkeeping(&k) {
            continue;
        }
        if excluded(&k) {
            rep.telemetry_excluded.insert(k);
            continue;
        }
        live.insert(k, held(&v));
    }
    let keys: BTreeSet<&String> = fold.keys().chain(live.keys()).collect();
    for k in keys {
        rep.compared += 1;
        match (fold.get(k), live.get(k)) {
            (Some(f), Some(l)) if f == l => {}
            (Some(_), Some(_)) => rep.mismatches.push(Mismatch::Value { key: k.clone() }),
            (Some(f), None) => {
                let ttl = Object::decode(&f.2).is_ok_and(|o| carries_ttl(k, &o));
                if ex.ttl_swept && ttl && expired.contains(k) {
                    rep.ttl_swept.insert(k.clone());
                } else {
                    rep.mismatches.push(Mismatch::Missing { key: k.clone() });
                }
            }
            (None, _) => rep.mismatches.push(Mismatch::Unfolded { key: k.clone() }),
        }
    }
    Ok(rep)
}

/// The skew guard's way out, printed by every drill: after a restore the control plane can be
/// seconds old and the planning plane hours old, and the repair sweeps refuse until both agree.
pub const SKEW_PROCEDURE: &str = "after a restore: (0) move SPIRA_RUN/lc-ship aside and ship the restored store under a new SPIRA_LC_BACKUP_URL prefix, since its revisions repeat ones already shipped; (1) restore bd from lc-backup's tar at its stamped planning watermark; (2) reconcile: re-file in bd the planning writes the tar is missing, which lie between the two planes' watermarks; (3) lift the guard: stamp both planes to one planning watermark, or raise SPIRA_PLANE_SKEW_MAX past the measured skew for one pass of reconcile-content, drop-orphans and the rowless repair, then put it back";

/// The newest local snapshot in `ship_dir/sent/`, as `(rev, path)`.
pub fn newest_snapshot(ship_dir: &Path) -> Result<(u64, std::path::PathBuf), String> {
    let dir = ship_dir.join("sent").join(SNAPSHOT);
    names(&dir)?
        .into_iter()
        .filter_map(|n| revlog::parse_snapshot_name(&n).map(|r| (r, dir.join(n))))
        .max()
        .ok_or_else(|| format!("no snapshot in {}: nothing has been shipped", dir.display()))
}

/// `spira-lc admin-drill`: restores the newest local snapshot and every uploaded run after it
/// into `scratch/control.db`, then folds and compares against a second copy of the snapshot.
pub fn drill(
    ship_dir: &Path,
    scratch: &Path,
    ex: Exclusions,
) -> Result<(Restored, Report), String> {
    let (rev, snapshot) = newest_snapshot(ship_dir)?;
    let name = revlog::snapshot_name(rev);
    let staged = fs::read_to_string(super::ship::sum_path(ship_dir, &name))
        .map_err(|e| format!("snapshot {name} has no staged sha256: {e}"))?;
    let now = super::ship::sha256_file(&snapshot)?;
    if staged.trim() != now {
        return Err(format!(
            "snapshot {name} changed since it was staged: sha256 {now}, staged {}",
            staged.trim()
        ));
    }
    fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let (out, base_path) = (scratch.join("control.db"), scratch.join("base.db"));
    let restored = restore::restore(&snapshot, &ship_dir.join("sent").join(REVLOG), &out)?;
    if base_path.exists() {
        return Err(format!("{} exists", base_path.display()));
    }
    fs::copy(&snapshot, &base_path).map_err(|e| format!("{}: {e}", base_path.display()))?;
    let open = |p: &Path| {
        SqliteBackend::open(p, Options::default()).map_err(|e| format!("{}: {e}", p.display()))
    };
    let report = fold_compare(&open(&base_path)?, &restored.runs, &open(&out)?, ex)?;
    Ok((restored, report))
}

/// The drill's output: the counts, each exclusion by key, each mismatch, and the procedure.
pub fn render(r: &Report) -> String {
    let mut out = vec![format!(
        "drill {}: snapshot at {}, restored through {}, {} keys compared, {} telemetry keys excluded, {} ttl_swept, {} mismatches",
        if r.passed() { "ok" } else { "FAILED" },
        r.snapshot_rev,
        r.through,
        r.compared,
        r.telemetry_excluded.len(),
        r.ttl_swept.len(),
        r.mismatches.len()
    )];
    out.extend(
        r.telemetry_excluded
            .iter()
            .map(|k| format!("excluded {k} (telemetry: never evented)")),
    );
    out.extend(r.ttl_swept.iter().map(|k| format!("ttl_swept {k}")));
    out.extend(r.mismatches.iter().map(|m| format!("mismatch {m}")));
    out.push(format!("skew guard: {SKEW_PROCEDURE}"));
    out.join("\n")
}
