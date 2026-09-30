//! suite-select — the one suite selector (DESIGN.md). Pure modules (`header`, `glob`,
//! `names`, `corpus`, `select`, `budget`, `timing`, `gate`) and one IO module (`io`) for git.
//!
//! Every stage fails closed: a selection that cannot be computed is a [`Refusal`], never an
//! empty or a whole-corpus selection by accident.

pub mod budget;
pub mod corpus;
pub mod gate;
pub mod glob;
pub mod header;
pub mod io;
pub mod names;
pub mod select;
pub mod timing;

/// Exit status of a refusal: the selector could not compute a selection (NO_VERDICT at the
/// gate — the machinery, not the branch).
pub const EXIT_REFUSED: i32 = 75;
/// Exit status of an unclaimed source file: the branch changed a source file no suite
/// claims (the branch's fault).
pub const EXIT_UNCLAIMED: i32 = 1;
/// Exit status of a usage error.
pub const EXIT_USAGE: i32 = 2;

/// The selector could not compute a selection; the message says what it could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn refuse<T>(msg: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal(msg.into()))
}
