//! `watchd` as a library: the manifest, the log+cursor contract, and `notify`'s pure parts
//! (the actionable filter, the fingerprint, the age format, the ask texts) that the scheduler's
//! `inbox.notify` task reuses. The `watchd` binary is a face over these modules.

pub mod commands;
pub mod context;
pub mod cursor;
pub mod filter;
pub mod fs_ops;
pub mod health;
pub mod iso8601;
pub mod lock;
pub mod manifest;
pub mod next;
pub mod notify;
pub mod ops;
pub mod paths;
pub mod rows;
pub mod send;
pub mod sop_run;
pub mod tail;
