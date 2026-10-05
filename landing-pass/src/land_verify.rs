//! Landed verification (family R, decomposition row 17, sp-81t4d): `landed`, `landed_sha`,
//! `bead_cited_commit_on_base`, `pr_merged`, `land_subject`, `other_beads_on_conflicts`,
//! `conflict_reopen_note`, `bead_is_work_type`, `bead_close_on_land` — the lib.sh functions
//! this crate absorbed. `content_landed` (the rest of the family) was already native
//! (`Git::content_landed`, `RealGit`); it is untouched here. `bead_land_status` (the
//! family's one dead name) had no caller left anywhere in the tree and is not ported.
//!
//! THREE OUTCOMES, NOT TWO (law-closed-is-not-landed). [`landed`] answers found / not found
//! / cannot tell, and "cannot tell" — an unresolvable land ref — is a NAMED refusal a caller
//! must never fold into "not landed": doing that is exactly what reopened sp-pd-ci-green four
//! times (lib.sh's own `landed` doc comment). [`Lib::close_on_land`] and
//! [`Ports::bead_cited_commit_on_base`]'s git checks both read ancestry with
//! `merge-base --is-ancestor`, never by comparing a tip SHA to a remembered one — a tip
//! moves under a caller holding a stale copy.
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
use std::process::{Command, Stdio};

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
// landed / landed_sha
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `landed`/`landed_sha`'s one search, against refs already resolved by the caller
/// (`spira_landrefs` — `spira_config::repos::landrefs`). `refs` empty is the caller's own
/// "cannot tell" (an unresolvable land ref); this never guesses one. Trusts only two
/// subject shapes — the queue's own merge subject (`land_subject`'s own output) or a
/// bead's own commit (`<id>:` — never a substring, the colon must follow immediately) —
/// never a body mention (sp-dgaig).
pub fn landed(git: &dyn Git, repo: &Path, id: &str, refs: &[String]) -> Option<String> {
    if refs.is_empty() {
        return None;
    }
    let out = git.log_grep(repo, id, refs)?;
    let land = format!("spira: land {id}");
    let land_sp = format!("{land} ");
    let own = format!("{id}:");
    for line in out.lines() {
        let Some((sha, subj)) = line.split_once('\t') else { continue };
        if subj == land || subj.starts_with(&land_sp) || subj.starts_with(&own) {
            return Some(sha.to_string());
        }
    }
    None
}

// ──────────────────────────────────────────────────────────────────────────────
// bead_cited_commit_on_base
// ──────────────────────────────────────────────────────────────────────────────

/// A candidate sha pulled from a bead's notes: `true` for a declared hand-landing
/// ("landed as <sha>" / "hand-landed <sha>"), `false` for a bare hex token in prose.
pub type Candidate = (bool, String);

/// lib.sh `bead_cited_commit_on_base`'s note scan: every "declared" shape across every
/// note, in order, each sha seen once — THEN every bare hex token across every note, same
/// dedup. A bare sha in prose is never sufficient on its own (law-closed-is-not-landed);
/// it only becomes a candidate here, and [`bead_cited_commit_on_base`] still requires the
/// commit it names to cite the bead by id.
pub fn cited_candidates(notes: &[String]) -> Vec<Candidate> {
    let declared = Regex::new(r"(?:landed\s+as|hand-landed)\s+([0-9a-f]{7,40})").expect("static regex");
    let bare = Regex::new(r"[0-9a-f]{7,40}").expect("static regex");
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for n in notes {
        let low = n.to_lowercase();
        for m in declared.captures_iter(&low) {
            let sha = m[1].to_string();
            if seen.insert(sha.clone()) {
                out.push((true, sha));
            }
        }
    }
    for n in notes {
        let low = n.to_lowercase();
        for m in bare.find_iter(&low) {
            let sha = m.as_str().to_string();
            if seen.insert(sha.clone()) {
                out.push((false, sha));
            }
        }
    }
    out
}

