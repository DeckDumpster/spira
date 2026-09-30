//! The one boundary: launching `${SPIRA_GH:-gh}`. Production is [`crate::real::RealGh`];
//! unit tests use a fake that records calls and answers canned JSON.

use std::path::Path;

/// What one `gh` invocation returned.
#[derive(Debug, Clone, Default)]
pub struct GhOut {
    pub code: i32,
    pub stdout: Vec<u8>,
}

impl GhOut {
    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

pub trait Gh {
    /// `cd repo && timeout $GH_TIMEOUT $SPIRA_GH <args...>` (stderr discarded), or without a
    /// cwd when `repo` is `None`. Mirrors bash `ghq()` called under `2>/dev/null`.
    fn call(&self, repo: Option<&Path>, args: &[&str]) -> GhOut;

    /// Same, but stdin is `input` (when given) and stdout+stderr are merged — bash's
    /// `branch-protect` (`2>&1`) and `pr-create` (body on stdin) shape.
    fn call_merged(&self, repo: Option<&Path>, args: &[&str], input: Option<&[u8]>) -> GhOut;
}

/// A program run as a plain subprocess (the artifact-zip Python reader, `gh api` for a
/// binary zip download). Kept separate from [`Gh`] because it is not always `gh` itself.
pub trait Proc {
    /// Run `program` with `args`; stdin is `input` when given. Returns (rc, stdout bytes).
    fn run(&self, program: &str, args: &[&str], input: Option<&[u8]>) -> (i32, Vec<u8>);
}
