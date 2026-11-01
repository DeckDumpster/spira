//! The control store's off-box copy without a second reader: lc-serve, the only process that
//! opens the store, ships every run of revisions and an hourly snapshot through a put-only key,
//! and a restore is the snapshot plus a replay of the runs after it. Nothing here depends on
//! SQLite's WAL format.

pub mod drill;
pub mod put;
pub mod restore;
pub mod revlog;
pub mod ship;
