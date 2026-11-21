//! The crate's integration tests, linked as one test binary (sp-qd5io5): each module is one
//! former `tests/<name>.rs` file, which linked as its own executable carrying the whole crate.

#[macro_use]
extern crate spira_test_id;
mod agent;
mod callworld;
mod gh_review;
mod ci;
mod jq;
mod loop_gate;
mod loop_ports;
mod loop_registry;
mod loop_rounds;
mod loop_supersede;
mod pilgrim;
mod probe;
mod proctrace;
mod roundvm;
mod runstate;
mod scenario;
mod scenarios_load;
mod world;
