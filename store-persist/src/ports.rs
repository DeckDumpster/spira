//! What store-persist needs from outside itself: a clock, and a Dolt engine.

use std::path::Path;

pub trait Clock {
    fn now_ms(&self) -> u64;
    fn sleep(&self, secs: u64);
}

/// How a sync reaches the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conn {
    /// Through the running server, as its local superuser over TCP, so the CLI can never fall
    /// back to opening the store itself and taking the lock the server restarts on.
    Server { port: u16, secret: String },
    /// No server holds the store: the CLI opens `<data>/<db>` directly.
    Offline,
}

pub trait Dolt {
    /// `CALL dolt_backup('sync-url', 'file://<dest>')` on `data/db`: branches, tags and working
    /// sets, incremental against what `dest` already holds.
    fn sync(&self, conn: &Conn, data: &Path, db: &str, dest: &Path) -> Result<(), String>;
    /// `CALL dolt_gc()` on `data/db` (`--full` when `full`): rewrites the live store without the
    /// chunks no root reaches.
    fn gc(&self, conn: &Conn, data: &Path, db: &str, full: bool) -> Result<(), String>;
    /// Length of `main`'s commit log.
    fn history_len(&self, conn: &Conn, data: &Path, db: &str) -> Result<u64, String>;
    /// Builds a side branch holding the history bounded to `keep`; returns the tip of `main` it was cut from.
    fn squash_history(&self, conn: &Conn, data: &Path, db: &str, keep: u64) -> Result<String, String>;
    /// Replays what `main` gained since `tip` onto the side branch and moves `main` onto it.
    fn graft_history(&self, conn: &Conn, data: &Path, db: &str, tip: &str) -> Result<(), String>;
    /// Removes a side branch an interrupted rewrite left.
    fn abandon_history(&self, conn: &Conn, data: &Path, db: &str) -> Result<(), String>;
    /// Opens the database at `db_dir` with no server and returns `dolt_hashof_db()`.
    fn verify(&self, db_dir: &Path) -> Result<String, String>;
    /// Whether `pid` is a live Dolt process.
    fn alive(&self, pid: u32) -> bool;
}
