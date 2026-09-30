//! The boundary `real.rs` implements against git, `/proc`, the filesystem and process
//! spawning; the unit tests drive `engine.rs` directly, without a `World` at all, because
//! every decision it makes is a pure function over already-gathered data (DESIGN.md
//! "Design"). `World` exists only for the imperative shell in `main.rs`: resolving a
//! branch's key, reading the state directory, and running the detached gate.

use std::path::{Path, PathBuf};

pub trait World {
    /// `git -C <repo> rev-parse --verify -q <rev>^{commit}`.
    fn rev_parse(&self, repo: &Path, rev: &str) -> Option<String>;
    /// The `lib.sh` seam: `repo_root <name>`, then `spira_landref <repo-root>` if that
    /// resolved. `Err` when the repository map has no entry (fatal — DESIGN.md "The key").
    fn resolve_repo(&self, repo_name: &str) -> Result<PathBuf, String>;
    fn landref(&self, repo: &Path) -> Option<String>;

    fn exists(&self, p: &Path) -> bool;
    /// A file's content with trailing newlines stripped — every state-directory field except
    /// `out` is read this way in the bash, through `$(cat …)` command substitution.
    fn read(&self, p: &Path) -> Option<String>;
    /// `out`'s content verbatim (the bash's plain `cat "$OUT"` for a FAILED verdict prints it
    /// exactly, trailing newline and all).
    fn read_raw(&self, p: &Path) -> Option<String>;
    fn read_i32(&self, p: &Path) -> Option<i32>;
    fn mkdir_p(&self, p: &Path);
    /// Write via `.<name>.<pid>` and rename — never a reader sees a half-written file.
    fn write_atomic(&self, p: &Path, content: &str);
    fn remove_dir_all(&self, p: &Path);

    fn now(&self) -> u64;
    fn sleep(&self, secs: u64);
    fn sleep_ms(&self, ms: u64);
    fn pid(&self) -> i64;

    /// `/proc/<pid>` exists.
    fn proc_exists(&self, pid: i64) -> bool;
    /// `/proc/<pid>/cmdline`, NUL-joined bytes turned into a space-joined string.
    fn proc_cmdline(&self, pid: i64) -> Option<String>;
    /// Field 5 of `/proc/<pid>/stat` — the process group id.
    fn proc_pgid(&self, pid: i64) -> Option<i64>;
    /// Every other pid on the box and its cmdline, for `unmanaged_gate()`. Best-effort: a pid
    /// that vanishes mid-scan is skipped, not an error.
    fn list_other_procs(&self, exclude: i64) -> Vec<(i64, String)>;

    fn kill(&self, pid: i64, group: bool);

    /// Start the detached run: `setsid <this binary> --exec <branch> <repo>` (falling back to
    /// a plain background spawn when `setsid` is unavailable), stdin/stdout/stderr all
    /// `/dev/null`. Returns once the child has been asked to start; it does not wait for it.
    fn spawn_detached(&self, branch: &str, repo_name: &str);

    /// The `--exec` body: run `bash <home>/gate.sh <branch> <repo>`, its combined output
    /// appended to `out_path`. Returns the exit code.
    fn run_gate(&self, home: &Path, branch: &str, repo_name: &str, out_path: &Path) -> i32;

    fn eprintln(&self, s: &str);
    fn print(&self, s: &str);
}
