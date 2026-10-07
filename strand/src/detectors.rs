//! The stranded-work detectors (wave4-decomposition.md family T, row 29, sp-8ofmt):
//! `detect_livelocked`, `detect_incident_needs_builder`, `detect_invalid_closed` and
//! `all_partition_members` (`detect_landed_but_open`, `detect_closed_unlanded_states` and
//! `detect_false_blockers` were deleted in sp-mve9i — see their section below). The sentinel-side half of family T (`detect_unclaimable_ready`,
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
//! GIT AND `spira-lc` ARE REACHED AS SUBPROCESSES, BY NAME ON PATH, never re-derived:
//! "landed" is the lifecycle record's LANDED state (`spira-lc state <id>`). Content-on-base is
//! the Sending's own proof (`sending::git::Git::content_on_base`, the same answer as
//! `spira-lc content-landed`), called in-process; the plain merge-tree clean check runs `git`.
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
// git plumbing the detectors need. The content-on-base proof itself is the Sending's own
// (`sending::git::Git::content_on_base`), not a copy here.
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

// ──────────────────────────────────────────────────────────────────────────────
// all_partition_members
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `all_partition_members`: every unfinished bead a partition's own labels match,
/// EVERY EXCLUSION DROPPED — the claimable-set predicate answers "can this be claimed right
/// now"; this answers "whose is it", which a poisoned or asked-about bead still is. One id
/// per line, first-seen order, deduplicated. "Unfinished" is the lifecycle row's answer
/// (READY, WORKING or REWORK — what bd's `open,in_progress` meant), never bd's status
/// (sp-mve9i); with the machine unreadable there is no answer, so no members.
pub fn all_partition_members(cfg: &Config) -> String {
    let specs = probe::roster(cfg, None).unwrap_or_default();
    let Ok(lc) = spira_config::lc_state::list().map(spira_config::lc_state::index) else {
        return String::new();
    };
    let per_spec = specs
        .iter()
        .filter(|spec| !spec.labels.is_empty())
        .map(|spec| list_beads(cfg, &["--all", "--label", spec.labels.as_str()]).into_iter().map(|b| b.id).collect())
        .collect();
    unfinished_members(per_spec, &lc).join("\n")
}

