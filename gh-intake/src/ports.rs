//! Every external dependency gh-intake has, as traits — so `run()` in `logic.rs` is pure
//! decision-making over data a test can hand it directly, and only `real.rs` touches the
//! network, the beads store, or another program. See DESIGN.md §"Ports".
//!
//! `Gh`, `Git`, `Closeouts` and `Mail::send_question` are the closeout half: the write-back
//! to the public tracker, driven by the pending-closeout rows.

use crate::store::KnownRef;
use std::path::Path;

/// GitHub's REST API, read-only. `Http` has no credential parameter anywhere in its
/// signature: the real adapter attaches the forge App token itself, and no caller can
/// choose to send anything else (law-beads-is-never-public).
pub trait Http {
    /// GET `url` with a `timeout_secs` budget. `Ok((status, body))` on any HTTP response
    /// (including 4xx/5xx — the caller decides what a status means); `Err` only for a
    /// transport failure (DNS, connect, timeout).
    fn get(&self, url: &str, timeout_secs: u64) -> Result<(u16, Vec<u8>), String>;
}

/// When intake last completed a poll: the only memory a run keeps between runs, so that a
/// rate-limit deferral can tell one refused run from a persistent one.
pub trait Stamp {
    fn now(&self) -> i64;
    fn last_ok(&self) -> Option<i64>;
    fn mark_ok(&self, at: i64);
}

/// The issue numbers a repository already had open when intake first read it: recorded so a
/// backlog is never filed, and the only way to tell a never-read repository from a quiet one.
pub trait Seen {
    /// `None` when intake has never read `slug`.
    fn load(&self, slug: &str) -> Option<Vec<u64>>;
    fn save(&self, slug: &str, numbers: &[u64]);
    /// The external reference and bead id of the last work bead filed for `slug`.
    fn last_filed(&self, slug: &str) -> Option<(String, String)>;
    fn save_last_filed(&self, slug: &str, external_ref: &str, id: &str);
}

/// The beads store, called directly (`bd -C <db> ...`) exactly as gh-intake.sh did — not
/// through lib.sh's `bdq` retry wrapper, which this script never used.
pub trait Bd {
    /// The beads whose external reference is exactly `external_ref`, any status. `Err` when
    /// the store could not answer — never read as none (law-absence-needs-a-positive-control).
    fn by_ref(&self, external_ref: &str) -> Result<Vec<KnownRef>, String>;

    /// `bd -C <db> create <title> --external-ref <ref> --labels <labels> -t bug -p <priority>
    /// --body-file -`, body on stdin. Returns the new bead's id, `None` when it was not filed.
    fn create(&self, title: &str, external_ref: &str, labels: &str, priority: &str, body: &[u8]) -> Option<String>;

    /// `bd -C <db> note <id> <text>`.
    fn note(&self, id: &str, text: &str) -> bool;

    /// `bd -C <db> close <id> --reason <reason>`.
    fn close(&self, id: &str, reason: &str) -> bool;

    /// `bd -C <db> show <id> --json` — lib.sh's `gh_issue_closeout`/`_gh_close_ask_unblock`
    /// read a bead's `external_ref`/`dependencies` through this (sp-j3fim). `bd show`
    /// returns a bare object OR a one-element array of one; callers normalise either shape.
    fn show_json(&self, id: &str) -> Option<serde_json::Value>;

    /// `bd -C <db> dep remove <id> <other>`, best effort (sp-j3fim, `_gh_close_ask_unblock`).
    fn dep_remove(&self, id: &str, other: &str) -> bool;

    /// `bd -C <db> dep relate <from> <to>`, best effort (sp-j3fim, `_gh_close_ask_unblock`).
    fn dep_relate(&self, from: &str, to: &str) -> bool;
}

/// A bead as the lifecycle machine holds it: its state and the commit that state names (the
/// delivery's merge sha once landed, else the bead's tip).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LcBead {
    pub state: String,
    pub sha: String,
    /// The row's `reason`: a SUPERSEDED row's successor id, a DROPPED row's drop reason.
    pub reason: String,
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

    /// `owner/name` of the GitHub remote of registered repo `name`'s checkout; `None` when the
    /// repo has no checkout, no remote, or a remote that is not on GitHub.
    fn forge_slug(&self, _name: &str) -> Option<String> {
        None
    }

    /// Every repo name the map carries, in map order.
    fn all_names(&self) -> Vec<String>;

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
    fn send_question(&self, from: &str, subject: &str, default: &str, bead_id: &str, body: &[u8]) -> Result<String, String>;
}

/// The `gh` CLI, run as `timeout "${GH_TIMEOUT:-120}" "${SPIRA_GH:-gh}" <args>`. gh-intake's
/// closeout side is the first caller inside this crate; ingest stays HTTP-only.
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
}

/// One pending closeout: a `github:` bead that reached LANDED, SUPERSEDED or DROPPED, as the
/// lifecycle store recorded it in the transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub bead_id: String,
    pub external_ref: String,
    pub outcome_state: String,
    pub sha: String,
    /// The operator ask opened for this bead, once there is one.
    pub ask_id: Option<String>,
    pub attempts: u64,
}

/// The pending closeouts (`spira-lc gh-closeout`): the only list a drain reads.
pub trait Closeouts {
    fn pending(&self) -> Result<Vec<Pending>, String>;
    /// The issue is settled: remove the row.
    fn done(&self, bead: &str) -> bool;
    fn asked(&self, bead: &str, ask: &str) -> bool;
    /// An attempt failed: count it, keep the row.
    fn failed(&self, bead: &str) -> bool;
}
