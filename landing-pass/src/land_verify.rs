//! Landed verification (family R, decomposition row 17, sp-81t4d): `pr_merged`,
//! `land_subject`, `other_beads_on_conflicts`, `conflict_reopen_note`, `bead_is_work_type`
//! and the push pass's own close — the lib.sh functions this crate absorbed. The
//! commit-subject and cited-commit oracles (`landed`, `bead_cited_commit_on_base`) are
//! deleted (sp-2c1n0): whether a bead landed is the lifecycle record's LANDED state
//! (`spira-lc state`), never a search of the base's history. Content-on-base is
//! `Git::content_on_base` (`RealGit`).
//!
//! The pr/ask/reap-adjacent pieces (`conflict_reopen_note`'s git plumbing, `close_on_land`'s
//! bd close + reap) run real subprocesses (`bdq`, `sending`) by bare name on PATH
//! (sp-gypjk), the same convention every other direct-subprocess call in this crate and in
//! `groomer`/`rebase-stale` already follows — never through the lib.sh seam, which this
//! family retires for these names.

use crate::model::BeadRow;
use crate::ports::Git;
use crate::report::Reporter;
use regex::Regex;
use std::path::Path;
use std::process::Stdio;

// ──────────────────────────────────────────────────────────────────────────────
// land_subject
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `land_subject <id>`: "spira: land <id>", or "spira: land <id> — <title>" when the
/// bead has a title. `title` is whatever `bd show` returned (already collapsed to one line
/// and cut to 120 characters by the caller's own bd read — see [`collapse_title`]).
pub fn land_subject(id: &str, title: &str) -> String {
    if title.is_empty() {
        format!("spira: land {id}")
    } else {
        format!("spira: land {id} — {title}")
    }
}

