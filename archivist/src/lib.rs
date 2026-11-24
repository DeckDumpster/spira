//! archivist — rescue a full session's unfinished business before it is cleared
//! (DESIGN.md). [`state`], [`candidates`], [`digest`], [`transcripts`], [`prompt`] and
//! [`config`] are pure or fs-only and table-tested directly. [`seam`] is the impure
//! boundary onto lib.sh's capacity machinery, the `ctx-meter` library and the external tools
//! (`archive.sh`, `mail`, the agent CLI). [`lock`] is the two flocks one archive run
//! holds. [`run`] composes all of it into `sweep`, `list`, `now`, `mark`, `state`,
//! `record` and `digest-send`.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod brain;
pub mod candidates;
pub mod config;
pub mod covered;
pub mod digest;
pub mod lock;
pub mod prompt;
pub mod run;
pub mod seam;
pub mod state;
pub mod transcripts;
