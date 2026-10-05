//! Every external dependency gh-intake has, as traits — so `run()` in `logic.rs` is pure
//! decision-making over data a test can hand it directly, and only `real.rs` touches the
//! network, the beads store, or another program. See DESIGN.md §"Ports".
//!
//! `Bd`'s closeout-side methods, `Gh` and `Git` were added for the closeout half
//! (sp-j3fim, "wave 4.31", wave4-decomposition.md row AB): gh-intake was a one-way
//! ingest gate until now; it is bidirectional from this bead on. `Mail::send_question`
//! is the operator-ask half of the same addition.

use std::path::Path;

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

    /// `bd -C <db> show <id> --json` — lib.sh's `gh_issue_closeout`/`_gh_close_ask_unblock`
    /// read a bead's `external_ref`/`dependencies` through this (sp-j3fim). `bd show`
    /// returns a bare object OR a one-element array of one; callers normalise either shape.
    fn show_json(&self, id: &str) -> Option<serde_json::Value>;

    /// `bd -C <db> list --status <which> --label <ask-label> --limit 0 --json` (sp-j3fim):
    /// `ask_already_open`/`ask_closed_subject`/`_gh_close_ask_unblock`/
    /// `_gh_resolve_stale_asks` all read the operator-ask queue through this one shape. An
    /// ask is not a work bead, so its bd status is its state (`spira_config::nonwork`, sp-mve9i).
    fn list_asks(&self, which: spira_config::nonwork::Which, label: &str) -> Option<serde_json::Value>;

    /// `bd -C <db> dep remove <id> <other>`, best effort (sp-j3fim, `_gh_close_ask_unblock`).
    fn dep_remove(&self, id: &str, other: &str) -> bool;

    /// `bd -C <db> dep relate <from> <to>`, best effort (sp-j3fim, `_gh_close_ask_unblock`).
    fn dep_relate(&self, from: &str, to: &str) -> bool;
}

/// A bead as the lifecycle machine holds it: its state and the commit that state names (the
/// delivery's merge sha once landed, else the bead's tip).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LcBead {
    pub state: String,
    pub sha: String,
}

/// The lifecycle machine (`spira-lc show`), the one source of bead state.
pub trait Lifecycle {
    /// `Ok(None)` when the machine has no row for `id`; `Err` when it could not answer —
    /// never confused with no row (law-absence-needs-a-positive-control).
    fn bead(&self, id: &str) -> Result<Option<LcBead>, String>;
}

/// `repo_root <name>` (family U) and the base-ref families (U/W), resolved in-process
/// through `spira_config::repos` (sp-k6lku, "wave 4.13"; sp-j3fim extends this with the
/// landref/landrefs/all-names calls the closeout family needs).
pub trait Repo {
    /// `Some(path)` when the map has an entry AND that checkout has a `.git`; `None` otherwise
    /// — the two cases gh-intake.sh's caller collapses into one die().
    fn root_with_git(&self, name: &str) -> Option<String>;

    /// `spira_repos` (sp-j3fim): every repo name the map carries, in map order —
    /// `gh-intake backfill`'s scan loop (ported from `gh-issue-backfill.sh`).
    fn all_names(&self) -> Vec<String>;

    /// `spira_landref <repo-path>` (family W): the single base ref, or `None` when it does
    /// not resolve. Used by `backfill`'s own looser ancestry search (sp-j3fim) — kept
    /// separate from `landrefs` below on purpose; see `Git::grep_ancestor`.
    fn landref(&self, repo_path: &str) -> Option<String>;

    /// `spira_landrefs <repo-path>` (family W): the land ref plus its local counterpart
    /// when one exists, both as refs `Git::landing_commit` greps. Empty when unresolvable.
    fn landrefs(&self, repo_path: &str) -> Vec<String>;
}

/// The operator channel gh-intake touches. `mail` (rewritten and retired from bash by
/// sp-ooh1k), called by bare name on the launcher PATH.
pub trait Mail {
    /// The daily digest of new untrusted issues (ingest side, kind `note`, no default).
    fn send_operator_note(&self, subject: &str, body: &[u8]) -> bool;

    /// `mail send operator --from <from> --subject <subject> --kind question --default
    /// <default> --bead <bead_id>`, body on stdin (sp-j3fim, `gh_issue_ask_unlanded`).
    /// `--bead` is what lets the real `mail` binary wire the ask it creates to the work
    /// bead with `dep relate` (never `dep add` — a reopened bead must not be stranded
    /// behind its own ask). `Ok(())` on success; `Err(stderr)` otherwise — stderr may
    /// itself be empty, exactly as lib.sh's `_err="$(... 2>&1 >/dev/null)"` could capture
    /// nothing from a probe fault.
    fn send_question(&self, from: &str, subject: &str, default: &str, bead_id: &str, body: &[u8]) -> Result<(), String>;
}

/// lib.sh's `ghq` (`ghq() { command bdq __ghq "$@"; }` — never its own binary): `bdq
/// __ghq <args>`, which execs `timeout "${GH_TIMEOUT:-120}" "${SPIRA_GH:-gh}" "$@"`, the
/// one door onto the `gh` CLI every family already shells through. gh-intake's closeout
/// side (sp-j3fim) is the first caller inside this crate; ingest stays HTTP-only.
pub trait Gh {
    /// `ghq issue view <n> --repo <repo> --json state -q .state`, trimmed. Empty string on
    /// any failure to invoke, a non-zero exit, or empty output — the same `|| _st=""` the
    /// bash gave every caller.
    fn issue_state(&self, repo: &str, issue_n: &str) -> String;
    /// `ghq issue comment <n> --repo <repo> --body <body>`.
    fn issue_comment(&self, repo: &str, issue_n: &str, body: &str) -> bool;
    /// `ghq issue close <n> --repo <repo>`.
    fn issue_close(&self, repo: &str, issue_n: &str) -> bool;
}

/// Plain git, read mostly. No credential anywhere (same law as `Http`): every call reads
/// a local checkout already on disk.
pub trait Git {
    /// `git -C <repo> rev-parse --short <sha>`.
    fn rev_parse_short(&self, repo: &Path, sha: &str) -> Option<String>;
    /// `git -C <repo> log --format=%s -1 <sha>`.
    fn subject_of(&self, repo: &Path, sha: &str) -> Option<String>;
    /// lib.sh's newest-landing-commit lookup: the newest commit on any of `refs` whose subject names `id`
    /// (`spira: land <id>`, optionally ` <title>`, or `<id>:...`) — `-F` (fixed-string)
    /// `--grep`, subject-shape checked after. `None` when nothing matches.
    fn landing_commit(&self, repo: &Path, id: &str, refs: &[String]) -> Option<String>;
    /// `gh-issue-backfill.sh`'s OWN looser ancestry search, kept as its own reader rather
    /// than unified with `landing_commit` (test-land-commit-contract.sh, "gap G4": the two are
    /// allowed to disagree in general and are only proven to agree on one fixture): the
    /// first commit on `land_ref` whose message contains `id` anywhere (`git log --grep`,
    /// no `-F`, no subject-shape check).
    fn grep_ancestor(&self, repo: &Path, id: &str, land_ref: &str) -> Option<String>;
    /// `git -C <repo> cat-file -e <sha>` AND `git -C <repo> merge-base --is-ancestor <sha>
    /// <land_ref>` — the backfill script's fallback to the machine's recorded sha.
    fn sha_is_ancestor(&self, repo: &Path, sha: &str, land_ref: &str) -> bool;
}