/// `" ".join(t.split())[:120]` — the python extraction `land_subject` used: every run of
/// whitespace (including a newline) becomes one space, then the first 120 Unicode
/// characters (not bytes) of that.
pub fn collapse_title(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// bead_is_work_type
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `bead_is_work_type <issue-type>`: true when `t` is one of the space-separated
/// words in `close_types` (`SPIRA_WORK_CLOSE_TYPES`, default `"task bug feature"`).
pub fn is_work_type(t: &str, close_types: &str) -> bool {
    !t.is_empty() && close_types.split_whitespace().any(|w| w == t)
}

// ──────────────────────────────────────────────────────────────────────────────
// pr_merged
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `pr_merged <repo> <branch>`: a pull request whose head is `branch` is MERGED.
/// Evidence for not reopening, never for deleting (`content_on_base` is the exact, local
/// check a destroying caller must use instead).
///
/// `ghq` IS NOT A PROGRAM. lib.sh's `ghq() { command bdq __ghq "$@"; }` is itself a shim
/// onto `bdq`'s internal `__ghq` subcommand (`timeout $GH_TIMEOUT $SPIRA_GH "$@"`, bdq.rs
/// `cmd_ghq`) — there is no `ghq` binary on PATH to exec. This calls that same subcommand
/// directly, exactly what the bash shim would have called.
pub fn pr_merged(repo: &Path, branch: &str) -> bool {
    let mut c = spira_config::bounded::bounded("bdq");
    c.current_dir(repo).arg("__ghq").args(["pr", "view", branch, "--json", "state", "-q", ".state"]);
    c.stdin(Stdio::null()).stderr(Stdio::null());
    match c.output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim() == "MERGED",
        _ => false,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// other_beads_on_conflicts / conflict_reopen_note
// ──────────────────────────────────────────────────────────────────────────────

/// The pure half of lib.sh `other_beads_on_conflicts`: every `<prefix>-[a-z0-9]+` id in
/// `subjects` (git commit subjects), sorted and deduplicated (`sort -u`), excluding
/// `own_id` itself.
pub fn other_bead_ids(subjects: &str, own_id: &str) -> String {
    let prefix = own_id.split('-').next().unwrap_or(own_id);
    let Ok(re) = Regex::new(&format!(r"{}-[a-z0-9]+", regex::escape(prefix))) else { return String::new() };
    let mut ids = std::collections::BTreeSet::new();
    for m in re.find_iter(subjects) {
        let s = m.as_str();
        if s != own_id {
            ids.insert(s.to_string());
        }
    }
    ids.into_iter().collect::<Vec<_>>().join(" ")
}

/// lib.sh `other_beads_on_conflicts <repo> <br> <base> <files>`: space-separated bead ids
/// whose commits touched `files` on `base` since `br` diverged, excluding `br`'s own id.
/// `files` empty, or no merge base, or no matching commits → `""` (never an error the
/// caller must handle — a conflict note is still owed either way).
pub fn other_beads_on_conflicts(git: &dyn Git, repo: &Path, br: &str, base: &str, files: &str) -> String {
    if files.trim().is_empty() {
        return String::new();
    }
    let own_id = br.strip_prefix("spira/").unwrap_or(br);
    let Some(mb) = git.merge_base(repo, base, &format!("refs/heads/{br}")) else { return String::new() };
    let paths: Vec<&str> = files.split_whitespace().collect();
    let Some(subjects) = git.log_subjects(repo, &format!("{}..{base}", mb.trim()), &paths) else { return String::new() };
    if subjects.trim().is_empty() {
        return String::new();
    }
    other_bead_ids(&subjects, own_id)
}

/// The pure half of lib.sh `conflict_reopen_note`: the note text, given everything its git
/// calls would have resolved (`base_display`, `rn` — the commit count, `other_beads`).
#[allow(clippy::too_many_arguments)]
pub fn format_conflict_note(br: &str, base_display: &str, name: &str, conflicts: &str, actor: &str, rq_n: &str, rn: &str, other_beads: &str) -> String {
    let conflicts_disp = if conflicts.is_empty() { "unknown" } else { conflicts };
    let mut note = format!(
        "Reopened by {actor}: {br} does not rebase onto {base_display} in {name}; conflicts in {conflicts_disp}. \
This is rebase-conflict attempt {rq_n} on this bead. The branch carries {rn} commit(s) from the previous \
session — resume from the existing work."
    );
    if other_beads.is_empty() {
        note.push_str(" A merge conflict is not an escalation — the next aeon is handed the rebase and must resolve it.");
    } else {
        note.push_str(&format!(
            " Those files were changed on {base_display} by {other_beads} — check whether this work is already landed before resolving."
        ));
    }
    note
}

/// lib.sh `conflict_reopen_note <repo> <br> <base> <name> <conflicts> <actor> [rq_n=1]`.
#[allow(clippy::too_many_arguments)]
pub fn conflict_reopen_note(git: &dyn Git, repo: &Path, br: &str, base: &str, name: &str, conflicts: &str, actor: &str, rq_n: &str) -> String {
    let base_display = base.strip_prefix("refs/remotes/").unwrap_or(base);
    let rn = git.count(repo, &format!("{base}..{br}")).map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    let other_beads = other_beads_on_conflicts(git, repo, br, base, conflicts);
    format_conflict_note(br, base_display, name, conflicts, actor, rq_n, &rn, &other_beads)
}

// ──────────────────────────────────────────────────────────────────────────────
// close on land (the push pass's own bd close)
// ──────────────────────────────────────────────────────────────────────────────

/// Whether the push pass closes this bead for a landed reason: its lifecycle row says the
/// builder handed it on and it did not end another way — SUBMITTED, CERTIFIED, IN_DELIVERY
/// or LANDED (sp-mve9i; `spira-lc close-on-land`'s own rule). Never bd's status or the
/// retired submitted label. A bead the machine holds no row for is left alone.
pub fn should_close_on_land(row: &BeadRow) -> bool {
    matches!(row.state.as_str(), "SUBMITTED" | "CERTIFIED" | "IN_DELIVERY" | "LANDED")
}

fn label_value<'a>(labels: &'a [String], prefix: &str) -> Option<&'a str> {
    labels.iter().find_map(|l| l.strip_prefix(prefix))
}

/// The close through the lifecycle machine (sp-3fue0j): the row is already LANDED here, so
/// `spira-lc close` records nothing new and closes the store. `true` only on a clean exit.
fn bdq_close(id: &str, reason: &str) -> bool {
    spira_config::lifecycle_row::close_landed(id, reason, "landing-pass").is_ok()
}