/// `(^|[^a-z0-9-])<id>([^a-z0-9-]|$)` against the raw (not lowercased) commit body — the
/// same case-sensitive character class lib.sh's grep used; bead ids are their own lowercase
/// form, so this is deliberately not case-insensitive.
pub fn id_named_in(body: &str, id: &str) -> bool {
    let Ok(re) = Regex::new(&format!(r"(^|[^a-z0-9-]){}([^a-z0-9-]|$)", regex::escape(id))) else { return false };
    re.is_match(body)
}

/// lib.sh `bead_cited_commit_on_base <id> <repo> <base>` → `Some((sha, "cited-declared" |
/// "cited-named"))`. Every candidate must first verify as a real commit AND be an ancestor
/// of `base` (ancestry, never a tip comparison) before its shape is trusted at all.
pub fn bead_cited_commit_on_base(git: &dyn Git, repo: &Path, base: &str, id: &str, notes: &[String]) -> Option<(String, &'static str)> {
    for (declared, sha) in cited_candidates(notes) {
        if git.rev_parse(repo, &format!("{sha}^{{commit}}")).is_none() {
            continue;
        }
        if !git.is_ancestor(repo, &sha, base) {
            continue;
        }
        if declared {
            return Some((sha, "cited-declared"));
        }
        if let Some(body) = git.commit_body(repo, &sha) {
            if id_named_in(&body, id) {
                return Some((sha, "cited-named"));
            }
        }
    }
    None
}

// ──────────────────────────────────────────────────────────────────────────────
// pr_merged
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `pr_merged <repo> <branch>`: a pull request whose head is `branch` is MERGED.
/// Evidence for not reopening, never for deleting (`content_landed` is the exact, local
/// check a destroying caller must use instead).
///
/// `ghq` IS NOT A PROGRAM. lib.sh's `ghq() { command bdq __ghq "$@"; }` is itself a shim
/// onto `bdq`'s internal `__ghq` subcommand (`timeout $GH_TIMEOUT $SPIRA_GH "$@"`, bdq.rs
/// `cmd_ghq`) — there is no `ghq` binary on PATH to exec. This calls that same subcommand
/// directly, exactly what the bash shim would have called.
pub fn pr_merged(repo: &Path, branch: &str) -> bool {
    let mut c = Command::new("bdq");
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
// bead_close_on_land
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `bead_close_on_land`'s own three-way read of a bead's status: `submitted` only
/// when bd's raw status is not already `closed` and the submitted label is present — the
/// one case this function acts on. `BeadRow::status` already folds a submitted label into
/// `"closed"` (sp-qsona) for every OTHER reader, which is exactly why this reads
/// `raw_status` instead: closed and submitted-but-not-yet-closed must stay distinguishable
/// here, or a bead already closed for real gets re-closed.
pub fn close_on_land_status(row: &BeadRow, submitted_label: &str) -> &'static str {
    let submitted = row.labels.iter().any(|l| l == submitted_label);
    if row.raw_status == "closed" {
        "closed"
    } else if submitted {
        "submitted"
    } else {
        "other"
    }
}

fn label_value<'a>(labels: &'a [String], prefix: &str) -> Option<&'a str> {
    labels.iter().find_map(|l| l.strip_prefix(prefix))
}

