//! The test registry: each test's lifecycle state and its history, in one SQLite file only this
//! crate's binary opens. A test with no row is active.

pub mod cli;
pub mod id;
pub mod store;

pub use store::{
    activate, deny_list, digest, disable, history, init, list, open, quarantine, show, Row, State,
};
