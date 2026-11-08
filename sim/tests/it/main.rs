//! The crate's integration tests, linked as one test binary (sp-qd5io5): each module is one
//! former `tests/<name>.rs` file, which linked as its own executable carrying the whole crate.

mod common;

mod actors;
mod fixture_schema;
mod profile_tasks;
mod replay;
mod runtime_tasks;
mod whole_loop;
