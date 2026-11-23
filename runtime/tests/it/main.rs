//! The crate's integration tests, linked as one test binary (sp-qd5io5): each module is one
//! former `tests/<name>.rs` file, which linked as its own executable carrying the whole crate.

#[macro_use]
extern crate spira_test_id;
mod effect_key;
mod emit_to;
mod halt;
mod host_pools;
mod in_process;
mod journal_calls;
mod journal_cannot_tell;
mod keyed_state_clear;
mod memory_only;
mod no_unit_children;
mod observe_mode;
mod open_bounded;
mod open_retries;
mod queued_deadline;
mod status;
mod sys_proc;
mod turn_state;
mod unit_ran;
mod unit_ran_wake;
mod virtual_driver;
