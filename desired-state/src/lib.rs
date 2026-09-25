//! spira's desired-state document: typed resource kinds, the producer contract, and the
//! composer that merges a federation of producer fragments into one versioned composite.
//!
//! `spira apply` and the canonical default document (`examples/default.toml`) build on this
//! library; see `src/main.rs` for the CLI. Status — comparing this desired state against
//! observed reality — is a separate bead's concern; nothing here writes status.

pub mod compose;
pub mod producer;
pub mod resource;
pub mod store;
