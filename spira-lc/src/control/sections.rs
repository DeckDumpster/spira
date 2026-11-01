//! The control store's section verbs: the library's `section`, `intent`, `lock` and `pass`,
//! and `publish-section` (the lifecycle section), which reads this binary's own records.

pub use spira_lc::sections::*;

pub mod lifecycle;
