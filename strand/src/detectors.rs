//! The stranded-work detectors (wave4-decomposition.md family T, row 29, sp-8ofmt):
//! `detect_livelocked`, `detect_landed_but_open`, `detect_closed_unlanded_states`,
//! `detect_false_blockers`, `detect_incident_needs_builder`, `detect_invalid_closed` and
//! `all_partition_members`. The sentinel-side half of family T (`detect_unclaimable_ready`,
//! `file_unclaimable_incidents`, `detect_branch_collisions`, `park_branch_collisions`) is a
//! different bead ("two beads", decomposition table row T) and stays a lib.sh seam here —
//! [`unclaimable_lines`] reaches it the same way `sentinel`'s own Rust crate still does
//! (`bash -c '. lib.sh; detect_unclaimable_ready'`), never re-derived.
//!
//! OUTPUT IS BYTE-COMPATIBLE with the lib.sh functions this replaces: `groomer`,
//! `maechen-trigger` and `cockpit-collect` parse `LIVELOCK …`/`STATE …`/`INVALID-CLOSED …`/
//! `UNFILED-FOLLOW …` lines, and lib.sh's own functions become one-line shims onto this
//! crate's binary (`strand detect-livelocked`, …) so a caller that still sources lib.sh
//! directly (a test suite, `attempts.sh`) keeps working unchanged.
//!
//! GIT AND `landing-pass` ARE REACHED AS SUBPROCESSES, BY NAME ON PATH, never re-derived:
//! `landed`/`landed_sha` already has one canonical, tested implementation
//! (`landing-pass landed <id> <repo>`, family R, sp-81t4d); calling it is the same
//! "RETIRE rather than port" discipline as lib.sh's own `landed()` shim. `content_landed`
//! and the plain merge-tree clean check have no CLI door yet, so they run the same `git`
//! commands lib.sh's own (still-bash) `content_landed` runs — no new logic, just moved.
//! `detect_invalid_closed`'s statute-phrase predicates live in one file shared with aeon's
//! close-time fence (`close-reason-flags.py`, its own header: "both callers exec() this
//! file so the two cannot disagree") — this pipes the same embedded script into `python3`
//! rather than re-deriving the regexes in Rust, for the same reason.

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

use spira_config::repos::Registry;

use crate::config::Config;
use crate::model::{self, Bead};
use crate::probe;

/// `re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", s)[:max]` — every lib.sh title-sanitizer in this
/// family uses this exact character class and cutoff.
fn sanitize_title(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c == ' ' || c.is_ascii_alphanumeric() || "._/:,()#+-".contains(c) { c } else { ' ' })
        .collect();
    cleaned.chars().take(max).collect()
}

/// The first label value carrying `prefix` (`"repo:"`, `"branch:"`), or `None`.
fn label_value<'a>(labels: &'a [String], prefix: &str) -> Option<&'a str> {
    labels.iter().find_map(|l| l.strip_prefix(prefix))
}

fn registry(cfg: &Config) -> Registry {
    let home = cfg.home.clone().unwrap_or_default();
    Registry::from_env(std::env::vars().collect(), &home)
}

/// Best-effort `bd list`: a transient failure reads exactly as "no rows", matching every
/// bash caller's own `2>/dev/null` plus `[ -n "$raw" ] || return 0` guard — this family
/// never turns a flaky query into a propagated error.
fn list_beads(cfg: &Config, args: &[&str]) -> Vec<Bead> {
    let mut full = vec!["list"];
    full.extend_from_slice(args);
    full.extend(["--limit", "0", "--json"]);
    let raw = probe::bd(cfg, &full, None).unwrap_or_default();
    if raw.trim().is_empty() {
        return Vec::new();
    }
    model::parse_beads(&raw).unwrap_or_default()
}

// ──────────────────────────────────────────────────────────────────────────────
// git plumbing content_landed / merge-tree need (no CLI door onto either exists yet —
// lib.sh's own `content_landed` is still bash for the same reason).
// ──────────────────────────────────────────────────────────────────────────────

