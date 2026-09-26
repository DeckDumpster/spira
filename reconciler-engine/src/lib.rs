//! The reconciler's invariant engine: (desired, observed) in, per-invariant gaps and remedy
//! verdicts out. [`core`] is the pure diff-plus-hysteresis logic; [`io`] is the seam that
//! persists hysteresis state across passes and appends each pass's verdicts to the time
//! series. Callers (czar-pass and its successors) own observation — reading units, /proc,
//! the store, the forge — and feed the result in as a [`core::RawStatus`].

pub mod core;
pub mod io;

pub use core::{last_remedy, record_remedy, step, HysteresisState, RawStatus, Verdict};
