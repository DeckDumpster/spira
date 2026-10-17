//! The mender's side of a failing round pass. `pack` is what it is given, the read verbs are
//! how it asks for more, `outcome` is how it answers. Every other fact about the failure —
//! the deadline, the caps, the escalation check — is the lifecycle machine's, not this crate's.

pub mod telemetry;
pub mod outcome;
pub mod pack;
pub mod signature;
pub mod verbs;
