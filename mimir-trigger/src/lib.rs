//! mimir-trigger as a library: the pure trigger engine ([`engine`]), the lane family
//! ([`lanes`]), the binary's ports ([`ports`], [`real`]) and the observe reads the scheduler's
//! trigger tasks (`trigger.mimir`, `trigger.groom`, `trigger.warden`) share ([`observe`]).

pub mod engine;
pub mod lanes;
pub mod observe;
pub mod ports;
pub mod real;
