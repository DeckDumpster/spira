//! Native port of `spira/lib.sh`'s GitHub issue-closeout family (sp-j3fim, "wave 4.31",
//! wave4-decomposition.md row AB): `gh_issue_closeout`, `_gh_close_ask_unblock`,
//! `gh_issue_ask_unlanded`, `_gh_resolve_stale_asks`, `_gh_unlanded_scan`. It also carries
//! `ask_already_open` and `ask_closed_subject` — family C's dedupe primitives, left in
//! lib.sh by sp-31hjr ("wave 4.30") purely because this family was the last bash caller
//! naming them directly.
//!
//! `ask_already_open` is NOT shared with sentinel's or landing-pass's own native copies
//! (sp-31hjr gave each of those crates its own; this codebase does not thread a shared lib
//! dependency across binaries for a function this small — see `sending`'s `landed()`,
//! `cockpit-collect`'s and `sentinel`'s own copies of the same subject-grep shape for the
//! established precedent). This is gh-intake's own copy, not a bash fallback — lib.sh's
//! copy retires outright once this lands: no bash caller is left anywhere in the tree.
//!
//! The write-back complement to gh-intake's one-way ingest (`logic.rs`). Intake holds no
//! credential; this runs only from the credentialed landing path, exactly as lib.sh's
//! comment on `gh_issue_closeout` said. No bead notes, bodies or internal judgement reach
//! the public tracker (law-beads-is-never-public): every string sent to GitHub below is
//! built from a commit sha/subject (already public) or a fixed sentence.

use crate::ports::{Bd, Gh, Git, Mail, Repo};
use std::path::{Path, PathBuf};

// ────────────────────────────────────────────────────────────────────────────────────────
// Settings
// ────────────────────────────────────────────────────────────────────────────────────────

pub struct Ctx {
    /// `$SPIRA_RUN` — `gh-closed/<id>` markers, the `landstate/<id>` read, and the
    /// `gh-wait-log/<id>` in-flight throttle all live under this.
    pub run: PathBuf,
    /// `$SPIRA_ASK_LABEL` — the label an operator ask carries.
    pub ask_label: String,
    /// `$SPIRA_GH_ASK_GRACE_SECS` (default 3600): a bead closed more recently than this is
    /// not yet asked about — closing the bead and landing its commit are separate passes.
    pub grace_secs: i64,
}

pub struct Deps<'a> {
    pub bd: &'a dyn Bd,
    pub gh: &'a dyn Gh,
    pub git: &'a dyn Git,
    pub repo: &'a dyn Repo,
    pub mail: &'a dyn Mail,
}

// ────────────────────────────────────────────────────────────────────────────────────────
// $SPIRA_RUN state — plain fs, not behind a port (single-process file I/O; see
// landing-pass/src/landstate.rs for the same choice).
// ────────────────────────────────────────────────────────────────────────────────────────

mod run_state {
    use std::fs;
    use std::path::Path;

    pub fn mark_exists(run: &Path, id: &str) -> bool {
        run.join("gh-closed").join(id).exists()
    }

    pub fn mark_write(run: &Path, id: &str) {
        let dir = run.join("gh-closed");
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join(id), b"");
    }

    /// `$SPIRA_RUN/landstate/<id>`'s first line, split on whitespace: (state, sha).
    pub fn landstate(run: &Path, id: &str) -> Option<(String, String)> {
        let text = fs::read_to_string(run.join("landstate").join(id)).ok()?;
        let line = text.lines().next()?;
        let mut it = line.split_whitespace();
        let state = it.next()?.to_string();
        let sha = it.next().unwrap_or("").to_string();
        Some((state, sha))
    }

    /// The in-flight wait-log throttle (`$SPIRA_RUN/gh-wait-log/<id>`): true — and the
    /// file is refreshed to `now` — when nothing was logged in the last `grace` seconds;
    /// false, with no write, when the last log is still fresh.
    pub fn wait_due(run: &Path, id: &str, now: i64, grace: i64) -> bool {
        let dir = run.join("gh-wait-log");
        let path = dir.join(id);
        let last: Option<i64> = fs::read_to_string(&path).ok().and_then(|s| s.split_whitespace().next().and_then(|t| t.parse().ok()));
        let due = match last {
            None => true,
            Some(l) => now - l > grace,
        };
        if due {
            let _ = fs::create_dir_all(&dir);
            let _ = fs::write(&path, format!("{now}\n"));
        }
        due
    }
}

// ────────────────────────────────────────────────────────────────────────────────────────
// Pure parsing / formatting — every string below matches lib.sh byte-for-byte.
// ────────────────────────────────────────────────────────────────────────────────────────

/// `ext_ref#github:*` -> (repo, issue number). `gh_repo` is everything before the FIRST
/// `#`; `issue_n` is everything after the LAST `#` (lib.sh: `${gh_part%%#*}` /
/// `${gh_part##*#}`). `None` when the issue number is empty or not all digits.
pub fn parse_gh_ref(ext_ref: &str) -> Option<(String, String)> {
    let rest = ext_ref.strip_prefix("github:").unwrap_or(ext_ref);
    let gh_repo = rest.split('#').next().unwrap_or("").to_string();
    let issue_n = rest.rsplit('#').next().unwrap_or("").to_string();
    if issue_n.is_empty() || !issue_n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((gh_repo, issue_n))
}

/// `gh_issue_ask_unlanded`'s dedupe subject, also what `_gh_resolve_stale_asks` parses
/// back out of a title.
pub fn close_issue_subject(ext_ref: &str, id: &str) -> String {
    format!("Close GitHub issue {ext_ref} for bead {id}")
}

/// `^Close GitHub issue (\S+) for bead (\S+)$` -> (ext_ref, bead_id); `None` on no match.
pub fn parse_close_issue_title(title: &str) -> Option<(String, String)> {
    let rest = title.strip_prefix("Close GitHub issue ")?;
    let (ext, bid) = rest.split_once(" for bead ")?;
    if ext.is_empty() || bid.is_empty() || ext.chars().any(char::is_whitespace) || bid.chars().any(char::is_whitespace) {
        return None;
    }
    Some((ext.to_string(), bid.to_string()))
}

