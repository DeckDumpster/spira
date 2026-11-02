//! A restore: the snapshot, then every run after it replayed through the backend, each txn at
//! its own revision. It reads local files only; fetching them from the bucket is the
//! operator's, under their own identity.

use std::fs;
use std::path::{Path, PathBuf};

use state_store::{Backend, Write};

use super::revlog::{self, Run};
use super::ship::names;
use crate::{Options, SqliteBackend};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restored {
    pub snapshot_rev: u64,
    /// The head of the restored store: the last revision replayed, or the snapshot's own.
    pub through: u64,
    /// The runs replayed, oldest first, each with at least one revision after the snapshot.
    pub runs: Vec<Run>,
}

/// The runs in `dir` that a restore from revision `after` needs, in order; refused, naming the
/// revisions missing, when they leave a break.
pub fn plan(dir: &Path, after: u64) -> Result<Vec<PathBuf>, String> {
    let mut runs = Vec::new();
    for n in names(dir)? {
        let (from, to) = revlog::parse_run_name(&n)
            .ok_or_else(|| format!("{}: not a revision run", dir.join(&n).display()))?;
        runs.push((from, to, dir.join(n)));
    }
    let mut through = after;
    let mut out = Vec::new();
    for (from, to, path) in runs {
        if to <= through {
            continue;
        }
        if from > through + 1 {
            return Err(format!(
                "gap: revisions {}..={} are in no run (the next run is {})",
                through + 1,
                from - 1,
                revlog::run_name(from, to)
            ));
        }
        through = to;
        out.push(path);
    }
    Ok(out)
}

/// Each txn of `run` after the store's head, as the backend's commit of that revision.
pub fn replay(b: &SqliteBackend, run: &Run) -> Result<(), String> {
    for (main, entries) in revlog::mains(run) {
        let head = b.head().map_err(|e| e.to_string())?;
        if main <= head {
            continue;
        }
        let batch: Vec<Write> = entries
            .iter()
            .map(|e| match &e.value {
                Some(v) => Ok(Write::Put {
                    key: e.key.clone(),
                    value: v.clone(),
                    create_rev: e.create_rev.ok_or_else(|| {
                        format!("{} at {main} is a put with no create revision", e.key)
                    })?,
                    prev: e.prev,
                }),
                None => Ok(Write::Delete {
                    key: e.key.clone(),
                    prev: e.prev.unwrap_or_default(),
                }),
            })
            .collect::<Result<_, String>>()?;
        let got = b.commit(main - 1, &batch).map_err(|e| {
            format!("revision {main} does not follow the restored head {head}: {e}")
        })?;
        if got != main {
            return Err(format!("revision {main} committed as {got}"));
        }
    }
    Ok(())
}

/// `spira-lc admin-restore --snapshot <file> --revlog <dir> --out <path>`. Every run is read and
/// checked before anything is written; `out` must not exist, and is left whole or absent.
pub fn restore(snapshot: &Path, revlog_dir: &Path, out: &Path) -> Result<Restored, String> {
    if out.exists() {
        return Err(format!(
            "{} exists; a restore never overwrites",
            out.display()
        ));
    }
    fs::copy(snapshot, out).map_err(|e| format!("{}: {e}", snapshot.display()))?;
    let res = (|| {
        let b = SqliteBackend::open(out, Options::default()).map_err(|e| e.to_string())?;
        let snapshot_rev = b.head().map_err(|e| e.to_string())?;
        let mut runs = Vec::new();
        for p in plan(revlog_dir, snapshot_rev)? {
            let bytes = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            runs.push(revlog::decode(&bytes).map_err(|e| format!("{}: {e}", p.display()))?);
        }
        for r in &runs {
            replay(&b, r)?;
        }
        let through = b.head().map_err(|e| e.to_string())?;
        Ok(Restored {
            snapshot_rev,
            through,
            runs,
        })
    })();
    let _ = fs::remove_file(format!("{}.lock", out.display()));
    if res.is_err() {
        let _ = fs::remove_file(out);
        for side in ["-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{side}", out.display()));
        }
    }
    res
}
