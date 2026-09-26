//! The reconciler's invariant engine: (desired, observed) in, per-invariant gaps and remedy
//! verdicts out. [`core`] is the pure diff-plus-hysteresis logic; [`alert`] is the pure half
//! of the evidence-carrying wake to the Concierge; [`io`] is the seam that persists
//! hysteresis state across passes and appends each pass's verdicts to the time series.
//! Callers (czar-pass and its successors) own observation — reading units, /proc, the
//! store, the forge — and feed the result in as a [`core::RawStatus`].

pub mod alert;
pub mod core;
pub mod io;

pub use alert::{classify_escalation, compose_alert, should_alert, EscalationClass};
pub use core::{last_remedy, record_remedy, step, HysteresisState, RawStatus, Verdict};