/// The ids, first-seen and deduplicated across `per_spec`, whose lifecycle row is still
/// before its builder's hand-off. A bead with no row can never be worked: not a member.
pub fn unfinished_members(per_spec: Vec<Vec<String>>, lc: &std::collections::HashMap<String, spira_config::lc_state::Row>) -> Vec<String> {
    let mut seen = HashSet::new();
    per_spec
        .into_iter()
        .flatten()
        .filter(|id| lc.get(id).is_some_and(|r| !r.past_builder()))
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────

// detect_landed_but_open, detect_closed_unlanded_states and detect_false_blockers are
// deleted (sp-mve9i): each was a disagreement between bd's `status` and the lifecycle
// record (open-but-LANDED, closed-but-not-LANDED, and the dependents of the latter). bd's
// status is inert for work beads (design §3.4), so the disagreement is no longer a state a
// bead can be in; CHECK5-LC (sentinel/src/lifecycle.rs) reads the drifts that remain from the
// spira-lc rows alone. The groomer stopped calling them in sp-jnwbn.

// ──────────────────────────────────────────────────────────────────────────────
// detect_incident_needs_builder
// ──────────────────────────────────────────────────────────────────────────────

/// lib.sh `detect_incident_needs_builder`: one `STATE <id> incident-is-code — …` line for
/// every unfinished bead carrying the incident label whose recorded `branch:` already has a
/// commit ahead of the repo's base. An incident bead is a work bead (sp-jgjvh): bd supplies
/// the labelled beads' content, and "unfinished" (what bd's open/in_progress meant) is the
/// lifecycle row's READY/WORKING/REWORK — [`unfinished_incidents`]. A machine that cannot
/// answer is an Err, never "no incident".
pub fn detect_incident_needs_builder(cfg: &Config) -> Result<String, String> {
    if cfg.incident_label.is_empty() {
        return Err("SPIRA_INCIDENT_LABEL is unset — source conf.sh".into());
    }
    let lc = spira_config::lc_state::list().map(spira_config::lc_state::index).map_err(|e| format!("cannot tell: {e}"))?;
    let reg = registry(cfg);
    let home = reg.home_repo().to_string();
    let mut out = Vec::new();
    let labelled = list_beads(cfg, &["--all", "--label", cfg.incident_label.as_str()]);
    for b in unfinished_incidents(labelled, &lc) {
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

/// The incident beads whose lifecycle row still owes builder work; a bead with no row is not
/// live work (it can never be claimed; CHECK-ROWLESS reports it).
pub fn unfinished_incidents(beads: Vec<Bead>, lc: &std::collections::HashMap<String, spira_config::lc_state::Row>) -> Vec<Bead> {
    beads.into_iter().filter(|b| lc.get(&b.id).is_some_and(|r| !r.past_builder())).collect()
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
            // Held is the lifecycle row's ask/poison hold, never a label (sp-psztcc).
            let held: HashSet<String> = spira_config::lc_state::list()
                .map(|rows| rows.into_iter().filter(|r| r.held("ask") || r.held("poison")).map(|r| r.bead_id).collect())
                .unwrap_or_default();
            for b in list_beads(cfg, &[]) {
                if held.contains(&b.id) || b.has(&cfg.groom_ask_label) {
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

/// The rows of a `bd list --json` array that carry a non-empty `close_reason`, re-serialised
/// for the embedded script; the input unchanged when it is not an array.
fn with_close_reason(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw.trim()) {
        Ok(serde_json::Value::Array(a)) => serde_json::Value::Array(
            a.into_iter()
                .filter(|r| r.get("close_reason").and_then(|v| v.as_str()).is_some_and(|v| !v.trim().is_empty()))
                .collect(),
        )
        .to_string(),
        _ => raw.to_string(),
    }
}

/// lib.sh `detect_invalid_closed`: `INVALID-CLOSED`/`UNFILED-FOLLOW`/`ALLOWED-IC` lines for
/// closed beads whose close reasons admit unfinished work or imply untracked follow-on work.
pub fn detect_invalid_closed(cfg: &Config) -> String {
    let mut args: Vec<&str> = vec!["list", "--all", "--limit", "0", "--json"];
    if !cfg.scope_label.is_empty() {
        args = vec!["list", "--all", "--label", &cfg.scope_label, "--limit", "0", "--json"];
    }
    let raw = probe::bd(cfg, &args, None).unwrap_or_default();
    if raw.trim().is_empty() {
        return String::new();
    }
    // The close records are the subject: a bead that carries a close reason, whatever bd's
    // status says (sp-mve9i: bd status is not read for a work bead; the reason is content).
    let raw = with_close_reason(&raw);
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


    /// sp-mve9i: a partition's members are the beads whose lifecycle row is unfinished —
    /// READY, WORKING, REWORK — deduplicated in first-seen order; no row, no member.
    #[test]
    fn partition_members_are_the_unfinished_lifecycle_rows() {
        use spira_config::lc_state::Row;
        let lc = [("a", "READY"), ("b", "WORKING"), ("c", "SUBMITTED"), ("d", "REWORK"), ("e", "LANDED")]
            .iter()
            .map(|(i, st)| (i.to_string(), Row { bead_id: i.to_string(), state: st.to_string(), ..Default::default() }))
            .collect();
        let per = vec![vec!["b".into(), "c".into(), "a".into()], vec!["a".into(), "d".into(), "e".into(), "z".into()]];
        assert_eq!(unfinished_members(per, &lc), vec!["b", "a", "d"]);
    }

    /// sp-jgjvh: an incident is unfinished by its lifecycle row, whatever bd's status says;
    /// a rowless bead is not live work.
    #[test]
    fn incidents_needing_a_builder_are_the_unfinished_lifecycle_rows() {
        use spira_config::lc_state::Row;
        let beads = model::parse_beads(
            r#"[{"id":"a","status":"closed"},{"id":"b","status":"open"},{"id":"c","status":"in_progress"},{"id":"d","status":"open"}]"#,
        )
        .unwrap();
        let lc = [("a", "WORKING"), ("b", "SUBMITTED"), ("c", "REWORK")]
            .iter()
            .map(|(i, st)| (i.to_string(), Row { bead_id: i.to_string(), state: st.to_string(), ..Default::default() }))
            .collect();
        let ids: Vec<String> = unfinished_incidents(beads, &lc).into_iter().map(|b| b.id).collect();
        assert_eq!(ids, vec!["a", "c"]);
    }

    #[test]
    fn invalid_closed_reads_the_rows_that_carry_a_close_reason() {
        let out = with_close_reason(r#"[{"id":"a","status":"open","close_reason":"done"},{"id":"b","status":"closed","close_reason":""},{"id":"c","status":"closed"}]"#);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["id"], "a");
        assert_eq!(with_close_reason("not json"), "not json");
    }
}
