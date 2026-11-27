//! mimir-trigger as a library: the pure trigger engine ([`engine`]), the lane family
//! ([`lanes`]) and the observe reads the scheduler's
//! trigger tasks (`trigger.mimir`, `trigger.groom`, `trigger.warden`) share ([`observe`]).

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod engine;
pub mod lanes;
pub mod observe;