fn git_branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .current_dir(repo)
        .args(["show-ref", "--verify", "-q", &format!("refs/heads/{branch}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_rev_list_count(repo: &Path, range: &str) -> Option<u64> {
    let o = Command::new("git").current_dir(repo).args(["rev-list", "--count", range]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

fn git_is_ancestor(repo: &Path, a: &str, b: &str) -> bool {
    Command::new("git")
        .current_dir(repo)
        .args(["merge-base", "--is-ancestor", a, b])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_rev_parse(repo: &Path, rev: &str) -> Option<String> {
    let o = Command::new("git").current_dir(repo).args(["rev-parse", rev]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// `git merge-tree --write-tree <base> <branch>`'s exit status alone — a plain "would this
/// merge without conflict", distinct from [`content_landed`]'s tree-equality question.
fn merge_tree_clean(repo: &Path, base: &str, branch: &str) -> bool {
    Command::new("git")
        .current_dir(repo)
        .args(["merge-tree", "--write-tree", base, branch])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// lib.sh `content_landed <repo> <branch> <base>`, unchanged (still bash there — no CLI
/// door exists onto it yet): an ancestor branch is landed outright; otherwise a clean
/// merge whose resulting tree equals the base's own tree means the content is already there.
fn content_landed(repo: &Path, branch: &str, base: &str) -> bool {
    let Some(ahead) = git_rev_list_count(repo, &format!("{base}..{branch}")) else { return false };
    if git_is_ancestor(repo, branch, base) {
        return true;
    }
    if ahead == 0 {
        return false;
    }
    let Some(merged) = merge_tree_write_tree(repo, base, branch) else { return false };
    let Some(basetree) = git_rev_parse(repo, &format!("{base}^{{tree}}")) else { return false };
    merged == basetree
}

fn merge_tree_write_tree(repo: &Path, base: &str, branch: &str) -> Option<String> {
    let o = Command::new("git").current_dir(repo).args(["merge-tree", "--write-tree", base, branch]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let out = String::from_utf8_lossy(&o.stdout);
    let first = out.lines().next().unwrap_or("").to_string();
    (!first.is_empty()).then_some(first)
}

/// `landing-pass landed <id> <repo>` (family R, sp-81t4d): prints the sha on a found exit,
/// nothing on "not landed" or "cannot tell" — both of which this reads as `None`, exactly as
/// `landed "$id" "$r_path" 2>/dev/null && continue` only continues on the found exit.
fn landed_sha_via_cli(id: &str, repo_path: &str) -> Option<String> {
    let o = Command::new("landing-pass").args(["landed", id, repo_path]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

// ──────────────────────────────────────────────────────────────────────────────
// all_partition_members
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `all_partition_members`: every open/in_progress bead a partition's own labels
/// match, EVERY EXCLUSION DROPPED — the claimable-set predicate answers "can this be
/// claimed right now"; this answers "whose is it", which a poisoned or asked-about bead
/// still is. One id per line, first-seen order, deduplicated.
pub fn all_partition_members(cfg: &Config) -> String {
    let specs = probe::roster(cfg, None).unwrap_or_default();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for spec in specs {
        if spec.labels.is_empty() {
            continue;
        }
        for b in list_beads(cfg, &["--status", "open,in_progress", "--label", spec.labels.as_str()]) {
            if seen.insert(b.id.clone()) {
                out.push(b.id);
            }
        }
    }
    out.join("\n")
}

// ──────────────────────────────────────────────────────────────────────────────
// detect_landed_but_open
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `detect_landed_but_open`: one `STATE <id> landed-but-open — <evidence>` line for
/// every open/in_progress work bead whose repository's base already carries a commit
/// landing it — no partition filter, the whole graph (sp-0qp7s: a scan bounded to one
/// partition cannot see a bead filed under another).
pub fn detect_landed_but_open(cfg: &Config) -> String {
    let reg = registry(cfg);
    let home = reg.home_repo().to_string();
    let mut out = Vec::new();
    for b in list_beads(cfg, &["--status", "open,in_progress"]) {
        let ty = b.issue_type.as_deref().unwrap_or("");
        if !cfg.work_close_types.split_whitespace().any(|w| w == ty) {
            continue;
        }
        let repo = label_value(&b.labels, "repo:").unwrap_or(home.as_str()).to_string();
        let Some(root) = reg.root(&repo) else { continue };
        if let Some(sha) = landed_sha_via_cli(&b.id, &root) {
            if reopened_after_landing(cfg, &reg, &b.id, &root, &sha) {
                continue;
            }
            let sha_disp = if sha.is_empty() { "a commit".to_string() } else { sha };
            out.push(format!("STATE {} landed-but-open — {} names it on {}'s base; close it", b.id, sha_disp, repo));
        }
    }
    out.join("\n")
}

const UTC_STAMP: &str = "--date=format-local:%Y-%m-%dT%H:%M:%SZ";

/// Newest commit naming `id` on `base`, as a UTC `…Z` stamp (sorts lexicographically).
fn newest_naming_commit(repo: &Path, base: &str, id: &str, sha: &str) -> Option<String> {
    let stamp = |args: &[&str]| -> Option<String> {
        let o = Command::new("git").current_dir(repo).env("TZ", "UTC").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (o.status.success() && !s.is_empty()).then_some(s)
    };
    let pattern = format!("{}([^.a-z0-9]|$)", id);
    let by_sha = if sha.is_empty() { None } else { stamp(&["show", "-s", UTC_STAMP, "--format=%cd", sha]) };
    let by_grep = stamp(&["log", "-1", "-E", UTC_STAMP, "--format=%cd", &format!("--grep={pattern}"), base]);
    by_sha.into_iter().chain(by_grep).max()
}

/// The bead's last `reopened` event, as a UTC `…Z` stamp, from bd's own event table.
fn last_reopen(cfg: &Config, id: &str) -> Option<String> {
    let q = format!("select max(created_at) as t from events where issue_id='{}' and event_type='reopened'", id.replace('\'', ""));
    let raw = probe::bd(cfg, &["sql", "--json", q.as_str()], None).ok()?;
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    v.get(0)?.get("t")?.as_str().map(str::to_string)
}

/// A bead reopened after the newest commit naming it was reopened because that commit did
/// not fix it; only a later commit can make it landed again.
fn reopened_after_landing(cfg: &Config, reg: &Registry, id: &str, root: &str, sha: &str) -> bool {
    let Some(reopen) = last_reopen(cfg, id) else { return false };
    let Some((base, _)) = spira_config::repos::landrefs(reg, root) else { return false };
    match newest_naming_commit(Path::new(root), &base, id, sha) {
        Some(commit) => reopen > commit,
        None => false,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// detect_closed_unlanded_states
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `detect_closed_unlanded_states`: one STATE line per closed work bead, across
/// every partition this host watches, that carries none of CHECK 5's recognised landing
/// signals and that `landed()` cannot prove via the base's own commit graph.
pub fn detect_closed_unlanded_states(cfg: &Config) -> String {
    let specs = probe::roster(cfg, None).unwrap_or_default();
    let reg = registry(cfg);
    let home = reg.home_repo().to_string();

    let mut seen = HashSet::new();
    let mut candidates: Vec<(String, String, String)> = Vec::new();
    for spec in specs {
        if spec.labels.is_empty() {
            continue;
        }
        let excl: Vec<&str> = spec.exclude.split(',').filter(|s| !s.is_empty()).collect();
        for b in list_beads(cfg, &["--label", spec.labels.as_str(), "--status", "closed"]) {
            if !b.is_closed() {
                continue;
            }
            let ty = b.issue_type.as_deref().unwrap_or("");
            if !cfg.work_close_types.split_whitespace().any(|w| w == ty) {
                continue;
            }
            if excl.iter().any(|x| b.has(x)) {
                continue;
            }
            if b.labels.iter().any(|l| l.starts_with("delivers:")) {
                continue;
            }
            if b.has("spira-dropped") || b.has("content-landed") {
                continue;
            }
            if b.dependencies.iter().any(|d| d.dep_type.as_deref() == Some("supersedes")) {
                continue;
            }
            if b.assignee.as_deref().is_some_and(|a| !a.is_empty()) {
                continue;
            }
            if !seen.insert(b.id.clone()) {
                continue;
            }
            let repo = label_value(&b.labels, "repo:").unwrap_or("").to_string();
            let br = label_value(&b.labels, "branch:").unwrap_or("").to_string();
            candidates.push((b.id, repo, br));
        }
    }

    let mut out = Vec::new();
    for (id, repo, br) in candidates {
        let repo_disp = if repo.is_empty() { home.clone() } else { repo };
        let Some(root) = reg.root(&repo_disp) else { continue };
        let repo_path = Path::new(&root);
        if landed_sha_via_cli(&id, &root).is_some() {
            continue;
        }
        if br.is_empty() || !git_branch_exists(repo_path, &br) {
            let suffix = if br.is_empty() { String::new() } else { format!(" (branch: label {br} names no ref)") };
            out.push(format!("STATE {id} closed-no-branch — repo {repo_disp}{suffix}; nothing committed, no landing record"));
            continue;
        }
        let Some((base, _local)) = spira_config::repos::landrefs(&reg, &root) else { continue };
        if base.is_empty() {
            continue;
        }
        if content_landed(repo_path, &br, &base) {
            continue;
        }
        if merge_tree_clean(repo_path, &base, &br) {
            out.push(format!("STATE {id} closed-never-landed batch-ready {repo_disp} {br} {base} — merges cleanly, ready to requeue"));
        } else {
            out.push(format!("STATE {id} closed-never-landed conflict {repo_disp} {br} {base} — does not merge, needs a rebase"));
        }
    }
    out.join("\n")
}

// ──────────────────────────────────────────────────────────────────────────────
// detect_false_blockers
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `detect_false_blockers <blocker-ids>`: one `STATE <id> blocked-by-unlanded
/// <blocker> — …` line for every open/in_progress bead depending (`type=blocks`) on one of
/// `blockers` (space or newline separated). Empty input is a no-op, never a query.
pub fn detect_false_blockers(cfg: &Config, blockers: &str) -> String {
    let blockers: HashSet<&str> = blockers.split_whitespace().collect();
    if blockers.is_empty() {
        return String::new();
    }
    let mut out = Vec::new();
    for b in list_beads(cfg, &["--status", "open,in_progress"]) {
        let mut emitted: HashSet<&str> = HashSet::new();
        for target in b.blocks_targets() {
            if blockers.contains(target) && emitted.insert(target) {
                out.push(format!("STATE {} blocked-by-unlanded {} — depends on {}, which is closed but its work never landed", b.id, target, target));
            }
        }
    }
    out.join("\n")
}

// ──────────────────────────────────────────────────────────────────────────────
// detect_incident_needs_builder
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `detect_incident_needs_builder`: one `STATE <id> incident-is-code — …` line for
/// every open/in_progress bead carrying the incident label whose recorded `branch:` already
/// has a commit ahead of the repo's base.
pub fn detect_incident_needs_builder(cfg: &Config) -> Result<String, String> {
    if cfg.incident_label.is_empty() {
        return Err("SPIRA_INCIDENT_LABEL is unset — source conf.sh".into());
    }
    let reg = registry(cfg);
    let home = reg.home_repo().to_string();
    let mut out = Vec::new();
    for b in list_beads(cfg, &["--status", "open,in_progress", "--label", cfg.incident_label.as_str()]) {
        let Some(br) = label_value(&b.labels, "branch:") else { continue };
        let br = br.to_string();
        let repo = label_value(&b.labels, "repo:").unwrap_or("").to_string();
        let repo_disp = if repo.is_empty() { home.clone() } else { repo };
        let Some(root) = reg.root(&repo_disp) else { continue };
        let repo_path = Path::new(&root);
        if !git_branch_exists(repo_path, &br) {
            continue;
        }
        let Some((base, _local)) = spira_config::repos::landrefs(&reg, &root) else { continue };
        if base.is_empty() {
            continue;
        }
        let Some(ahead) = git_rev_list_count(repo_path, &format!("{base}..{br}")) else { continue };
        if ahead == 0 {
            continue;
        }
        out.push(format!("STATE {} incident-is-code — {} commit(s) already on {} ahead of {}; remaining work is a code change, not operational", b.id, ahead, br, base));
    }
    Ok(out.join("\n"))
}

// ──────────────────────────────────────────────────────────────────────────────
// detect_livelocked
// ──────────────────────────────────────────────────────────────────────────────

/// The sentinel-side half of family T (decomposition table: "Yes, in two beads") — not
/// ported here. `detect_unclaimable_ready` stays in lib.sh until that bead lands; this
/// reaches it exactly as `sentinel`'s own Rust crate still does (`seams::DETECT_UNCLAIMABLE`).
fn unclaimable_lines(cfg: &Config) -> String {
    let Some(home) = cfg.home.as_ref() else { return String::new() };
    let lib = home.join("lib.sh");
    if !lib.is_file() {
        return String::new();
    }
    let script = r#". "$0" >/dev/null 2>&1 || exit 97; detect_unclaimable_ready"#;
    let lib_s = lib.to_string_lossy().into_owned();
    // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own release's
    // bin/+spira/ on the CHILD's PATH, never only inherited.
    let extra = spira_config::release_env::child_path_env_for_process();
    let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let o = probe::run("bash", &["-c", script, lib_s.as_str()], None, &extra);
    if !o.ok {
        return String::new();
    }
    o.stdout
}

/// lib.sh `detect_livelocked`: one `LIVELOCK <id> <category> — <reason>` line per row, four
/// categories — `unclaimable` (reused, see [`unclaimable_lines`]), `ask-no-overseer`,
/// `ci-stuck`, `unmapped-repo`. Never fails outright: a sub-query that cannot run reads as
/// "nothing in that category", matching every bash `2>/dev/null` guard in the original.
pub fn detect_livelocked(cfg: &Config) -> String {
    let mut out = Vec::new();

    for line in unclaimable_lines(cfg).lines() {
        let Some(rest) = line.strip_prefix("UNCLAIMABLE ") else { continue };
        let bid = rest.split(' ').next().unwrap_or("");
        let reason = rest.split_once(" — ").map(|(_, r)| r).unwrap_or("");
        out.push(format!("LIVELOCK {bid} unclaimable — {reason}"));
    }

    for b in list_beads(cfg, &["--label", cfg.vocab.ask.as_str()]) {
        if !b.has(&cfg.vocab.ask) || b.has("overseer") {
            continue;
        }
        let title = sanitize_title(b.title.as_deref().unwrap_or(""), 60);
        out.push(format!(
            "LIVELOCK {} ask-no-overseer — missing overseer label; the decisions pane cannot see this bead and no aeon can claim it; add overseer label. title: {}",
            b.id, title
        ));
    }

    {
        let reg = registry(cfg);
        let home = reg.home_repo().to_string();
        for b in list_beads(cfg, &["--all", "--label", cfg.ci_label.as_str()]) {
            if b.is_closed() {
                continue;
            }
            let repo = label_value(&b.labels, "repo:").unwrap_or(home.as_str()).to_string();
            let title = sanitize_title(b.title.as_deref().unwrap_or(""), 60);
            if reg.land(&repo) != "pr" {
                out.push(format!(
                    "LIVELOCK {} ci-stuck — repo {} land mode is not pr; {} will never clear; strip the label or change the repo land mode. title: {}",
                    b.id, repo, cfg.ci_label, title
                ));
            }
        }

        if reg.map_present() {
            let valid: HashSet<String> = reg.names().into_iter().collect();
            for b in list_beads(cfg, &[]) {
                if b.has(&cfg.vocab.ask) || b.has(&cfg.groom_ask_label) || b.has(&cfg.vocab.poison) {
                    continue;
                }
                let repo_labels: Vec<&str> = b.labels.iter().filter_map(|l| l.strip_prefix("repo:")).collect();
                if repo_labels.is_empty() {
                    continue;
                }
                let bad: Vec<&str> = repo_labels.into_iter().filter(|r| !valid.contains(*r)).collect();
                if bad.is_empty() {
                    continue;
                }
                let title = sanitize_title(b.title.as_deref().unwrap_or(""), 60);
                out.push(format!(
                    "LIVELOCK {} unmapped-repo — repo:{} not in the repo map; aeon.sh refuses to claim it; fix the label or add the repo to the repo map. title: {}",
                    b.id,
                    bad.join(", "),
                    title
                ));
            }
        }
    }

    out.join("\n")
}

// ──────────────────────────────────────────────────────────────────────────────
// detect_invalid_closed
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `detect_invalid_closed`'s own embedded python, verbatim (close-reason-flags.py's
/// own header: "both callers exec() this file so the two cannot disagree" — this pipes the
/// SAME script through `python3` rather than re-deriving the regexes in Rust).
const INVALID_CLOSED_SCRIPT: &str = r#"
import sys, json, re, os

# Load detect_close_reason and check_unfiled_follow from the shared helper. The detector
# uses detect_close_reason (raw scan, no masking) so all occurrences reach Maechen;
# check_close_reason (quote-masked) belongs to the close-time fence in aeon.sh.
_flags_path = os.path.join(os.environ.get("SPIRA_HOME", ""), "close-reason-flags.py")
try:
    _ns = {"re": re, "__name__": ""}
    exec(open(_flags_path).read(), _ns)
    check_close_reason = _ns.get("detect_close_reason") or _ns["check_close_reason"]
    check_unfiled_follow = _ns["check_unfiled_follow"]
except Exception:
    check_close_reason = lambda r: None
    check_unfiled_follow = lambda r, id_prefix="sp": None

_id_prefix = os.environ.get("SPIRA_ID_PREFIX", "sp")

# ALLOWLIST. Beads whose id appears in $SPIRA_RUN/invalid-closed.allow are reported
# as ALLOWED-IC (not counted) rather than as INVALID-CLOSED or UNFILED-FOLLOW. Each
# line in the allowlist is "<id> <reason>" — the reason is what Maechen recorded when
# it judged the row a false positive (quotation) or resolved it (follow-up filed).
allowlist = {}
_run = os.environ.get("SPIRA_RUN", "")
_allow_path = os.path.join(_run, "invalid-closed.allow") if _run else ""
if _allow_path and os.path.isfile(_allow_path):
    with open(_allow_path) as _af:
        for _line in _af:
            _parts = _line.strip().split(None, 1)
            if _parts and re.match(r"^[a-z0-9]+(?:-[a-z0-9]+)+$", _parts[0]):
                allowlist[_parts[0]] = _parts[1] if len(_parts) > 1 else ""

try: d = json.load(sys.stdin)
except Exception: raise SystemExit
for i in (d if isinstance(d, list) else [d]):
    bid = i["id"]
    reason = i.get("close_reason") or ""
    title = re.sub(r"[^ A-Za-z0-9._/:,()#+-]", " ", (i.get("title") or ""))[:60]
    reason_short = re.sub(r"\s+", " ", reason.strip())[:120]

    if bid in allowlist:
        print("ALLOWED-IC %s — %s" % (bid, allowlist[bid]))
        continue

    hit = check_close_reason(reason)
    if hit:
        print("INVALID-CLOSED %s — close reason contains %r: %s. title: %s" % (
            bid, hit, reason_short, title))
        continue

    follow_hit = check_unfiled_follow(reason, _id_prefix)
    if follow_hit:
        print("UNFILED-FOLLOW %s — follow-on phrase %r without a tracking reference: %s. title: %s" % (
            bid, follow_hit, reason_short, title))
"#;

/// lib.sh `detect_invalid_closed`: `INVALID-CLOSED`/`UNFILED-FOLLOW`/`ALLOWED-IC` lines for
/// closed beads whose close reasons admit unfinished work or imply untracked follow-on work.
pub fn detect_invalid_closed(cfg: &Config) -> String {
    let mut args: Vec<&str> = vec!["list", "--status", "closed", "--limit", "0", "--json"];
    if !cfg.scope_label.is_empty() {
        args = vec!["list", "--status", "closed", "--label", &cfg.scope_label, "--limit", "0", "--json"];
    }
    let raw = probe::bd(cfg, &args, None).unwrap_or_default();
    if raw.trim().is_empty() {
        return String::new();
    }
    let home = cfg.home.clone().unwrap_or_default().to_string_lossy().into_owned();
    let run = cfg.run.clone().unwrap_or_default().to_string_lossy().into_owned();
    let o = probe::run(
        "python3",
        &["-c", INVALID_CLOSED_SCRIPT],
        Some(raw.as_bytes()),
        &[("SPIRA_HOME", home.as_str()), ("SPIRA_ID_PREFIX", cfg.id_prefix.as_str()), ("SPIRA_RUN", run.as_str())],
    );
    if !o.ok {
        return String::new();
    }
    o.stdout.trim_end_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_title_replaces_disallowed_characters_and_cuts_at_max() {
        assert_eq!(sanitize_title("hi \"there\" <x>", 20), "hi  there   x ");
        assert_eq!(sanitize_title(&"x".repeat(100), 5), "xxxxx");
    }

    #[test]
    fn label_value_finds_the_first_match() {
        let labels = vec!["repo:spira".to_string(), "branch:sp-1".to_string()];
        assert_eq!(label_value(&labels, "repo:"), Some("spira"));
        assert_eq!(label_value(&labels, "team:"), None);
    }

    fn git(dir: &Path, date: &str, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(dir)
            .env("GIT_COMMITTER_DATE", date)
            .env("GIT_AUTHOR_DATE", date)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn newest_naming_commit_finds_the_latest_exact_id_and_orders_against_a_reopen() {
        let tmp = testkit::TempDir::new("strand-nnc");
        let dir = tmp.path().to_path_buf();
        git(&dir, "2026-10-02T10:00:00Z", &["init", "-q", "-b", "main"]);
        git(&dir, "2026-10-02T10:00:00Z", &["commit", "-q", "--allow-empty", "-m", "sp-abc: first"]);
        git(&dir, "2026-10-02T12:00:00Z", &["commit", "-q", "--allow-empty", "-m", "sp-abcd: other bead"]);
        let first = newest_naming_commit(&dir, "main", "sp-abc", "").unwrap();
        assert_eq!(first, "2026-10-02T10:00:00Z");
        assert!("2026-10-02T11:00:00Z".to_string() > first, "reopened after the commit: not landed");
        git(&dir, "2026-10-02T13:00:00Z", &["commit", "-q", "--allow-empty", "-m", "sp-abc: real fix"]);
        let later = newest_naming_commit(&dir, "main", "sp-abc", "").unwrap();
        assert_eq!(later, "2026-10-02T13:00:00Z");
        assert!("2026-10-02T11:00:00Z".to_string() < later, "commit newer than the reopen: landed");
    }

    #[test]
    fn detect_false_blockers_is_a_noop_on_empty_input() {
        let cfg = Config::resolve(&crate::config::Live::load());
        assert_eq!(detect_false_blockers(&cfg, ""), "");
        assert_eq!(detect_false_blockers(&cfg, "   "), "");
    }
}
