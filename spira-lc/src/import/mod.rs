//! `admin-import --out <store path> --archive <file> [--from-archive]`: the dolt lifecycle
//! store, frozen, into a new state-store file at one import revision `R = max(event.seq) + 1`.
//!
//! It exports every table to the archive (or, with `--from-archive`, reads one already
//! written), maps every row to its control-plane key, bulk-loads them at `R`, and runs the
//! checks in `checks`. It refuses while lc-serve answers on the declared socket, since a
//! write after the export would be lost, and it never writes a store that fails a check: the
//! file is removed, so nothing can swap it in.

pub mod archive;
pub mod checks;
pub mod map;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::json;
use state_store::Backend;
use state_store_sqlite::{Options, SqliteBackend};

use crate::db::Conn;
use crate::{flag, CANNOT_TELL, REFUSED};

const USAGE: &str = "usage: spira-lc admin-import --out <store path> --archive <file> [--from-archive]";
const READ_PAGE: usize = 1_000;
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// The socket lc-serve answers on, when it is running: a connect that is accepted.
fn serving() -> Result<Option<String>, String> {
    let accepted = |p: &str| std::os::unix::net::UnixStream::connect(p).and_then(|s| s.set_read_timeout(Some(PROBE_TIMEOUT))).is_ok();
    match crate::client::route()? {
        crate::client::Route::Direct => Ok(None),
        crate::client::Route::Service(p) => Ok(accepted(&p).then_some(p)),
    }
}

fn store_files(out: &Path) -> Vec<PathBuf> {
    ["", "-wal", "-shm", ".lock"].iter().map(|s| PathBuf::from(format!("{}{s}", out.display()))).collect()
}

fn remove_store(out: &Path) {
    for f in store_files(out) {
        let _ = std::fs::remove_file(f);
    }
}

fn refused(failures: &[String]) -> (i32, String) {
    (REFUSED, format!("refused: admin-import: {} check(s) failed:\n{}", failures.len(), failures.join("\n")))
}

/// Every key in the store at `rev`, read back through the backend.
fn read_back(b: &SqliteBackend, rev: u64) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut out = BTreeMap::new();
    let mut after: Option<String> = None;
    loop {
        let page = b.range("", after.as_deref(), READ_PAGE, rev).map_err(|e| format!("reading the store back: {e}"))?;
        let full = page.len() == READ_PAGE;
        after = page.last().map(|(k, _)| k.clone());
        out.extend(page.into_iter().map(|(k, v)| (k, v.value)));
        if !full {
            return Ok(out);
        }
    }
}

/// Loads `p` into a new store at `out` and reads it back; `Err` leaves no store behind.
fn load(out: &Path, p: &map::Projection) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let b = SqliteBackend::open(out, Options::default()).map_err(|e| format!("{}: {e}", out.display()))?;
    b.bulk_load(p.rev, p.items.iter().map(|(k, i)| (k.clone(), i.value.clone())))
        .map_err(|e| format!("bulk load into {}: {e}", out.display()))?;
    read_back(&b, p.rev)
}

pub fn cmd_admin_import(args: &[String], conn: &Conn) -> (i32, String) {
    let (Some(out), Some(archive_path)) = (flag(args, "--out"), flag(args, "--archive")) else {
        return (CANNOT_TELL, USAGE.into());
    };
    let from_archive = args.iter().any(|a| a == "--from-archive");
    let (out, archive_path) = (PathBuf::from(out), PathBuf::from(archive_path));
    match serving() {
        Ok(Some(socket)) => {
            return (REFUSED, format!("refused: admin-import: lc-serve is answering on {socket}; stop lc-serve.socket and lc-serve.service first, so nothing writes the store after the export"))
        }
        Ok(None) => {}
        Err(e) => return (CANNOT_TELL, e),
    }
    if let Some(f) = store_files(&out).into_iter().find(|f| f.exists()) {
        return (REFUSED, format!("refused: admin-import: {} exists; the import loads only a new store", f.display()));
    }
    if !from_archive {
        if archive_path.exists() {
            return (REFUSED, format!("refused: admin-import: {} exists; pass --from-archive to import it", archive_path.display()));
        }
        if let Err(e) = archive::export(conn, &archive_path) {
            let _ = std::fs::remove_file(&archive_path);
            return (CANNOT_TELL, format!("cannot tell: admin-import: export: {e}"));
        }
    }
    let a = match archive::read(&archive_path) {
        Ok(a) => a,
        Err(f) => return refused(&f),
    };
    let no_record: BTreeSet<String> = match crate::plan::no_record_ids(conn) {
        Ok(ids) => ids,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: admin-import: the planning types: {e}")),
    };
    let p = match map::project(&a, &no_record) {
        Ok(p) => p,
        Err(f) => return refused(&f),
    };
    let run = match crate::lifecycle_run_dir() {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: admin-import: {e}")),
    };
    let failures = checks::cursors(&run, p.rev);
    if !failures.is_empty() {
        return refused(&failures);
    }
    let got = match load(&out, &p) {
        Ok(g) => g,
        Err(e) => {
            remove_store(&out);
            return (CANNOT_TELL, format!("cannot tell: admin-import: {e}"));
        }
    };
    let mut failures = checks::loaded(&a, &p, &got);
    failures.extend(checks::replay(&a, &no_record));
    if !failures.is_empty() {
        remove_store(&out);
        return refused(&failures);
    }
    let tables: serde_json::Map<String, serde_json::Value> = archive::TABLES
        .iter()
        .map(|t| {
            let n = |m: &BTreeMap<&str, usize>| m.get(t.name).copied().unwrap_or(0);
            (t.name.to_string(), json!({ "archived": a.rows(t.name).len(), "imported": n(&p.imported), "excluded": n(&p.excluded) }))
        })
        .collect();
    let summary = json!({
        "import_rev": p.rev,
        "out": out.display().to_string(),
        "archive": archive_path.display().to_string(),
        "keys": p.items.len(),
        "tables": tables,
        "checks": "passed",
    });
    (0, serde_json::to_string_pretty(&summary).unwrap_or_default())
}

#[cfg(test)]
mod tests;
