//! archivist — rescue a full session's unfinished business before it is cleared
//! (DESIGN.md). [`state`], [`candidates`], [`digest`], [`transcripts`], [`prompt`] and
//! [`config`] are pure or fs-only and table-tested directly. [`seam`] is the impure
//! boundary onto lib.sh's capacity machinery and the external tools (`ctx-meter.sh`,
//! `archive.sh`, `mail.sh`, the agent CLI). [`lock`] is the two flocks one archive run
//! holds. [`run`] composes all of it into `sweep`, `list`, `now`, `mark`, `state`,
//! `record` and `digest-send`.

pub mod candidates;
pub mod config;
pub mod digest;
pub mod lock;
pub mod prompt;
pub mod run;
pub mod seam;
pub mod state;
pub mod transcripts;