/// `gh_issue_closeout`'s comment body: `"Fixed in <sha_short>[ (<subject>)]\n\n
/// https://github.com/<gh_repo>/commit/<sha>"`.
pub fn comment_body(sha_short: &str, subject: &str, gh_repo: &str, sha: &str) -> String {
    let t = if subject.is_empty() { String::new() } else { format!(" ({subject})") };
    format!("Fixed in {sha_short}{t}\n\nhttps://github.com/{gh_repo}/commit/{sha}")
}

/// `gh_issue_ask_unlanded`'s mail body.
pub fn ask_unlanded_body(subj: &str, dflt: &str, id: &str, ext_ref: &str) -> String {
    format!(
        "## Question\n{subj}\n\n## Default\n{dflt}\n\n{id} was closed without a commit landing on the base branch, but it links to GitHub issue {ext_ref} which is still open.\n\nSuggested public reply: \"{dflt}\"\n"
    )
}

/// lib.sh `ask_already_open <subject>` — true when `subject` is a SUBSTRING of any title
/// in `open_titles` (python's `want in title`, not an exact match — see
/// `find_exact_open_ask` below for `_gh_close_ask_unblock`'s own exact-match dedupe).
pub fn ask_already_open(open_titles: &[String], subject: &str) -> bool {
    !subject.is_empty() && open_titles.iter().any(|t| t.contains(subject))
}

/// lib.sh `ask_closed_subject <subject>` — the id of the first CLOSED ask whose title
/// contains `subject`, in the order the store returned them.
pub fn ask_closed_subject(closed_rows: &[(String, String)], subject: &str) -> Option<String> {
    if subject.is_empty() {
        return None;
    }
    closed_rows.iter().find(|(_, t)| t.contains(subject)).map(|(id, _)| id.clone())
}

/// `_gh_close_ask_unblock`'s own ask lookup — an EXACT title match (`want == title`), not
/// `ask_already_open`'s substring test.
fn find_exact_open_ask(open_rows: &[(String, String)], subject: &str) -> Option<String> {
    open_rows.iter().find(|(_, t)| t == subject).map(|(id, _)| id.clone())
}

