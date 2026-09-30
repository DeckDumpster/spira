//! The boundary `real.rs` implements against the filesystem, the environment and
//! `tap_jsonl_rows` (`spira/tap-jsonl.sh`, not ported here — DESIGN.md "Non-goals").

use std::path::{Path, PathBuf};

/// One suite's `.result` file, already located and its basename extracted.
#[derive(Clone, Debug)]
pub struct ResultFile {
    pub suite: String,
    pub result_path: PathBuf,
    pub out_path: PathBuf,
}

pub trait World {
    /// `<root>/*.result` then `<root>/*/*.result`, each group sorted, in that order — the
    /// same two-shape scan and glob order the bash's `for f in "$ROOT"/*.result
    /// "$ROOT"/*/*.result` produced.
    fn result_files(&self, root: &Path) -> Vec<ResultFile>;

    /// A `.result` file's fields: `(status, seconds, rc)`. Missing or unparseable numeric
    /// fields come back as `None`.
    fn read_result(&self, p: &Path) -> (String, Option<u64>, Option<i32>);

    fn read(&self, p: &Path) -> Option<String>;

    /// The suite's own source file at `<home>/<suite>`, for the declared-timeout comment.
    fn suite_source(&self, home: &Path, suite: &str) -> Option<String>;

    /// `<root>-retry`'s result for `suite` (same two-level scan), first field only.
    fn retry_status(&self, root: &Path, suite: &str) -> Option<String>;

    /// `tap_jsonl_rows <suite> <src> <out> <fallback-status> <secs>` via the shared
    /// `tap-jsonl.sh` (DESIGN.md "Non-goals") — its stdout, one JSON row per line.
    fn tap_jsonl_rows(&self, home: &Path, suite: &str, src: &Path, out: &Path, fallback: &str, secs: &str) -> String;

    fn write(&self, p: &Path, content: &str);
    fn append(&self, p: &Path, content: &str);

    fn github_actions(&self) -> bool;
    fn step_summary_path(&self) -> Option<PathBuf>;
    fn batch_tail_lines(&self) -> usize;
    fn suite_timeout_default(&self) -> u64;

    fn print(&self, s: &str);
}
