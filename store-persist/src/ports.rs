//! What store-persist needs from outside itself: a clock, and a Dolt engine.

use std::path::Path;

pub trait Clock {
    fn now(&self) -> u64;
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
    /// Opens the database at `db_dir` with no server and returns `dolt_hashof_db()`.
    fn verify(&self, db_dir: &Path) -> Result<String, String>;
    /// Whether `pid` is a live Dolt process.
    fn alive(&self, pid: u32) -> bool;
}