/// `bdq close <id> --reason-file -`, the reason on stdin — the exact shape
/// `bead_close_on_land`'s heredoc wrote. `true` only on a clean exit.
fn bdq_close(id: &str, reason: &str) -> bool {
    let mut c = Command::new("bdq");
    c.args(["close", id, "--reason-file", "-"]);
    c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    let Ok(mut child) = c.spawn() else { return false };
    if let Some(mut si) = child.stdin.take() {
        use std::io::Write;
        let _ = si.write_all(reason.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// `sending reap-landed-branch [--status-from <f>] <id> <branch> <repo> <why>` — the same
/// shim `spira_reap_landed_branch` execs. `Ok(())` sent, `Err(its output)` otherwise
/// (failed or refused — both are "left for the Sending" from here).
fn sending_reap(status_file: Option<&str>, id: &str, branch: &str, repo: &str, why: &str) -> Result<(), String> {
    let mut c = Command::new("sending");
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

/// lib.sh `bead_close_on_land <id> <sha>` — the only place a work bead is closed for a
/// landed reason. Idempotent both ways: a bead already `closed` is left alone, and one
/// never marked submitted is left alone too. On a successful close, marks the ledger
/// LANDED (a write this crate already owns, [`crate::landstate::land_mark`] — no seam) and
/// best-effort reaps the branch through `sending` (family X's own chokepoint; never touched
/// directly here).
#[allow(clippy::too_many_arguments)]
pub fn close_on_land(git: &dyn Git, out: &Reporter, run: &Path, home: &Path, submitted_label: &str, row: Option<&BeadRow>, id: &str, sha: &str) {
    let Some(row) = row else { return };
    match close_on_land_status(row, submitted_label) {
        "closed" | "other" => return,
        _ => {}
    }
    let shown_sha = if sha.is_empty() { "unknown" } else { sha };
    let reason = format!("OUTCOME: landed\nClosed by the landing pass: work landed at {shown_sha} (law-closed-is-not-landed).\n");
    if !bdq_close(id, &reason) {
        out.log(&format!("land-close {id}: bd close failed — left submitted, CHECK 5 will report it"));
        return;
    }
    out.log(&format!("land-close {id}: closed at {shown_sha} (submitted -> landed)"));
    crate::landstate::land_mark(run, id, "LANDED", sha, "Closed by the landing pass", "");

    let (Some(repo_label), Some(branch_label)) = (label_value(&row.labels, "repo:"), label_value(&row.labels, "branch:")) else { return };
    let reg = spira_config::repos::Registry::from_env(std::env::vars().collect(), home);
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

    // ── landed / landed_sha: a real git repo, both directions + "cannot tell" ──

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

    #[test]
    fn landed_finds_the_writer_commit_by_ancestry_not_by_a_remembered_tip() {
        let dir = testkit::TempDir::new("land-verify-landed-writer");
        git_init(&dir);
        let sha = commit(&dir, "spira: land sp-fix");
        let refs = vec!["main".to_string()];
        assert_eq!(landed(&RealGit, &dir, "sp-fix", &refs), Some(sha));
    }

    #[test]
    fn landed_recognises_the_titled_writer_form_and_the_authors_own_commit() {
        let dir = testkit::TempDir::new("land-verify-landed-titled");
        git_init(&dir);
        let sha = commit(&dir, "spira: land sp-titled — a fix with a title");
        let refs = vec!["main".to_string()];
        assert_eq!(landed(&RealGit, &dir, "sp-titled", &refs), Some(sha));

        let sha2 = commit(&dir, "sp-own: did the thing");
        assert_eq!(landed(&RealGit, &dir, "sp-own", &refs), Some(sha2));
    }

    #[test]
    fn landed_returns_none_when_no_commit_names_the_id() {
        let dir = testkit::TempDir::new("land-verify-landed-none");
        git_init(&dir);
        let refs = vec!["main".to_string()];
        assert_eq!(landed(&RealGit, &dir, "sp-nocommit", &refs), None);
    }

    #[test]
    fn landed_trusts_a_landing_record_not_a_body_mention() {
        // A commit whose subject merely TALKS ABOUT the id must not count (sp-dgaig).
        let dir = testkit::TempDir::new("land-verify-landed-mention");
        git_init(&dir);
        commit(&dir, "fix something related to sp-mentioned's analysis");
        let refs = vec!["main".to_string()];
        assert_eq!(landed(&RealGit, &dir, "sp-mentioned", &refs), None);
    }

    #[test]
    fn landed_cannot_tell_when_refs_do_not_resolve() {
        // The caller's own refusal: empty refs (an unresolvable land ref) is "cannot tell",
        // never folded into "not landed".
        let dir = testkit::TempDir::new("land-verify-landed-cannot-tell");
        git_init(&dir);
        commit(&dir, "spira: land sp-fix");
        assert_eq!(landed(&RealGit, &dir, "sp-fix", &[]), None, "empty refs must be handled by the caller as exit 2, not folded in here");
    }

    // ── bead_cited_commit_on_base ───────────────────────────────────────────

    #[test]
    fn cited_candidates_orders_declared_before_bare_each_deduplicated() {
        let notes = vec!["hand-landed deadbeef1234, also mentions deadbeef1234 again".to_string(), "landed as cafef00dcafef00d".to_string(), "see also abc1234abc1234".to_string()];
        let got = cited_candidates(&notes);
        assert_eq!(got, vec![(true, "deadbeef1234".to_string()), (true, "cafef00dcafef00d".to_string()), (false, "abc1234abc1234".to_string())]);
    }

    #[test]
    fn id_named_in_requires_a_whole_token_not_a_substring() {
        assert!(id_named_in("fixed sp-a today", "sp-a"));
        assert!(id_named_in("sp-a: did the thing", "sp-a"));
        assert!(!id_named_in("sp-ab did the thing", "sp-a"));
        assert!(!id_named_in("xsp-a did the thing", "sp-a"));
    }

    #[test]
    fn bead_cited_commit_on_base_accepts_a_declared_sha_that_verifies_and_is_on_base() {
        let dir = testkit::TempDir::new("land-verify-cited-declared");
        git_init(&dir);
        let sha = commit(&dir, "unrelated subject");
        let notes = vec![format!("hand-landed {sha}")];
        let got = bead_cited_commit_on_base(&RealGit, &dir, "main", "sp-x", &notes);
        assert_eq!(got, Some((sha, "cited-declared")));
    }

    #[test]
    fn bead_cited_commit_on_base_accepts_a_bare_sha_only_when_the_commit_names_the_id() {
        let dir = testkit::TempDir::new("land-verify-cited-named");
        git_init(&dir);
        let sha = commit(&dir, "sp-y: did the fix");
        let notes = vec![format!("see {sha} for the fix")];
        let got = bead_cited_commit_on_base(&RealGit, &dir, "main", "sp-y", &notes);
        assert_eq!(got, Some((sha, "cited-named")));
    }

    #[test]
    fn bead_cited_commit_on_base_refuses_a_bare_sha_whose_commit_does_not_name_the_id() {
        let dir = testkit::TempDir::new("land-verify-cited-bare-refused");
        git_init(&dir);
        let sha = commit(&dir, "unrelated subject, names nobody");
        let notes = vec![format!("see {sha} for the fix")];
        assert_eq!(bead_cited_commit_on_base(&RealGit, &dir, "main", "sp-z", &notes), None, "a bare sha in prose is never sufficient on its own");
    }

    #[test]
    fn bead_cited_commit_on_base_refuses_a_sha_not_on_the_base() {
        let dir = testkit::TempDir::new("land-verify-cited-off-base");
        git_init(&dir);
        // A sha that verifies as a commit but is not reachable from `main` at all.
        let notes = vec!["hand-landed 0000000deadbeef".to_string()];
        assert_eq!(bead_cited_commit_on_base(&RealGit, &dir, "main", "sp-q", &notes), None);
    }

    // ── other_beads_on_conflicts / conflict_reopen_note ─────────────────────

    #[test]
    fn other_bead_ids_sorts_dedups_and_excludes_its_own_id() {
        let subjects = "sp-b: fix\nsp-c: fix\nsp-b: fix again\nsp-own: unrelated";
        assert_eq!(other_bead_ids(subjects, "sp-own"), "sp-b sp-c");
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

    // ── bead_close_on_land's status read ────────────────────────────────────

    fn row(raw_status: &str, labels: &[&str]) -> BeadRow {
        BeadRow {
            id: "sp-a".into(),
            status: raw_status.into(),
            raw_status: raw_status.into(),
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

    #[test]
    fn close_on_land_status_is_closed_when_bd_already_says_closed() {
        assert_eq!(close_on_land_status(&row("closed", &["spira-submitted"]), "spira-submitted"), "closed");
    }

    #[test]
    fn close_on_land_status_is_submitted_only_with_the_label_and_not_yet_closed() {
        assert_eq!(close_on_land_status(&row("open", &["spira-submitted"]), "spira-submitted"), "submitted");
        assert_eq!(close_on_land_status(&row("open", &[]), "spira-submitted"), "other");
    }
}