fn short7(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn short8(sha: &str) -> String {
    sha.chars().take(8).collect()
}

// ────────────────────────────────────────────────────────────────────────────────────────
// bd row shapes
// ────────────────────────────────────────────────────────────────────────────────────────

/// `bd list --json`'s wrapper shapes: a bare array, or an object carrying `issues`/`data`.
fn list_rows(v: &serde_json::Value) -> Vec<&serde_json::Value> {
    match v {
        serde_json::Value::Array(a) => a.iter().collect(),
        serde_json::Value::Object(o) => o.get("issues").or_else(|| o.get("data")).and_then(|x| x.as_array()).map(|a| a.iter().collect()).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `bd show --json`'s shape: a bare object, or a one-element array of one.
fn show_row(v: &serde_json::Value) -> Option<&serde_json::Value> {
    match v {
        serde_json::Value::Array(a) => a.first(),
        o @ serde_json::Value::Object(_) => Some(o),
        _ => None,
    }
}

fn titles(v: &serde_json::Value) -> Vec<(String, String)> {
    list_rows(v)
        .into_iter()
        .map(|r| {
            let id = r.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let title = r.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
            (id, title)
        })
        .collect()
}

fn external_ref_of(v: &serde_json::Value) -> String {
    show_row(v).and_then(|r| r.get("external_ref")).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn blocks(v: &serde_json::Value, ask_id: &str) -> bool {
    let Some(row) = show_row(v) else { return false };
    row.get("dependencies")
        .and_then(|d| d.as_array())
        .map(|deps| {
            deps.iter().any(|dep| {
                dep.get("dependency_type").and_then(|x| x.as_str()) == Some("blocks") && dep.get("id").and_then(|x| x.as_str()) == Some(ask_id)
            })
        })
        .unwrap_or(false)
}

pub struct ClosedGhRow {
    pub id: String,
    pub ext: String,
    pub superseder: Option<String>,
    pub repo_label: String,
    pub closed_at: String,
}

fn closed_github_rows(v: &serde_json::Value) -> Vec<ClosedGhRow> {
    list_rows(v)
        .into_iter()
        .filter_map(|r| {
            if r.get("status").and_then(|s| s.as_str()) != Some("closed") {
                return None;
            }
            let ext = r.get("external_ref").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if !ext.starts_with("github:") {
                return None;
            }
            let id = r.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if id.is_empty() {
                return None;
            }
            let superseder = r.get("dependencies").and_then(|d| d.as_array()).and_then(|deps| {
                deps.iter().find_map(|dep| {
                    let t = dep.get("dependency_type").or_else(|| dep.get("type")).and_then(|x| x.as_str());
                    if t == Some("supersedes") {
                        dep.get("id").and_then(|x| x.as_str()).or_else(|| dep.get("blocked_by").and_then(|x| x.as_str())).map(String::from)
                    } else {
                        None
                    }
                })
            });
            let repo_label = r
                .get("labels")
                .and_then(|l| l.as_array())
                .and_then(|a| a.iter().find_map(|l| l.as_str().and_then(|s| s.strip_prefix("repo:")).map(String::from)))
                .unwrap_or_default();
            let closed_at = r.get("closed_at").and_then(|x| x.as_str()).unwrap_or("").to_string();
            Some(ClosedGhRow { id, ext, superseder, repo_label, closed_at })
        })
        .collect()
}

// ────────────────────────────────────────────────────────────────────────────────────────
// Orchestration — one function per lib.sh function, same names in the doc comments.
// ────────────────────────────────────────────────────────────────────────────────────────

/// lib.sh `gh_issue_closeout <bead-id> <landed-sha> <repo-path>`.
pub fn gh_issue_closeout(d: &Deps, ctx: &Ctx, id: &str, sha: &str, repo_path: &Path) -> Vec<String> {
    let mut log = Vec::new();
    if run_state::mark_exists(&ctx.run, id) {
        return log;
    }

    let ext_ref = d.bd.show_json(id).map(|v| external_ref_of(&v)).unwrap_or_default();
    if !ext_ref.starts_with("github:") {
        return log;
    }

    let Some((gh_repo, issue_n)) = parse_gh_ref(&ext_ref) else {
        log.push(format!("gh-closeout {id}: malformed external_ref {ext_ref} — skipping"));
        return log;
    };

    if d.gh.issue_state(&gh_repo, &issue_n) == "CLOSED" {
        run_state::mark_write(&ctx.run, id);
        return log;
    }

    let sha_short = d.git.rev_parse_short(repo_path, sha).unwrap_or_else(|| short7(sha));
    let subject = d.git.subject_of(repo_path, sha).unwrap_or_default();
    let body = comment_body(&sha_short, &subject, &gh_repo, sha);

    if d.gh.issue_comment(&gh_repo, &issue_n, &body) && d.gh.issue_close(&gh_repo, &issue_n) {
        run_state::mark_write(&ctx.run, id);
        log.push(format!("gh-closeout {id}: closed {ext_ref} as {sha_short}"));
    } else {
        log.push(format!("gh-closeout {id}: could not comment or close {ext_ref}"));
    }
    log
}

/// lib.sh `_gh_close_ask_unblock <subject> <work-bead-id>`.
pub fn gh_close_ask_unblock(d: &Deps, ctx: &Ctx, subj: &str, work_id: &str) -> Vec<String> {
    let mut log = Vec::new();
    let Some(open) = d.bd.list_by_label("open", &ctx.ask_label) else { return log };
    let Some(ask_id) = find_exact_open_ask(&titles(&open), subj) else { return log };
    let Some(row) = d.bd.show_json(work_id) else { return log };
    if !blocks(&row, &ask_id) {
        return log;
    }
    let _ = d.bd.dep_remove(work_id, &ask_id);
    let _ = d.bd.dep_relate(&ask_id, work_id);
    log.push(format!("gh-closeout {work_id}: converted blocking ask {ask_id} to relates_to"));
    log
}

/// lib.sh `gh_issue_ask_unlanded <bead-id> <external-ref> [draft]`.
pub fn gh_issue_ask_unlanded(d: &Deps, ctx: &Ctx, id: &str, ext_ref: &str, draft: Option<&str>) -> Vec<String> {
    let mut log = Vec::new();
    let Some((gh_repo, issue_n)) = parse_gh_ref(ext_ref) else { return log };

    if run_state::mark_exists(&ctx.run, id) {
        return log;
    }

    if d.gh.issue_state(&gh_repo, &issue_n) == "CLOSED" {
        run_state::mark_write(&ctx.run, id);
        log.push(format!("gh-closeout {id}: {ext_ref} already closed on forge — skipping"));
        return log;
    }

    let subj = close_issue_subject(ext_ref, id);
    log.extend(gh_close_ask_unblock(d, ctx, &subj, id));

    let open = d.bd.list_by_label("open", &ctx.ask_label);
    let open_titles: Vec<String> = open.as_ref().map(titles).unwrap_or_default().into_iter().map(|(_, t)| t).collect();
    if ask_already_open(&open_titles, &subj) {
        return log;
    }

    let closed = d.bd.list_by_label("closed", &ctx.ask_label);
    if let Some(answered) = closed.as_ref().and_then(|v| ask_closed_subject(&titles(v), &subj)) {
        run_state::mark_write(&ctx.run, id);
        log.push(format!("gh-closeout {id}: ask {answered} already answered — marker written, no re-ask"));
        return log;
    }

    let dflt = draft.filter(|d| !d.is_empty()).unwrap_or("post a comment explaining the resolution and close the issue");
    let body = ask_unlanded_body(&subj, dflt, id, ext_ref);
    match d.mail.send_question("Landing gate <gate@spira>", &subj, dflt, body.as_bytes()) {
        Ok(()) => log.push(format!("gh-closeout {id}: asked operator about {ext_ref}")),
        Err(e) if !e.is_empty() => log.push(format!("gh-closeout {id}: ask refused ({e})")),
        Err(_) => log.push(format!("gh-closeout {id}: ask refused — probe fault: mail produced no reason")),
    }
    log
}

/// lib.sh `_gh_resolve_stale_asks`.
pub fn gh_resolve_stale_asks(d: &Deps, ctx: &Ctx) -> Vec<String> {
    let mut log = Vec::new();
    let Some(open) = d.bd.list_by_label("open", &ctx.ask_label) else { return log };
    for (ask_id, title) in titles(&open) {
        let Some((ext, bid)) = parse_close_issue_title(&title) else { continue };
        let Some((gh_repo, issue_n)) = parse_gh_ref(&ext) else { continue };
        if d.gh.issue_state(&gh_repo, &issue_n) != "CLOSED" {
            continue;
        }
        run_state::mark_write(&ctx.run, &bid);
        let reason = format!("{ext} is closed on GitHub — resolved automatically; nothing further for the operator.\n");
        let _ = d.bd.close(&ask_id, &reason);
        log.push(format!("gh-closeout {bid}: {ext} found closed — resolved stale ask {ask_id}"));
    }
    log
}

/// lib.sh `_gh_unlanded_scan`.
pub fn gh_unlanded_scan(d: &Deps, ctx: &Ctx, now: i64) -> Vec<String> {
    let mut log = gh_resolve_stale_asks(d, ctx);

    let Some(all) = d.bd.list_all_json() else { return log };
    for row in closed_github_rows(&all) {
        if run_state::mark_exists(&ctx.run, &row.id) {
            continue;
        }

        if let Some(rp) = d.repo.root_with_git(&row.repo_label) {
            let refs = d.repo.landrefs(&rp);
            if let Some(sha) = d.git.landed_sha(Path::new(&rp), &row.id, &refs) {
                log.extend(gh_issue_closeout(d, ctx, &row.id, &sha, Path::new(&rp)));
                continue;
            }
            if let Some(sup) = row.superseder.as_deref().filter(|s| !s.is_empty()) {
                if let Some(sha) = d.git.landed_sha(Path::new(&rp), sup, &refs) {
                    log.extend(gh_issue_closeout(d, ctx, &row.id, &sha, Path::new(&rp)));
                    continue;
                }
            }
        }

        let ls = run_state::landstate(&ctx.run, &row.id);
        if ls.as_ref().map(|(s, _)| s.as_str()) == Some("LANDED") {
            continue;
        }

        if matches!(ls.as_ref().map(|(s, _)| s.as_str()), Some("CERTIFIED") | Some("BATCHED") | Some("GATED") | Some("REBASED") | Some("CONTENT")) {
            let (state, _) = ls.unwrap();
            if run_state::wait_due(&ctx.run, &row.id, now, 3600) {
                log.push(format!("gh-closeout {}: {} in flight ({state}) — waiting on landing", row.id, row.ext));
            }
            continue;
        }

        let mut draft = String::new();
        if let Some(sup) = row.superseder.as_deref().filter(|s| !s.is_empty()) {
            if let Some((sup_st, sup_sha)) = run_state::landstate(&ctx.run, sup) {
                if sup_st == "LANDED" {
                    draft = format!("This issue was fixed by {sup} ({})", short8(&sup_sha));
                }
            }
        }

        if !row.closed_at.is_empty() {
            if let Some(age) = seconds_since(&row.closed_at, now) {
                if age < ctx.grace_secs {
                    continue;
                }
            }
        }

        log.extend(gh_issue_ask_unlanded(d, ctx, &row.id, &row.ext, if draft.is_empty() { None } else { Some(&draft) }));
    }
    log
}

/// `gh-issue-backfill.sh` (sp-j3fim): apply `gh_issue_closeout` to existing closed+landed
/// beads, with its OWN looser ancestry search — see `Git::grep_ancestor`'s doc comment for
/// why this is deliberately not `gh_unlanded_scan`'s `landed_sha`. Returns the report lines
/// plus (found, closed, skipped, would-close) for the summary line's counts.
pub fn backfill(d: &Deps, ctx: &Ctx, dry_run: bool) -> (Vec<String>, u32, u32, u32, u32) {
    let mut out = Vec::new();
    let (mut n_found, mut n_closed, mut n_skipped, mut n_dry) = (0u32, 0u32, 0u32, 0u32);

    let Some(all) = d.bd.list_all_json() else {
        out.push("gh-issue-backfill: could not read the store".to_string());
        return (out, 0, 0, 0, 0);
    };

    for row in closed_github_rows(&all) {
        n_found += 1;

        if run_state::mark_exists(&ctx.run, &row.id) {
            out.push(format!("gh-issue-backfill: {}: already closed — skipping", row.id));
            n_skipped += 1;
            continue;
        }

        let ls_sha = run_state::landstate(&ctx.run, &row.id).map(|(_, sha)| sha).unwrap_or_default();

        let mut found: Option<(String, String)> = None; // (sha, repo_path)
        for name in d.repo.all_names() {
            let Some(rp) = d.repo.root_with_git(&name) else { continue };
            let Some(lref) = d.repo.landref(&rp) else { continue };
            if let Some(sha) = d.git.grep_ancestor(Path::new(&rp), &row.id, &lref) {
                found = Some((sha, rp));
                break;
            }
            if !ls_sha.is_empty() && d.git.sha_is_ancestor(Path::new(&rp), &ls_sha, &lref) {
                found = Some((ls_sha.clone(), rp));
                break;
            }
        }

        let Some((sha, repo_path)) = found else {
            if dry_run {
                out.push(format!("gh-issue-backfill: {}: no commit on land ref — would ask operator", row.id));
            } else {
                out.push(format!("gh-issue-backfill: {}: no commit on land ref — asking operator", row.id));
                out.extend(gh_issue_ask_unlanded(d, ctx, &row.id, &row.ext, None));
            }
            n_skipped += 1;
            continue;
        };

        if dry_run {
            out.push(format!("would close {} ({}) as {}", row.ext, row.id, short8(&sha)));
            n_dry += 1;
            continue;
        }

        out.extend(gh_issue_closeout(d, ctx, &row.id, &sha, Path::new(&repo_path)));
        if run_state::mark_exists(&ctx.run, &row.id) {
            n_closed += 1;
        } else {
            n_skipped += 1;
        }
    }

    if dry_run {
        out.push(format!("gh-issue-backfill: dry run — found {n_found}, would close {n_dry}, skip {}", n_found - n_dry));
    } else {
        out.push(format!("gh-issue-backfill: found {n_found}, closed {n_closed}, skipped {n_skipped}"));
    }
    (out, n_found, n_closed, n_skipped, n_dry)
}

// ────────────────────────────────────────────────────────────────────────────────────────
// A tiny hand-rolled ISO-8601 UTC parser — the same one landing-pass/src/util.rs carries
// for `bead_context`'s age (sp-31hjr); duplicated rather than shared, same reasoning as
// `ask_already_open` above.
// ────────────────────────────────────────────────────────────────────────────────────────

fn parse_iso(ts: &str) -> Option<i64> {
    let ts = ts.trim();
    if ts.len() < 19 {
        return None;
    }
    let b = ts.as_bytes();
    let num = |a: usize, z: usize| -> Option<i64> { ts.get(a..z)?.parse().ok() };
    if b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d, h, mi, s) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut rest = &ts[19..];
    if let Some(r) = rest.strip_prefix('.') {
        let n = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        rest = &r[n..];
    }
    let off = if rest == "Z" {
        0
    } else if rest.len() == 6 && (rest.starts_with('+') || rest.starts_with('-')) && &rest[3..4] == ":" {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let oh: i64 = rest[1..3].parse().ok()?;
        let om: i64 = rest[4..6].parse().ok()?;
        sign * (oh * 3600 + om * 60)
    } else {
        return None;
    };
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + s - off)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn seconds_since(iso: &str, now: i64) -> Option<i64> {
    parse_iso(iso).map(|t| now - t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{Bd, Gh, Git, Mail, Repo};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    // ── pure parsing / formatting ───────────────────────────────────────────────────────

    #[test]
    fn parse_gh_ref_splits_on_first_and_last_hash() {
        assert_eq!(parse_gh_ref("github:fixture/testrepo#7"), Some(("fixture/testrepo".into(), "7".into())));
        assert_eq!(parse_gh_ref("github:fixture/testrepo#"), None);
        assert_eq!(parse_gh_ref("github:fixture/testrepo#7x"), None);
        assert_eq!(parse_gh_ref("github:"), None);
    }

    #[test]
    fn close_issue_subject_round_trips_through_parse() {
        let s = close_issue_subject("github:a/b#5", "sp-x");
        assert_eq!(s, "Close GitHub issue github:a/b#5 for bead sp-x");
        assert_eq!(parse_close_issue_title(&s), Some(("github:a/b#5".into(), "sp-x".into())));
        assert_eq!(parse_close_issue_title("Close GitHub issue a b for bead sp-x"), None, "whitespace inside a \\S+ group must refuse");
        assert_eq!(parse_close_issue_title("Close GitHub issue a#1 for bead sp-x trailing"), None, "anchored at $, no trailing text");
    }

    #[test]
    fn comment_body_cites_sha_and_commit_link_with_and_without_subject() {
        let b = comment_body("abc1234", "a fix", "fixture/testrepo", "abc1234full");
        assert_eq!(b, "Fixed in abc1234 (a fix)\n\nhttps://github.com/fixture/testrepo/commit/abc1234full");
        let b2 = comment_body("abc1234", "", "fixture/testrepo", "abc1234full");
        assert_eq!(b2, "Fixed in abc1234\n\nhttps://github.com/fixture/testrepo/commit/abc1234full");
    }

    #[test]
    fn ask_already_open_is_substring_not_exact() {
        let open = vec!["Close GitHub issue github:a/b#1 for bead sp-x — extra".to_string()];
        assert!(ask_already_open(&open, "Close GitHub issue github:a/b#1 for bead sp-x"));
        assert!(!ask_already_open(&open, ""));
        assert!(!ask_already_open(&[], "anything"));
    }

    #[test]
    fn find_exact_open_ask_requires_exact_title_match() {
        let open = vec![("sp-ask1".to_string(), "Close GitHub issue github:a/b#1 for bead sp-x — extra".to_string())];
        assert_eq!(find_exact_open_ask(&open, "Close GitHub issue github:a/b#1 for bead sp-x"), None);
        let exact = vec![("sp-ask2".to_string(), "Close GitHub issue github:a/b#1 for bead sp-x".to_string())];
        assert_eq!(find_exact_open_ask(&exact, "Close GitHub issue github:a/b#1 for bead sp-x"), Some("sp-ask2".to_string()));
    }

    #[test]
    fn ask_closed_subject_takes_the_first_substring_match() {
        let closed = vec![("sp-a".to_string(), "noise".to_string()), ("sp-b".to_string(), "Close GitHub issue x#1 for bead sp-y".to_string())];
        assert_eq!(ask_closed_subject(&closed, "Close GitHub issue x#1 for bead sp-y"), Some("sp-b".to_string()));
        assert_eq!(ask_closed_subject(&closed, "nothing matches this"), None);
    }

    // ── fakes ────────────────────────────────────────────────────────────────────────────

    #[derive(Default)]
    struct FakeBd {
        show: RefCell<BTreeMap<String, serde_json::Value>>,
        open: RefCell<Vec<(String, String)>>, // (id, title)
        closed: RefCell<Vec<(String, String)>>,
        all: RefCell<serde_json::Value>,
        dep_removes: RefCell<Vec<(String, String)>>,
        dep_relates: RefCell<Vec<(String, String)>>,
        closes: RefCell<Vec<(String, String)>>,
    }

    impl Bd for FakeBd {
        fn list_all_json(&self) -> Option<serde_json::Value> {
            Some(self.all.borrow().clone())
        }
        fn create(&self, _: &str, _: &str, _: &str, _: &str, _: &[u8]) -> bool {
            true
        }
        fn note(&self, _: &str, _: &str) -> bool {
            true
        }
        fn close(&self, id: &str, reason: &str) -> bool {
            self.closes.borrow_mut().push((id.to_string(), reason.to_string()));
            self.closed.borrow_mut().retain(|(i, _)| i != id);
            true
        }
        fn show_json(&self, id: &str) -> Option<serde_json::Value> {
            self.show.borrow().get(id).cloned()
        }
        fn list_by_label(&self, status: &str, _label: &str) -> Option<serde_json::Value> {
            let rows = match status {
                "open" => self.open.borrow().clone(),
                "closed" => self.closed.borrow().clone(),
                _ => Vec::new(),
            };
            Some(serde_json::Value::Array(rows.into_iter().map(|(id, title)| serde_json::json!({"id": id, "title": title})).collect()))
        }
        fn dep_remove(&self, id: &str, other: &str) -> bool {
            self.dep_removes.borrow_mut().push((id.to_string(), other.to_string()));
            true
        }
        fn dep_relate(&self, from: &str, to: &str) -> bool {
            self.dep_relates.borrow_mut().push((from.to_string(), to.to_string()));
            true
        }
    }

    #[derive(Default)]
    struct FakeGh {
        state: RefCell<BTreeMap<String, String>>, // "repo#n" -> state
        comments: RefCell<Vec<(String, String, String)>>,
        closes: RefCell<Vec<(String, String)>>,
        fail_comment: RefCell<bool>,
    }

    impl FakeGh {
        fn set_state(&self, repo: &str, n: &str, st: &str) {
            self.state.borrow_mut().insert(format!("{repo}#{n}"), st.to_string());
        }
    }

    impl Gh for FakeGh {
        fn issue_state(&self, repo: &str, issue_n: &str) -> String {
            self.state.borrow().get(&format!("{repo}#{issue_n}")).cloned().unwrap_or_default()
        }
        fn issue_comment(&self, repo: &str, issue_n: &str, body: &str) -> bool {
            if *self.fail_comment.borrow() {
                return false;
            }
            self.comments.borrow_mut().push((repo.to_string(), issue_n.to_string(), body.to_string()));
            true
        }
        fn issue_close(&self, repo: &str, issue_n: &str) -> bool {
            self.closes.borrow_mut().push((repo.to_string(), issue_n.to_string()));
            true
        }
    }

    #[derive(Default)]
    struct FakeGit {
        /// repo-path -> id -> sha, for `landed_sha`/`grep_ancestor`.
        landed: RefCell<BTreeMap<String, BTreeMap<String, String>>>,
        subjects: RefCell<BTreeMap<String, String>>,
    }

    impl FakeGit {
        fn seed_landed(&self, repo: &str, id: &str, sha: &str) {
            self.landed.borrow_mut().entry(repo.to_string()).or_default().insert(id.to_string(), sha.to_string());
        }
    }

    impl Git for FakeGit {
        fn rev_parse_short(&self, _repo: &Path, sha: &str) -> Option<String> {
            Some(sha.chars().take(7).collect())
        }
        fn subject_of(&self, _repo: &Path, sha: &str) -> Option<String> {
            self.subjects.borrow().get(sha).cloned()
        }
        fn landed_sha(&self, repo: &Path, id: &str, refs: &[String]) -> Option<String> {
            if refs.is_empty() {
                return None;
            }
            self.landed.borrow().get(&repo.display().to_string()).and_then(|m| m.get(id)).cloned()
        }
        fn grep_ancestor(&self, repo: &Path, id: &str, _land_ref: &str) -> Option<String> {
            self.landed.borrow().get(&repo.display().to_string()).and_then(|m| m.get(id)).cloned()
        }
        fn sha_is_ancestor(&self, _repo: &Path, _sha: &str, _land_ref: &str) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct FakeRepo {
        names: RefCell<Vec<String>>,
        roots: RefCell<BTreeMap<String, String>>,
        landrefs_map: RefCell<BTreeMap<String, Vec<String>>>,
        landref_map: RefCell<BTreeMap<String, String>>,
    }

    impl FakeRepo {
        fn add(&self, name: &str, root: &str, landref: &str) {
            self.names.borrow_mut().push(name.to_string());
            self.roots.borrow_mut().insert(name.to_string(), root.to_string());
            self.landrefs_map.borrow_mut().insert(root.to_string(), vec![landref.to_string()]);
            self.landref_map.borrow_mut().insert(root.to_string(), landref.to_string());
        }
    }

    impl Repo for FakeRepo {
        fn root_with_git(&self, name: &str) -> Option<String> {
            self.roots.borrow().get(name).cloned()
        }
        fn all_names(&self) -> Vec<String> {
            self.names.borrow().clone()
        }
        fn landref(&self, repo_path: &str) -> Option<String> {
            self.landref_map.borrow().get(repo_path).cloned()
        }
        fn landrefs(&self, repo_path: &str) -> Vec<String> {
            self.landrefs_map.borrow().get(repo_path).cloned().unwrap_or_default()
        }
    }

    #[derive(Default)]
    struct FakeMail {
        sent: RefCell<Vec<(String, String, String, String)>>, // from, subject, default, body
        fail: RefCell<Option<String>>,
    }

    impl Mail for FakeMail {
        fn send_operator_note(&self, _subject: &str, _body: &[u8]) -> bool {
            true
        }
        fn send_question(&self, from: &str, subject: &str, default: &str, body: &[u8]) -> Result<(), String> {
            if let Some(e) = self.fail.borrow().clone() {
                return Err(e);
            }
            self.sent.borrow_mut().push((from.to_string(), subject.to_string(), default.to_string(), String::from_utf8_lossy(body).into_owned()));
            Ok(())
        }
    }

    fn ctx(run: &Path) -> Ctx {
        Ctx { run: run.to_path_buf(), ask_label: "needs-operator".to_string(), grace_secs: 3600 }
    }

    // ── gh_issue_closeout ────────────────────────────────────────────────────────────────

    #[test]
    fn closeout_comments_and_closes_a_landed_issue_then_is_idempotent() {
        let tmp = testkit::TempDir::new("gh-intake-closeout-idempotent");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        bd.show.borrow_mut().insert("sp-a".into(), serde_json::json!({"external_ref": "github:fixture/testrepo#7"}));
        let gh = FakeGh::default();
        gh.set_state("fixture/testrepo", "7", "OPEN");
        let git = FakeGit::default();
        git.subjects.borrow_mut().insert("deadbeef".into(), "the fix".into());
        let repo = FakeRepo::default();
        let mail = FakeMail::default();
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let c = ctx(&run);

        let log = gh_issue_closeout(&d, &c, "sp-a", "deadbeef", Path::new("/r"));
        assert_eq!(gh.comments.borrow().len(), 1);
        assert_eq!(gh.closes.borrow().len(), 1);
        assert!(log[0].contains("closed github:fixture/testrepo#7 as deadbee"), "{log:?}");
        assert!(run_state::mark_exists(&run, "sp-a"));

        // second call: idempotent, no gh call.
        let log2 = gh_issue_closeout(&d, &c, "sp-a", "deadbeef", Path::new("/r"));
        assert!(log2.is_empty());
        assert_eq!(gh.comments.borrow().len(), 1, "no second comment call");
    }

    #[test]
    fn closeout_is_a_no_op_for_a_bead_with_no_github_ref() {
        let tmp = testkit::TempDir::new("gh-intake-closeout-no-ref");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        bd.show.borrow_mut().insert("sp-b".into(), serde_json::json!({"external_ref": ""}));
        let (gh, git, repo, mail) = (FakeGh::default(), FakeGit::default(), FakeRepo::default(), FakeMail::default());
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let log = gh_issue_closeout(&d, &ctx(&run), "sp-b", "deadbeef", Path::new("/r"));
        assert!(log.is_empty());
        assert!(gh.comments.borrow().is_empty());
        assert!(!run_state::mark_exists(&run, "sp-b"));
    }

    #[test]
    fn closeout_skips_an_already_closed_forge_issue_but_still_marks_it() {
        let tmp = testkit::TempDir::new("gh-intake-closeout-already-closed");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        bd.show.borrow_mut().insert("sp-c".into(), serde_json::json!({"external_ref": "github:fixture/testrepo#9"}));
        let gh = FakeGh::default();
        gh.set_state("fixture/testrepo", "9", "CLOSED");
        let (git, repo, mail) = (FakeGit::default(), FakeRepo::default(), FakeMail::default());
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        gh_issue_closeout(&d, &ctx(&run), "sp-c", "deadbeef", Path::new("/r"));
        assert!(gh.comments.borrow().is_empty());
        assert!(run_state::mark_exists(&run, "sp-c"));
    }

    // ── gh_issue_ask_unlanded + dedupe, both directions ─────────────────────────────────

    #[test]
    fn ask_unlanded_files_an_ask_when_none_exists_then_dedupes_the_second_pass() {
        // DIRECTION 1: a missed ask must not stay missed — the first call files one.
        let tmp = testkit::TempDir::new("gh-intake-ask-dedupe");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        let gh = FakeGh::default();
        gh.set_state("fixture/testrepo", "2", "OPEN");
        let (git, repo) = (FakeGit::default(), FakeRepo::default());
        let mail = FakeMail::default();
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let c = ctx(&run);

        let log = gh_issue_ask_unlanded(&d, &c, "sp-scan2", "github:fixture/testrepo#2", None);
        assert!(log.iter().any(|l| l.contains("asked operator")), "{log:?}");
        assert_eq!(mail.sent.borrow().len(), 1);
        let subj = close_issue_subject("github:fixture/testrepo#2", "sp-scan2");
        bd.open.borrow_mut().push(("sp-ask1".to_string(), subj));

        // DIRECTION 2: a duplicate ask must not spam — the open tracking bead dedupes it.
        let log2 = gh_issue_ask_unlanded(&d, &c, "sp-scan2", "github:fixture/testrepo#2", None);
        assert!(log2.is_empty(), "{log2:?}");
        assert_eq!(mail.sent.borrow().len(), 1, "no second mail");
    }

    #[test]
    fn ask_unlanded_writes_the_durable_marker_once_answered_and_asks_no_more() {
        let tmp = testkit::TempDir::new("gh-intake-ask-answered");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        let subj = close_issue_subject("github:fixture/testrepo#92", "sp-scan2");
        bd.closed.borrow_mut().push(("sp-ask1".to_string(), subj));
        let gh = FakeGh::default();
        gh.set_state("fixture/testrepo", "92", "OPEN");
        let (git, repo) = (FakeGit::default(), FakeRepo::default());
        let mail = FakeMail::default();
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let c = ctx(&run);

        let log = gh_issue_ask_unlanded(&d, &c, "sp-scan2", "github:fixture/testrepo#92", None);
        assert!(log.iter().any(|l| l.contains("already answered")), "{log:?}");
        assert!(mail.sent.borrow().is_empty(), "no mail once already answered");
        assert!(run_state::mark_exists(&run, "sp-scan2"));

        // Regression (Defect 2): the next scan must stay silent, not re-ask.
        let log2 = gh_issue_ask_unlanded(&d, &c, "sp-scan2", "github:fixture/testrepo#92", None);
        assert!(log2.is_empty());
    }

    #[test]
    fn ask_unlanded_malformed_ref_is_silently_skipped() {
        let tmp = testkit::TempDir::new("gh-intake-ask-malformed");
        let run = tmp.path().join("run");
        let (bd, gh, git, repo, mail) = (FakeBd::default(), FakeGh::default(), FakeGit::default(), FakeRepo::default(), FakeMail::default());
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let log = gh_issue_ask_unlanded(&d, &ctx(&run), "sp-x", "github:fixture/testrepo#", None);
        assert!(log.is_empty());
    }

    #[test]
    fn ask_unlanded_mail_refusal_is_logged_with_reason_and_no_asked_line() {
        let tmp = testkit::TempDir::new("gh-intake-ask-refused");
        let run = tmp.path().join("run");
        let (bd, gh) = (FakeBd::default(), FakeGh::default());
        gh.set_state("fixture/testrepo", "3", "OPEN");
        let (git, repo) = (FakeGit::default(), FakeRepo::default());
        let mail = FakeMail::default();
        *mail.fail.borrow_mut() = Some("repeat refused — stub".to_string());
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let log = gh_issue_ask_unlanded(&d, &ctx(&run), "sp-y", "github:fixture/testrepo#3", None);
        assert!(log.iter().any(|l| l.contains("ask refused") && l.contains("repeat refused")), "{log:?}");
        assert!(!log.iter().any(|l| l.contains("asked operator")));
    }

    #[test]
    fn ask_unlanded_mail_probe_fault_with_empty_stderr_is_named() {
        let tmp = testkit::TempDir::new("gh-intake-ask-probe-fault");
        let run = tmp.path().join("run");
        let (bd, gh) = (FakeBd::default(), FakeGh::default());
        gh.set_state("fixture/testrepo", "3", "OPEN");
        let (git, repo) = (FakeGit::default(), FakeRepo::default());
        let mail = FakeMail::default();
        *mail.fail.borrow_mut() = Some(String::new());
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let log = gh_issue_ask_unlanded(&d, &ctx(&run), "sp-y", "github:fixture/testrepo#3", None);
        assert!(log.iter().any(|l| l.contains("probe fault")), "{log:?}");
    }

    // ── _gh_close_ask_unblock ───────────────────────────────────────────────────────────

    #[test]
    fn close_ask_unblock_converts_a_blocking_dep_and_ignores_a_non_blocking_one() {
        let tmp = testkit::TempDir::new("gh-intake-unblock");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        let subj = close_issue_subject("github:fixture/testrepo#101", "sp-ct1");
        bd.open.borrow_mut().push(("sp-ask1".to_string(), subj.clone()));
        bd.show.borrow_mut().insert("sp-ct1".into(), serde_json::json!({"dependencies": [{"dependency_type": "blocks", "id": "sp-ask1"}]}));
        let (gh, git, repo, mail) = (FakeGh::default(), FakeGit::default(), FakeRepo::default(), FakeMail::default());
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let log = gh_close_ask_unblock(&d, &ctx(&run), &subj, "sp-ct1");
        assert!(log.iter().any(|l| l.contains("converted blocking ask sp-ask1")), "{log:?}");
        assert_eq!(bd.dep_removes.borrow().as_slice(), &[("sp-ct1".to_string(), "sp-ask1".to_string())]);
        assert_eq!(bd.dep_relates.borrow().as_slice(), &[("sp-ask1".to_string(), "sp-ct1".to_string())]);

        // A new ask relates_to from the start (mail's own wiring) — no blocking dep, so
        // unblock is a no-op: positive control that the detector only fires when asked.
        bd.show.borrow_mut().insert("sp-ct2".into(), serde_json::json!({"dependencies": []}));
        let subj2 = close_issue_subject("github:fixture/testrepo#102", "sp-ct2");
        bd.open.borrow_mut().push(("sp-ask2".to_string(), subj2.clone()));
        let log2 = gh_close_ask_unblock(&d, &ctx(&run), &subj2, "sp-ct2");
        assert!(log2.is_empty());
    }

    // ── _gh_resolve_stale_asks ───────────────────────────────────────────────────────────

    #[test]
    fn resolve_stale_asks_closes_an_open_ask_once_its_issue_is_closed_on_the_forge() {
        let tmp = testkit::TempDir::new("gh-intake-stale-asks");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        let subj = close_issue_subject("github:fixture/testrepo#95", "sp-scan5");
        bd.open.borrow_mut().push(("sp-ask5".to_string(), subj));
        let gh = FakeGh::default();
        gh.set_state("fixture/testrepo", "95", "CLOSED");
        let (git, repo, mail) = (FakeGit::default(), FakeRepo::default(), FakeMail::default());
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let log = gh_resolve_stale_asks(&d, &ctx(&run));
        assert!(log.iter().any(|l| l.contains("resolved stale ask sp-ask5")), "{log:?}");
        assert_eq!(bd.closes.borrow().len(), 1);
        assert!(run_state::mark_exists(&run, "sp-scan5"));
    }

    // ── gh_unlanded_scan ─────────────────────────────────────────────────────────────────

    fn seed_closed_github(bd: &FakeBd, rows: Vec<serde_json::Value>) {
        *bd.all.borrow_mut() = serde_json::Value::Array(rows);
    }

    #[test]
    fn unlanded_scan_asks_for_a_truly_unlanded_bead_but_not_for_one_landed_by_commit() {
        let tmp = testkit::TempDir::new("gh-intake-scan-landed-vs-not");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        bd.show.borrow_mut().insert("sp-landed".into(), serde_json::json!({"external_ref": "github:fixture/testrepo#94"}));
        bd.show.borrow_mut().insert("sp-unlanded".into(), serde_json::json!({"external_ref": "github:fixture/testrepo#95"}));
        seed_closed_github(
            &bd,
            vec![
                serde_json::json!({"id":"sp-landed","status":"closed","external_ref":"github:fixture/testrepo#94","labels":["repo:fixture"],"closed_at":"2026-09-05T00:00:00Z"}),
                serde_json::json!({"id":"sp-unlanded","status":"closed","external_ref":"github:fixture/testrepo#95","labels":["repo:fixture"],"closed_at":"2000-01-01T00:00:00Z"}),
            ],
        );
        let gh = FakeGh::default();
        gh.set_state("fixture/testrepo", "94", "OPEN");
        gh.set_state("fixture/testrepo", "95", "OPEN");
        let git = FakeGit::default();
        git.seed_landed("/r", "sp-landed", "cafebabe");
        let repo = FakeRepo::default();
        repo.add("fixture", "/r", "origin/main");
        let mail = FakeMail::default();
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let now = 2_000_000_000; // long after both fixture timestamps

        let log = gh_unlanded_scan(&d, &ctx(&run), now);
        assert!(log.iter().any(|l| l.contains("closed github:fixture/testrepo#94")), "landed bead should be closed out: {log:?}");
        assert!(log.iter().any(|l| l.contains("asked operator about github:fixture/testrepo#95")), "unlanded bead should be asked about: {log:?}");
        assert!(run_state::mark_exists(&run, "sp-landed"));
        assert!(!run_state::mark_exists(&run, "sp-unlanded"));
    }

    #[test]
    fn unlanded_scan_waits_silently_on_a_certified_landstate_and_throttles_the_wait_log() {
        let tmp = testkit::TempDir::new("gh-intake-scan-certified");
        let run = tmp.path().join("run");
        std::fs::create_dir_all(run.join("landstate")).unwrap();
        std::fs::write(run.join("landstate").join("sp-scan1"), "CERTIFIED abc1234 1700000000\n").unwrap();
        let bd = FakeBd::default();
        seed_closed_github(&bd, vec![serde_json::json!({"id":"sp-scan1","status":"closed","external_ref":"github:fixture/testrepo#91","labels":[],"closed_at":"2026-09-05T00:00:00Z"})]);
        let (gh, git, repo) = (FakeGh::default(), FakeGit::default(), FakeRepo::default());
        let mail = FakeMail::default();
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let now = 2_000_000_000;

        let log = gh_unlanded_scan(&d, &ctx(&run), now);
        assert!(log.iter().any(|l| l.contains("in flight (CERTIFIED)") && l.contains("waiting on landing")), "{log:?}");
        assert!(mail.sent.borrow().is_empty(), "no ask sent while in flight");

        // Immediately again: throttled, no second "waiting" line.
        let log2 = gh_unlanded_scan(&d, &ctx(&run), now + 10);
        assert!(log2.is_empty(), "{log2:?}");
    }

    #[test]
    fn unlanded_scan_respects_the_grace_period_for_a_just_closed_bead() {
        let tmp = testkit::TempDir::new("gh-intake-scan-grace");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        let now = 2_000_000_000i64;
        let closed_at = "2026-09-05T00:00:00Z";
        let age = now - super::parse_iso(closed_at).unwrap();
        assert!(age > 0);
        seed_closed_github(&bd, vec![serde_json::json!({"id":"sp-fresh","status":"closed","external_ref":"github:fixture/testrepo#99","labels":[],"closed_at":closed_at})]);
        let (gh, git, repo) = (FakeGh::default(), FakeGit::default(), FakeRepo::default());
        let mail = FakeMail::default();
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let mut c = ctx(&run);
        c.grace_secs = age + 10_000; // still inside the grace window

        let log = gh_unlanded_scan(&d, &c, now);
        assert!(log.is_empty(), "{log:?}");
        assert!(mail.sent.borrow().is_empty());
    }

    #[test]
    fn unlanded_scan_backfill_mirrors_closeout_via_its_own_ancestry_search() {
        let tmp = testkit::TempDir::new("gh-intake-backfill");
        let run = tmp.path().join("run");
        let bd = FakeBd::default();
        bd.show.borrow_mut().insert("sp-fix".into(), serde_json::json!({"external_ref": "github:fixture/testrepo#7"}));
        seed_closed_github(&bd, vec![serde_json::json!({"id":"sp-fix","status":"closed","external_ref":"github:fixture/testrepo#7","labels":["repo:fixture"],"closed_at":"2026-09-05T00:00:00Z"})]);
        let gh = FakeGh::default();
        gh.set_state("fixture/testrepo", "7", "OPEN");
        let git = FakeGit::default();
        git.seed_landed("/r", "sp-fix", "deadbeefcafe");
        let repo = FakeRepo::default();
        repo.add("fixture", "/r", "origin/main");
        let mail = FakeMail::default();
        let d = Deps { bd: &bd, gh: &gh, git: &git, repo: &repo, mail: &mail };
        let c = ctx(&run);

        let (dry_out, found, _closed, _skipped, would) = backfill(&d, &c, true);
        assert_eq!(found, 1);
        assert_eq!(would, 1);
        assert!(dry_out.iter().any(|l| l.contains("would close github:fixture/testrepo#7 (sp-fix) as deadbeef")), "{dry_out:?}");
        assert!(gh.comments.borrow().is_empty(), "dry-run makes no gh calls");

        let (out, found2, closed2, _skipped2, _would2) = backfill(&d, &c, false);
        assert_eq!(found2, 1);
        assert_eq!(closed2, 1);
        assert!(out.iter().any(|l| l.contains("closed github:fixture/testrepo#7")), "{out:?}");
        assert!(run_state::mark_exists(&run, "sp-fix"));
    }
}