/// `sending reap-landed-branch [--status-from <f>] <id> <branch> <repo> <why>` — the same
/// shim `spira_reap_landed_branch` execs. `Ok(())` sent, `Err(its output)` otherwise
/// (failed or refused — both are "left for the Sending" from here).
fn sending_reap(status_file: Option<&str>, id: &str, branch: &str, repo: &str, why: &str) -> Result<(), String> {
    let mut c = spira_config::bounded::bounded("sending");
    c.arg("reap-landed-branch");
    if let Some(f) = status_file {
        c.arg("--status-from").arg(f);
    }
    c.args([id, branch, repo, why]);
    c.stdin(Stdio::null()).stderr(Stdio::null());
    match c.output() {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() {
                Ok(())
            } else {
                Err(if text.is_empty() { "unknown".to_string() } else { text })
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The push pass's close-on-land `<id> <sha>` — the only place a work bead is closed for a
/// landed reason. Acts only on a bead [`should_close_on_land`] says the builder handed on;
/// one never handed on, or ended another way, is left alone. On a successful close, best-effort reaps the branch through `sending` (family X's own chokepoint; never touched
/// directly here).
#[allow(clippy::too_many_arguments)]
pub fn close_on_land(git: &dyn Git, out: &Reporter, home: &Path, row: Option<&BeadRow>, id: &str, sha: &str) {
    let Some(row) = row else { return };
    if !should_close_on_land(row) {
        return;
    }
    let shown_sha = if sha.is_empty() { "unknown" } else { sha };
    let reason = format!("OUTCOME: landed\nClosed by the landing pass: work landed at {shown_sha} (law-closed-is-not-landed).\n");
    if !bdq_close(id, &reason) {
        out.log(&format!("land-close {id}: bd close failed — left submitted (LANDED is on the lifecycle record)"));
        return;
    }
    out.log(&format!("land-close {id}: closed at {shown_sha} (submitted -> landed)"));

    let (Some(repo_label), Some(branch_label)) = (label_value(&row.labels, "repo:"), label_value(&row.labels, "branch:")) else { return };
    let reg = spira_config::repos::Registry::from_env_checkout(std::env::vars().collect(), home);
    let Some(root) = reg.root(repo_label) else { return };
    if !git.branch_exists(Path::new(&root), branch_label) {
        return;
    }
    let status_file = std::env::var("SPIRA_STATUS_FILE").ok();
    match sending_reap(status_file.as_deref(), id, branch_label, &root, &format!("landed at {shown_sha}")) {
        Ok(()) => out.log(&format!("land-close {id}: reaped branch {branch_label}")),
        Err(e) => out.log(&format!("land-close {id}: branch {branch_label} not reaped: {e} — left for the Sending")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    // ── land_subject / collapse_title ──────────────────────────────────────

    #[test]
    fn land_subject_appends_the_title() {
        assert_eq!(land_subject("sp-titled", "a fix with a title"), "spira: land sp-titled — a fix with a title");
    }

    #[test]
    fn land_subject_is_bare_with_no_title() {
        assert_eq!(land_subject("sp-x", ""), "spira: land sp-x");
    }

    #[test]
    fn collapse_title_joins_whitespace_and_cuts_at_120_chars() {
        assert_eq!(collapse_title("a   fix\nwith\ttitle"), "a fix with title");
        let long = "x".repeat(200);
        assert_eq!(collapse_title(&long).chars().count(), 120);
    }

    // ── bead_is_work_type ──────────────────────────────────────────────────

    #[test]
    fn is_work_type_matches_a_word_in_the_list() {
        assert!(is_work_type("bug", "task bug feature"));
        assert!(!is_work_type("spike", "task bug feature"));
        assert!(!is_work_type("", "task bug feature"));
        // A substring is not a word: "buggy" must not match "bug".
        assert!(!is_work_type("buggy", "task bug feature"));
    }

    // ── a real git repo, for the conflict-note family below ──

    fn git_init(dir: &Path) {
        let run = |args: &[&str]| {
            StdCommand::new("git").current_dir(dir).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").args(args).output().unwrap();
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["commit", "-q", "--allow-empty", "-m", "initial"]);
    }

    fn commit(dir: &Path, msg: &str) -> String {
        StdCommand::new("git").current_dir(dir).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").args(["commit", "-q", "--allow-empty", "-m", msg]).output().unwrap();
        String::from_utf8(StdCommand::new("git").current_dir(dir).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string()
    }

    use crate::real::RealGit;

    // ── other_beads_on_conflicts / conflict_reopen_note ─────────────────────

    #[test]
    fn other_bead_ids_sorts_dedups_and_excludes_its_own_id() {
        let subjects = "sp-b: fix\nsp-c: fix\nsp-b: fix again\nsp-own: unrelated";
        assert_eq!(other_bead_ids(subjects, "sp-own"), "sp-b sp-c");
    }

    #[test]
    fn other_bead_ids_keeps_a_longer_id_distinct_from_its_prefix() {
        let subjects = "sp-a9gk: fix\nsp-a9g: other";
        assert_eq!(other_bead_ids(subjects, "sp-a9g"), "sp-a9gk");
        assert_eq!(other_bead_ids(subjects, "sp-a9gk"), "sp-a9g");
    }

    #[test]
    fn other_beads_on_conflicts_is_empty_when_files_is_empty() {
        let dir = testkit::TempDir::new("land-verify-other-beads-empty");
        git_init(&dir);
        assert_eq!(other_beads_on_conflicts(&RealGit, &dir, "spira/sp-a", "main", ""), "");
    }

    #[test]
    fn other_beads_on_conflicts_finds_an_id_that_touched_the_conflicted_file() {
        let dir = testkit::TempDir::new("land-verify-other-beads-real");
        git_init(&dir);
        StdCommand::new("git").current_dir(&dir).args(["branch", "spira/sp-a"]).output().unwrap();
        std::fs::write(dir.join("f.txt"), "base change\n").unwrap();
        StdCommand::new("git").current_dir(&dir).args(["add", "f.txt"]).output().unwrap();
        commit(&dir, "sp-b: touched f.txt on the base");
        assert_eq!(other_beads_on_conflicts(&RealGit, &dir, "spira/sp-a", "main", "f.txt"), "sp-b");
    }

    #[test]
    fn format_conflict_note_names_other_beads_when_present() {
        let note = format_conflict_note("spira/sp-a", "origin/main", "spira", "f.txt", "sentinel", "2", "3", "sp-b");
        assert!(note.contains("rebase-conflict attempt 2"));
        assert!(note.contains("3 commit(s)"));
        assert!(note.contains("Those files were changed on origin/main by sp-b"));
    }

    #[test]
    fn format_conflict_note_names_no_escalation_when_no_other_beads() {
        let note = format_conflict_note("spira/sp-a", "origin/main", "spira", "", "sentinel", "1", "0", "");
        assert!(note.contains("conflicts in unknown"));
        assert!(note.contains("A merge conflict is not an escalation"));
    }

    // ── close-on-land's state read ─────────────────────────────────────

    fn row(state: &str, labels: &[&str]) -> BeadRow {
        BeadRow {
            id: "sp-a".into(),
            state: state.into(),
            repo: "spira".into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            superseded: false,
            closed_at: "9999-99-99".into(),
            priority: 1,
            external_ref: None,
            title: String::new(),
            notes: Vec::new(),
        }
    }

    /// sp-mve9i: the close is the lifecycle row's call. A bead the builder handed on (the
    /// machine's SUBMITTED through LANDED) is closed whatever bd shows; one still with a
    /// builder, one dropped or superseded, or one with no row is left alone — the submitted
    /// label (still on this row) decides nothing.
    #[test]
    fn close_on_land_reads_the_lifecycle_state_not_bd_status_or_the_label() {
        for st in ["SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED"] {
            assert!(should_close_on_land(&row(st, &[])), "{st}");
        }
        for st in ["READY", "WORKING", "REWORK", "DROPPED", "SUPERSEDED", "DONE", "-"] {
            assert!(!should_close_on_land(&row(st, &["spira-submitted"])), "{st}");
        }
    }
}
