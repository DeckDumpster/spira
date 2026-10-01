//! Every external dependency gh-intake has, as traits — so `run()` in `logic.rs` is pure
//! decision-making over data a test can hand it directly, and only `real.rs` touches the
//! network, the beads store, or another program. See DESIGN.md §"Ports".

/// GitHub's REST API, read-only. gh-intake.sh never set an `Authorization:` header and
/// this stays true here: `Http` has no credential parameter anywhere in its signature
/// (law-beads-is-never-public).
pub trait Http {
    /// GET `url` with a `timeout_secs` budget. `Ok((status, body))` on any HTTP response
    /// (including 4xx/5xx — the caller decides what a status means); `Err` only for a
    /// transport failure (DNS, connect, timeout).
    fn get(&self, url: &str, timeout_secs: u64) -> Result<(u16, Vec<u8>), String>;
}

/// The beads store, called directly (`bd -C <db> ...`) exactly as gh-intake.sh did — not
/// through lib.sh's `bdq` retry wrapper, which this script never used.
pub trait Bd {
    /// `bd -C <db> list --all --limit 0 --json`, parsed. `None` on any failure to invoke
    /// or parse — never confused with a genuinely empty store (law-absence-needs-a-positive-control).
    fn list_all_json(&self) -> Option<serde_json::Value>;

    /// `bd -C <db> create <title> --external-ref <ref> --labels <labels> -t bug -p <priority>
    /// --body-file -`, body on stdin. Returns whether the call succeeded.
    fn create(&self, title: &str, external_ref: &str, labels: &str, priority: &str, body: &[u8]) -> bool;

    /// `bd -C <db> note <id> <text>`.
    fn note(&self, id: &str, text: &str) -> bool;

    /// `bd -C <db> close <id> --reason <reason>`.
    fn close(&self, id: &str, reason: &str) -> bool;
}

/// `repo_root <name>` from `lib.sh`, unported (spira/lib.sh is last in the rewrite order —
/// see the inventory, group 4). Resolved exactly as gate-check's Rust port resolves it: a
/// `bash -c`, sourcing lib.sh, run once.
pub trait Repo {
    /// `Some(path)` when the map has an entry AND that checkout has a `.git`; `None` otherwise
    /// — the two cases gh-intake.sh's caller collapses into one die().
    fn root_with_git(&self, name: &str) -> Option<String>;
}

/// The one piece of the operator channel gh-intake touches: the daily digest of new
/// untrusted issues. `mail` (rewritten and retired from bash by sp-ooh1k), called by
/// bare name on the launcher PATH.
pub trait Mail {
    fn send_operator_note(&self, subject: &str, body: &[u8]) -> bool;
}
